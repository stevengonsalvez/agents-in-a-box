//! The sidecar supervisor: find the hangar daemon for this home, or start the
//! bundled one, then hold this surface's presence against it.
//!
//! The state machine is the base spec's local transport row:
//!
//! ```text
//! probe ── hello ok ─────────────────────────────▶ Connected (attached)
//!   │ ── hello refused: ranges do not overlap ────▶ Incompatible (no spawn)
//!   │ no answer
//!   ▼
//! spawn ── child exits 0 in grace (lost the flock) ─▶ wait hello ─▶ Connected (attached)
//!   │ ── child alive, hello answers ────────────────────────────────▶ Connected (spawned)
//!   │ ── child exits non-zero or never answers ─▶ retry (3) ─▶ Degraded
//!   ▲
//!   └── the presence connection is lost ◀── Reconnecting
//! ```
//!
//! The daemon's own flock (`<home>/hangar/daemon.lock`) decides which of two
//! spawners wins; the supervisor adds no second guard. A spawned daemon runs in
//! its own session and is never killed when the app exits.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_hangar_client::{DaemonClient, DaemonError, PresenceLease, PresenceState};
use ainb_hangar_proto::auth::HelloResult;
use ainb_hangar_proto::connections::{SurfaceInfo, SurfaceKind};
use ainb_hangar_proto::protocol::ProtocolRange;
use tokio::sync::{Notify, watch};

/// Where the supervisor looks and what it starts.
#[derive(Debug, Clone)]
pub struct SidecarConfig {
    /// The hangar home: `$AINB_HANGAR_HOME`, else `~/.agents-in-a-box`.
    pub hangar_home: PathBuf,
    /// The bundled `ainb-hangar-daemon` binary.
    pub daemon_bin: PathBuf,
    /// How long a spawned child has to either exit 0 (it lost the flock to a
    /// daemon that already owns this home) or keep running.
    pub grace: Duration,
    /// How long a daemon has to answer hello once it is running. Longer than
    /// a cold store migration takes.
    pub hello_budget: Duration,
    /// Spawns that crash or never answer before the supervisor gives up.
    pub max_spawn_attempts: u32,
    /// The wait before each reconnect after the daemon is lost; a loss past
    /// the last step leaves the app degraded.
    pub reconnect_backoff: Vec<Duration>,
}

impl SidecarConfig {
    /// The defaults for `hangar_home` and `daemon_bin`.
    #[must_use]
    pub fn new(hangar_home: PathBuf, daemon_bin: PathBuf) -> Self {
        Self {
            hangar_home,
            daemon_bin,
            grace: Duration::from_secs(2),
            // Past the longest cold boot the tests allow (a fresh store's
            // migrations on a cold runner, 90 s), so a slow first start is
            // waited for rather than given up on.
            hello_budget: Duration::from_secs(180),
            max_spawn_attempts: 3,
            reconnect_backoff: RECONNECT_BACKOFF.to_vec(),
        }
    }

    /// The socket this home's daemon listens on.
    #[must_use]
    pub fn socket(&self) -> PathBuf {
        ainb_hangar_client::socket_path_in(&self.hangar_home)
    }

    /// Where a spawned daemon's stdout and stderr go, for "show log".
    #[must_use]
    pub fn log_path(&self) -> PathBuf {
        self.hangar_home.join("hangar").join("desktop-sidecar.log")
    }

    /// A client for this home, stamped as the desktop surface. Errors when the
    /// daemon has not written its token yet.
    fn client(&self) -> Result<DaemonClient, DaemonError> {
        let token_path = ainb_hangar_proto::auth::token_file_in(&self.hangar_home);
        let token = std::fs::read_to_string(&token_path)
            .map_err(|error| DaemonError::Token(error.to_string()))?;
        let mut client = DaemonClient::with_parts(self.socket(), token.trim().to_string());
        client.set_surface(surface());
        Ok(client)
    }
}

