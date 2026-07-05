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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal::unix::{SignalKind, signal};
use tracing::{debug, error, info, warn};

use crate::envelope::Envelope;
use crate::fallback::FallbackFile;
use crate::osnotify::{Debouncer, NativeTransport, notify};
use crate::paths::Paths;
use crate::pid::PidFile;
use crate::store::{RetentionPolicy, Store};

/// After this many consecutive failures of a background loop, escalate the log
/// from `warn!` to `error!` — a single transient blip is expected and retried
/// quietly, but a sustained streak means the loop is effectively dead (poisoned
/// store, gone disk) and must be loud enough for a supervisor / operator to
/// restart the daemon rather than the failure being invisible.
const ESCALATE_AFTER_CONSECUTIVE: u32 = 5;

/// Run the events-log prune every N ingest ticks (not every tick — pruning is
/// pure overhead when the log is under the cap). At the 1.5s production cadence
/// this is roughly every ~5 minutes.
const EVENTS_PRUNE_EVERY_TICKS: u32 = 200;

/// Log a background-loop failure, escalating to `error!` once the consecutive
/// streak crosses [`ESCALATE_AFTER_CONSECUTIVE`]. Below the threshold it is a
/// `warn!`; at/after it, an `error!` carrying the streak length so the loud
/// line is unmistakable to a supervisor scraping logs.
fn escalate(consecutive: u32, what: &str, error: &str) {
    if consecutive >= ESCALATE_AFTER_CONSECUTIVE {
        error!(
            consecutive_failures = consecutive,
            what, error, "background loop failing repeatedly; daemon may need restart"
        );
    } else {
        warn!(what, error, "background loop failed; will retry");
    }
}

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
    /// How often the ingest tailer folds `events.jsonl` into the
    /// `events` table. Short (agent-deck cadence) so event-sourced
    /// state stays fresh; tests use a tiny value to converge fast.
    pub ingest_interval: std::time::Duration,
    /// How often the transition loop materializes `current_state` from
    /// the `events` log. Same short agent-deck cadence as `ingest_interval`
    /// so a freshly-ingested event becomes queryable state within ~2 ticks.
    pub materialize_interval: std::time::Duration,
}

impl RunConfig {
    /// Build a config rooted under [`Paths::from_home`] with all
    /// defaults from the spec.
    pub fn from_home() -> Result<Self> {
        Ok(Self {
            paths: Paths::from_home()?,
            retention: RetentionPolicy::default(),
            os_notifications: true,
            ingest_interval: std::time::Duration::from_millis(1500),
            materialize_interval: std::time::Duration::from_millis(1500),
        })
    }
}

/// A `JoinHandle` that aborts its task on drop. Keeps the ingest tailer
/// (and, in Wave 3, the materializer) bounded to the daemon's lifetime:
/// when `run_daemon` returns and the handle drops, the background loop
/// is cancelled rather than leaked.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Exclusive startup ownership lock (M-D3).
///
/// The pre-existing startup sequence — "if the pid is dead, remove the stale
/// socket and bind" — is NOT atomic, and the PID file (`File::create`, which
/// truncates rather than excludes) does not keep a racer out. Two daemons
/// starting at the same instant can both observe a dead pid, both `remove_file`
/// the socket, and both `bind`, leaving the loser silently un-served.
///
/// This guard makes ownership exclusive BEFORE any socket mutation: it creates a
/// `notify.lock` file with `O_EXCL` (`create_new`), which the kernel guarantees
/// succeeds for exactly one creator. If the lock already exists we read the pid
/// it records; a LIVE different pid means a real daemon owns startup (we bail),
/// while a DEAD pid means the lock is stale (a crashed predecessor) and we
/// recover it by removing and retrying the exclusive create once. The winner
/// holds the lock for its whole lifetime and removes it on drop.
#[derive(Debug)]
struct StartupLock {
    path: PathBuf,
}

