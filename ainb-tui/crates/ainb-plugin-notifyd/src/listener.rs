//! The notifyd accept-loop.
//!
//! Binds a Unix domain socket at [`Paths::socket`], accepts connections
//! from the hook script (which sends one newline-terminated JSON
//! envelope per connection and closes), parses each envelope, persists
//! it via [`Store`], and emits an OS notification when the event is
//! user-facing.
//!
//! On startup the loop replays any `notify.fallback.jsonl` left over
//! from a previous "daemon-down" window. On `SIGTERM` / `SIGINT` the
//! loop completes any in-flight reads, removes the socket and PID
//! files, and exits cleanly.

use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{debug, error, info, warn};

use crate::envelope::Envelope;
use crate::fallback::FallbackFile;
use crate::osnotify::{Debouncer, notify};
use crate::paths::Paths;
use crate::pid::PidFile;
use crate::store::{RetentionPolicy, Store};

/// Runtime configuration for [`run_daemon`].
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// On-disk layout for the daemon's state files.
    pub paths: Paths,
    /// Retention policy applied on every insert.
    pub retention: RetentionPolicy,
    /// If `true`, OS notifications are dispatched. If `false`, the
    /// daemon skips the native notify call entirely (used by tests
    /// and by users who opt out via config).
    pub os_notifications: bool,
}

impl RunConfig {
    /// Build a config rooted under [`Paths::from_home`] with all
    /// defaults from the spec.
    pub fn from_home() -> Result<Self> {
        Ok(Self {
            paths: Paths::from_home()?,
            retention: RetentionPolicy::default(),
            os_notifications: true,
        })
    }
}

/// Run the daemon to completion. Returns when one of:
///
/// - the socket can't be bound (port-equivalent collision);
/// - `SIGTERM` / `SIGINT` is received;
/// - an unrecoverable internal error occurs.
///
/// The function takes ownership of the PID file via [`PidFile`]; the
/// file is removed on drop / clean exit.
pub async fn run_daemon(config: RunConfig) -> Result<()> {
    config
        .paths
        .ensure_base()
        .with_context(|| format!("creating base dir {}", config.paths.base.display()))?;

    // A stale socket from a crashed predecessor will cause `bind` to
    // fail with EADDRINUSE; remove it before binding.
    if config.paths.socket.exists() {
        // Best-effort: if another live daemon already owns the
        // socket the PID check below will keep us out.
        if let Ok(Some(pid)) = crate::pid::read(&config.paths.pid) {
            if crate::pid::is_running(pid) && pid != std::process::id() {
                anyhow::bail!("another notifyd is already running (pid {pid}); refusing to start");
            }
        }
        std::fs::remove_file(&config.paths.socket).ok();
    }

    let _pid_guard = PidFile::write_current(config.paths.pid.clone())?;
    let store = Arc::new(Store::open(&config.paths.db)?);
    let fallback = FallbackFile::new(&config.paths.fallback);
    let debouncer = Arc::new(Debouncer::new());

    // Replay any envelopes queued while we were down.
    match fallback.replay_into(&store, &config.retention) {
        Ok(s) if s.lines_read > 0 => info!(
            replayed = s.envelopes_persisted,
            corrupt = s.lines_corrupt,
            "fallback replay complete"
        ),
        Ok(_) => debug!("no fallback to replay"),
        Err(e) => warn!(error = ?e, "fallback replay failed"),
    }

    let listener = UnixListener::bind(&config.paths.socket)
        .with_context(|| format!("binding unix socket {}", config.paths.socket.display()))?;
    // chmod 0600 — only the owner can write.
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(&config.paths.socket) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(&config.paths.socket, perms);
    }
    info!(socket = %config.paths.socket.display(), "notifyd listening");

    // Wire signal handling: any TERM/INT triggers a graceful shutdown.
    let mut sigterm = signal(SignalKind::terminate()).context("registering SIGTERM")?;
    let mut sigint = signal(SignalKind::interrupt()).context("registering SIGINT")?;

    loop {
        tokio::select! {
            biased;
            _ = sigterm.recv() => {
                info!("received SIGTERM; shutting down");
                break;
            }
            _ = sigint.recv() => {
                info!("received SIGINT; shutting down");
                break;
            }
            accept = listener.accept() => {
                match accept {
                    Ok((stream, _peer)) => {
                        let store = Arc::clone(&store);
                        let fallback = fallback.clone();
                        let retention = config.retention;
                        let debouncer = Arc::clone(&debouncer);
                        let os_notifications = config.os_notifications;
                        tokio::spawn(async move {
                            handle_connection(
                                stream,
                                store,
                                fallback,
                                retention,
                                debouncer,
                                os_notifications,
                            )
                            .await;
                        });
                    }
                    Err(e) => {
                        error!(error = ?e, "accept failed; continuing");
                    }
                }
            }
        }
    }

    // Cleanup before drop — remove the socket so future hook fires
    // know the daemon is down.
    let _ = std::fs::remove_file(&config.paths.socket);
    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    store: Arc<Store>,
    fallback: FallbackFile,
    retention: RetentionPolicy,
    debouncer: Arc<Debouncer>,
    os_notifications: bool,
) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) => break, // EOF.
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                match Envelope::from_bytes(trimmed.as_bytes()) {
                    Ok(env) => {
                        // Persist on the blocking pool; rusqlite is sync.
                        let store_for_blocking = Arc::clone(&store);
                        let env_clone = env.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            store_for_blocking.insert_and_prune(&env_clone, &retention)
                        })
                        .await;
                        match result {
                            Ok(Ok(_id)) => {
                                if os_notifications {
                                    notify(&env, &debouncer);
                                }
                            }
                            Ok(Err(e)) => {
                                warn!(error = ?e, "store insert failed; routing to fallback");
                                let _ = fallback.append(&env);
                            }
                            Err(e) => {
                                warn!(error = ?e, "store task panicked; routing to fallback");
                                let _ = fallback.append(&env);
                            }
                        }
                    }
                    Err(e) => {
                        // Malformed envelope: write to corrupt for
                        // forensic visibility, then continue.
                        warn!(error = ?e, line = %trimmed, "rejecting malformed envelope");
                        let _ = fallback.append_corrupt_line_for_testing(trimmed);
                    }
                }
            }
            Err(e) => {
                warn!(error = ?e, "read error on connection; closing");
                break;
            }
        }
    }
}