/// Where the connection to the daemon stands.
///
/// Carries what the process needs (the daemon pid, the log path). The webview
/// gets [`SidecarState::view`] instead, which carries neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidecarState {
    /// Probing for a daemon, or starting one.
    Starting,
    /// A daemon answered hello and this surface's presence is held.
    Connected {
        /// The daemon's pid, as its ownership lock names it.
        daemon_pid: Option<u32>,
        /// Whether this supervisor started it, rather than attaching to one
        /// that was already running or won the race.
        spawned: bool,
        /// The daemon's build version, as its hello named it; `None` from a
        /// daemon that predates the negotiation. Shown, never branched on.
        daemon_version: Option<String>,
        /// The protocol range the daemon speaks; the legacy range from a
        /// daemon that answered a bare `{}`.
        protocol: ProtocolRange,
    },
    /// The presence connection was lost; the supervisor is finding a daemon
    /// again.
    Reconnecting { error: String },
    /// A daemon owns this home and refused this build's protocol range on the
    /// first frame. Nothing was spawned: the daemon's flock would refuse the
    /// child too. The operator moves one of the two binaries and "Retry"
    /// probes again.
    Incompatible {
        /// The daemon's own sentence, verbatim: it names the fix.
        message: String,
        /// Whether the daemon's range sits above this build's, so the app is
        /// the older side and an update of the app is the fix, rather than a
        /// stop of the daemon.
        daemon_is_newer: bool,
    },
    /// No daemon could be found or started. "Retry" runs the probe again.
    Degraded { error: String, log: PathBuf },
}

/// What the webview's banner is told: no pid, no filesystem path, and a flag
/// for whether "show log" has anything to show. Serialised as
/// `{"state": "degraded", "error": ..., "has_log": true}`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SidecarView {
    Starting,
    Connected {
        spawned: bool,
        daemon_version: Option<String>,
        protocol: ProtocolRange,
    },
    Reconnecting {
        error: String,
    },
    Incompatible {
        message: String,
        daemon_is_newer: bool,
    },
    Degraded {
        error: String,
        has_log: bool,
    },
}

impl SidecarState {
    /// The banner's view of this state.
    #[must_use]
    pub fn view(&self) -> SidecarView {
        match self {
            Self::Starting => SidecarView::Starting,
            Self::Connected {
                spawned,
                daemon_version,
                protocol,
                ..
            } => SidecarView::Connected {
                spawned: *spawned,
                daemon_version: daemon_version.clone(),
                protocol: *protocol,
            },
            Self::Reconnecting { error } => SidecarView::Reconnecting {
                error: scrub_paths(error),
            },
            Self::Incompatible {
                message,
                daemon_is_newer,
            } => SidecarView::Incompatible {
                message: scrub_paths(message),
                daemon_is_newer: *daemon_is_newer,
            },
            Self::Degraded { error, log } => SidecarView::Degraded {
                error: scrub_paths(error),
                has_log: log.is_file(),
            },
        }
    }
}

