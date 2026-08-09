//! Discovery + classification of running `notifyd` processes.
//!
//! The daemon's own [`crate::pid`] / [`crate::install::status`] surfaces
//! only know about the single pid recorded in `notify.pid`. That is
//! structurally blind to *orphans* — extra `ainb notifyd` processes left
//! behind by a spawn race or a `brew upgrade` that never bound the socket.
//! This module enumerates every notifyd-family process on the host and
//! classifies each against the canonical owner so the TUI can show — and
//! the user can reap — orphans.
//!
//! Enumeration shells out to `ps` rather than pulling in a process-table
//! crate: it runs only on demand (when the Daemons overlay is open), the
//! two supported targets (macOS, Linux) both ship a `ps` that accepts
//! `-axo pid=,etime=,args=`, and the classification logic — the part worth
//! testing — is a pure function over the parsed rows.

use std::path::Path;
use std::process::Command;

use crate::Paths;

/// A pid this process has PROVED it is allowed to signal.
///
/// The only constructor is [`OwnedPid::holding_one_of`], and every signal sent by
/// this module takes an `OwnedPid`, so "signal a process we do not own" is a type
/// error rather than something a reviewer has to spot.
///
/// It exists because [`enumerate`] reads `ps -axo` across every user and every
/// `$AINB_HANGAR_HOME` on the host, while [`scan`] resolves the live-owner list from
/// exactly ONE home. A notifyd serving a different home was therefore classified
/// [`DaemonClass::Orphan`] and SIGTERM/SIGKILLed. Observed live: `ainb notifyd
/// restart` inside an isolated sandbox home killed the developer's real daemon.
#[derive(Debug)]
struct OwnedPid(u32);

impl OwnedPid {
    /// Prove `pid` belongs to this stack, or refuse to signal it.
    ///
    /// The proof is possession: the process must hold a unix socket bound to one of
    /// `sockets` (this home's `notify.sock` / `approve.sock`), under the current uid.
    /// Both are bound by the daemon at startup, so a wedged owner that stopped
    /// serving still proves ownership, while a notifyd from another home never can.
    ///
    /// Fails closed. No `lsof`, an unreadable process, a released socket, or any
    /// other ambiguity answers `None`, and the caller spares the process: leaking an
    /// orphan is cheap, killing a live foreign daemon is not.
    fn holding_one_of(pid: u32, sockets: &[&Path]) -> Option<Self> {
        sockets.iter().any(|socket| pid_holds_socket(pid, socket)).then_some(Self(pid))
    }

    /// The proven pid, for reporting.
    fn get(&self) -> u32 {
        self.0
    }
}

/// Does `pid` hold a unix socket bound to `socket`, under the current uid?
///
/// `lsof -a -p <pid> -u <uid> -U -F n` lists the bound NAME of every unix socket the
/// process holds; `-a` ANDs the pid and uid filters, so another user's process
/// answers empty. Matching the bound name (rather than looking the path up with
/// `lsof -- <path>`) is deliberate: a daemon whose socket file was replaced by a
/// successor still reports the name it bound, which is exactly the wedged owner a
/// reap is for.
fn pid_holds_socket(pid: u32, socket: &Path) -> bool {
    let uid = nix::unistd::Uid::current().as_raw().to_string();
    let Ok(out) = Command::new("lsof")
        .args(["-a", "-p", &pid.to_string(), "-u", &uid, "-U", "-F", "n"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix('n'))
        .map(ainb_hangar_core::lsof::strip_type_suffix)
        .any(|name| socket_names_match(name, socket))
}

/// Compare a bound socket name from `lsof` to the path we expect.
///
/// Exact string match first: the daemon binds the very path [`Paths`] resolves. The
/// fallback re-resolves both parent directories so a home reached through a symlink
/// (`/tmp` -> `/private/tmp` on macOS) still matches; the socket file itself is not
/// canonicalized because a wedged owner's path may already have been replaced.
fn socket_names_match(name: &str, expected: &Path) -> bool {
    if Path::new(name) == expected {
        return true;
    }
    let real_dir = |p: &Path| p.parent().and_then(|d| std::fs::canonicalize(d).ok());
    match (
        real_dir(Path::new(name)),
        real_dir(expected),
        Path::new(name).file_name(),
        expected.file_name(),
    ) {
        (Some(a), Some(b), Some(x), Some(y)) => a == b && x == y,
        _ => false,
    }
}

/// One running notifyd-family process discovered on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifydProc {
    /// Process id.
    pub pid: u32,
    /// Resolved binary path from `argv[0]` — e.g.
    /// `/opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb`. The version often
    /// reads straight off this path.
    pub bin: String,
    /// Full command line as reported by `ps`.
    pub cmd: String,
    /// Elapsed-time string from `ps` (`[[DD-]HH:]MM:SS`), best-effort.
    pub etime: String,
}