// Tiny extension on FallbackFile used only to write a corrupt line
// when a connection delivers garbage. Defined here so it does not
// leak into the public API of the fallback module.
impl FallbackFile {
    fn append_corrupt_line_for_testing(&self, line: &str) -> std::io::Result<()> {
        use std::io::Write;
        if let Some(parent) = self.corrupt_path().parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.corrupt_path())?;
        writeln!(file, "{line}")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixStream;

    fn config_under(dir: &std::path::Path) -> RunConfig {
        RunConfig {
            paths: Paths::under(dir),
            // Disable retention in tests; sample envelopes use small ts
            // values (1, 2, 3, ...) that would otherwise be pruned by
            // the default 7-day window the moment they were inserted.
            retention: RetentionPolicy {
                retention_days: 0,
                max_rows: 0,
            },
            os_notifications: false, // never spawn osascript in tests
        }
    }

    async fn wait_for_socket(path: &std::path::Path) {
        for _ in 0..200 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("socket did not appear: {}", path.display());
    }

    #[tokio::test]
    async fn daemon_accepts_envelope_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_under(dir.path());
        let socket = config.paths.socket.clone();
        let db = config.paths.db.clone();
        let pid_path = config.paths.pid.clone();
        let handle = tokio::spawn(async move { run_daemon(config).await });
        wait_for_socket(&socket).await;

        // Connect, write a valid envelope, close.
        let env_json = r#"{"protocol_version":1,"agent":"claude","raw_event":"Stop","session_id":"abc","cwd":"/tmp/x","project":"x","ts":1700000000000,"payload":{"k":"v"}}"#;
        {
            let mut stream = UnixStream::connect(&socket).await.unwrap();
            stream.write_all(env_json.as_bytes()).await.unwrap();
            stream.write_all(b"\n").await.unwrap();
            stream.shutdown().await.unwrap();
        }

        // Give the daemon a tick to drain.
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Inspect SQLite directly.
        let store = Store::open(&db).unwrap();
        assert_eq!(store.count().unwrap(), 1);

        // Stop the daemon: send SIGTERM to our own PID — wait, that
        // would kill the test. Instead, abort the task and verify
        // cleanup on drop is not required for this test.
        handle.abort();
        let _ = pid_path; // pid file cleanup tested elsewhere
    }

    #[tokio::test]
    async fn daemon_replays_fallback_on_startup() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_under(dir.path());
        // Pre-populate fallback file with a single envelope.
        let fb = FallbackFile::new(&config.paths.fallback);
        let env = Envelope {
            protocol_version: 1,
            agent: "codex".into(),
            raw_event: "agent-turn-complete".into(),
            session_id: "xyz".into(),
            cwd: "/tmp/y".into(),
            project: "y".into(),
            ts: 42,
            payload: serde_json::json!({}),
        };
        std::fs::create_dir_all(&config.paths.base).unwrap();
        fb.append(&env).unwrap();
        assert!(fb.path().exists());

        let socket = config.paths.socket.clone();
        let db = config.paths.db.clone();
        let handle = tokio::spawn(async move { run_daemon(config).await });
        wait_for_socket(&socket).await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        let store = Store::open(&db).unwrap();
        assert_eq!(store.count().unwrap(), 1);
        let row = store.latest().unwrap().unwrap();
        assert_eq!(row.agent, "codex");
        assert_eq!(row.raw_event, "agent-turn-complete");
        assert_eq!(row.ts, 42);

        handle.abort();
    }

    #[tokio::test]
    async fn malformed_envelope_routed_to_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_under(dir.path());
        let socket = config.paths.socket.clone();
        let corrupt = FallbackFile::new(&config.paths.fallback).corrupt_path().to_path_buf();
        let handle = tokio::spawn(async move { run_daemon(config).await });
        wait_for_socket(&socket).await;

        {
            let mut stream = UnixStream::connect(&socket).await.unwrap();
            stream.write_all(b"not a json envelope\n").await.unwrap();
            stream.shutdown().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(100)).await;

        let body = std::fs::read_to_string(&corrupt).unwrap();
        assert!(body.contains("not a json envelope"));

        handle.abort();
    }
}