/// `text` with every whitespace-separated token that names a filesystem path
/// replaced by `<path>`. Errors keep their paths for the log file; the
/// webview is told what went wrong, not where this user's files live.
#[must_use]
pub fn scrub_paths(text: &str) -> String {
    text.split(' ')
        .map(|token| {
            let bare =
                token.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '(' | ')' | '\'' | '"'));
            if bare.contains('/') || bare.contains(":\\") || bare.starts_with('~') {
                token.replace(bare, "<path>")
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The last `max_bytes` of the sidecar log, for "show log". `None` when there
/// is no log yet.
#[must_use]
pub fn log_tail(config: &SidecarConfig, max_bytes: u64) -> Option<String> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let mut file = std::fs::File::open(config.log_path()).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(len.saturating_sub(max_bytes))).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The running supervisor. Dropping it stops supervising; the daemon stays.
pub struct Sidecar {
    state: watch::Receiver<SidecarState>,
    retry: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}

impl Sidecar {
    /// Start supervising `config`'s daemon. Must be called inside a tokio
    /// runtime. Marks this process as a surface, so no other daemon call from
    /// it lists as its own connection.
    #[must_use]
    pub fn start(config: SidecarConfig) -> Self {
        ainb_hangar_client::mark_process_as_surface();
        let (state_tx, state) = watch::channel(SidecarState::Starting);
        let retry = Arc::new(Notify::new());
        let task = tokio::spawn(supervise(config, state_tx, Arc::clone(&retry)));
        Self { state, retry, task }
    }

    /// Observe the connection state.
    #[must_use]
    pub fn state(&self) -> watch::Receiver<SidecarState> {
        self.state.clone()
    }

    /// Leave `Degraded` and probe again.
    pub fn retry(&self) {
        self.retry.notify_one();
    }
}

impl Drop for Sidecar {
    fn drop(&mut self) {
        // Aborting drops the presence lease, which closes its socket. The
        // daemon itself is in its own session and keeps running.
        self.task.abort();
    }
}

/// The surface metadata every hello from this process carries.
fn surface() -> SurfaceInfo {
    SurfaceInfo {
        kind: SurfaceKind::Desktop,
        pid: std::process::id(),
    }
}

/// The default reconnect backoff: 1 s, 4 s, 16 s, as the spec's reconnect row.
const RECONNECT_BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(4),
    Duration::from_secs(16),
];

/// A connection that stayed up this long before it was lost starts the
/// backoff over, so a daemon that restarts once a day never exhausts it.
const STABLE_CONNECTION: Duration = Duration::from_secs(60);