/// How a discovered notifyd process relates to the canonical daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonClass {
    /// Holds the live, accepting socket — the process actually serving
    /// notifications. Spared from a reap, whatever `notify.pid` says.
    LiveOwner,
    /// The pid recorded in `notify.pid`, but it is not the live socket
    /// holder — a wedged / superseded owner. Reapable.
    StaleOwner,
    /// A running notifyd that is neither the live socket holder nor the
    /// recorded owner — an orphan that should be reaped.
    Orphan,
}

impl DaemonClass {
    /// Short human label for the TUI.
    pub fn label(self) -> &'static str {
        match self {
            Self::LiveOwner => "LIVE owner",
            Self::StaleOwner => "STALE owner",
            Self::Orphan => "ORPHAN",
        }
    }

    /// Whether this class is healthy (the one we want) vs something the
    /// user probably wants to clean up.
    pub fn is_healthy(self) -> bool {
        matches!(self, Self::LiveOwner)
    }
}

/// A discovered process plus its relationship to the canonical daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedDaemon {
    /// The underlying process.
    pub proc: NotifydProc,
    /// Live owner / stale owner / orphan.
    pub class: DaemonClass,
    /// True when this process's binary differs from the currently-running
    /// `ainb` binary — i.e. a stale install lingering after an upgrade.
    pub binary_drift: bool,
}

/// Enumerate every running notifyd-family process. Returns an empty vec
/// if `ps` is unavailable or fails — discovery is best-effort.
pub(crate) fn enumerate() -> Vec<NotifydProc> {
    let output = match Command::new("ps").args(["-axo", "pid=,etime=,args="]).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output)
        .lines()
        .filter_map(parse_ps_line)
        .filter(is_notifyd)
        .collect()
}

/// Parse one `pid etime args...` line from `ps`. `ps` right-justifies the
/// pid and pads between columns, so split on whitespace runs and rejoin
/// the command remainder (original spacing is irrelevant for matching and
/// display).
fn parse_ps_line(line: &str) -> Option<NotifydProc> {
    let mut toks = line.split_whitespace();
    let pid: u32 = toks.next()?.parse().ok()?;
    let etime = toks.next()?.to_string();
    let rest: Vec<&str> = toks.collect();
    if rest.is_empty() {
        return None;
    }
    let cmd = rest.join(" ");
    let bin = rest[0].to_string();
    Some(NotifydProc {
        pid,
        bin,
        cmd,
        etime,
    })
}

/// Does this process look like a *daemon* notifyd? Matches only the explicit
/// daemon forms — `ainb notifyd run` and the slim `ainb-notifyd run` binary.
///
/// The subcommand must be exactly `run`. A missing subcommand used to count as
/// daemon mode, which made every command line whose FINAL token happens to be
/// `notifyd` — `ainb logs notifyd`, `ainb plugin install notifyd` — classify as a
/// reapable daemon. The cost of the two mistakes is not symmetric: missing a
/// bare-invoked daemon leaves it running, matching a CLI invocation kills it.
///
/// Transient `ainb notifyd status|stop|install|uninstall|list` calls are excluded
/// for the same reason. The `ainb` TUI itself never carries a `notifyd` token.
fn is_notifyd(p: &NotifydProc) -> bool {
    let base = p.bin.rsplit('/').next().unwrap_or(&p.bin);
    let toks: Vec<&str> = p.cmd.split_whitespace().collect();
    // The subcommand token that selects daemon mode, if any.
    let sub = match base {
        "ainb" => match toks.iter().position(|t| *t == "notifyd") {
            Some(i) => toks.get(i + 1).copied(),
            None => return false,
        },
        "ainb-notifyd" => toks.get(1).copied(),
        _ => return false,
    };
    // Daemon mode is an explicit `run`, never the bare command.
    sub == Some("run")
}