impl StartupLock {
    /// Acquire the exclusive startup lock at `path`, recovering a stale lock left
    /// by a crashed predecessor. Returns an error if a *live* daemon already owns
    /// it (the caller should bail without touching the socket).
    fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Two attempts: the first may lose to a stale lock we then recover.
        for attempt in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true) // O_EXCL: exactly one creator wins.
                .open(path)
            {
                Ok(mut f) => {
                    // We won. Record our pid so a future racer can liveness-check us.
                    let _ = writeln!(f, "{}", std::process::id());
                    return Ok(Self {
                        path: path.to_path_buf(),
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Someone holds (or crashed holding) the lock. Decide which.
                    let holder = std::fs::read_to_string(path)
                        .ok()
                        .and_then(|s| s.trim().parse::<u32>().ok());
                    match holder {
                        Some(pid) if crate::pid::is_running(pid) && pid != std::process::id() => {
                            anyhow::bail!(
                                "another notifyd is starting/running (lock pid {pid}); refusing to start"
                            );
                        }
                        // Dead/unreadable holder → stale lock from a crash. Recover
                        // it and retry the exclusive create exactly once.
                        _ if attempt == 0 => {
                            warn!(?path, "removing stale startup lock");
                            std::fs::remove_file(path).ok();
                            continue;
                        }
                        // Still contended after recovery → a genuine race we lost.
                        _ => anyhow::bail!(
                            "lost startup race for {} after stale-lock recovery",
                            path.display()
                        ),
                    }
                }
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("creating startup lock {}", path.display()));
                }
            }
        }
        unreachable!("startup lock acquire loop always returns within 2 attempts")
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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

    // M-D3: claim exclusive startup ownership BEFORE touching the socket. This
    // O_EXCL lock (with stale-lock recovery) guarantees exactly one daemon
    // proceeds to the non-atomic remove-stale-socket + bind sequence below, so
    // two daemons racing startup can no longer both remove + bind. Held for the
    // process lifetime; removed on drop.
    let _startup_lock = StartupLock::acquire(&config.paths.spawn_lock)?;

    // A stale socket from a crashed predecessor will cause `bind` to
    // fail with EADDRINUSE; remove it before binding. The startup lock above
    // already excludes a live competitor, but keep the PID cross-check as a
    // second, independent signal before we delete another process's socket.
    if config.paths.socket.exists() {
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

    // INGEST TAILER (Wave 2): fold the durable `events.jsonl` (appended
    // by the lifecycle hook) into the SQLite `events` log. This is the
    // event-sourcing intake — daemon-down-safe (the hook appends whether
    // or not the daemon is up) and crash-safe (a persisted byte offset
    // means a restart catches up the un-ingested suffix without
    // re-reading already-offset bytes; a partial trailing line is left
    // for the next pass). Runs on a short ticker like the
    // `cli/fleet/daemon.rs` loop; SQLite is sync behind the store Mutex,
    // so each pass runs on the blocking pool via `spawn_blocking`. The
    // task is aborted on shutdown when this handle drops.
    let _ingest_task = {
        let store = Arc::clone(&store);
        let events_jsonl = config.paths.events_jsonl.clone();
        let interval = config.ingest_interval;
        let retention = config.retention;
        AbortOnDrop(tokio::spawn(async move {
            // Consecutive-failure counter: a single transient failure is a
            // `warn!` + retry, but a SUSTAINED failure streak (e.g. the store
            // mutex was poisoned and recovery is also failing, or the disk is
            // gone) is escalated to `error!` so it is loud enough for a
            // supervisor / operator to act on rather than dying invisibly while
            // the socket keeps accepting. The mutex poison itself is recovered
            // in `Store::conn`, so this guards the residual "still broken" case.
            let mut consecutive_failures: u32 = 0;
            // Prune the events log occasionally (not every tick) to bound its
            // growth, mirroring the notifications retention sweep.
            let mut ticks_since_prune: u32 = 0;
            loop {
                let store_pass = Arc::clone(&store);
                let path = events_jsonl.clone();
                match tokio::task::spawn_blocking(move || {
                    crate::ingest::ingest_once(&store_pass, &path)
                })
                .await
                {
                    Ok(Ok(summary)) => {
                        consecutive_failures = 0;
                        if summary.events_ingested > 0
                            || summary.lines_corrupt > 0
                            || summary.reset_for_rotation
                        {
                            debug!(
                                ingested = summary.events_ingested,
                                corrupt = summary.lines_corrupt,
                                offset = summary.offset,
                                more_pending = summary.more_pending,
                                reset_for_rotation = summary.reset_for_rotation,
                                "events.jsonl ingest pass"
                            );
                        }
                    }
                    Ok(Err(e)) => {
                        consecutive_failures += 1;
                        escalate(
                            consecutive_failures,
                            "events.jsonl ingest",
                            &format!("{e:?}"),
                        );
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        escalate(consecutive_failures, "ingest task", &format!("{e:?}"));
                    }
                }

                // Periodic events-log prune. Cheap when under the cap.
                ticks_since_prune += 1;
                if ticks_since_prune >= EVENTS_PRUNE_EVERY_TICKS {
                    ticks_since_prune = 0;
                    let store_prune = Arc::clone(&store);
                    match tokio::task::spawn_blocking(move || {
                        crate::ingest::prune_events(&store_prune, &retention)
                    })
                    .await
                    {
                        Ok(Ok(n)) if n > 0 => debug!(pruned = n, "events log prune"),
                        Ok(Ok(_)) => {}
                        Ok(Err(e)) => warn!(error = ?e, "events prune failed; will retry"),
                        Err(e) => warn!(error = ?e, "events prune task panicked; will retry"),
                    }
                }

                tokio::time::sleep(interval).await;
            }
        }))
    };

    // TRANSITION LOOP (Wave 3): fold the `events` log into the materialized
    // `current_state` read model, agent-deck style. Runs AFTER the ingest task
    // is spawned so freshly-tailed lines are in `events` before the next fold
    // tick (a missed tick simply catches up on the following one — the
    // materialize cursor is durable). Like the ingest task it runs each pass on
    // the blocking pool (rusqlite is sync behind the store Mutex) and is
    // aborted on shutdown when this handle drops. This materializes ONLY
    // hook-sourced state (`source = "hook"`); the tmux / non-Claude / transient
    // ERR fold is merged at READ time in Wave 4 (notifyd has no `ainb-core`
    // dependency, so the heavy `classify()` path stays in the reader layer).
    let _materialize_task = {
        let store = Arc::clone(&store);
        let interval = config.materialize_interval;
        AbortOnDrop(tokio::spawn(async move {
            let mut consecutive_failures: u32 = 0;
            loop {
                let store_pass = Arc::clone(&store);
                match tokio::task::spawn_blocking(move || {
                    crate::transition::materialize(&store_pass)
                })
                .await
                {
                    Ok(Ok(upserted)) => {
                        consecutive_failures = 0;
                        if upserted > 0 {
                            debug!(upserted, "current_state materialize pass");
                        }
                    }
                    Ok(Err(e)) => {
                        consecutive_failures += 1;
                        escalate(consecutive_failures, "materialize", &format!("{e:?}"));
                    }
                    Err(e) => {
                        consecutive_failures += 1;
                        escalate(consecutive_failures, "materialize task", &format!("{e:?}"));
                    }
                }
                tokio::time::sleep(interval).await;
            }
        }))
    };

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
                        // Only persist events that need the human. The
                        // hook script delivers every registered lifecycle
                        // event, but high-frequency telemetry
                        // (SessionStart / UserPromptSubmit / PostToolUse /
                        // PreCompact) would bury the actionable signal —
                        // a single agent turn fires dozens of PostToolUse.
                        // Drop them here using the same `is_user_facing`
                        // predicate that gates OS notifications, so the
                        // inbox + per-session badge only ever reflect
                        // "come look at this session". This is the
                        // belt-and-suspenders filter: fresh installs also
                        // stop *firing* the telemetry hooks at the source
                        // (see plugins/ainb-hooks manifests), but this
                        // guard keeps stale installs clean too.
                        if !crate::osnotify::is_user_facing(&env) {
                            continue;
                        }
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
                                    // Honour the daemon's OS-channel routing (tcp T5): a
                                    // board-only / Os-excluded attention suppresses the OS
                                    // notification; an absent daemon fails open (notifies).
                                    // Zero-size resolver/transport — built per fire, no state.
                                    notify(
                                        &env,
                                        &debouncer,
                                        &crate::resolver::HangarRuleResolver::new(),
                                        &NativeTransport,
                                    )
                                    .await;
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
            // Tight ingest cadence so the daemon-level ingest test
            // converges quickly without long sleeps.
            ingest_interval: std::time::Duration::from_millis(20),
            // Same tight cadence for the transition loop.
            materialize_interval: std::time::Duration::from_millis(20),
        }
    }

    #[test]
    fn startup_lock_excludes_a_second_live_holder() {
        // M-D3: a lock held by a DIFFERENT live process must exclude a new
        // acquire — exactly-one-owner. Spawn a real, signalable child process and
        // record ITS pid as the holder so the liveness cross-check sees a genuine
        // live, different daemon.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("notify.spawn.lock");
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a live holder process");
        let holder_pid = child.id();
        assert_ne!(holder_pid, std::process::id());
        assert!(
            crate::pid::is_running(holder_pid),
            "spawned holder must be live"
        );
        std::fs::write(&lock, format!("{holder_pid}\n")).unwrap();

        let second = StartupLock::acquire(&lock);
        // Tidy up the child regardless of the assertion outcome.
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            second.is_err(),
            "a live different-pid holder must exclude a second acquire"
        );
        assert!(
            second.unwrap_err().to_string().contains("refusing to start"),
            "error should explain the live holder"
        );
    }

    #[test]
    fn startup_lock_recovers_a_stale_lock_from_a_dead_pid() {
        // M-D3 recovery: a lock file left by a CRASHED predecessor (a dead pid)
        // must be reclaimed, not block startup forever.
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("notify.spawn.lock");
        // Simulate a crashed predecessor: a lock file naming an impossible pid.
        std::fs::write(&lock, "2147483647\n").unwrap();
        let recovered = StartupLock::acquire(&lock);
        assert!(
            recovered.is_ok(),
            "a stale lock from a dead pid must be recoverable"
        );
        // The reclaimed lock now records OUR pid.
        let body = std::fs::read_to_string(&lock).unwrap();
        assert_eq!(body.trim(), std::process::id().to_string());
    }

    #[test]
    fn startup_lock_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("notify.spawn.lock");
        {
            let _held = StartupLock::acquire(&lock).unwrap();
            assert!(lock.exists(), "lock present while held");
        }
        assert!(!lock.exists(), "lock removed on drop");
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
    async fn daemon_drops_telemetry_keeps_actionable() {
        // The daemon must only persist events that need the human.
        // Fire two telemetry events (PostToolUse, SessionStart) and
        // two actionable ones (Stop, Notification:idle_prompt); only
        // the actionable pair should land in SQLite.
        let dir = tempfile::tempdir().unwrap();
        let config = config_under(dir.path());
        let socket = config.paths.socket.clone();
        let db = config.paths.db.clone();
        let handle = tokio::spawn(async move { run_daemon(config).await });
        wait_for_socket(&socket).await;

        let envelopes = [
            r#"{"protocol_version":1,"agent":"claude","raw_event":"PostToolUse","session_id":"s","cwd":"/tmp/x","project":"x","ts":1,"payload":{}}"#,
            r#"{"protocol_version":1,"agent":"claude","raw_event":"SessionStart","session_id":"s","cwd":"/tmp/x","project":"x","ts":2,"payload":{}}"#,
            r#"{"protocol_version":1,"agent":"claude","raw_event":"Stop","session_id":"s","cwd":"/tmp/x","project":"x","ts":3,"payload":{}}"#,
            r#"{"protocol_version":1,"agent":"claude","raw_event":"Notification:idle_prompt","session_id":"s","cwd":"/tmp/x","project":"x","ts":4,"payload":{}}"#,
        ];
        for env_json in envelopes {
            let mut stream = UnixStream::connect(&socket).await.unwrap();
            stream.write_all(env_json.as_bytes()).await.unwrap();
            stream.write_all(b"\n").await.unwrap();
            stream.shutdown().await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;

        let store = Store::open(&db).unwrap();
        // Only Stop + Notification:idle_prompt survived.
        assert_eq!(store.count().unwrap(), 2, "telemetry should be dropped");
        let events: std::collections::HashSet<String> = store
            .list(false, None, None, 10)
            .unwrap()
            .into_iter()
            .map(|r| r.raw_event)
            .collect();
        assert!(events.contains("Stop"));
        assert!(events.contains("Notification:idle_prompt"));
        assert!(!events.contains("PostToolUse"));
        assert!(!events.contains("SessionStart"));

        handle.abort();
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
    async fn daemon_ingests_events_jsonl_into_event_log() {
        // The running daemon's ingest tailer folds canonical lines from
        // events.jsonl (what the lifecycle hook appends) into the SQLite
        // `events` table on its short ticker.
        let dir = tempfile::tempdir().unwrap();
        let config = config_under(dir.path());
        let socket = config.paths.socket.clone();
        let db = config.paths.db.clone();
        let events_jsonl = config.paths.events_jsonl.clone();
        std::fs::create_dir_all(&config.paths.base).unwrap();
        // Pre-seed one line; append another after the daemon is up.
        std::fs::write(
            &events_jsonl,
            "{\"ts\":100,\"session_id\":\"s1\",\"cwd\":\"/tmp/p\",\"transcript_path\":\"/t/s1.jsonl\",\"agent\":\"claude\",\"event_type\":\"SessionStart\",\"matcher\":null,\"payload\":{}}\n",
        )
        .unwrap();

        let handle = tokio::spawn(async move { run_daemon(config).await });
        wait_for_socket(&socket).await;

        // Append a second line while the daemon runs (offset-driven tail).
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&events_jsonl).unwrap();
            f.write_all(b"{\"ts\":200,\"session_id\":\"s1\",\"cwd\":\"/tmp/p\",\"transcript_path\":\"/t/s1.jsonl\",\"agent\":\"claude\",\"event_type\":\"Stop\",\"matcher\":null,\"payload\":{}}\n").unwrap();
        }

        // Poll the event log (ingest_interval is 20ms in tests).
        let mut ingested = 0;
        for _ in 0..100 {
            let store = Store::open(&db).unwrap();
            ingested = store.events_since(0).unwrap().len();
            if ingested >= 2 {
                break;
            }
            drop(store);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(ingested, 2, "both canonical lines ingested into events");

        handle.abort();
    }

    #[tokio::test]
    async fn daemon_materializes_current_state_from_events_jsonl() {
        // End-to-end through the daemon's two background loops: a canonical
        // PreToolUse(AskUserQuestion) line appended to events.jsonl is ingested
        // into `events` AND folded into `current_state` as an ASK row carrying
        // the question — no transcript scan, no socket round-trip.
        let dir = tempfile::tempdir().unwrap();
        let config = config_under(dir.path());
        let socket = config.paths.socket.clone();
        let db = config.paths.db.clone();
        let events_jsonl = config.paths.events_jsonl.clone();
        std::fs::create_dir_all(&config.paths.base).unwrap();
        std::fs::write(
            &events_jsonl,
            "{\"ts\":100,\"session_id\":\"s1\",\"cwd\":\"/tmp/p\",\"transcript_path\":\"/t/s1.jsonl\",\"agent\":\"claude\",\"event_type\":\"PreToolUse\",\"matcher\":\"AskUserQuestion\",\"parent\":\"par-1\",\"payload\":{\"tool_input\":{\"questions\":[{\"question\":\"Pick one\",\"options\":[{\"label\":\"yes\"}]}]}}}\n",
        )
        .unwrap();

        let handle = tokio::spawn(async move { run_daemon(config).await });
        wait_for_socket(&socket).await;

        // Poll current_state (both loops run at 20ms in tests).
        let mut got = None;
        for _ in 0..150 {
            let store = Store::open(&db).unwrap();
            if let Some(row) = store.get_current_state("s1", "/tmp/p").unwrap() {
                got = Some(row);
                break;
            }
            drop(store);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let row = got.expect("ASK row materialized into current_state");
        assert_eq!(row.kind, "ASK");
        assert_eq!(row.source, "hook");
        assert_eq!(row.parent.as_deref(), Some("par-1"));
        assert!(
            row.context.as_deref().unwrap().contains("Pick one"),
            "ASK context carries the question text"
        );

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