async fn supervise(config: SidecarConfig, state: watch::Sender<SidecarState>, retry: Arc<Notify>) {
    let degrade = |error: String| {
        tracing::warn!(%error, "no hangar daemon for the desktop; degraded");
        state.send_replace(SidecarState::Degraded {
            error,
            log: config.log_path(),
        });
    };
    let mut losses = 0usize;
    loop {
        let (spawned, hello) = match find_or_start(&config).await {
            Ok(found) => found,
            Err(Refusal::Incompatible {
                message,
                daemon_is_newer,
            }) => {
                tracing::warn!(%message, daemon_is_newer, "the daemon owning this home refused this build; not spawning");
                state.send_replace(SidecarState::Incompatible {
                    message,
                    daemon_is_newer,
                });
                retry.notified().await;
                losses = 0;
                state.send_replace(SidecarState::Starting);
                continue;
            }
            Err(Refusal::NoDaemon(error)) => {
                degrade(error);
                retry.notified().await;
                losses = 0;
                state.send_replace(SidecarState::Starting);
                continue;
            }
        };
        let lease = {
            let config = config.clone();
            PresenceLease::spawn_with(surface(), Box::new(move || config.client()))
        };
        let mut presence = lease.state();

        // Connected only once the lease holds, so "connected" means the
        // daemon lists this surface, and only within the budget.
        let held = tokio::time::timeout(config.hello_budget, async {
            loop {
                if matches!(*presence.borrow_and_update(), PresenceState::Connected) {
                    return true;
                }
                if presence.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await;
        if !matches!(held, Ok(true)) {
            lease.close().await;
            degrade(format!(
                "the daemon answered but did not list this surface within {:?}",
                config.hello_budget
            ));
            retry.notified().await;
            losses = 0;
            state.send_replace(SidecarState::Starting);
            continue;
        }
        let pid = daemon_pid(&config.hangar_home);
        // Logged, not framed: the webview is told "connected" and nothing
        // about the process, while the window's own log says which daemon this
        // is, which is what a proof run compares with the one a CLI finds.
        tracing::info!(daemon_pid = ?pid, spawned, daemon_version = ?hello.daemon_version, "attached to the hangar daemon");
        state.send_replace(SidecarState::Connected {
            daemon_pid: pid,
            spawned,
            daemon_version: hello.daemon_version,
            protocol: hello.protocol,
        });
        let connected_at = Instant::now();

        let lost = loop {
            if presence.changed().await.is_err() {
                break "presence ended".to_string();
            }
            match presence.borrow_and_update().clone() {
                PresenceState::Connected => {}
                PresenceState::Waiting { error } => {
                    break error.unwrap_or_else(|| "the connection dropped".to_string());
                }
                PresenceState::Closed => break "presence closed".to_string(),
            }
        };
        lease.close().await;
        losses = if connected_at.elapsed() >= STABLE_CONNECTION {
            1
        } else {
            losses + 1
        };
        let Some(backoff) = config.reconnect_backoff.get(losses - 1) else {
            degrade(format!(
                "the daemon was lost {losses} times in a row; last: {lost}"
            ));
            retry.notified().await;
            losses = 0;
            state.send_replace(SidecarState::Starting);
            continue;
        };
        tracing::warn!(error = %lost, ?backoff, "desktop lost the hangar daemon; reconnecting");
        state.send_replace(SidecarState::Reconnecting { error: lost });
        tokio::time::sleep(*backoff).await;
    }
}

/// Why no daemon could be attached to.
#[derive(Debug)]
enum Refusal {
    /// A daemon owns the home and cannot serve this build. Spawning is no
    /// remedy: the child would lose the flock to the same daemon.
    Incompatible {
        message: String,
        daemon_is_newer: bool,
    },
    /// Nothing answered, or the bundled daemon could not be started.
    NoDaemon(String),
}

impl From<String> for Refusal {
    fn from(error: String) -> Self {
        Self::NoDaemon(error)
    }
}

/// Attach to a live daemon, or start the bundled one. `true` when this call's
/// child is the daemon that answered; the hello it answered beside it.
async fn find_or_start(config: &SidecarConfig) -> Result<(bool, HelloResult), Refusal> {
    match hello(config).await {
        Ok(hello) => return Ok((false, hello)),
        Err(error) => incompatible(&error)?,
    }
    let mut last_error = String::from("the daemon never started");
    for attempt in 1..=config.max_spawn_attempts {
        let mut child = spawn_daemon(config)?;
        let grace_end = Instant::now() + config.grace;
        let exited = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if Instant::now() >= grace_end => break None,
                Ok(None) => tokio::time::sleep(Duration::from_millis(50)).await,
                Err(error) => {
                    return Err(Refusal::NoDaemon(format!(
                        "could not watch the daemon child: {error}"
                    )));
                }
            }
        };
        match exited {
            // It lost the flock: another daemon owns this home. Attach to it.
            Some(status) if status.success() => {
                return wait_for_hello(config, None).await.map(|hello| (false, hello));
            }
            Some(status) => {
                last_error = format!(
                    "the daemon exited with {status} (attempt {attempt} of {}); see {}",
                    config.max_spawn_attempts,
                    config.log_path().display()
                );
            }
            None => match wait_for_hello(config, Some(&mut child)).await {
                Ok(hello) => {
                    reap_when_done(child);
                    return Ok((true, hello));
                }
                Err(refusal @ Refusal::Incompatible { .. }) => {
                    // The answer came from a daemon that owns the home and is
                    // not this child; the child is losing the flock to it.
                    reap_when_done(child);
                    return Err(refusal);
                }
                Err(Refusal::NoDaemon(error)) => {
                    // A child that took the home's lock is the daemon for this
                    // home, still booting: killing it would kill the only
                    // daemon, and every retry would kill the next. Leave it,
                    // reap it when it exits, and stop spawning more.
                    if daemon_pid(&config.hangar_home) == Some(child.id()) {
                        reap_when_done(child);
                        return Err(Refusal::NoDaemon(format!(
                            "{error}; the daemon holds this home and is still starting"
                        )));
                    }
                    // It never owned the home: stop it and reap it rather than
                    // leave a half-started process or a zombie behind.
                    let _ = child.kill();
                    let _ = child.wait();
                    last_error = format!("{error} (attempt {attempt})");
                }
            },
        }
    }
    Err(Refusal::NoDaemon(last_error))
}

/// `Err` when `error` is the daemon refusing this build's range, so the
/// caller stops before it spawns; `Ok` for every other failure.
fn incompatible(error: &DaemonError) -> Result<(), Refusal> {
    match error {
        DaemonError::Incompatible {
            daemon,
            client,
            message,
            ..
        } => Err(Refusal::Incompatible {
            message: message.clone(),
            // A refusal that did not say its range reads as an older daemon:
            // the stop verb is the fix that always holds; an update of this
            // app is offered only when the daemon proved it is the newer one.
            daemon_is_newer: daemon.is_some_and(|daemon| daemon.min > client.max),
        }),
        _ => Ok(()),
    }
}

/// Reap the daemon this process started whenever it exits, so a daemon that
/// dies while the app runs does not linger as a zombie. It never signals it.
fn reap_when_done(mut child: Child) {
    let spawned =
        std::thread::Builder::new()
            .name("ainb-desktop-sidecar-reaper".into())
            .spawn(move || {
                let _ = child.wait();
            });
    if let Err(error) = spawned {
        tracing::warn!(%error, "no reaper for the daemon child; it may linger as a zombie");
    }
}

/// Start the bundled daemon for this home in its own session, logging to the
/// sidecar log, so it outlives the app.
fn spawn_daemon(config: &SidecarConfig) -> Result<Child, String> {
    let log_path = config.log_path();
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|error| format!("could not open {}: {error}", log_path.display()))?;
    let stderr = log
        .try_clone()
        .map_err(|error| format!("could not open {}: {error}", log_path.display()))?;
    let mut command = Command::new(&config.daemon_bin);
    command
        .env(HANGAR_HOME_ENV, &config.hangar_home)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: `setsid` is async-signal-safe and touches no memory of the
        // parent; it only moves the child into a new session so the app
        // exiting (or its terminal closing) does not signal the daemon.
        unsafe {
            command.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from));
        }
    }
    command.spawn().map_err(|error| {
        format!(
            "could not start the bundled daemon {}: {error}",
            config.daemon_bin.display()
        )
    })
}