/// Classify each discovered process. `live_pids` is the set of pids that
/// actually hold an **accepting** socket (lsof ∩ a successful connect probe)
/// — the authoritative liveness signal. A process is the live owner iff it
/// holds that socket, *regardless of what `notify.pid` records*: in a spawn
/// race the pid that won the socket can differ from the one in the file, and
/// reaping must never kill whoever is actually serving. The pid file only
/// distinguishes a wedged recorded owner (`StaleOwner`) from a plain orphan
/// when nothing is serving. Pure — the testable core.
pub(crate) fn classify(
    procs: Vec<NotifydProc>,
    owner_pid: Option<u32>,
    live_pids: &[u32],
    current_bin: Option<&str>,
) -> Vec<ClassifiedDaemon> {
    procs
        .into_iter()
        .map(|p| {
            let class = if live_pids.contains(&p.pid) {
                DaemonClass::LiveOwner
            } else if owner_pid == Some(p.pid) {
                DaemonClass::StaleOwner
            } else {
                DaemonClass::Orphan
            };
            let binary_drift = current_bin.is_some_and(|cur| !same_binary(&p.bin, cur));
            ClassifiedDaemon {
                proc: p,
                class,
                binary_drift,
            }
        })
        .collect()
}

/// Compare two binary paths for identity, canonicalizing both so a
/// symlinked install (e.g. a `~/.cargo/bin` or Homebrew shim) doesn't read
/// as drift. A path that no longer resolves (old install removed by an
/// upgrade) falls back to a string compare — which correctly reports
/// drift.
fn same_binary(a: &str, b: &str) -> bool {
    match (std::fs::canonicalize(a).ok(), std::fs::canonicalize(b).ok()) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// True when a daemon is actually accepting on the unix socket. A bare
/// socket *file* left by a crashed daemon connects with `ECONNREFUSED`, so
/// this correctly returns false for a wedged owner — unlike `Path::exists`.
/// The throwaway connection sends nothing; the daemon reads EOF and skips
/// it, same as the hook script's `nc` probe.
fn socket_accepting(path: &std::path::Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Pids holding the unix socket, via `lsof -t`. Empty when `lsof` is absent
/// or nothing holds the path. Used only once the socket is confirmed
/// accepting, so the result identifies the live listener to spare from a
/// reap. Best-effort: if `lsof` can't be run we fall back to pid-file-only
/// liveness (the historical behaviour).
fn socket_listener_pids(path: &std::path::Path) -> Vec<u32> {
    let Ok(out) = Command::new("lsof").arg("-t").arg(path).output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect()
}

/// Outcome of a [`reap`] sweep.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ReapReport {
    /// Pids that were signalled and confirmed gone.
    pub killed: Vec<u32>,
    /// Pids that could not be reaped, with the reason (e.g. `EPERM` —
    /// another user's process — or survival past `SIGKILL`).
    pub failed: Vec<(u32, String)>,
    /// The healthy live owner left untouched, if one was found.
    pub spared: Option<u32>,
    /// Pids left alone because this process could not prove they belong to this
    /// home (see [`OwnedPid`]) — typically a notifyd serving a different
    /// `$AINB_HANGAR_HOME`, which is never ours to kill.
    pub spared_unproven: Vec<u32>,
}

/// The pids worth reaping from a classified set: everything that is not a
/// healthy live owner (orphans plus a wedged stale owner). Pure — the
/// selection is the part worth testing; the killing is not.
pub(crate) fn reapable(daemons: &[ClassifiedDaemon]) -> Vec<u32> {
    daemons.iter().filter(|d| !d.class.is_healthy()).map(|d| d.proc.pid).collect()
}

/// Deliver a real signal. Takes an [`OwnedPid`], so it is unreachable without a
/// completed ownership proof.
fn signal_owned(owned: &OwnedPid, sig: nix::sys::signal::Signal) -> nix::Result<()> {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(owned.0 as i32), sig)
}

/// Signal-delivery seam, so a test can observe what a reap DECIDED to signal
/// without any process actually dying.
type Signaller<'a> = &'a dyn Fn(&OwnedPid, nix::sys::signal::Signal) -> nix::Result<()>;

/// Kill every notifyd process that isn't the healthy live owner — the
/// orphans (and a wedged stale owner) the TUI flags — but only after proving
/// each one belongs to THIS home (see [`OwnedPid`]). Sends `SIGTERM`, waits
/// briefly, then `SIGKILL`s any survivor: wedged daemons don't always honour
/// `SIGTERM`. Kills by exact proven pid only — never a name match or signal
/// broadcast.
pub fn reap() -> ReapReport {
    let Ok(paths) = Paths::from_home() else {
        return ReapReport::default();
    };
    reap_with(&paths, &scan(), &signal_owned)
}