/// The variable the daemon resolves its home from.
const HANGAR_HOME_ENV: &str = "AINB_HANGAR_HOME";

async fn hello(config: &SidecarConfig) -> Result<HelloResult, DaemonError> {
    config.client()?.hello().await
}

/// Wait for hello to answer within the budget. A child that dies first ends
/// the wait early, and so does a daemon that refuses this build's range.
async fn wait_for_hello(
    config: &SidecarConfig,
    mut child: Option<&mut Child>,
) -> Result<HelloResult, Refusal> {
    let deadline = Instant::now() + config.hello_budget;
    loop {
        let error = match hello(config).await {
            Ok(hello) => return Ok(hello),
            Err(error) => {
                incompatible(&error)?;
                error.to_string()
            }
        };
        if let Some(child) = child.as_deref_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(Refusal::NoDaemon(format!(
                    "the daemon exited with {status} before answering; see {}",
                    config.log_path().display()
                )));
            }
        }
        if Instant::now() >= deadline {
            return Err(Refusal::NoDaemon(format!(
                "no daemon answered on {} within {:?}: {error}",
                config.socket().display(),
                config.hello_budget
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The pid the daemon's ownership lock names.
#[must_use]
pub fn daemon_pid(hangar_home: &Path) -> Option<u32> {
    std::fs::read_to_string(hangar_home.join("hangar").join("daemon.lock"))
        .ok()?
        .trim()
        .parse()
        .ok()
}