/// The reap decision, parameterised on the home and the signal delivery so it can
/// be driven end-to-end in a test against decoy processes we spawned ourselves.
fn reap_with(paths: &Paths, daemons: &[ClassifiedDaemon], signal: Signaller<'_>) -> ReapReport {
    use nix::errno::Errno;
    use nix::sys::signal::Signal;

    // Both sockets are bound by our daemon at startup; holding either proves the
    // process belongs to this home.
    let sockets = [paths.socket.as_path(), paths.approve_socket.as_path()];
    let spared = daemons.iter().find(|d| d.class.is_healthy()).map(|d| d.proc.pid);
    let mut report = ReapReport {
        spared,
        ..Default::default()
    };

    // Phase 1 — prove ownership, then SIGTERM; collect the ones that took it.
    let mut pending = Vec::new();
    for pid in reapable(daemons) {
        let Some(owned) = OwnedPid::holding_one_of(pid, &sockets) else {
            report.spared_unproven.push(pid);
            continue;
        };
        match signal(&owned, Signal::SIGTERM) {
            Ok(()) => pending.push(owned),
            Err(Errno::ESRCH) => report.killed.push(pid), // already gone
            Err(e) => report.failed.push((pid, e.to_string())), // EPERM, etc.
        }
    }

    // Phase 2 — give them a moment, then SIGKILL any survivor and confirm. The
    // proof is REDONE first: a target that died during the grace can have its pid
    // recycled by an unrelated process, which must not inherit our SIGKILL.
    if !pending.is_empty() {
        std::thread::sleep(std::time::Duration::from_millis(500));
        for owned in pending {
            let pid = owned.get();
            if crate::pid::is_running(pid) {
                match OwnedPid::holding_one_of(pid, &sockets) {
                    Some(still_ours) => {
                        let _ = signal(&still_ours, Signal::SIGKILL);
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    None => {
                        report.spared_unproven.push(pid);
                        continue;
                    }
                }
            }
            if crate::pid::is_running(pid) {
                report.failed.push((pid, "survived SIGKILL".to_string()));
            } else {
                report.killed.push(pid);
            }
        }
    }

    // If nothing live remains, the recorded owner (if any) is now a dead pid
    // — drop the dangling pid file so `status` / the next lazy-spawn don't
    // read a stale owner, mirroring what `cmd_stop` does. A SIGKILL'd owner
    // never runs its `PidFile::Drop`, so this is the only cleanup point.
    if report.spared.is_none() && !report.killed.is_empty() {
        let _ = std::fs::remove_file(&paths.pid);
    }

    report
}

/// Outcome of a [`restart`] — the single resume/repair command for a dead
/// or wedged approve socket. Serialisable so the CLI can render it as
/// `--format json`.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RestartOutcome {
    /// The prior owner pid that was signalled to stop, if one was live.
    pub stopped: Option<u32>,
    /// A live pid recorded in `notify.pid` that was NOT signalled because it
    /// could not be proved to hold one of this home's sockets — a recycled pid,
    /// or a pid file copied from another home. Left running on purpose.
    pub stop_refused: Option<u32>,
    /// Wedged / orphan pids reaped before the fresh spawn.
    pub reaped: Vec<u32>,
    /// Pid of the freshly detach-spawned daemon.
    pub spawned: Option<u32>,
    /// Whether the approve socket came back up within the bind timeout.
    /// `true` means still-blocked `client_await` waiters can re-dial and
    /// resume; `false` means the caller should investigate (the waiters
    /// keep re-dialling until their own deadline regardless).
    pub socket_bound: bool,
}

/// argv for respawning *this* daemon binary. The shared `notifyd` CLI is
/// reached two ways: the standalone `ainb-notifyd` binary (daemon verbs at
/// the top level → `run`) and the host `ainb notifyd run` subcommand. Pick
/// by the current exe's basename so a restart re-execs the right entrypoint
/// regardless of which one called it.
fn daemon_respawn_argv() -> (std::path::PathBuf, Vec<&'static str>) {
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("ainb"));
    let name = exe.file_name().and_then(|n| n.to_str()).unwrap_or("");
    (exe.clone(), respawn_args_for(name))
}

/// Pure argv picker: the standalone `ainb-notifyd` binary exposes daemon
/// verbs at the top level (`run`); the host `ainb` binary nests them under
/// `notifyd`.
fn respawn_args_for(exe_name: &str) -> Vec<&'static str> {
    if exe_name.starts_with("ainb-notifyd") {
        vec!["run"]
    } else {
        vec!["notifyd", "run"]
    }
}

/// Detach-spawn a fresh daemon: null stdio + its own process group so a
/// closing tmux pane or terminal SIGHUP can't take it down — the Rust
/// equivalent of the hook script's `nohup ainb notifyd </dev/null
/// >/dev/null 2>&1 &`. Returns the child pid.
fn spawn_detached() -> anyhow::Result<u32> {
    use anyhow::Context;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let (exe, args) = daemon_respawn_argv();
    let child = Command::new(&exe)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("spawning {} {}", exe.display(), args.join(" ")))?;
    Ok(child.id())
}

/// Poll the approve socket until something is accepting on it, up to
/// `timeout`. Returns whether it came up.
fn wait_for_socket_bound(path: &std::path::Path, timeout: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    loop {
        if socket_accepting(path) {
            return true;
        }
        if started.elapsed() >= timeout {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// The single resume/repair command (goal req: "one command to resume/repair;
/// must not lose the waiting hook"). Stops any current owner, reaps wedged /
/// orphan stragglers, then detach-spawns a fresh daemon and waits for the
/// approve socket to bind. Because [`crate::broker::client_await`] re-dials
/// on every `REDIAL_INTERVAL` until its own deadline, a still-blocked
/// permission waiter re-registers itself the moment the socket is back — so
/// restarting the daemon is all it takes to resume pending prompts, and no
/// waiting hook is ever lost. `stop`-first (not just reap) because
/// [`crate::run_daemon`] refuses to start while a live owner holds the pid.
pub fn restart(bind_timeout: std::time::Duration) -> anyhow::Result<RestartOutcome> {
    use nix::sys::signal::Signal;

    let paths = Paths::from_home()?;
    let mut outcome = RestartOutcome::default();

    // 1. Stop the recorded owner and wait for it to actually exit — spawning
    //    while it still holds the pid would make the new daemon bail. The pid file
    //    is a claim, not proof: a pid recorded before a crash can be recycled by an
    //    unrelated process, so the owner is signalled only once it proves it holds
    //    one of this home's sockets.
    let sockets = [paths.socket.as_path(), paths.approve_socket.as_path()];
    if let Ok(Some(pid)) = crate::pid::read(&paths.pid) {
        if crate::pid::is_running(pid) {
            match OwnedPid::holding_one_of(pid, &sockets) {
                Some(owned) => {
                    let _ = signal_owned(&owned, Signal::SIGTERM);
                    outcome.stopped = Some(pid);
                    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    while crate::pid::is_running(pid) && std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    // Re-prove before escalating: the pid may have exited and been
                    // recycled inside the grace window.
                    if crate::pid::is_running(pid) {
                        if let Some(still_ours) = OwnedPid::holding_one_of(pid, &sockets) {
                            let _ = signal_owned(&still_ours, Signal::SIGKILL);
                            std::thread::sleep(std::time::Duration::from_millis(100));
                        }
                    }
                }
                None => outcome.stop_refused = Some(pid),
            }
        }
    }

    // 2. Reap any wedged / orphan stragglers so the fresh daemon is the only
    //    notifyd-family process left.
    outcome.reaped = reap().killed;

    // 3. Detach-spawn a fresh daemon and wait for the approve socket to bind.
    outcome.spawned = Some(spawn_detached()?);
    outcome.socket_bound = wait_for_socket_bound(&paths.approve_socket, bind_timeout);

    Ok(outcome)
}

/// One-shot scan: enumerate notifyd processes and classify them against
/// the on-disk owner pid, the live socket, and this process's own binary.
/// Used by the TUI's Daemons overlay.
pub fn scan() -> Vec<ClassifiedDaemon> {
    let Ok(paths) = Paths::from_home() else {
        return Vec::new();
    };
    let owner_pid = crate::pid::read(&paths.pid).ok().flatten();
    let current_bin = std::env::current_exe().ok().and_then(|p| p.to_str().map(String::from));
    // Authoritative liveness: who actually holds an *accepting* socket. The
    // connect probe proves something is serving; lsof names the listener to
    // spare. A wedged listener that bound but hung fails the connect probe,
    // so it stays reapable.
    let live_pids = if socket_accepting(&paths.socket) {
        socket_listener_pids(&paths.socket)
    } else {
        Vec::new()
    };
    classify(enumerate(), owner_pid, &live_pids, current_bin.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;
    use tempfile::TempDir;

    fn proc(pid: u32, bin: &str) -> NotifydProc {
        NotifydProc {
            pid,
            bin: bin.to_string(),
            cmd: format!("{bin} notifyd run"),
            etime: "01:23".to_string(),
        }
    }

    /// A real process, spawned by us, that holds `socket` open.
    ///
    /// The listener is bound here and handed to a `sleep` child as its stdin, so
    /// `lsof` reports the child holding that bound name — the same evidence a real
    /// notifyd leaves. Killed by its exact pid on drop; never by name.
    struct Decoy {
        _listener: std::os::unix::net::UnixListener,
        child: std::process::Child,
    }

    impl Decoy {
        fn holding(socket: &Path) -> Self {
            let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind decoy");
            let handed = listener.try_clone().expect("clone decoy socket");
            let child = Command::new("/bin/sleep")
                .arg("30")
                .stdin(std::process::Stdio::from(std::os::fd::OwnedFd::from(
                    handed,
                )))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn decoy");
            Self {
                _listener: listener,
                child,
            }
        }

        fn pid(&self) -> u32 {
            self.child.id()
        }
    }

    impl Drop for Decoy {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// Raw `lsof` output for `pid`'s unix sockets, for failure messages only.
    ///
    /// Mirrors the argv [`pid_holds_socket`] uses, and reports the exit status and
    /// stderr it discards, so a red test says which of the two it hit: no name to
    /// match, or a name that did not match.
    fn lsof_dump(pid: u32) -> String {
        let uid = nix::unistd::Uid::current().as_raw().to_string();
        match Command::new("lsof")
            .args(["-a", "-p", &pid.to_string(), "-u", &uid, "-U", "-F", "n"])
            .output()
        {
            Ok(out) => format!(
                "  status={}\n  stdout={:?}\n  stderr={:?}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => format!("  lsof failed to spawn: {e}"),
        }
    }

    /// One orphan row for `pid`, as `scan()` would classify a stranger notifyd.
    fn orphan_row(pid: u32) -> Vec<ClassifiedDaemon> {
        vec![ClassifiedDaemon {
            proc: proc(pid, "/usr/local/bin/ainb"),
            class: DaemonClass::Orphan,
            binary_drift: false,
        }]
    }

    /// Run a reap against `paths`, recording what it decided to signal instead of
    /// delivering it. Nothing dies; the decision is the thing under test.
    fn reap_recording(
        paths: &Paths,
        daemons: &[ClassifiedDaemon],
    ) -> (ReapReport, Vec<(u32, Signal)>) {
        let delivered = std::sync::Mutex::new(Vec::new());
        let report = reap_with(paths, daemons, &|owned: &OwnedPid, sig: Signal| {
            delivered.lock().unwrap().push((owned.get(), sig));
            Ok(())
        });
        let delivered = delivered.into_inner().unwrap();
        (report, delivered)
    }

    /// The live incident: `ainb notifyd restart` under an isolated sandbox home
    /// SIGKILLed the developer's real daemon, because `enumerate()` sees every
    /// home on the host while `scan()` resolves the owner from just one.
    #[test]
    fn reap_spares_a_notifyd_serving_another_home() {
        let mine = TempDir::new().unwrap();
        let theirs = TempDir::new().unwrap();
        let my_paths = Paths::under(mine.path());
        let their_paths = Paths::under(theirs.path());
        let decoy = Decoy::holding(&their_paths.socket);

        let (report, delivered) = reap_recording(&my_paths, &orphan_row(decoy.pid()));

        assert!(
            delivered.is_empty(),
            "a notifyd holding another home's socket must never be signalled, got {delivered:?}"
        );
        assert_eq!(report.spared_unproven, vec![decoy.pid()]);
        assert!(report.killed.is_empty());
    }

    /// The other half: the guard must not turn the reaper into a no-op. A wedged
    /// daemon holding THIS home's socket is still reaped, SIGTERM then SIGKILL.
    #[test]
    fn reap_signals_a_notifyd_serving_this_home() {
        let mine = TempDir::new().unwrap();
        let my_paths = Paths::under(mine.path());
        let decoy = Decoy::holding(&my_paths.socket);

        let (report, delivered) = reap_recording(&my_paths, &orphan_row(decoy.pid()));

        // The recording signaller never actually kills, so the decoy survives the
        // grace and the escalation fires too — both signals prove the target was
        // accepted by the ownership proof at both decision points.
        //
        // On a mismatch, dump what the proof actually saw. A bare `left == right`
        // says only that nothing was signalled, which is indistinguishable from
        // the proof failing for an environmental reason: `pid_holds_socket` folds
        // a missing `lsof`, a non-zero exit and an unparsable name into the same
        // `false`. The raw output separates "lsof cannot name this socket here"
        // from "the matching logic is wrong".
        assert_eq!(
            delivered,
            vec![
                (decoy.pid(), Signal::SIGTERM),
                (decoy.pid(), Signal::SIGKILL)
            ],
            "ownership proof did not hold for the decoy.\n\
             spared_unproven={:?}\n\
             expected socket={}\n\
             pid_holds_socket={}\n\
             raw lsof -a -p {} -u {} -U -F n:\n{}",
            report.spared_unproven,
            my_paths.socket.display(),
            pid_holds_socket(decoy.pid(), &my_paths.socket),
            decoy.pid(),
            nix::unistd::Uid::current().as_raw(),
            lsof_dump(decoy.pid()),
        );
        assert!(report.spared_unproven.is_empty());
    }

    fn classified(class: DaemonClass, pid: u32) -> ClassifiedDaemon {
        ClassifiedDaemon {
            proc: proc(pid, "/x/ainb"),
            class,
            binary_drift: false,
        }
    }

    #[test]
    fn respawn_argv_standalone_uses_top_level_run() {
        assert_eq!(respawn_args_for("ainb-notifyd"), vec!["run"]);
        assert_eq!(respawn_args_for("ainb-notifyd-x86"), vec!["run"]);
    }

    #[test]
    fn respawn_argv_host_binary_nests_under_notifyd() {
        assert_eq!(respawn_args_for("ainb"), vec!["notifyd", "run"]);
        assert_eq!(respawn_args_for(""), vec!["notifyd", "run"]);
    }

    #[test]
    fn wait_for_socket_bound_true_when_listener_up() {
        let dir = std::env::temp_dir().join(format!("ainb-sockbound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("live.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(wait_for_socket_bound(
            &sock,
            std::time::Duration::from_millis(200)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn wait_for_socket_bound_false_when_absent() {
        let sock = std::env::temp_dir().join(format!("ainb-nosock-{}.sock", std::process::id()));
        std::fs::remove_file(&sock).ok();
        assert!(!wait_for_socket_bound(
            &sock,
            std::time::Duration::from_millis(120)
        ));
    }

    #[test]
    fn reapable_selects_everything_but_the_live_owner() {
        let ds = vec![
            classified(DaemonClass::LiveOwner, 1),
            classified(DaemonClass::Orphan, 2),
            classified(DaemonClass::StaleOwner, 3),
            classified(DaemonClass::Orphan, 4),
        ];
        assert_eq!(reapable(&ds), vec![2, 3, 4]);
    }

    #[test]
    fn reapable_empty_when_only_live_owner() {
        let ds = vec![classified(DaemonClass::LiveOwner, 1)];
        assert!(reapable(&ds).is_empty());
    }

    #[test]
    fn owner_holding_live_socket_is_live() {
        let out = classify(
            vec![proc(100, "/usr/local/bin/ainb")],
            Some(100),
            &[100],
            None,
        );
        assert_eq!(out[0].class, DaemonClass::LiveOwner);
    }

    #[test]
    fn owner_not_serving_is_stale() {
        let out = classify(vec![proc(100, "/usr/local/bin/ainb")], Some(100), &[], None);
        assert_eq!(out[0].class, DaemonClass::StaleOwner);
    }

    #[test]
    fn non_owner_is_orphan() {
        let out = classify(
            vec![proc(200, "/usr/local/bin/ainb")],
            Some(100),
            &[100],
            None,
        );
        assert_eq!(out[0].class, DaemonClass::Orphan);
    }

    #[test]
    fn live_socket_holder_is_spared_even_when_pid_file_points_elsewhere() {
        // Spawn race: the process that won the socket (200) is not the pid
        // the file records (100, now dead). The live server must classify as
        // LiveOwner and never be reaped — the bug this guards against.
        let out = classify(vec![proc(200, "/x/ainb")], Some(100), &[200], None);
        assert_eq!(out[0].class, DaemonClass::LiveOwner);
        assert!(
            reapable(&out).is_empty(),
            "must not reap the live socket holder"
        );
    }

    #[test]
    fn no_owner_and_nothing_serving_makes_everything_orphan() {
        let out = classify(
            vec![proc(100, "/a/ainb"), proc(200, "/b/ainb")],
            None,
            &[],
            None,
        );
        assert!(out.iter().all(|d| d.class == DaemonClass::Orphan));
    }

    #[test]
    fn binary_drift_flagged_for_nonexistent_mismatched_path() {
        // Neither path resolves, so falls back to string compare → drift.
        let out = classify(
            vec![proc(100, "/opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb")],
            Some(100),
            &[100],
            Some("/opt/homebrew/Cellar/ainb/1.9.4/libexec/ainb"),
        );
        assert!(out[0].binary_drift);
        assert_eq!(out[0].class, DaemonClass::LiveOwner);
    }

    #[test]
    fn no_drift_when_paths_match() {
        let out = classify(
            vec![proc(100, "/same/path/ainb")],
            Some(100),
            &[100],
            Some("/same/path/ainb"),
        );
        assert!(!out[0].binary_drift);
    }

    #[test]
    fn parse_ps_line_extracts_fields() {
        let p = parse_ps_line(
            "  41530 06-04:04:24 /opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb notifyd run",
        )
        .unwrap();
        assert_eq!(p.pid, 41530);
        assert_eq!(p.etime, "06-04:04:24");
        assert_eq!(p.bin, "/opt/homebrew/Cellar/ainb/1.7.4/libexec/ainb");
        assert!(is_notifyd(&p));
    }

    #[test]
    fn is_notifyd_rejects_plain_ainb_tui() {
        let p = NotifydProc {
            pid: 1,
            bin: "/usr/local/bin/ainb".to_string(),
            cmd: "/usr/local/bin/ainb".to_string(),
            etime: "01:00".to_string(),
        };
        assert!(!is_notifyd(&p));
    }

    #[test]
    fn is_notifyd_accepts_slim_binary() {
        let p = NotifydProc {
            pid: 1,
            bin: "/usr/local/bin/ainb-notifyd".to_string(),
            cmd: "/usr/local/bin/ainb-notifyd run".to_string(),
            etime: "01:00".to_string(),
        };
        assert!(is_notifyd(&p));
    }

    /// Was `is_notifyd_accepts_bare_subcommand`, which asserted the opposite.
    /// A missing subcommand meant "daemon" to the old matcher, so ANY command
    /// line ending in `notifyd` — `ainb logs notifyd`, `ainb plugin install
    /// notifyd` — was classified as a reapable daemon. Losing a bare-invoked
    /// daemon from the overlay costs nothing; SIGKILLing a CLI call does.
    #[test]
    fn is_notifyd_rejects_argv_without_an_explicit_run() {
        for cmd in [
            "/usr/local/bin/ainb notifyd",
            "/usr/local/bin/ainb logs notifyd",
            "/usr/local/bin/ainb plugin install notifyd",
            "/usr/local/bin/ainb-notifyd",
        ] {
            let p = NotifydProc {
                pid: 1,
                bin: cmd.split_whitespace().next().unwrap().to_string(),
                cmd: cmd.to_string(),
                etime: "01:00".to_string(),
            };
            assert!(!is_notifyd(&p), "should reject `{cmd}`");
        }
    }

    #[test]
    fn is_notifyd_rejects_transient_cli_subcommands() {
        // `ainb notifyd status|stop|install|...` are short-lived CLI calls,
        // not daemons — labelling one ORPHAN with a kill hint would be wrong.
        for sub in ["status", "stop", "install", "uninstall", "list"] {
            let p = NotifydProc {
                pid: 1,
                bin: "/usr/local/bin/ainb".to_string(),
                cmd: format!("/usr/local/bin/ainb notifyd {sub}"),
                etime: "00:01".to_string(),
            };
            assert!(!is_notifyd(&p), "should reject `ainb notifyd {sub}`");
        }
    }
}
