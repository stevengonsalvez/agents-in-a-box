// ABOUTME: Hangar daemon lifecycle: locate, start, upgrade, stop, restart and
// prune the `ainb-hangar-daemon` process and its homes. The service layer
// (session launch, the daemons probe) drives it directly; the `ainb hangar`
// CLI in `ainb-core` re-exports it for its `daemon` verbs.

use anyhow::{Context, Result};
use clap::Args;

/// Options for `hangar daemon prune`.
///
/// The verb reports by default and only acts under `--yes`, because acting
/// means deleting a directory tree. It never signals a process, under either
/// mode: the ownership records that would name the target live in a
/// world-writable temp tree, so a stale or copied one can name any pid on the
/// machine, up to and including the developer's real daemon.
#[derive(Args, Debug, Clone)]
pub struct PruneArgs {
    /// Actually delete the homes that no live process owns. Without it, nothing
    /// on disk is touched. Homes with a live owner are always left alone.
    #[arg(long)]
    pub yes: bool,
    /// Only consider homes untouched for at least this many days (default 1).
    ///
    /// A live run's home looks exactly like a leaked one: no daemon is ever
    /// spawned into an ephemeral home, so nothing records an owner there. Age
    /// is what separates "a run that finished yesterday" from "a run that is
    /// still going". `0` removes that gate.
    #[arg(long, value_name = "DAYS", default_value_t = PRUNE_DEFAULT_MIN_AGE_DAYS)]
    pub older_than: u64,
}

/// Default `--older-than`: a home touched within the last day is presumed to
/// belong to a run that is still going.
pub const PRUNE_DEFAULT_MIN_AGE_DAYS: u64 = 1;

/// Env override pointing at the `ainb-hangar-daemon` binary to spawn.
///
/// Production resolves the binary as a sibling of the running `ainb` executable
/// (then falls back to `$PATH`); the integration test sets this to the
/// cargo-built daemon binary so `start` spawns the test artifact, not whatever
/// `ainb-hangar-daemon` happens to be installed.
pub const DAEMON_BIN_ENV: &str = "AINB_HANGAR_DAEMON_BIN";

/// Resolve the path to the daemon's PID file: `<hangar_home>/hangar/daemon.pid`.
pub fn daemon_pid_path() -> Result<std::path::PathBuf> {
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    // One source of truth with the daemon's own boot-time self-registration.
    Ok(ainb_hangar_daemon::pid_path_in(&home))
}

/// Path to the file recording the version of the binary that started the
/// running daemon, written beside the pid file at launch.
///
/// This lets startup hand off to a newer installed daemon after `brew upgrade`,
/// without letting an older debug binary downgrade a newer owner.
pub fn daemon_version_path() -> Result<std::path::PathBuf> {
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    Ok(home.join("hangar").join("daemon.version"))
}

/// Path to the exact executable that successfully claimed this Hangar home.
pub fn daemon_binary_path() -> Result<std::path::PathBuf> {
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    Ok(home.join("hangar").join("daemon.binary"))
}

/// True only when a recorded release version is older than this binary.
///
/// Version files contain Cargo package versions, so a three-component numeric
/// comparison is sufficient and avoids adding a dependency to the launcher.
/// Unknown, malformed, prerelease, and equal versions deliberately keep the
/// existing owner: autostart may upgrade, never guess or downgrade.
pub fn release_version_parts(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.').map(str::parse::<u64>);
    let parsed = [
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ];
    parts.next().is_none().then_some(parsed)
}

pub fn daemon_upgrade_required(running: Option<&str>, mine: &str) -> bool {
    match (
        running.and_then(release_version_parts),
        release_version_parts(mine),
    ) {
        (Some(running), Some(mine)) => running < mine,
        _ => false,
    }
}

/// Infer a release version only from Homebrew's immutable Cellar path.
///
/// This bridges installs where the old daemon predates `daemon.version`. Other
/// launch paths stay unknown, so a development or manually-installed owner is
/// never replaced merely because its version cannot be proven.
pub fn homebrew_daemon_version(command: &str) -> Option<String> {
    let marker = "/Cellar/ainb/";
    command
        .split_whitespace()
        .find_map(|word| word.split_once(marker).map(|(_, tail)| tail))
        .and_then(|tail| tail.split('/').next())
        .filter(|version| release_version_parts(version).is_some())
        .map(str::to_string)
}

/// Resolve a live owner's version from its launch record, or from Homebrew's
/// immutable Cellar path during the one-time upgrade from pre-record releases.
pub fn running_daemon_version(pid: Option<u32>) -> Option<String> {
    daemon_version_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
        .or_else(|| {
            pid.and_then(|pid| i32::try_from(pid).ok())
                .and_then(ainb_hangar_daemon::single_instance::process_argv)
                .and_then(|args| homebrew_daemon_version(&args))
        })
}

/// Runtime identity of the daemon that owns this Hangar home.
///
/// The Daemons overlay uses this exact probe rather than inferring ownership
/// from a reachable socket. That keeps its displayed pid, launch path, version,
/// and upgrade warning consistent with `ainb hangar daemon status`.
#[derive(Debug, Clone, Default)]
pub struct DaemonRuntimeStatus {
    pub pid: Option<u32>,
    pub command: Option<String>,
    pub version: Option<String>,
    /// A released daemon older than this binary is still serving this home.
    pub old: bool,
}

/// Return the authoritative recorded owner and its launch identity.
pub fn daemon_runtime_status() -> DaemonRuntimeStatus {
    let pid = running_daemon_pid().ok().flatten();
    let command = daemon_binary_path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
        .or_else(|| {
            pid.and_then(|pid| i32::try_from(pid).ok())
                .and_then(ainb_hangar_daemon::single_instance::process_binary)
        });
    let version = running_daemon_version(pid);
    let old =
        pid.is_some() && daemon_upgrade_required(version.as_deref(), env!("CARGO_PKG_VERSION"));
    DaemonRuntimeStatus {
        pid,
        command,
        version,
        old,
    }
}

/// Read the recorded daemon pid, or `None` if the file is absent/empty/garbage.
pub fn read_daemon_pid(path: &std::path::Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

/// The pid of the daemon that currently owns this hangar home, if any.
///
/// Ownership lives in `<hangar_home>/hangar/daemon.lock`, which the daemon
/// publishes atomically as the first statement of `boot` and holds for its whole
/// life, unlike `daemon.pid`, a last-write-wins note written at the end of boot.
///
/// The ownership lock answers first. The pid file is consulted only as a
/// fallback, for the upgrade window in which a daemon built before the lock
/// existed is still running, it registers a pid but never takes the lock, and
/// without this shim `status` and `stop` would report it as not running.
//
// ponytail: drop the pid-file fallback a release after the lock ships; by then
// no pre-lock daemon can still be alive.
pub fn running_daemon_pid() -> Result<Option<u32>> {
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    Ok(running_daemon_pid_in(&home))
}

/// [`running_daemon_pid`] against an explicit hangar home, so the
/// lock-beats-pid-file precedence is testable without touching the environment.
pub fn running_daemon_pid_in(home: &std::path::Path) -> Option<u32> {
    let socket = home.join("hangar.sock");
    let owner = |path: std::path::PathBuf| {
        read_daemon_pid(&path).filter(|pid| pid_owns_a_home(*pid, &socket))
    };
    owner(ainb_hangar_daemon::single_instance::lock_path_in(home))
        .or_else(|| owner(ainb_hangar_daemon::pid_path_in(home)))
}

/// Is `pid` this home's daemon?
///
/// PROOF first: a process holding `<hangar home>/hangar.sock` under our uid is
/// this home's daemon and can be nothing else ([`OwnedPid::holding_socket`]).
/// That is the same evidence `stop` demands before signalling, so every verb now
/// agrees about who is running.
///
/// The argv shape is the boot-window fallback ONLY. The daemon publishes its
/// lock as the first statement of `boot` but binds the socket at the end, so in
/// between there is a live, legitimate daemon holding no socket yet.
/// `is_hangar_daemon_args` is deliberately narrow and must NEVER credit a bare
/// `ainb`: on a layout with no sidecar binary the daemon self-execs as
/// `ainb hangar daemon run`, so daemon and TUI share an executable, and a
/// recycled pid landing on the user's TUI would otherwise read as this home's
/// daemon, wedging the home with ZERO daemons for as long as that TUI lived.
pub fn pid_owns_a_home(pid: u32, socket: &std::path::Path) -> bool {
    if !pid_is_running(pid) {
        return false;
    }
    if OwnedPid::holding_socket(pid, socket).is_some() {
        return true;
    }
    i32::try_from(pid).is_ok_and(|pid| {
        ainb_hangar_daemon::single_instance::process_argv(pid)
            .is_some_and(|args| argv_credits_a_daemon(&args))
    })
}

/// The argv half of [`pid_owns_a_home`], as a pure function so the boot-window
/// fallback's exact credit rule is testable without spawning a process.
///
/// A recognised daemon shape counts, EXCEPT from a `cargo test` binary.
pub fn argv_credits_a_daemon(args: &str) -> bool {
    !is_cargo_test_binary(args) && ainb_hangar_daemon::single_instance::is_hangar_daemon_args(args)
}

/// Is this argv a `cargo test` binary (`.../target/<profile>/deps/<name>-<hash>`)?
///
/// A test binary that self-exec'd as `... hangar daemon run` (#715) writes the
/// REAL `daemon.pid` but never binds the socket. The argv fallback above would
/// then credit it as this home's daemon on shape alone, autostart would decline
/// to start anything, and every socket client would get ECONNREFUSED for as
/// long as that pid lived -- a home wedged with ZERO daemons, which is exactly
/// what the fallback exists to avoid.
///
/// Deliberately NOT applied to `single_instance::holder_is_live_daemon`: that
/// caller has the opposite asymmetry (a false negative there steals a live
/// holder's lock and puts two daemons on one home) and already respects an
/// in-process `boot()` inside a test binary via `shares_our_command_line`.
pub fn is_cargo_test_binary(args: &str) -> bool {
    let Some(argv0) = args.split_whitespace().next() else {
        return false;
    };
    let path = std::path::Path::new(argv0);
    path.parent()
        .and_then(std::path::Path::file_name)
        .is_some_and(|dir| dir == "deps")
        && path.components().any(|part| part.as_os_str() == "target")
}

/// Is `pid` a live process? `kill(pid, 0)` succeeds iff it exists (and we may
/// signal it), so this is a non-destructive liveness probe.
///
/// `EPERM` counts as ALIVE: the process exists, we simply do not own it. Reading
/// it as dead (the prior behaviour) would let a daemon running under another
/// account be treated as absent and duplicated. Matches the daemon-side
/// `beads_adapter::lock::pid_alive`, so both halves agree on what "running"
/// means.
pub fn pid_is_running(pid: u32) -> bool {
    use nix::errno::Errno;
    use nix::sys::signal::kill;
    use nix::unistd::Pid;
    matches!(
        kill(Pid::from_raw(pid as i32), None),
        Ok(()) | Err(Errno::EPERM)
    )
}

/// Resolve how `start` launches the daemon: the dedicated
/// `ainb-hangar-daemon` binary when one can be found ([`DAEMON_BIN_ENV`]
/// override → sibling of the current executable → `$PATH`), else re-exec this
/// very `ainb` binary with `hangar daemon run`. The daemon library is
/// compiled into `ainb`, and installed layouts (e.g. Homebrew) ship no
/// sidecar binary, without the fallback, `start` failed with an error nobody
/// saw (the TUI plugin spawns this CLI with discarded stdio) and the offline
/// panel sat there forever.
pub fn resolve_daemon_launch() -> (std::path::PathBuf, Vec<&'static str>) {
    // An explicit override is honoured verbatim, if it points at nothing,
    // fail loudly rather than silently running something else.
    if let Some(p) = std::env::var_os(DAEMON_BIN_ENV).filter(|p| !p.is_empty()) {
        return (std::path::PathBuf::from(p), Vec::new());
    }
    let exe = std::env::current_exe().ok();
    resolve_daemon_launch_for(exe.as_deref())
}

/// Modification time of `path`, or `None` if it can't be read.
pub fn file_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Is a sibling `ainb-hangar-daemon` fresh enough to prefer over the self-exec
/// fallback, given the sibling's and this `ainb`'s modification times?
///
/// The daemon library is compiled INTO `ainb`, so the self-exec fallback
/// (`ainb hangar daemon run`) always executes code exactly as fresh as this
/// binary. The standalone sibling is therefore only a nicety, and a *stale*
/// one is actively harmful: a plain `cargo build` (default-members) historically
/// rebuilt only `ainb`, leaving an older `ainb-hangar-daemon` beside it, so
/// `start` silently launched pre-fix daemon code. We consequently prefer the
/// sibling ONLY when it is at least as new as `ainb`; an older sibling is
/// treated as a stale build and skipped in favour of the fresh embedded daemon.
/// When either mtime is unreadable we degrade to trusting the sibling, preserving
/// the prior behaviour for layouts where file times are unavailable.
pub fn sibling_daemon_is_fresh(
    sibling_mtime: Option<std::time::SystemTime>,
    exe_mtime: Option<std::time::SystemTime>,
) -> bool {
    match (sibling_mtime, exe_mtime) {
        (Some(sibling), Some(exe)) => sibling >= exe,
        _ => true,
    }
}

/// The `current_exe`-parameterised core of [`resolve_daemon_launch`], split out
/// so the sibling-vs-self-exec decision is unit-testable without depending on
/// the test runner's own executable path. `exe` is the resolved path of the
/// running `ainb` (`None` iff `current_exe` failed).
pub fn resolve_daemon_launch_for(
    exe: Option<&std::path::Path>,
) -> (std::path::PathBuf, Vec<&'static str>) {
    if let Some(exe) = exe {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("ainb-hangar-daemon");
            if sibling.exists() {
                if sibling_daemon_is_fresh(file_mtime(&sibling), file_mtime(exe)) {
                    return (sibling, Vec::new());
                }
                // Stale sibling (older than this `ainb`): don't silently launch
                // pre-fix daemon code. Self-exec the fresh embedded daemon.
                return (exe.to_path_buf(), vec!["hangar", "daemon", "run"]);
            }
        }
    }
    if let Some(on_path) = find_on_path("ainb-hangar-daemon") {
        return (on_path, Vec::new());
    }
    // Self-exec fallback. `current_exe` failing is effectively unreachable;
    // degrade to the bare `ainb` name resolved by the OS if it does.
    let me = exe.map_or_else(
        || std::path::PathBuf::from("ainb"),
        std::path::Path::to_path_buf,
    );
    (me, vec!["hangar", "daemon", "run"])
}

/// First `$PATH` entry containing a file named `name`.
pub fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).map(|d| d.join(name)).find(|c| c.is_file())
}

/// What [`ensure_hangar_daemon`] did about this process's hangar home.
///
/// Reported rather than swallowed because the autostart has two callers with
/// opposite needs. The TUI treats every outcome the same (it renders the offline
/// panel until a daemon appears), but `ainb run`'s Codex remote thread connects
/// to the daemon on its very next statement, so for it a silent
/// [`Self::SkippedEphemeralHome`] is not a skip at all: it is a connect failure
/// a few lines later, with nothing in the message naming the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonAutostart {
    /// A daemon is expected on this home: one was spawned, an older release was
    /// handed off, or a live one already owned it.
    ///
    /// Deliberately ONE variant, not two. A separate "already running" reading
    /// is unobservable: `start_daemon_if_stopped` is a no-op when a live owner
    /// holds the home, so the difference is only a pid read taken a moment
    /// earlier, and both readings mean the same thing to every caller. Splitting
    /// them would invite a caller to branch on a race.
    Started,
    /// The home is ephemeral and nobody asked for it, so no daemon was started
    /// and none ever will be for this home. A caller that needs the daemon must
    /// turn this into an error naming the remedy (set `$AINB_HANGAR_HOME` to a
    /// durable path); a caller that only wants one if it is useful may ignore it.
    SkippedEphemeralHome,
    /// The spawn was attempted and failed. Non-fatal by design: the error is
    /// already logged, and the TUI still launches.
    Failed,
}

/// Best-effort autostart of the Hangar daemon before the TUI connects.
///
/// Idempotent (a live pid is a no-op) and non-fatal: a spawn failure is logged
/// and swallowed so the TUI still launches (it shows the offline panel until the
/// daemon comes up). Quiet, no stdout, since the TUI owns the terminal. Mirrors
/// `mcp_pool`'s `ensure_daemon` warn-and-continue.
pub fn ensure_hangar_daemon(launcher: LauncherLifetime) -> DaemonAutostart {
    let home = ainb_hangar_daemon::hangar_dir().ok();
    ensure_hangar_daemon_with(home.as_deref(), hangar_home_was_requested(), || {
        match start_or_upgrade_daemon(false, launcher) {
            Ok(outcome) => outcome,
            Err(e) => {
                tracing::warn!(error = %e, "hangar daemon autostart failed (TUI continues)");
                DaemonAutostart::Failed
            }
        }
    })
}

/// [`ensure_hangar_daemon`] with its two impure inputs and its one side effect
/// passed in: the resolved home, whether it was asked for, and the spawn itself.
///
/// The gate's headline promise is that nothing is SPAWNED into an ephemeral
/// home, and only a seam can prove that. Asserting on the predicate alone
/// leaves the wiring between predicate and spawn untested, which is how a gate
/// that is computed and then ignored still passes its suite.
///
/// `home` is `None` only when the hangar home cannot be resolved at all. There
/// is then no path to judge, so the spawn runs and fails (or succeeds) on its
/// own terms rather than being declined on a guess.
pub fn ensure_hangar_daemon_with<F>(
    home: Option<&std::path::Path>,
    home_was_requested: bool,
    spawn: F,
) -> DaemonAutostart
where
    F: FnOnce() -> DaemonAutostart,
{
    if let Some(home) = home {
        if !autostart_allowed(home, home_was_requested) {
            tracing::info!(
                home = %home.display(),
                "hangar home is ephemeral; not autostarting a daemon"
            );
            return DaemonAutostart::SkippedEphemeralHome;
        }
    }
    spawn()
}

/// Was this process's hangar home asked for, or merely derived from `$HOME`?
///
/// `ainb_hangar_core::hangar_home` reads `$AINB_HANGAR_HOME` first and falls
/// back to `~/.agents-in-a-box`, so a set, non-empty value is the one signal
/// that separates "the caller chose this home" from "this home is wherever
/// `$HOME` happened to point".
///
/// The environment read lives here, alone, so [`autostart_allowed`] stays a
/// pure function: `std::env::set_var` is process-global and this suite is
/// multi-threaded, so the decision has to be testable without mutating it.
///
/// This function is the ONE impure input the whole autostart gate turns on, so
/// its rule is split out into [`home_was_requested`] and pinned there: reading
/// it wrong (crediting an empty value, say) silently re-enables the spawn this
/// gate exists to prevent, and every other test in the gate would still pass.
pub fn hangar_home_was_requested() -> bool {
    home_was_requested(std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV).as_deref())
}

/// Does this raw `$AINB_HANGAR_HOME` value mean "the caller chose this home"?
///
/// Set-and-non-empty, which is EXACTLY the test
/// `ainb_hangar_core::paths::hangar_home` applies before honouring the variable
/// (`var_os(..).filter(|p| !p.is_empty())`). The two must agree: a value this
/// says was requested but that one ignores would credit an explicit choice to a
/// home derived from `$HOME`, which is the ephemeral case the gate declines.
pub fn home_was_requested(value: Option<&std::ffi::OsStr>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}

/// Should the autostart bring a daemon up for this home?
///
/// No, when the home is ephemeral AND was derived from `$HOME` rather than
/// asked for. Nothing consumes such a daemon, and the home is deleted the
/// moment the harness exits, taking every guard that could address the process
/// later with it: the pid file, the single-instance lock and `hangar daemon
/// stop` are all home-scoped. The whole boot (store open, migrations, socket
/// bind, sweepers) is spent to produce a process whose only future is the
/// parent-death watchdog. Do not spawn it; the watchdog stays the backstop for
/// every other way a daemon reaches such a home.
///
/// An explicit `$AINB_HANGAR_HOME` under the temp dir is a deliberate choice
/// (every tripwire in this repo makes it, and does want a daemon), so it still
/// autostarts, as does any home under a real `$HOME`. The explicit CLI verbs
/// (`hangar daemon start`, `daemon run`, `daemon setup`) and the Daemons
/// screen's control never come through here: an explicit request is honoured
/// whatever the home looks like. Issue #784 follow-up.
pub fn autostart_allowed(hangar_home: &std::path::Path, home_was_requested: bool) -> bool {
    home_was_requested || !is_under_temp_dir(hangar_home)
}

/// Start a missing daemon, or hand off an older recorded release to this one.
///
/// Unknown, equal, prerelease, and newer owners are deliberately left alone:
/// autostart may upgrade a released daemon but must not guess or downgrade.
pub fn start_or_upgrade_daemon(
    announce: bool,
    launcher: LauncherLifetime,
) -> Result<DaemonAutostart> {
    let pid = running_daemon_pid().ok().flatten();
    let running = running_daemon_version(pid);
    if pid.is_some() && daemon_upgrade_required(running.as_deref(), env!("CARGO_PKG_VERSION")) {
        restart_daemon(announce, launcher)?;
        return Ok(DaemonAutostart::Started);
    }
    start_daemon_if_stopped(announce, launcher)?;
    // `start_daemon_if_stopped` is a no-op when a live owner already holds the
    // home, so this reports "a daemon is expected here" either way. The pid read
    // above is not consulted for the outcome: it was taken before the spawn, so
    // a daemon that appeared in between would make the two disagree, and no
    // caller can act on the difference anyway.
    Ok(DaemonAutostart::Started)
}

/// File name of the captured daemon stderr, inside the daemon's log dir.
///
/// Deliberately NOT `daemon.*`: `ainb_hangar_core::logs::log_files_newest_first`
/// globs that log dir with `starts_with("daemon")` and feeds the newest match to
/// the JSON log-tail surfaces. A `daemon.stderr.log` there would become "the
/// newest daemon log", parse as zero JSON lines, and silently blank
/// `ainb hangar logs tail` and the Daemons pane.
pub const DAEMON_STDERR_LOG: &str = "hangar-daemon.stderr.log";

/// Roll the stderr capture over at this size, keeping one previous generation:
/// so the file is bounded at ~2x this, forever, with no background rotator.
pub const DAEMON_STDERR_MAX_BYTES: u64 = 1024 * 1024;

/// Open the append-mode stderr capture for a daemon about to be spawned,
/// rotating it first if it has grown past [`DAEMON_STDERR_MAX_BYTES`].
///
/// Rotation happens only here, at spawn time: the daemon holds the fd for its
/// whole life, so rotating underneath a running daemon would silently orphan its
/// writes into an unlinked inode. One spawn, one rotation check, one generation
/// kept (`.1`).
pub fn open_daemon_stderr_log(log_dir: &std::path::Path) -> Result<std::fs::File> {
    std::fs::create_dir_all(log_dir).context("create hangar log dir")?;
    let path = log_dir.join(DAEMON_STDERR_LOG);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= DAEMON_STDERR_MAX_BYTES) {
        // Best-effort: a failed roll just means we keep appending to a big file,
        // which beats refusing to capture stderr at all.
        std::fs::rename(&path, path.with_extension("log.1")).ok();
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open daemon stderr log {}", path.display()))
}

/// True when `path` sits under the system temp dir.
///
/// Checks `$TMPDIR` plus the fixed `/tmp` and `/var/tmp` roots, because on macOS
/// `std::env::temp_dir()` is `$TMPDIR` (`/var/folders/...`) and says nothing
/// about `/tmp`, which is where this repo's own recording harnesses put their
/// homes (`mktemp -d /tmp/bj.XXXXXX`).
///
/// Both sides are canonicalized on the second attempt because those roots are
/// reached through symlinks on macOS (`/tmp` -> `/private/tmp`), so a caller
/// that resolved its home would otherwise slip past a raw prefix test.
pub fn is_under_temp_dir(path: &std::path::Path) -> bool {
    let resolved = canonical_or_self(path);
    temp_roots()
        .iter()
        .any(|root| path.starts_with(root) || resolved.starts_with(canonical_or_self(root)))
}

/// The roots [`is_under_temp_dir`] treats as "the system temp dir", and the
/// only places [`run_daemon_prune`] will ever look for a leaked home.
///
/// A `TMPDIR=/` makes every path on the machine read as ephemeral, the real
/// home included, so a root that contains everything is dropped: nothing is
/// learned from it, and prune would offer to delete the whole filesystem.
pub fn temp_roots() -> Vec<std::path::PathBuf> {
    [
        std::env::temp_dir(),
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/var/tmp"),
    ]
    .into_iter()
    .filter(|root| !contains_the_whole_filesystem(root))
    .collect()
}

/// Does this candidate temp root contain every path on the machine?
///
/// Asked of BOTH forms, because [`is_under_temp_dir`] decides membership with
/// both: a raw prefix test against the root as written, and a resolved prefix
/// test against its canonical form. A `TMPDIR=/..` has a raw parent (`/`) and
/// so walks past a raw-only check, while the form membership actually uses
/// resolves to `/` and matches every path there is.
pub fn contains_the_whole_filesystem(root: &std::path::Path) -> bool {
    root.parent().is_none() || canonical_or_self(root).parent().is_none()
}

/// `path` with every symlink resolved, or `path` itself when it cannot be
/// resolved (it may not exist yet, which is not an error to any caller here).
pub fn canonical_or_self(path: &std::path::Path) -> std::path::PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Does the process launching a daemon stay alive to use it?
///
/// The parent-death watchdog is only ever correct for a launcher that outlives
/// the daemon it wants. `ainb hangar daemon start` and `ainb doctor` exit about
/// a second after the spawn: binding a daemon to one of those would stand it
/// down immediately and report success for a process that is already dying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherLifetime {
    /// The TUI: it lives for as long as the daemon is wanted.
    Persistent,
    /// A one-shot CLI verb, which is expected to leave the daemon behind.
    Ephemeral,
}

/// Should the daemon we are about to spawn be bound to this process's life?
///
/// Three conditions, all required:
///
/// 1. We outlive it. A one-shot verb must leave a daemon that survives it.
/// 2. Its home is ephemeral, so no other guard can ever address it again:
///    `single_instance`, `hangar daemon stop` and the pid file are all
///    home-scoped, and the home dies with whoever created it (any harness
///    running under `HOME=$(mktemp -d)`). macOS has no `PR_SET_PDEATHSIG` and
///    the child is detached into its own process group, so it otherwise
///    reparents to launchd and runs forever.
/// 3. Nobody upstream already declared a parent. A harness that set the env var
///    itself named a longer-lived owner on purpose; overriding it with our pid
///    would kill the daemon the harness is still driving.
///
/// Inert for a home under a real `$HOME`, so the Homebrew daemon is untouched.
/// A deliberate dev-build daemon on a temp `AINB_HANGAR_HOME` IS in scope, but
/// only when started by a persistent launcher. Issue #784.
///
/// Known gap: a restart carries no binding forward. `ainb doctor` or
/// `hangar daemon restart` replacing a daemon that the TUI had bound leaves an
/// unbound one behind, which is the pre-#784 behaviour rather than a new leak.
/// Reading the incumbent's environment to inherit its parent is the fix, and it
/// is deliberately not attempted here.
pub fn should_bind_daemon_to_parent(
    hangar_home: &std::path::Path,
    launcher: LauncherLifetime,
    parent_already_declared: bool,
) -> bool {
    launcher == LauncherLifetime::Persistent
        && !parent_already_declared
        && is_under_temp_dir(hangar_home)
}

/// Arm the spawned daemon's parent-death watchdog when [`should_bind_daemon_to_parent`]
/// says this daemon must not outlive us.
pub fn arm_watchdog_for_ephemeral_home(
    command: &mut std::process::Command,
    hangar_home: &std::path::Path,
    launcher: LauncherLifetime,
) {
    let declared = [
        ainb_hangar_daemon::PARENT_PID_ENV,
        ainb_hangar_daemon::LEGACY_PARENT_PID_ENV,
    ]
    .iter()
    .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));

    if !should_bind_daemon_to_parent(hangar_home, launcher, declared) {
        return;
    }
    let pid = std::process::id().to_string();
    // A daemon that stands down for this reason writes its last line into
    // <home>/hangar/logs, inside the home that is about to be deleted. Say so
    // here too, where the log survives.
    tracing::info!(
        home = %hangar_home.display(),
        parent = %pid,
        "hangar home is ephemeral; binding the daemon's life to this process"
    );
    // Both names: the daemon binary we launch may be an installed build that
    // predates the rename and only knows the legacy one.
    command
        .env(ainb_hangar_daemon::PARENT_PID_ENV, &pid)
        .env(ainb_hangar_daemon::LEGACY_PARENT_PID_ENV, &pid);
}

/// Spawn the daemon as a detached background child unless it is already running,
/// recording its EXACT pid. When `announce` is true the outcome is printed
/// (the `hangar daemon start` CLI verb); the TUI autostart passes `false`.
pub fn start_daemon_if_stopped(announce: bool, launcher: LauncherLifetime) -> Result<()> {
    // `resolve_daemon_launch` self-execs `(current_exe, ["hangar","daemon","run"])`
    // on both of its fallback paths. Under `cargo test` that exe is the TEST
    // binary, and libtest reads the argv as name filters, so the "daemon" is a
    // detached re-run of every test matching `hangar`/`daemon`/`run`. See
    // `crate::self_exec_guard` and issue #715.
    if crate::self_exec_guard::running_under_cargo_test() {
        anyhow::bail!(
            "refusing to start the hangar daemon from a cargo test binary \
             (current_exe is a test harness, not `ainb`)"
        );
    }
    let pid_path = daemon_pid_path()?;

    // Already running? Bail out cleanly rather than spawning a duplicate.
    //
    // This is now an OPTIMISATION, not the guard. It is still a check-then-act
    // across two processes, so it can lose a race, but a duplicate that gets
    // spawned anyway declines the home for itself (the daemon takes an exclusive
    // lock before it opens anything) and exits, which is what makes the residual
    // race harmless rather than a pile of daemons.
    if let Some(pid) = running_daemon_pid()? {
        if announce {
            println!("hangar daemon: already running (pid {pid})");
        }
        return Ok(());
    }
    // Stale pid file from a crashed daemon: drop it before re-spawning. The
    // stale LOCK needs no handling here, the booting daemon reclaims it
    // atomically, which is the only race-free way to take it.
    std::fs::remove_file(&pid_path).ok();

    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).context("create hangar home dir")?;
    }

    let (bin, args) = resolve_daemon_launch();
    let launched = if args.is_empty() {
        bin.display().to_string()
    } else {
        format!("{} {}", bin.display(), args.join(" "))
    };
    // A daemon that dies OUTSIDE `tracing` (a panic in the standalone binary,
    // an `abort`, an OOM kill) writes its last words to stderr. Discarding
    // them is why two of the four observed deaths left no trace at all: no
    // ERROR line, no panic, the JSON log just stops mid-stream. Capture stderr
    // to a file; stdin/stdout stay null (the child must not inherit this
    // process's controlling terminal).
    //
    // `log_dir` is not in scope here, but `pid_path` is: it resolves to
    // <hangar_home>/hangar/daemon.pid, so the logs live in its parent's `logs/`.
    let stderr = pid_path
        .parent()
        .context("resolve hangar home dir from pid path")
        .and_then(|hangar| open_daemon_stderr_log(&hangar.join("logs")));
    let stderr = match stderr {
        Ok(file) => std::process::Stdio::from(file),
        // Never block a daemon start on its diagnostic sink: fall back to the
        // old discard, loudly.
        Err(e) => {
            tracing::warn!(error = %e, "daemon stderr capture unavailable; discarding stderr");
            std::process::Stdio::null()
        }
    };
    let mut command = std::process::Command::new(&bin);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(stderr);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Ok(home) = ainb_hangar_daemon::hangar_dir() {
        arm_watchdog_for_ephemeral_home(&mut command, &home, launcher);
    }
    let mut child = command.spawn().with_context(|| format!("spawn daemon `{launched}`"))?;

    // A daemon that dies instantly (unbootable db, broken binary) used to look
    // identical to a clean start, the TUI's `[s]` action reported success and
    // the offline panel sat there forever. Give the child a beat, then
    // reap-check: `try_wait` returns the exit status iff it already died.
    std::thread::sleep(std::time::Duration::from_millis(400));
    let exited = child.try_wait().context("probe daemon child")?;
    // A clean exit is not a failure: it is how a daemon reports that another one
    // already owns this home (it lost the ownership lock and declined). Only a
    // NON-ZERO exit means the daemon could not boot.
    if let Some(status) = exited.filter(|status| !status.success()) {
        anyhow::bail!(
            "daemon exited immediately ({status}) — launched `{launched}`; \
             run `ainb hangar daemon run` in a terminal to see why"
        );
    }

    // Record the pid of the daemon that actually OWNS the home, which on a lost
    // race is the incumbent rather than the child we just spawned. The child
    // publishes the lock as its first action, so it is on disk by now; falling
    // back to the child pid keeps a slow-starting daemon recorded.
    let mut owner = running_daemon_pid()?;
    if exited.is_some() && owner.is_none() {
        // Our child exited 0 (it lost the lock) and yet nobody owns the home,
        // the incumbent it lost to has since exited too. Nothing is wrong here,
        // the home is simply free again, so try once more before calling it a
        // failure. Only a second empty-handed attempt is worth an error.
        if let Some(pid) = respawn_once(&bin, &args, launcher)? {
            owner = Some(pid);
        } else {
            anyhow::bail!(
                "daemon exited immediately ({}) without claiming the home — launched \
                 `{launched}`; run `ainb hangar daemon run` in a terminal to see why",
                exited.map_or_else(|| "?".to_string(), |s| s.to_string())
            );
        }
    }
    let pid = owner.unwrap_or_else(|| child.id());
    // `child` is intentionally dropped without `wait`, the daemon is meant to
    // outlive this CLI invocation.
    std::fs::write(&pid_path, format!("{pid}\n"))
        .with_context(|| format!("write pid file {}", pid_path.display()))?;

    // Record the version of the binary now serving, beside the pid. The launcher
    // and the daemon it spawns share the workspace version, so this is the
    // running daemon's version, but ONLY when our child is the one serving.
    // On the lost-race path the incumbent is some other build, and stamping our
    // version over it would silence the very skew warning `status` exists to
    // print. Best-effort: a write failure must not fail the start (the skew
    // check just degrades to "unknown").
    // Only when OUR child is the daemon this start recorded. That is the child
    // holding the lock at the probe, OR the no-owner fallback above: with no
    // lock on disk there is no incumbent whose stamp we could clobber, and the
    // pid file we just wrote names the child, so the stamp must match it. A
    // probe that merely finds the child still alive is NOT enough when someone
    // ELSE holds the lock: a slow starter can go on to lose the race, and
    // stamping our version over the incumbent's silences the very skew warning
    // `status` exists to print.
    if owner == Some(child.id()) || (owner.is_none() && exited.is_none()) {
        if let Ok(vpath) = daemon_version_path() {
            std::fs::write(&vpath, format!("{}\n", env!("CARGO_PKG_VERSION"))).ok();
        }
        if let Ok(bpath) = daemon_binary_path() {
            std::fs::write(&bpath, format!("{}\n", bin.display())).ok();
        }
    }

    if announce {
        if exited.is_some() {
            println!("hangar daemon: already running (pid {pid})");
        } else {
            println!("hangar daemon: started (pid {pid})");
        }
    }
    Ok(())
}

/// How long `stop` waits for the signalled daemon to actually exit before
/// giving up on confirming it.
pub const DAEMON_STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// Last-resort bound after a verified daemon ignores graceful shutdown.
pub const DAEMON_STOP_KILL_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Block until `pid` is gone or `grace` elapses; `true` iff it exited.
///
/// `stop` used to declare success the instant `SIGTERM` was delivered, which is
/// not the same thing as the daemon being gone, and `restart` immediately
/// spawned a replacement into a socket the old one still held.
pub fn wait_for_pid_exit(pid: u32, grace: std::time::Duration) -> bool {
    /// 60 probes across the grace window: fast enough that a normal stop returns
    /// in tens of milliseconds, cheap enough to be free.
    const PROBE: std::time::Duration = std::time::Duration::from_millis(50);
    let deadline = std::time::Instant::now() + grace;
    while std::time::Instant::now() < deadline {
        if !pid_is_running(pid) {
            return true;
        }
        std::thread::sleep(PROBE);
    }
    !pid_is_running(pid)
}

/// Record the daemon's exit on its behalf after a `stop`.
///
/// `SIGTERM`'s default disposition runs NO user code in the target, so the
/// daemon cannot write its own exit reason for this path, but the stopper knows
/// exactly why it died. Without this, a deliberate `stop` would leave the same
/// evidence as the SIGKILL/OOM deaths the breadcrumbs exist to catch: a stale
/// heartbeat and no exit reason.
///
/// When the process did NOT die, the heartbeat is left alone (it is still the
/// truth) and the reason records the unconfirmed kill.
pub fn record_daemon_stop_breadcrumb(pid: u32, died: bool, signal: &str) {
    let Ok(home) = ainb_hangar_daemon::hangar_dir() else {
        return;
    };
    if died {
        ainb_hangar_daemon::observability::record_external_exit(
            &home,
            pid,
            &format!("stopped by `ainb hangar daemon stop` ({signal}, exit confirmed)"),
        );
    } else {
        tracing::warn!(pid, "daemon survived SIGTERM; leaving heartbeat in place");
    }
}

/// Resolve the daemon's control socket: `<hangar_home>/hangar.sock`, the one the
/// RPC server binds, and the proof of identity `stop` demands before signalling.
pub fn daemon_socket_path() -> Result<std::path::PathBuf> {
    let home = ainb_hangar_daemon::hangar_dir().context("resolve hangar home")?;
    Ok(home.join("hangar.sock"))
}

/// The ownership lock that sits beside `<home>/hangar/daemon.pid`.
pub fn lock_beside(pid_path: &std::path::Path) -> std::path::PathBuf {
    pid_path.with_file_name("daemon.lock")
}

/// A pid this process has PROVED it may signal.
///
/// The only constructor is [`OwnedPid::holding_socket`] and the only signal path
/// takes one, so "signal whatever integer the pid file happened to contain" is a
/// type error. A pid file is a claim, not proof: the recorded process may have died
/// without cleaning up and had its pid recycled by something unrelated, the
/// classic way an automated `stop` takes down an innocent process.
#[derive(Debug, PartialEq, Eq)]
pub struct OwnedPid(u32);

impl OwnedPid {
    /// Prove `pid` is this home's daemon: it must hold a unix socket bound to
    /// `socket` (`<hangar home>/hangar.sock`) under the current uid. Fails closed,
    /// no `lsof`, an unreadable process, or any ambiguity answers `None`.
    fn holding_socket(pid: u32, socket: &std::path::Path) -> Option<Self> {
        pid_holds_socket(pid, socket).then_some(Self(pid))
    }
}

/// What `stop` is allowed to do about the pid recorded in the pid file.
#[derive(Debug, PartialEq, Eq)]
pub enum StopDecision {
    /// No usable pid recorded: absent, empty, or unparseable file.
    NotRecorded,
    /// The recorded pid is not running, a stale file to clean up.
    Stale(u32),
    /// Running, but it cannot be proved to be this home's daemon. Left alone.
    Unproven(u32),
    /// Running and proved ours.
    Signal(OwnedPid),
}

/// Decide what `stop` may do, given the pid file and the socket that proves
/// ownership. Split out from [`run_daemon_stop`] so the decision is testable
/// against real decoy processes without signalling anything.
pub fn stop_decision(pid_path: &std::path::Path, socket: &std::path::Path) -> StopDecision {
    // The ownership lock is published as the FIRST statement of boot; the pid
    // file is written at the very end of it. Reading only the pid file left a
    // window, seconds on a cold home, in which a daemon started by launchd, by
    // the binary directly, or by any route other than `daemon start` was
    // unstoppable: `stop` said "not running" while `start` said "already
    // running". Ask the lock first, exactly like `status` and the autostart do.
    let recorded = read_daemon_pid(&lock_beside(pid_path)).or_else(|| read_daemon_pid(pid_path));
    match recorded {
        None => StopDecision::NotRecorded,
        Some(pid) if !pid_is_running(pid) => StopDecision::Stale(pid),
        Some(pid) => match OwnedPid::holding_socket(pid, socket) {
            Some(owned) => StopDecision::Signal(owned),
            None => StopDecision::Unproven(pid),
        },
    }
}

/// `hangar daemon stop`: signal the EXACT recorded pid once it has PROVED it is
/// this home's daemon, then remove the file.
///
/// The pid is read back from the PID file and must then be shown to hold
/// `<hangar home>/hangar.sock` under our uid ([`OwnedPid`]) before any signal is
/// sent (never a name-based `pkill`, and never a bare pid-file integer). A stale
/// pid file (process already gone) is cleaned up; an absent file is reported as
/// "not running"; an unprovable pid is reported and left running.
///
/// Waits (bounded) for the process to actually exit, then records the exit
/// breadcrumb on its behalf: see [`record_daemon_stop_breadcrumb`].
pub fn stop_daemon(announce: bool) -> Result<()> {
    let pid_path = daemon_pid_path()?;
    let socket = daemon_socket_path()?;
    match stop_decision(&pid_path, &socket) {
        StopDecision::Signal(owned) => {
            use nix::sys::signal::{Signal, kill};
            use nix::unistd::Pid;
            let pid = owned.0;
            kill(Pid::from_raw(pid as i32), Signal::SIGTERM)
                .with_context(|| format!("send SIGTERM to pid {pid}"))?;
            let mut died = wait_for_pid_exit(pid, DAEMON_STOP_GRACE);
            let mut signal = "SIGTERM";
            if !died {
                // `OwnedPid` proves this is OUR daemon holding OUR socket. A
                // wedged signal handler must not turn repeated starts into an
                // orphaned-daemon pile, so escalation remains exact-pid only.
                kill(Pid::from_raw(pid as i32), Signal::SIGKILL)
                    .with_context(|| format!("send SIGKILL to unresponsive pid {pid}"))?;
                died = wait_for_pid_exit(pid, DAEMON_STOP_KILL_GRACE);
                signal = "SIGKILL after SIGTERM grace";
            }
            record_daemon_stop_breadcrumb(pid, died, signal);
            if died {
                std::fs::remove_file(&pid_path).ok();
                clear_daemon_version_record();
                if announce {
                    println!("hangar daemon: stopped (signalled pid {pid})");
                }
            } else {
                // Keep the pid file: it still names a live daemon. Dropping it
                // here (as this did unconditionally) would let the next `start`
                // spawn a SECOND daemon onto the same SQLite file: new write
                // contention, which is the last thing this daemon needs.
                if announce {
                    println!(
                        "hangar daemon: SIGTERM sent to pid {pid}, still alive after {}s \
                         (pid file kept; re-run stop or `kill -9 {pid}`)",
                        DAEMON_STOP_GRACE.as_secs()
                    );
                }
            }
        }
        StopDecision::Unproven(pid) => {
            // Fail closed. Either the daemon has not bound its socket yet, `lsof`
            // is unavailable, or, the case this exists for, the recorded pid now
            // belongs to somebody else entirely. Keep the pid file: it is the only
            // record of what was claimed, and a `start` must not race a daemon that
            // may still be coming up.
            if announce {
                println!(
                    "hangar daemon: refusing to signal pid {pid} - it does not hold {} \
                     (not this home's daemon, or the socket is not bound yet)",
                    socket.display()
                );
            }
        }
        StopDecision::Stale(pid) => {
            std::fs::remove_file(&pid_path).ok();
            clear_daemon_version_record();
            if announce {
                println!("hangar daemon: not running (cleaned up stale pid {pid})");
            }
        }
        StopDecision::NotRecorded => {
            clear_daemon_version_record();
            if announce {
                println!("hangar daemon: not running");
            }
        }
    }
    Ok(())
}

/// Remove launch identity records only after ownership is known to be gone.
///
/// A failed upgrade handoff leaves the incumbent alive, so retaining its
/// version is what lets the next `ensure_hangar_daemon` retry that handoff.
pub fn clear_daemon_version_record() {
    if let Ok(path) = daemon_version_path() {
        std::fs::remove_file(path).ok();
    }
    if let Ok(path) = daemon_binary_path() {
        std::fs::remove_file(path).ok();
    }
}

pub fn run_daemon_stop() -> Result<()> {
    stop_daemon(true)
}

/// What one leaked home's OWN ownership records say about its owner.
///
/// Every variant is decided from that home's own `hangar/daemon.lock` and
/// `hangar/daemon.pid`, and nothing else. The process table is NEVER scanned for
/// a matching argv: a `hangar daemon run` command line does not say which home
/// it serves, so matching on it would credit daemons belonging to other homes,
/// including a developer's real one (the hazard documented in
/// `single_instance::is_hangar_daemon_args`). The process table is consulted
/// only to ask what a pid a record already named is running.
///
/// The variants below split "a live process is named" three ways because the
/// REPORT differs, not because the action does: every one of them is left
/// alone. Prune signals nothing, so the only decision left is remove or keep,
/// and any live pid keeps the home.
#[derive(Debug, PartialEq, Eq)]
pub enum PruneVerdict {
    /// Not ours to touch: outside every temp root, under the invoking user's
    /// real home, unresolvable, or the home this very process resolved.
    Protected,
    /// A live process on this machine is running with this home as its `$HOME`
    /// or its `$AINB_HANGAR_HOME`.
    ///
    /// Not decided from any record inside the home: it is read from the process
    /// table, which is the only place a home in ACTIVE use differs from a leaked
    /// one. Since #784 no daemon is ever spawned into an ephemeral home, so a
    /// live run leaves no lock and no pid file, and every other verdict here
    /// would read it as litter.
    ///
    /// Also the verdict every candidate takes when the process table cannot
    /// answer at all: a missing answer is not evidence that nothing is running.
    InUse,
    /// Touched too recently to be presumed abandoned (see `--older-than`).
    Recent,
    /// Neither record names anyone: nothing ever recorded an owner for this
    /// home.
    NoOwner,
    /// A record naming a process that is gone.
    DeadOwner(u32),
    /// The home's OWN ownership lock names a live process the process table
    /// positively identifies as a hangar daemon.
    LiveDaemon(u32),
    /// A record naming a live process that is positively NOT a hangar daemon:
    /// the pid was recycled after the daemon died.
    Stranger(u32),
    /// A live pid whose ownership of this home cannot be proven: the process
    /// table could not answer, or the pid is named only by `daemon.pid`, which
    /// is a status note rather than the ownership record.
    Unconfirmed(u32),
    /// A record exists but yields no pid (unreadable, empty, or garbage).
    /// Fails closed: an unparsed file is not evidence that nothing is running.
    Unreadable,
}

impl PruneVerdict {
    /// The pid this home's records name as a LIVE owner, if any.
    ///
    /// Drives both the remedy line and the summary's left-alone breakdown, so
    /// the two can never disagree about which homes have an owner.
    const fn live_owner(&self) -> Option<u32> {
        match self {
            Self::LiveDaemon(pid) | Self::Stranger(pid) | Self::Unconfirmed(pid) => Some(*pid),
            Self::Protected
            | Self::InUse
            | Self::Recent
            | Self::NoOwner
            | Self::DeadOwner(_)
            | Self::Unreadable => None,
        }
    }

    /// May `--yes` delete this home?
    ///
    /// Only when both records agree that nobody is alive. `Unreadable` fails
    /// closed (an unparsed file is not evidence that nothing is running),
    /// `Protected` is the guard's refusal, and `InUse`/`Recent` are the two
    /// answers that keep a home whose records say nothing at all: a live
    /// ephemeral run has no owner record to name it.
    const fn is_removable(&self) -> bool {
        matches!(self, Self::NoOwner | Self::DeadOwner(_))
    }
}

/// What prune actually did about one home.
///
/// Deliberately has no variant that names a signalled process: removing a
/// directory is the only mutation this verb performs, so there is no outcome
/// left for one to describe.
#[derive(Debug, PartialEq, Eq)]
pub enum PruneAction {
    /// Dry run: the home is removable and `--yes` would delete it. Nothing was
    /// touched.
    Reported,
    /// A live owner (or an unreadable record, or the guard) forbids removal, in
    /// a dry run or otherwise. The home stays exactly as it is, and so does
    /// whatever process its records name.
    LeftAlone,
    /// The home directory was deleted.
    Removed,
    /// The home could not be deleted.
    RemoveFailed(String),
}

/// A home prune has already validated, carrying the EXACT path the guard
/// approved.
///
/// Handing [`prunable_in`]'s caller the raw path back would mean the delete
/// re-resolves every component a second time, so a symlink swapped in after the
/// guard answered would send `remove_dir_all` somewhere the guard never saw.
/// Temp roots are world-writable, which makes that a race an unprivileged local
/// process can actually run. One resolution, carried through to the removal, and
/// nothing downstream re-derives it.
pub struct PrunableHome(std::path::PathBuf);

/// The trees prune must never enter, resolved once per run.
///
/// Passed in rather than read inside the guard so every refusal is testable
/// against a fabricated subject. With the real values, a fabricated home is
/// already saved by the temp-root refusal, so a broken identity or real-home
/// check would go unnoticed.
pub struct ProtectedRoots {
    /// The hangar home THIS process resolved. Standing our own daemon's home
    /// down mid-verb is not cleanup.
    mine: Option<std::path::PathBuf>,
    /// The invoking user's home directory, from `dirs::home_dir()`.
    real_home: Option<std::path::PathBuf>,
}

impl ProtectedRoots {
    /// The roots as this process actually sees them.
    fn current() -> Self {
        Self {
            mine: ainb_hangar_daemon::hangar_dir().ok(),
            real_home: dirs::home_dir(),
        }
    }

    /// Explicit roots, so a test can isolate one refusal at a time. With the
    /// real values every fabricated subject is already saved by the temp-root
    /// check, and a broken refusal here would go unnoticed.
    #[cfg(test)]
    fn of(mine: Option<&std::path::Path>, real_home: Option<&std::path::Path>) -> Self {
        Self {
            mine: mine.map(std::path::Path::to_path_buf),
            real_home: real_home.map(std::path::Path::to_path_buf),
        }
    }

    /// Does `resolved` sit under a tree prune may not touch?
    fn refuses(&self, resolved: &std::path::Path) -> bool {
        let ours = self.mine.as_deref().is_some_and(|mine| canonical_or_self(mine) == resolved);
        let under_real_home = self
            .real_home
            .as_deref()
            .is_some_and(|home| resolved.starts_with(canonical_or_self(home)));
        ours || under_real_home
    }
}

/// May prune touch this home at all?
///
/// The refusals are absolute, and the SHAPE test is the load-bearing one. A
/// prefix test ("resolves somewhere under a temp root") approves the temp root
/// itself and every tree beneath it, so a single symlink turns `--yes` into a
/// `remove_dir_all` of the whole temp tree. What prune exists to delete has
/// exactly one shape, so that shape is what is approved:
///
/// ```text
/// <temp root>/<one component>/.agents-in-a-box
/// ```
///
/// The leaf must be named `.agents-in-a-box`, its parent must be a DIRECT child
/// of a temp root (so the approved path is always strictly two levels deeper
/// than the root, never the root itself), and it must be a directory.
///
/// On top of that:
///
/// * The path must RESOLVE at all, because an unresolvable path is one the
///   guard cannot speak about, and the resolved path is the one carried to the
///   removal.
/// * No component of the resolved path may be a symlink, checked with
///   `symlink_metadata` AFTER the resolution. Temp roots are world-writable, so
///   an unprivileged local process can swap a component for a symlink in the
///   window between `canonicalize` and `remove_dir_all`; a resolved path with a
///   symlink component is that swap, and it is refused rather than followed.
/// * It must not be the home this process resolved, and it must not sit under
///   `dirs::home_dir()`, checked INDEPENDENTLY of the temp-root test rather
///   than as its complement: the two are not opposites, because `$TMPDIR` is
///   attacker-influenced and a `TMPDIR` that is an ancestor of the real home
///   would otherwise make the whole home enumerable and deletable. Prune always
///   runs from a real shell with a real `$HOME`, so nothing legitimate lives
///   under both.
///
/// The roots arrive as an argument so each refusal is testable on its own.
pub fn prunable_in(home: &std::path::Path, protected: &ProtectedRoots) -> Option<PrunableHome> {
    let resolved = std::fs::canonicalize(home).ok()?;
    if !resolved.is_dir() {
        return None;
    }
    if resolved.file_name() != Some(std::ffi::OsStr::new(ainb_hangar_core::paths::HANGAR_DIR)) {
        return None;
    }
    let root = resolved.parent()?.parent()?;
    let directly_under_temp = temp_roots()
        .iter()
        .any(|temp| root == temp.as_path() || root == canonical_or_self(temp));
    if !directly_under_temp || protected.refuses(&resolved) || has_symlink_component(&resolved) {
        return None;
    }
    Some(PrunableHome(resolved))
}

/// Is any component of this already-resolved path a symlink right now?
///
/// `canonicalize` answers about the moment it ran. This re-reads every
/// component with `symlink_metadata` (which does NOT follow links), so a
/// component swapped for a symlink after the resolution is caught instead of
/// silently redirecting the removal. A component we cannot stat counts as a
/// symlink: prune fails closed on every unknown.
pub fn has_symlink_component(resolved: &std::path::Path) -> bool {
    resolved.ancestors().any(|component| {
        component.parent().is_some()
            && !std::fs::symlink_metadata(component)
                .is_ok_and(|meta| !meta.file_type().is_symlink())
    })
}

/// What one ownership record (the lock, or the pid file) says.
///
/// Absent and unreadable demand OPPOSITE handling, so they are not one `Option`:
/// an absent record is evidence of nobody, while an unreadable one is evidence
/// of nothing at all and must keep the home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerRecord {
    Absent,
    Unreadable,
    Pid(u32),
}

pub fn read_owner_record(path: &std::path::Path) -> OwnerRecord {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => OwnerRecord::Absent,
        Err(_) => OwnerRecord::Unreadable,
        Ok(text) => match text.trim().parse::<u32>() {
            // Pid 0 is "this process group" to `kill`, never a daemon.
            Ok(pid) if pid > 0 => OwnerRecord::Pid(pid),
            _ => OwnerRecord::Unreadable,
        },
    }
}

/// What the process table says a recorded pid is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidState {
    /// No such process.
    Dead,
    /// Positively a hangar daemon: `ps` answered with a recognised daemon argv.
    Daemon,
    /// Positively something else: `ps` answered, and not with a daemon.
    NotADaemon,
    /// The process table could not answer. NOT proof of anything.
    Unknown,
}

/// Ask the process table what `pid` is, with every unknown answering
/// [`PidState::Unknown`].
///
/// The polarity is the whole point. `holder_is_live_daemon` deliberately fails
/// OPEN (an unreadable `ps` answers "yes, respect the holder") because there it
/// arbitrates a lock, where a false negative puts two daemons on one home. Here
/// the answer decides how a home is DESCRIBED, and a confident "that pid is our
/// daemon" printed about a process nobody proved anything about is a claim the
/// report cannot make. So `ps` is asked directly and a missing answer is a
/// missing proof; `holder_is_live_daemon` is then required on top, so a pid must
/// clear both gates before the report names it a hangar daemon.
///
/// No answer here authorizes anything but a sentence. Prune signals nothing, in
/// either mode: every live pid, confirmed or not, keeps its home and is handed
/// to the operator with the literal command that stops it.
pub fn inspect_pid(pid: u32) -> PidState {
    if !pid_is_running(pid) {
        return PidState::Dead;
    }
    let Ok(signed) = i32::try_from(pid) else {
        return PidState::Unknown;
    };
    pid_state_from_argv(
        ainb_hangar_daemon::single_instance::process_argv(signed).as_deref(),
        || ainb_hangar_daemon::single_instance::holder_is_live_daemon(signed),
    )
}

/// The identity half of [`inspect_pid`], as a pure function so the fail-CLOSED
/// polarity is testable without a wedged `ps`.
///
/// `args` is `None` when the process table could not answer. `confirm` is
/// `holder_is_live_daemon`, which answers YES in exactly that case; it is
/// required IN ADDITION to a positive argv, never instead of one.
pub fn pid_state_from_argv(args: Option<&str>, confirm: impl FnOnce() -> bool) -> PidState {
    let Some(args) = args else {
        return PidState::Unknown;
    };
    if argv_credits_a_daemon(args) && confirm() {
        PidState::Daemon
    } else {
        PidState::NotADaemon
    }
}

/// Read one home's own ownership records and classify its owner.
pub fn prune_verdict(home: &PrunableHome) -> PruneVerdict {
    prune_verdict_from(
        read_owner_record(&ainb_hangar_daemon::single_instance::lock_path_in(&home.0)),
        read_owner_record(&ainb_hangar_daemon::pid_path_in(&home.0)),
        &inspect_pid,
    )
}

/// The decision table, as a pure function over what the two records say, so
/// every row is testable without a process or a filesystem.
///
/// The LOCK is the ownership record: the daemon publishes it as the first
/// statement of `boot` and holds it for its whole life, while `daemon.pid` is a
/// last-write-wins note written at the END of boot. So the lock is the only
/// record that can authorize a signal, and it is also the only record that can
/// prove a live owner during the boot window in which the pid file does not
/// exist yet, which is the window where deciding from the pid file alone
/// deletes a live daemon's home.
///
/// A pid the pid file names alone still FORBIDS removal (it is a claim, and a
/// live one), it simply cannot authorize signalling: that file is home-agnostic
/// evidence, and a stale one left in a leaked temp home can name any pid at all,
/// including the developer's real daemon.
pub fn prune_verdict_from(
    lock: OwnerRecord,
    pidfile: OwnerRecord,
    inspect: &dyn Fn(u32) -> PidState,
) -> PruneVerdict {
    if let OwnerRecord::Pid(pid) = lock {
        match inspect(pid) {
            PidState::Daemon => return PruneVerdict::LiveDaemon(pid),
            PidState::NotADaemon => return PruneVerdict::Stranger(pid),
            PidState::Unknown => return PruneVerdict::Unconfirmed(pid),
            PidState::Dead => {}
        }
    }
    if let OwnerRecord::Pid(pid) = pidfile {
        match inspect(pid) {
            PidState::NotADaemon => return PruneVerdict::Stranger(pid),
            PidState::Daemon | PidState::Unknown => return PruneVerdict::Unconfirmed(pid),
            PidState::Dead => {}
        }
    }
    // Nothing live is named by either record.
    if matches!(lock, OwnerRecord::Unreadable) || matches!(pidfile, OwnerRecord::Unreadable) {
        return PruneVerdict::Unreadable;
    }
    match (lock, pidfile) {
        (OwnerRecord::Pid(pid), _) | (_, OwnerRecord::Pid(pid)) => PruneVerdict::DeadOwner(pid),
        _ => PruneVerdict::NoOwner,
    }
}

/// The homes a live process on this machine is running in, read ONCE per run
/// from the process table.
///
/// `None` means the process table could not answer. That is not "nothing is
/// running": it is no evidence at all, so it fails CLOSED and every candidate
/// reads as [`PruneVerdict::InUse`].
///
/// Read-only, and it is the whole interaction prune has with the process table
/// here: the paths are compared, never the pids, and nothing is ever signalled.
pub struct LiveHomes(Option<std::collections::BTreeSet<std::path::PathBuf>>);

impl LiveHomes {
    /// Ask `ps` for every process's environment and collect the two variables
    /// that name a hangar home.
    ///
    /// `ps axeww` is the one spelling both `ps` implementations in play accept
    /// (macOS BSD `ps` and Linux procps): `e` appends the environment, `ww`
    /// stops it being truncated to the terminal width. Follows
    /// `single_instance::process_argv`'s style: one short-lived `ps`, its own
    /// child killed by handle if it wedges, and every failure answering "could
    /// not read" rather than "nothing found".
    fn from_process_table() -> Self {
        Self(read_process_table_homes())
    }

    /// Is this resolved home, or the directory that would be `$HOME` for it,
    /// claimed by a live process?
    ///
    /// `true` when the process table could not answer, which is what makes the
    /// gate fail closed.
    fn claims(&self, resolved_home: &std::path::Path) -> bool {
        let Some(homes) = &self.0 else {
            return true;
        };
        [Some(resolved_home), resolved_home.parent()]
            .into_iter()
            .flatten()
            .any(|candidate| homes.contains(candidate))
    }

    /// A process table that answered, naming exactly these homes.
    #[cfg(test)]
    fn answered(paths: &[&std::path::Path]) -> Self {
        Self(Some(
            paths
                .iter()
                .flat_map(|path| [path.to_path_buf(), canonical_or_self(path)])
                .collect(),
        ))
    }

    /// A process table that could not answer.
    #[cfg(test)]
    const fn unreadable() -> Self {
        Self(None)
    }
}

/// Everything outside one home's own records that decides whether prune may
/// remove it, resolved ONCE per run.
///
/// Passed in rather than read inside the decision so every gate is testable
/// against a fabricated home: with the real values a fixture is always saved by
/// some earlier refusal, which is how a broken later gate goes unnoticed.
pub struct PruneContext {
    protected: ProtectedRoots,
    live: LiveHomes,
    /// A home touched more recently than this is presumed to belong to a run
    /// that is still going.
    min_age: std::time::Duration,
    now: std::time::SystemTime,
}

impl PruneContext {
    /// The gates as this process actually sees them.
    fn current(min_age: std::time::Duration) -> Self {
        Self {
            protected: ProtectedRoots::current(),
            live: LiveHomes::from_process_table(),
            min_age,
            now: std::time::SystemTime::now(),
        }
    }

    /// Explicit gates, so a test can isolate one refusal at a time.
    #[cfg(test)]
    fn of(protected: ProtectedRoots, live: LiveHomes, min_age: std::time::Duration) -> Self {
        Self {
            protected,
            live,
            min_age,
            now: std::time::SystemTime::now(),
        }
    }
}

/// Classify one home and, when `apply`, carry the verdict out.
///
/// The order is the guarantee. `prunable_in` speaks first, so the developer's
/// own home is `Protected` whatever else is true of it. The live-process check
/// comes next, because it is the only gate that can see a home in ACTIVE use:
/// since #784 no daemon is spawned into an ephemeral home, so a running session
/// leaves no lock and no pid file, and the record-based verdicts below would
/// read it as `NoOwner` litter and delete it out from under the session.
///
/// The age gate is the backstop for a live run `ps` did not attribute, and it
/// speaks LAST of the three, after the home's own records. Both answers keep
/// the home, so the ordering cannot widen what `--yes` may remove; it decides
/// which SENTENCE the operator gets, and only the record-based one carries the
/// pid and the literal command that stops it. Since [`touched_within`] started
/// reading the files inside `hangar/`, a daemon rewriting its heartbeat keeps
/// its own home permanently fresh, so answering `Recent` first would withhold
/// the remedy [`describe_verdict`] promises from the one case that needs it.
///
/// `apply` chooses between reporting a removal and performing it. It cannot
/// widen WHAT is removable: a home with a live owner takes the same
/// [`PruneAction::LeftAlone`] under `--yes` as it does in a dry run, and
/// nothing here signals that owner.
pub fn prune_home_in(
    home: &std::path::Path,
    ctx: &PruneContext,
    apply: bool,
) -> (PruneVerdict, PruneAction) {
    let Some(home) = prunable_in(home, &ctx.protected) else {
        return (PruneVerdict::Protected, PruneAction::LeftAlone);
    };
    let verdict = if ctx.live.claims(&home.0) {
        PruneVerdict::InUse
    } else {
        let recorded = prune_verdict(&home);
        if recorded.live_owner().is_some() {
            recorded
        } else if touched_within(&home.0, ctx.now, ctx.min_age) {
            PruneVerdict::Recent
        } else {
            recorded
        }
    };
    let action = match (verdict.is_removable(), apply) {
        (false, _) => PruneAction::LeftAlone,
        (true, false) => PruneAction::Reported,
        (true, true) => remove_home(&home),
    };
    (verdict, action)
}

/// Was this home touched less than `min_age` ago?
///
/// The NEWEST mtime across three places, because they move for different
/// reasons:
///
/// * the home dir, whose mtime changes when the run creates or drops a
///   top-level entry;
/// * `<home>/hangar`, whose mtime changes when a socket, lock or log file
///   appears or goes;
/// * the files directly INSIDE `<home>/hangar` (`daemon.pid`, `daemon.lock`,
///   `daemon.heartbeat`, `daemon.binary`, and whatever else is there), which is
///   where a live run's continuous writing actually lands.
///
/// The third is not redundant with the second. POSIX moves a directory's mtime
/// when an entry is created, renamed or removed, never when a file already
/// inside it is written, so a daemon rewriting its heartbeat every few seconds
/// leaves both directories looking untouched. Reading only the two directories
/// aged out a home that was being written to continuously, and `--yes` deleted
/// it out from under the run.
///
/// Fails closed on every unknown: an mtime we cannot read, a `hangar/` we
/// cannot list, or a clock that reports a file in the future all keep the home.
/// `min_age` of zero disables the gate outright, which is what `--older-than 0`
/// asks for.
pub fn touched_within(
    home: &std::path::Path,
    now: std::time::SystemTime,
    min_age: std::time::Duration,
) -> bool {
    if min_age.is_zero() {
        return false;
    }
    let hangar = home.join("hangar");
    let mut candidates = vec![home.to_path_buf(), hangar.clone()];
    match std::fs::read_dir(&hangar) {
        Ok(entries) => {
            for entry in entries {
                // An entry we cannot even name is an unknown, and every unknown
                // keeps the home.
                let Ok(entry) = entry else { return true };
                candidates.push(entry.path());
            }
        }
        // A `hangar/` that is there but cannot be listed hides exactly the
        // files this gate exists to read.
        Err(_) if hangar.exists() => return true,
        Err(_) => {}
    }
    candidates
        .iter()
        .filter(|path| path.exists())
        .any(|path| !modified_at_least_ago(path, now, min_age))
}

/// Is `path`'s mtime provably at least `min_age` old?
///
/// `false` for every unreadable mtime and every mtime in the future, which is
/// what makes [`touched_within`] fail closed.
pub fn modified_at_least_ago(
    path: &std::path::Path,
    now: std::time::SystemTime,
    min_age: std::time::Duration,
) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= min_age)
}

pub fn remove_home(home: &PrunableHome) -> PruneAction {
    match std::fs::remove_dir_all(&home.0) {
        Ok(()) => PruneAction::Removed,
        Err(e) => PruneAction::RemoveFailed(e.to_string()),
    }
}

/// Every hangar home sitting under a temp root: `<temp root>/*/.agents-in-a-box`.
///
/// One level deep, which is the exact shape a `HOME=$(mktemp -d)` harness
/// leaves. Deduped by resolved path (`$TMPDIR` and `/tmp` are the same
/// directory wherever `TMPDIR` is unset) and sorted, so a dry run and the
/// `--yes` run that follows it list the same homes in the same order.
pub fn leaked_hangar_homes() -> Vec<std::path::PathBuf> {
    let mut seen = std::collections::BTreeSet::new();
    let mut homes = Vec::new();
    // Resolved once: the enumerator asks the same guard question of every
    // entry, and re-reading the roots per entry would let them drift mid-walk.
    let protected = ProtectedRoots::current();
    for root in temp_roots() {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let home = entry.path().join(ainb_hangar_core::paths::HANGAR_DIR);
            if !home.is_dir() || prunable_in(&home, &protected).is_none() {
                continue;
            }
            if seen.insert(canonical_or_self(&home)) {
                homes.push(home);
            }
        }
    }
    homes.sort();
    homes
}

/// The sentence for one verdict.
///
/// Every live owner ends in the LITERAL command that stops it, because prune
/// will not: a description of the remedy ("stop it yourself and re-run") leaves
/// the operator to work out the pid and the verb, and the pid is the one piece
/// of the situation only this report knows.
pub fn describe_verdict(verdict: &PruneVerdict) -> String {
    match verdict {
        PruneVerdict::Protected => "protected; left alone".to_string(),
        PruneVerdict::InUse => "in use by a live process; left alone".to_string(),
        PruneVerdict::Recent => "touched too recently to presume abandoned; left alone".to_string(),
        PruneVerdict::NoOwner => "stale home, no owner".to_string(),
        PruneVerdict::DeadOwner(pid) => format!("stale home, dead pid {pid}"),
        PruneVerdict::LiveDaemon(pid) => {
            format!("live owner: pid {pid} (a hangar daemon){}", remedy(*pid))
        }
        // No remedy for a stranger: prune has positively classified this pid as
        // NOT a hangar daemon, so a paste-ready `kill` would be this tool
        // pointing the operator at someone else's process.
        PruneVerdict::Stranger(pid) => {
            format!("live owner: pid {pid} (not a hangar daemon); left alone")
        }
        PruneVerdict::Unconfirmed(pid) => {
            format!("live owner: pid {pid} (unconfirmed){}", remedy(*pid))
        }
        PruneVerdict::Unreadable => "unreadable record; left alone".to_string(),
    }
}

/// The remedy clause for a home with a live owner.
///
/// Paste-ready on purpose. Prune has no way to tell this home's leaked daemon
/// from a pid someone wrote into a world-writable file, so the human who can is
/// handed the exact command rather than an instruction to compose one.
pub fn remedy(pid: u32) -> String {
    format!(" (kill {pid} to stop it, then re-run prune); left alone")
}

/// The suffix describing what `--yes` did, or `None` when nothing happened.
pub fn describe_action(action: &PruneAction) -> Option<String> {
    match action {
        PruneAction::Reported | PruneAction::LeftAlone => None,
        PruneAction::Removed => Some("removed".to_string()),
        PruneAction::RemoveFailed(e) => Some(format!("could not remove: {e}")),
    }
}

/// What a whole prune run did, counted so the summary can never imply a signal.
///
/// Removals and left-alone homes are counted SEPARATELY rather than summed into
/// one "acted on N" figure: that figure read as "N daemons dealt with", which is
/// exactly the claim this verb must not make.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneTally {
    /// Homes deleted (`--yes`), or that `--yes` would delete (dry run).
    removed: usize,
    /// Homes kept, for any reason.
    left_alone: usize,
    /// The subset of `left_alone` whose records name a live process. Reported
    /// on its own line so "left alone" never reads as "failed".
    live_owner: usize,
    /// The subset of `left_alone` a live process is running in. Counted apart
    /// from `live_owner` because the evidence is different: that one is a pid
    /// written inside the home, this one is the process table naming the home.
    in_use: usize,
    /// The subset of `left_alone` skipped only for being too recent.
    recent: usize,
}

impl PruneTally {
    const fn record(&mut self, verdict: &PruneVerdict, action: &PruneAction) {
        match action {
            PruneAction::Removed | PruneAction::Reported => self.removed += 1,
            PruneAction::LeftAlone | PruneAction::RemoveFailed(_) => {
                self.left_alone += 1;
                if verdict.live_owner().is_some() {
                    self.live_owner += 1;
                }
                match verdict {
                    PruneVerdict::InUse => self.in_use += 1,
                    PruneVerdict::Recent => self.recent += 1,
                    _ => {}
                }
            }
        }
    }
}

/// The closing summary line, as a pure function so its wording is pinned.
///
/// `applied` only changes the tense. It never changes which bucket a home fell
/// into, because `--yes` does not widen what prune may touch.
pub fn describe_summary(tally: &PruneTally, applied: bool) -> String {
    let verb = if applied {
        "home(s) removed"
    } else {
        "home(s) would be removed"
    };
    let mut reasons = Vec::new();
    if tally.live_owner > 0 {
        reasons.push(format!("{} with a live owner", tally.live_owner));
    }
    if tally.in_use > 0 {
        reasons.push(format!("{} in use by a live process", tally.in_use));
    }
    if tally.recent > 0 {
        reasons.push(format!("{} too recent", tally.recent));
    }
    let live = if reasons.is_empty() {
        String::new()
    } else {
        format!(" ({})", reasons.join(", "))
    };
    let nudge = if !applied && tally.removed > 0 {
        "; re-run with --yes to remove them"
    } else {
        ""
    };
    format!(
        "{} {verb}, {} left alone{live}{nudge}",
        tally.removed, tally.left_alone
    )
}

/// Everything one prune run decided, with nothing printed yet.
///
/// The verb's whole guarantee is the `--yes` -> `apply` threading: with `apply`
/// false nothing on disk may be touched. That is a property of the loop, not of
/// any one home, so the loop is a value-returning function and the CLI wrapper
/// below is left with nothing but `println!`.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PruneReport {
    /// One line per home, in the order they were considered.
    lines: Vec<String>,
    tally: PruneTally,
    /// The closing line, without the `hangar daemon prune: ` prefix.
    summary: String,
}

/// Classify (and, when `apply`, remove) every home in `homes`.
///
/// Pure apart from the removals it is asked for: the gates arrive in `ctx` and
/// the report comes back as data, so a test can drive a real dry run and a real
/// `--yes` run over the same fabricated homes and compare what survived.
pub fn prune_all(homes: &[std::path::PathBuf], ctx: &PruneContext, apply: bool) -> PruneReport {
    if homes.is_empty() {
        return PruneReport {
            lines: Vec::new(),
            tally: PruneTally::default(),
            summary: "no hangar homes under the system temp dir".to_string(),
        };
    }
    let mut report = PruneReport::default();
    for home in homes {
        let (verdict, action) = prune_home_in(home, ctx, apply);
        report.tally.record(&verdict, &action);
        let suffix = describe_action(&action).map_or_else(String::new, |a| format!(" -> {a}"));
        report.lines.push(format!(
            "{} - {}{}",
            home.display(),
            describe_verdict(&verdict),
            suffix
        ));
    }
    report.summary = describe_summary(&report.tally, apply);
    report
}

/// `hangar daemon prune`: report every hangar home left under the system temp
/// dir, and with `--yes` delete the ones that are stale, unowned and idle.
pub fn run_daemon_prune(args: &PruneArgs) -> Result<()> {
    let report = prune_all(
        &leaked_hangar_homes(),
        &PruneContext::current(prune_min_age(args)),
        args.yes,
    );
    for line in &report.lines {
        println!("{line}");
    }
    println!("hangar daemon prune: {}", report.summary);
    Ok(())
}

/// The age threshold `--older-than DAYS` asks for.
///
/// Named rather than inlined so the argv -> threshold path is assertable: this
/// and `args.yes` are the whole of what the verb reads, and an `--older-than`
/// that never reached the gate would leave `--yes` deleting today's runs with a
/// green suite.
///
/// Saturating, so an absurd day count clamps to "keep everything" instead of
/// wrapping into a threshold that removes it.
pub const fn prune_min_age(args: &PruneArgs) -> std::time::Duration {
    std::time::Duration::from_secs(args.older_than.saturating_mul(60 * 60 * 24))
}

pub fn restart_daemon(announce: bool, launcher: LauncherLifetime) -> Result<()> {
    if let Err(e) = stop_daemon(announce) {
        if announce {
            println!("hangar daemon: stop reported a problem, starting anyway: {e}");
        } else {
            tracing::warn!(error = %e, "hangar daemon stop reported a problem during upgrade handoff");
        }
    }
    // A stop that could not confirm the exit leaves a daemon mid-shutdown, still
    // holding its lock and its socket. Starting into that reads as "already
    // running" and returns, and when the old daemon finishes dying moments
    // later the home is left with NOTHING. Wait for the owner to actually go.
    let vacated = wait_until(RESTART_VACATE_BUDGET, || {
        matches!(running_daemon_pid(), Ok(None))
    });
    if !vacated {
        if let Ok(Some(pid)) = running_daemon_pid() {
            if announce {
                println!(
                    "hangar daemon: pid {pid} still owns this home after {}s; not starting a \
                     second one (re-run stop, or `kill -9 {pid}`)",
                    RESTART_VACATE_BUDGET.as_secs()
                );
            } else {
                tracing::warn!(pid, "hangar daemon did not vacate during upgrade handoff");
            }
            return Ok(());
        }
    }
    start_daemon_if_stopped(announce, launcher)
}

/// Spawn the daemon once more and report the pid that ends up owning the home.
///
/// Used only on the "our child declined and the incumbent then vanished" path:
/// the home was contested a moment ago and is free now, which is a race to
/// re-run, not a failure to report.
pub fn respawn_once(
    bin: &std::path::Path,
    args: &[&str],
    launcher: LauncherLifetime,
) -> Result<Option<u32>> {
    // Same rule as `start_daemon_if_stopped`: never detach a test harness.
    if crate::self_exec_guard::running_under_cargo_test() {
        anyhow::bail!(
            "refusing to respawn the hangar daemon from a cargo test binary \
             (current_exe is a test harness, not `ainb`)"
        );
    }
    let mut command = std::process::Command::new(bin);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    if let Ok(home) = ainb_hangar_daemon::hangar_dir() {
        arm_watchdog_for_ephemeral_home(&mut command, &home, launcher);
    }
    let child = command.spawn().context("respawn daemon")?;
    std::thread::sleep(std::time::Duration::from_millis(400));
    // `child` is intentionally dropped without `wait`, the daemon outlives us.
    let _ = child;
    running_daemon_pid()
}

/// How long `restart` waits for the outgoing daemon to release the home.
pub const RESTART_VACATE_BUDGET: std::time::Duration = std::time::Duration::from_secs(15);

/// Poll `cond` until true or `budget` elapses.
pub fn wait_until(budget: std::time::Duration, cond: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if cond() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}
/// Does `pid` hold a unix socket bound to `socket`, under the current uid?
///
/// `lsof -a -p <pid> -u <uid> -U -F n` lists the bound NAME of every unix socket
/// the process holds; `-a` ANDs the pid and uid filters, so another user's process
/// answers empty. (The same proof lives in `ainb-hangar-daemon`'s codex reaper and
/// in `ainb-plugin-notifyd`; neither crate can be depended on from here without
/// inverting the dependency graph.)
pub fn pid_holds_socket(pid: u32, socket: &std::path::Path) -> bool {
    let uid = nix::unistd::Uid::current().as_raw().to_string();
    let Ok(out) = std::process::Command::new("lsof")
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

/// Compare a bound socket name from `lsof` to the path we expect. Exact match
/// first; the fallback re-resolves both parent directories so a home reached
/// through a symlink (`/tmp` -> `/private/tmp` on macOS) still matches.
pub fn socket_names_match(name: &str, expected: &std::path::Path) -> bool {
    let name = std::path::Path::new(name);
    if name == expected {
        return true;
    }
    let real_dir = |p: &std::path::Path| p.parent().and_then(|dir| std::fs::canonicalize(dir).ok());
    match (
        real_dir(name),
        real_dir(expected),
        name.file_name(),
        expected.file_name(),
    ) {
        (Some(a), Some(b), Some(x), Some(y)) => a == b && x == y,
        _ => false,
    }
}

/// The environment variables that name a process's hangar home.
///
/// `$HOME` names the PARENT of the home (`hangar_home` appends
/// `.agents-in-a-box` to it), `$AINB_HANGAR_HOME` names the home itself. Both
/// readings are compared, so either variable identifies the same directory.
pub const LIVE_HOME_ENV_KEYS: [&str; 2] = ["HOME", ainb_hangar_core::paths::HANGAR_HOME_ENV];

/// Run `ps` and collect the home paths its environment dump names, or `None`
/// when `ps` could not be run, wedged, exited non-zero, or answered without
/// showing a single environment.
///
/// The pipe is drained on its OWN thread while the timeout is enforced here,
/// which `single_instance::process_ps_field`'s poll-then-read shape cannot do.
/// That shape is correct for the one short line it asks for; a whole machine's
/// environment is far larger than a pipe buffer, so `ps` blocks writing, never
/// exits, and a poll-first reader times out on every healthy run. Silently:
/// this gate fails CLOSED, so the symptom was prune reporting every home as in
/// use and removing nothing, forever.
pub fn read_process_table_homes() -> Option<std::collections::BTreeSet<std::path::PathBuf>> {
    let mut child = std::process::Command::new("ps")
        .arg("axeww")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let read = std::io::Read::read_to_string(&mut stdout, &mut text);
        // A closed receiver means we already gave up; dropping the text is the
        // whole cleanup.
        let _ = tx.send(read.ok().map(|_| text));
    });
    let Ok(Some(text)) = rx.recv_timeout(PRUNE_PS_TIMEOUT) else {
        // Reap our own child by its exact handle, which also unblocks the
        // reader thread. No other pid is ever signalled by this verb.
        let _ = child.kill();
        let _ = child.wait();
        return None;
    };
    if !child.wait().is_ok_and(|status| status.success()) {
        return None;
    }
    process_table_homes(&text)
}

/// The homes a `ps` dump names, or `None` when the dump is not an ANSWER.
///
/// Exiting 0 is not the same as answering. A `ps` that runs, succeeds and
/// prints no environment at all is `ps` declining the `e` flag (a hardened
/// kernel, a container with no `/proc`, a busybox `ps` that ignores it, output
/// something else truncated), not a machine on which nothing is running. Every
/// machine that can reach this code has at least one process with a `HOME`, so
/// a dump carrying no `HOME=` assignment anywhere is treated as "could not
/// answer" and fails CLOSED, exactly as an unrunnable `ps` does. Read as a
/// complete answer it named no homes, so every home on the machine reported as
/// free and `--yes` deleted the lot.
pub fn process_table_homes(text: &str) -> Option<std::collections::BTreeSet<std::path::PathBuf>> {
    if !text.contains("HOME=") {
        return None;
    }
    Some(home_paths_in_process_table(text))
}

/// How long prune waits for its `ps`, mirroring `single_instance`'s budget.
pub const PRUNE_PS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Every `HOME=` / `AINB_HANGAR_HOME=` value in a `ps axeww` dump, as a pure
/// function so the parse is testable without a process table.
///
/// Each value is stored raw AND resolved: `ps` reports `HOME=/tmp/x` while the
/// candidate resolves to `/private/tmp/x` on macOS, and a comparison that saw
/// only one of the two would miss the match and delete a live home.
pub fn home_paths_in_process_table(text: &str) -> std::collections::BTreeSet<std::path::PathBuf> {
    let mut homes = std::collections::BTreeSet::new();
    for line in text.lines() {
        for (key, value) in assignments_in_line(line) {
            if !LIVE_HOME_ENV_KEYS.contains(&key) || value.is_empty() {
                continue;
            }
            let raw = std::path::PathBuf::from(value);
            homes.insert(canonical_or_self(&raw));
            homes.insert(raw);
        }
    }
    homes
}

/// Split one `ps` line into its `KEY=VALUE` assignments, each value running to
/// the next assignment rather than to the next space.
///
/// `ps` prints the environment space-separated and unquoted, so tokenizing on
/// whitespace recorded `/tmp/my` for a `HOME=/tmp/my home`: the truncated path
/// matched no candidate, the live run read as abandoned, and `--yes` deleted
/// its home. The next ` KEY=` boundary is the only structure the dump has left,
/// so that is where a value is cut.
///
/// What no parse at this layer can resolve, because `ps` emits neither quoting
/// nor lengths to tell it from a real second assignment: a value whose own text
/// contains a space followed by something env-key shaped (`HOME=/tmp/a b=c`)
/// still ends at the wrong place, and a value containing a newline is lost with
/// the line split.
pub fn assignments_in_line(line: &str) -> Vec<(&str, &str)> {
    // (start of the key, byte index of its `=`), in the order they appear.
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut key_start = Some(0_usize);
    for (i, byte) in line.bytes().enumerate() {
        match byte {
            // Only a space can begin a new assignment; everything after an `=`
            // belongs to the value until one arrives.
            b' ' => key_start = Some(i + 1),
            b'=' => {
                if let Some(start) = key_start.take() {
                    if i > start && !line.as_bytes()[start].is_ascii_digit() {
                        spans.push((start, i));
                    }
                }
            }
            b if is_env_key_byte(b) => {}
            _ => key_start = None,
        }
    }
    spans
        .iter()
        .enumerate()
        .map(|(idx, &(start, eq))| {
            // Up to the space before the next assignment, or the end of the
            // line for the last one.
            let end = spans
                .get(idx + 1)
                .map_or(line.len(), |(next, _)| next.saturating_sub(1))
                .max(eq + 1);
            (&line[start..eq], &line[eq + 1..end])
        })
        .collect()
}

/// Bytes an environment variable name may be made of. `=` and space are the
/// two that end one, and anything else rules the token out as a key.
pub const fn is_env_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod ephemeral_home_watchdog_tests {
    //! Issue #784: a daemon autostarted under `HOME=$(mktemp -d)` outlives
    //! every guard in the system, because all of them are home-scoped and the
    //! home is deleted with its creator. The spawner binds such a daemon to its
    //! own life, and only when it is a launcher that outlives it.
    //!
    //! Nothing here spawns a process: the decision is a pure function, and the
    //! wiring assertion is on the `Command` the spawner would have run.

    use super::{LauncherLifetime, arm_watchdog_for_ephemeral_home, should_bind_daemon_to_parent};

    /// A home of the shape a `HOME=$(mktemp -d)` harness produces under `root`.
    fn ephemeral_home(root: &str) -> std::path::PathBuf {
        std::path::Path::new(root).join("bj.Q9x7fk").join(".agents-in-a-box")
    }

    /// The value the spawned daemon would see for `key`, but ONLY from an
    /// explicit mutation. `Command::get_envs` does not report the inherited
    /// environment, so `None` here means "we did not set it", not "the child
    /// will not see it".
    fn explicitly_set_env(command: &std::process::Command, key: &str) -> Option<String> {
        command.get_envs().find_map(|(name, value)| {
            (name == key).then(|| {
                value
                    .expect("watchdog env is set, never removed")
                    .to_string_lossy()
                    .into_owned()
            })
        })
    }

    #[test]
    fn binds_a_persistent_launcher_to_an_ephemeral_home() {
        for root in ["/tmp", "/var/tmp"] {
            assert!(
                should_bind_daemon_to_parent(
                    &ephemeral_home(root),
                    LauncherLifetime::Persistent,
                    false
                ),
                "a home under {root} is ephemeral"
            );
        }
        // `$TMPDIR` too, which on macOS is neither of the above.
        assert!(should_bind_daemon_to_parent(
            &std::env::temp_dir().join("bj.Q9x7fk").join(".agents-in-a-box"),
            LauncherLifetime::Persistent,
            false
        ));
    }

    #[test]
    fn never_binds_a_daemon_a_one_shot_verb_must_leave_behind() {
        // `ainb hangar daemon start` and `ainb doctor` exit ~1s after the
        // spawn. Binding there stands the daemon down immediately, while the
        // verb reports that it started one.
        assert!(!should_bind_daemon_to_parent(
            &ephemeral_home("/tmp"),
            LauncherLifetime::Ephemeral,
            false
        ));
    }

    #[test]
    fn never_overrides_a_parent_a_harness_already_declared() {
        // A harness that set the env var itself named a longer-lived owner on
        // purpose; replacing it with our pid kills the daemon it is driving.
        assert!(!should_bind_daemon_to_parent(
            &ephemeral_home("/tmp"),
            LauncherLifetime::Persistent,
            true
        ));
    }

    #[test]
    fn leaves_a_real_home_alone() {
        // The Homebrew daemon must keep outliving the invocation that started it.
        assert!(!should_bind_daemon_to_parent(
            std::path::Path::new("/Users/example/.agents-in-a-box"),
            LauncherLifetime::Persistent,
            false
        ));
    }

    #[test]
    fn leaves_the_running_users_real_home_alone() {
        // Same claim, against the home this machine actually resolves, so the
        // test fails if the temp roots ever start prefixing it.
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let home = home.join(".agents-in-a-box");
        if super::is_under_temp_dir(&home) {
            // This very suite is running under an ephemeral `$HOME`, which is
            // the case the other tests cover. Nothing to assert here.
            return;
        }
        assert!(!should_bind_daemon_to_parent(
            &home,
            LauncherLifetime::Persistent,
            false
        ));
    }

    #[test]
    fn arms_both_env_names_when_it_binds() {
        // The installed daemon we launch can predate the rename; it would
        // ignore the new name and become immortal again.
        let mut command = std::process::Command::new("/bin/true");
        arm_watchdog_for_ephemeral_home(
            &mut command,
            &ephemeral_home("/tmp"),
            LauncherLifetime::Persistent,
        );
        let ours = std::process::id().to_string();
        // Skipped when the suite itself runs with a parent already declared:
        // then the no-override rule wins, which its own test covers.
        let declared = [
            ainb_hangar_daemon::PARENT_PID_ENV,
            ainb_hangar_daemon::LEGACY_PARENT_PID_ENV,
        ]
        .iter()
        .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
        if declared {
            return;
        }
        assert_eq!(
            explicitly_set_env(&command, ainb_hangar_daemon::PARENT_PID_ENV).as_deref(),
            Some(ours.as_str())
        );
        assert_eq!(
            explicitly_set_env(&command, ainb_hangar_daemon::LEGACY_PARENT_PID_ENV).as_deref(),
            Some(ours.as_str())
        );
    }

    #[test]
    fn sets_nothing_when_it_does_not_bind() {
        let mut command = std::process::Command::new("/bin/true");
        arm_watchdog_for_ephemeral_home(
            &mut command,
            &ephemeral_home("/tmp"),
            LauncherLifetime::Ephemeral,
        );
        assert_eq!(command.get_envs().count(), 0);
    }
}

#[cfg(test)]
mod autostart_gate_tests {
    //! The autostart must not spawn a daemon into a home that dies with the
    //! run. PR #786 bound such a daemon to its parent; this is the step before
    //! it, where the process is never spawned at all.
    //!
    //! Every case drives the pure predicate with an explicit flag. Reading (let
    //! alone setting) `$AINB_HANGAR_HOME` here would be process-global and this
    //! suite is multi-threaded, so a sibling test could flip the answer
    //! underneath these.

    use super::{
        DaemonAutostart, autostart_allowed, ensure_hangar_daemon_with, hangar_home_was_requested,
        home_was_requested,
    };

    /// A home of the shape a `HOME=$(mktemp -d)` harness produces under `root`.
    fn ephemeral_home(root: &std::path::Path) -> std::path::PathBuf {
        root.join("bj.Q9x7fk").join(".agents-in-a-box")
    }

    /// Every temp root [`super::is_under_temp_dir`] recognises, so the gate is
    /// proven against `$TMPDIR` (macOS `/var/folders/...`) as well as the fixed
    /// roots the recording harnesses use.
    fn temp_roots() -> Vec<std::path::PathBuf> {
        vec![
            std::env::temp_dir(),
            std::path::PathBuf::from("/tmp"),
            std::path::PathBuf::from("/var/tmp"),
        ]
    }

    #[test]
    fn skips_an_ephemeral_home_nobody_asked_for() {
        for root in temp_roots() {
            let home = ephemeral_home(&root);
            assert!(
                !autostart_allowed(&home, false),
                "a home under {} is deleted with its creator; spawning a daemon \
                 into it is pure waste",
                root.display()
            );
        }
    }

    #[test]
    fn autostarts_an_ephemeral_home_that_was_asked_for() {
        // Every hangar tripwire sets `$AINB_HANGAR_HOME` to a temp dir and then
        // expects a daemon on it. Gating that would take the whole suite red.
        for root in temp_roots() {
            let home = ephemeral_home(&root);
            assert!(
                autostart_allowed(&home, true),
                "an explicit AINB_HANGAR_HOME under {} is a deliberate choice",
                root.display()
            );
        }
    }

    #[test]
    fn autostarts_a_real_home() {
        // The Homebrew daemon's home, with and without the env var set.
        let home = std::path::Path::new("/Users/example/.agents-in-a-box");
        assert!(autostart_allowed(home, false));
        assert!(autostart_allowed(home, true));
    }

    /// The headline behaviour, at the seam where the gate actually meets the
    /// spawn: no process is launched for an ephemeral home nobody asked for.
    ///
    /// Asserting on [`autostart_allowed`] alone leaves the wiring untested, and
    /// a gate that is computed and then ignored passes such a suite unchanged.
    #[test]
    fn never_spawns_into_an_ephemeral_home_nobody_asked_for() {
        for root in temp_roots() {
            let spawned = std::cell::Cell::new(false);
            let outcome = ensure_hangar_daemon_with(Some(&ephemeral_home(&root)), false, || {
                spawned.set(true);
                DaemonAutostart::Started
            });
            assert_eq!(outcome, DaemonAutostart::SkippedEphemeralHome);
            assert!(
                !spawned.get(),
                "a home under {} is deleted with its creator; nothing may be \
                 spawned into it",
                root.display()
            );
        }
    }

    /// The other half of the same wiring: the gate declines exactly one shape
    /// and lets every other one through to the spawn untouched.
    #[test]
    fn spawns_for_a_real_home_and_for_a_requested_ephemeral_one() {
        let real = std::path::Path::new("/Users/example/.agents-in-a-box");
        let cases: Vec<(std::path::PathBuf, bool)> = std::iter::once((real.to_path_buf(), false))
            .chain(temp_roots().into_iter().map(|r| (ephemeral_home(&r), true)))
            .collect();
        for (home, requested) in cases {
            let spawned = std::cell::Cell::new(false);
            let outcome = ensure_hangar_daemon_with(Some(&home), requested, || {
                spawned.set(true);
                DaemonAutostart::Started
            });
            assert_eq!(outcome, DaemonAutostart::Started, "home {}", home.display());
            assert!(spawned.get(), "home {}", home.display());
        }
    }

    /// An unresolvable hangar home is not a reason to decline: there is no path
    /// to judge, so the spawn RUNS and reports on its own terms.
    ///
    /// The assertion that matters is `spawned`. Comparing the returned outcome
    /// to what the stub returned would pin nothing at all - the function hands
    /// the stub's value straight back, so that comparison holds even if the
    /// spawn is never reached. What can actually break here is the gate
    /// inventing a decision about a home it never saw, so the outcome is
    /// asserted only against the one value the gate can produce by itself.
    #[test]
    fn spawns_when_the_home_cannot_be_resolved() {
        for requested in [false, true] {
            let spawned = std::cell::Cell::new(false);
            let outcome = ensure_hangar_daemon_with(None, requested, || {
                spawned.set(true);
                DaemonAutostart::Failed
            });
            assert!(
                spawned.get(),
                "no path to judge is not grounds to decline (requested={requested})"
            );
            assert_ne!(
                outcome,
                DaemonAutostart::SkippedEphemeralHome,
                "the gate reported an ephemeral home it never saw a path for"
            );
        }
    }

    /// The one impure input the whole gate turns on: set-and-non-empty means
    /// the caller chose this home, and nothing else does.
    ///
    /// Crediting an empty value here would make every `$HOME`-derived ephemeral
    /// home look deliberate, re-enabling the exact spawn the gate exists to
    /// prevent, and every other test in this module would still pass.
    #[test]
    fn only_a_set_non_empty_hangar_home_counts_as_requested() {
        use std::ffi::OsStr;
        assert!(
            !home_was_requested(None),
            "unset means the home came from $HOME"
        );
        assert!(
            !home_was_requested(Some(OsStr::new(""))),
            "`AINB_HANGAR_HOME=` is ignored by hangar_home, so it is not a choice"
        );
        for value in ["/tmp/x/.agents-in-a-box", "~/.agents-in-a-box", " "] {
            assert!(
                home_was_requested(Some(OsStr::new(value))),
                "value {value:?}"
            );
        }
    }

    /// The reader is wired to the right variable, asserted against a FIXED
    /// expectation.
    ///
    /// This suite runs under a harness that always sets `$AINB_HANGAR_HOME` to a
    /// real path (`is_some_and(!is_empty)`), so the reader's answer here is
    /// known ahead of time: `true`. Re-deriving the expectation by calling
    /// `home_was_requested` on the same variable the reader reads would compare
    /// the function under test with itself, and pass for a reader wired to the
    /// wrong variable, or to no variable at all.
    ///
    /// `set_var` is process-global and this suite is multi-threaded, so the
    /// variable is read, never written.
    #[test]
    fn the_env_reader_reads_the_hangar_home_variable() {
        let raw = std::env::var_os(ainb_hangar_core::paths::HANGAR_HOME_ENV);
        assert_eq!(
            ainb_hangar_core::paths::HANGAR_HOME_ENV,
            "AINB_HANGAR_HOME",
            "the reader is pointed at the variable hangar_home honours"
        );
        let Some(value) = raw else {
            // No harness value to judge: the rule itself is pinned above, and
            // there is nothing here that could distinguish a correct reader.
            assert!(
                !hangar_home_was_requested(),
                "an unset variable is not a choice"
            );
            return;
        };
        assert!(
            !value.is_empty(),
            "this suite is expected to run with a real AINB_HANGAR_HOME"
        );
        assert!(
            hangar_home_was_requested(),
            "a set, non-empty AINB_HANGAR_HOME is a deliberate choice"
        );
    }

    #[test]
    fn autostarts_the_running_users_real_home() {
        // Same claim against the home this machine actually resolves, so the
        // test fails if the temp roots ever start prefixing it.
        let Some(home) = dirs::home_dir() else {
            return;
        };
        let home = home.join(".agents-in-a-box");
        if super::is_under_temp_dir(&home) {
            // This suite is itself running under an ephemeral `$HOME`, which is
            // the case the other tests cover. Nothing to assert here.
            return;
        }
        assert!(autostart_allowed(&home, false));
    }
}

#[cfg(test)]
mod prune_tests {
    //! Every row of `hangar daemon prune`'s decision table, against homes this
    //! test fabricates in its own `TempDir`.
    //!
    //! The load-bearing property is the one that is easiest to lose: prune
    //! signals NOTHING. Removing a directory is its only mutation, so a home
    //! whose records name any live process is reported and kept, in a dry run
    //! and under `--yes` alike. Three tests exist for that alone, one per shape
    //! of live owner, and each holds a `Child` handle proving the process it
    //! spawned outlived the run.

    use super::{
        LiveHomes, OwnerRecord, PRUNE_DEFAULT_MIN_AGE_DAYS, PidState, ProtectedRoots, PruneAction,
        PruneArgs, PruneContext, PruneTally, PruneVerdict, contains_the_whole_filesystem,
        describe_action, describe_summary, describe_verdict, has_symlink_component,
        home_paths_in_process_table, leaked_hangar_homes, pid_state_from_argv, process_table_homes,
        prune_all, prune_home_in, prune_min_age, prune_verdict_from,
    };
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    /// Every gate that lives OUTSIDE a home's own ownership records, wide open:
    /// the real protected roots, a process table that answered and named
    /// nothing, and no age threshold.
    ///
    /// The decision-table tests want exactly this, so their subject reaches the
    /// records. Each test that is ABOUT one of these gates narrows that one gate
    /// and leaves the rest open, which is what makes its refusal attributable.
    fn every_outer_gate_open() -> PruneContext {
        PruneContext::of(
            ProtectedRoots::current(),
            LiveHomes::answered(&[]),
            Duration::ZERO,
        )
    }

    /// A pid no live process can ever wear, for fixtures that need a recorded
    /// owner that is definitively gone.
    ///
    /// Deliberately NOT a just-reaped child's pid: that number goes straight
    /// back into the allocator, so between writing the fixture and reading it
    /// the kernel may hand it to something real, and the test would then be
    /// asserting about a stranger's process. This value is above every
    /// platform's `pid_max` (macOS 99999, Linux at most 2^30), so `kill` can
    /// only ever answer `ESRCH` for it, and it still fits in a positive `i32`.
    const UNRECYCLABLE_PID: u32 = 2_000_000_000;

    /// A child this test spawned, held as a `Child` for its whole life.
    ///
    /// Signals go THROUGH that handle, never to a raw pid: `Child::kill` is a
    /// no-op once the child has been reaped, so this cannot signal a pid the
    /// kernel has since recycled onto somebody else's process. The same handle
    /// answers `alive`, for the same reason.
    ///
    /// The reaper thread is load-bearing: an unreaped child stays a zombie, and
    /// `kill(pid, 0)` succeeds for a zombie, so the bounded exit-wait inside
    /// prune would time out on a process that had already died. It polls
    /// `try_wait` rather than blocking in `wait` so the handle is never held
    /// across a wait, which is what lets `Drop` take it to signal. A real daemon
    /// is detached and never our child, so it never becomes one.
    struct OwnedChild {
        pid: u32,
        child: std::sync::Arc<std::sync::Mutex<std::process::Child>>,
    }

    impl OwnedChild {
        /// Spawn `program` as a long sleeper. `program` decides how prune reads
        /// it: a path ending in `ainb-hangar-daemon` is a daemon by argv, any
        /// other name is a stranger.
        fn spawn(program: &std::path::Path) -> Self {
            Self::spawn_with(program, None)
        }

        /// A sleeper whose `$HOME` is `home`, which is exactly how a live
        /// ephemeral run shows up in the process table: the harness sets `HOME`
        /// and every hangar path is derived from it.
        fn spawn_with_home(home: &std::path::Path) -> Self {
            Self::spawn_with(&sleep_binary(), Some(home))
        }

        fn spawn_with(program: &std::path::Path, home: Option<&std::path::Path>) -> Self {
            let mut command = std::process::Command::new(program);
            if let Some(home) = home {
                command.env("HOME", home);
            }
            let child = command
                .arg("120")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn test child");
            let pid = child.id();
            let child = std::sync::Arc::new(std::sync::Mutex::new(child));
            let reaper = std::sync::Arc::clone(&child);
            std::thread::spawn(move || {
                // Ends when the child does, which the `sleep` argument bounds
                // even if nothing ever signals it.
                while reaper
                    .lock()
                    .expect("reap test child")
                    .try_wait()
                    .is_ok_and(|exited| exited.is_none())
                {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            });
            Self { pid, child }
        }

        fn alive(&self) -> bool {
            self.child
                .lock()
                .expect("probe test child")
                .try_wait()
                .is_ok_and(|exited| exited.is_none())
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            // Through the handle, so a child the reaper already collected is
            // not signalled at all. Never by pid, and never by name.
            let _ = self.child.lock().expect("stop test child").kill();
        }
    }

    fn sleep_binary() -> std::path::PathBuf {
        ["/bin/sleep", "/usr/bin/sleep"]
            .iter()
            .map(std::path::PathBuf::from)
            .find(|p| p.exists())
            .expect("a sleep binary")
    }

    /// A `sleep` reachable under the name a hangar daemon sidecar carries, so a
    /// child of ours reads as a daemon to `holder_is_live_daemon` without
    /// anybody's real daemon being involved.
    fn daemon_shaped_sleep(dir: &std::path::Path) -> std::path::PathBuf {
        let link = dir.join("ainb-hangar-daemon");
        std::os::unix::fs::symlink(sleep_binary(), &link).expect("symlink daemon-shaped sleep");
        link
    }

    /// A hangar home of the shape a `HOME=$(mktemp -d)` run leaves behind.
    fn fabricate_home(root: &std::path::Path, pid_file: Option<&str>) -> std::path::PathBuf {
        let home = root.join(ainb_hangar_core::paths::HANGAR_DIR);
        std::fs::create_dir_all(home.join("hangar")).expect("mkdir hangar home");
        if let Some(text) = pid_file {
            std::fs::write(ainb_hangar_daemon::pid_path_in(&home), text).expect("write pid file");
        }
        home
    }

    /// Publish this home's OWN ownership lock naming `pid`, the way a daemon
    /// does as the first statement of `boot`.
    fn record_lock_owner(home: &std::path::Path, pid: u32) {
        std::fs::write(
            ainb_hangar_daemon::single_instance::lock_path_in(home),
            format!("{pid}"),
        )
        .expect("write ownership lock");
    }

    /// Row 1: neither record exists. Nothing ever recorded an owner, so the
    /// home is just litter.
    #[test]
    fn prune_removes_a_home_with_no_recorded_owner() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);

        assert_eq!(
            prune_home_in(&home, &every_outer_gate_open(), false),
            (PruneVerdict::NoOwner, PruneAction::Reported)
        );
        assert!(home.is_dir(), "a dry run must not delete anything");

        assert_eq!(
            prune_home_in(&home, &every_outer_gate_open(), true),
            (PruneVerdict::NoOwner, PruneAction::Removed)
        );
        assert!(!home.exists());
    }

    /// Row 2: a record naming a process that is gone.
    #[test]
    fn prune_removes_a_home_whose_recorded_pid_is_dead() {
        let root = tempfile::tempdir().expect("tempdir");
        let dead = UNRECYCLABLE_PID;
        let home = fabricate_home(root.path(), Some(&format!("{dead}\n")));
        record_lock_owner(&home, dead);

        assert_eq!(
            prune_home_in(&home, &every_outer_gate_open(), false),
            (PruneVerdict::DeadOwner(dead), PruneAction::Reported)
        );
        assert!(home.is_dir());

        assert_eq!(
            prune_home_in(&home, &every_outer_gate_open(), true),
            (PruneVerdict::DeadOwner(dead), PruneAction::Removed)
        );
        assert!(!home.exists());
    }

    /// Row 3, the headline refusal: a home whose OWN ownership lock names a
    /// live hangar daemon is reported and left completely alone, under `--yes`
    /// as much as in a dry run.
    ///
    /// This is the case the verb used to SIGTERM and then SIGKILL. It cannot:
    /// the lock lives in a world-writable temp tree, so a stale or copied one
    /// naming the developer's real daemon reaches this exact branch, and
    /// nothing available inside the home tells the two apart. The process stays
    /// up, the home stays on disk, and the report hands the operator the
    /// command.
    #[test]
    fn prune_leaves_a_live_daemon_its_home_recorded_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        let daemon = OwnedChild::spawn(&daemon_shaped_sleep(root.path()));
        let home = fabricate_home(root.path(), Some(&format!("{}\n", daemon.pid)));
        record_lock_owner(&home, daemon.pid);

        for apply in [false, true] {
            assert_eq!(
                prune_home_in(&home, &every_outer_gate_open(), apply),
                (PruneVerdict::LiveDaemon(daemon.pid), PruneAction::LeftAlone),
                "apply={apply}"
            );
            assert!(
                daemon.alive(),
                "prune signalled a pid it read out of a world-writable file \
                 (apply={apply})"
            );
            assert!(home.is_dir(), "apply={apply}");
        }

        // And the report names the remedy prune declined to perform itself.
        let pid = daemon.pid;
        let sentence = describe_verdict(&PruneVerdict::LiveDaemon(pid));
        assert!(
            sentence.contains(&format!("kill {pid}")),
            "the operator is left without the command: {sentence}"
        );
    }

    /// The lock alone still decides the verdict: `daemon.pid` is written at the
    /// END of boot, so a daemon that took the lock seconds ago has none yet.
    /// Deciding from the pid file alone reads that home as ownerless and
    /// deletes it out from under a live daemon.
    #[test]
    fn prune_keeps_a_home_a_live_daemon_claimed_with_the_lock_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        let daemon = OwnedChild::spawn(&daemon_shaped_sleep(root.path()));
        let home = fabricate_home(root.path(), None);
        record_lock_owner(&home, daemon.pid);

        for apply in [false, true] {
            assert_eq!(
                prune_home_in(&home, &every_outer_gate_open(), apply),
                (PruneVerdict::LiveDaemon(daemon.pid), PruneAction::LeftAlone),
                "apply={apply}"
            );
            assert!(
                home.is_dir(),
                "a home whose lock names a live daemon is not litter (apply={apply})"
            );
            assert!(daemon.alive(), "apply={apply}");
        }
    }

    /// The pid file is a home-agnostic note written at the end of boot, and a
    /// leaked temp home can carry a stale one naming any pid on the machine, up
    /// to and including the developer's real daemon. So a live daemon the lock
    /// does not name is reported and left alone, with its home kept as the
    /// record of the claim.
    #[test]
    fn prune_never_signals_a_daemon_the_ownership_lock_does_not_name() {
        let root = tempfile::tempdir().expect("tempdir");
        let daemon = OwnedChild::spawn(&daemon_shaped_sleep(root.path()));
        let home = fabricate_home(root.path(), Some(&format!("{}\n", daemon.pid)));

        for apply in [false, true] {
            assert_eq!(
                prune_home_in(&home, &every_outer_gate_open(), apply),
                (
                    PruneVerdict::Unconfirmed(daemon.pid),
                    PruneAction::LeftAlone
                ),
                "apply={apply}"
            );
            assert!(
                daemon.alive(),
                "a daemon this home never claimed must survive prune"
            );
            assert!(home.is_dir());
        }
    }

    /// Row 4, the false-positive guard: the recorded pid is alive but belongs to
    /// something else entirely (the daemon died and its pid was recycled).
    /// Signalling it is exactly the accident this verb must never have, so the
    /// process keeps running and its home is kept as the record of the claim.
    #[test]
    fn prune_leaves_a_live_non_daemon_pid_and_its_home_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        let stranger = OwnedChild::spawn(&sleep_binary());
        let home = fabricate_home(root.path(), Some(&format!("{}\n", stranger.pid)));

        for apply in [false, true] {
            assert_eq!(
                prune_home_in(&home, &every_outer_gate_open(), apply),
                (PruneVerdict::Stranger(stranger.pid), PruneAction::LeftAlone),
                "apply={apply}"
            );
            assert!(stranger.alive(), "an unrelated process must survive prune");
            assert!(home.is_dir(), "its home is the only record of the claim");
        }
    }

    /// Row 5: the pid file exists but yields no pid. Fails closed - a file we
    /// cannot read is not evidence that nothing is running.
    #[test]
    fn prune_leaves_an_unreadable_pid_file_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        for content in ["", "   \n", "not-a-pid", "0\n"] {
            let home = fabricate_home(root.path(), Some(content));
            assert_eq!(
                prune_home_in(&home, &every_outer_gate_open(), true),
                (PruneVerdict::Unreadable, PruneAction::LeftAlone),
                "content {content:?}"
            );
            assert!(home.is_dir(), "content {content:?}");
        }
    }

    /// Nothing outside the temp roots is prunable, however it is reached. A
    /// symlink under a temp root pointing out of it resolves outside, and prune
    /// deletes trees, so the check is on the RESOLVED path.
    #[test]
    fn prune_refuses_a_home_outside_the_temp_roots() {
        // A directory that is genuinely outside every temp root, that certainly
        // exists, and that this test only ever reads through: the crate's own
        // source dir standing in for the real `$HOME` such a symlink would
        // point at in the wild.
        let outside = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        if super::is_under_temp_dir(outside) {
            // Built from inside a temp root, so there is no "outside" to aim a
            // symlink at and nothing to prove here.
            return;
        }
        let root = tempfile::tempdir().expect("tempdir");
        let link = root.path().join(ainb_hangar_core::paths::HANGAR_DIR);
        std::os::unix::fs::symlink(outside, &link).expect("symlink out of the temp roots");

        // Dry run for the same reason as the test below: the subject resolves
        // to a real source tree, so a broken guard must go red, not act.
        assert_eq!(
            prune_home_in(&link, &every_outer_gate_open(), false),
            (PruneVerdict::Protected, PruneAction::LeftAlone),
            "a temp-root path that RESOLVES outside is not prunable"
        );
        assert_eq!(
            prune_home_in(
                std::path::Path::new("/etc"),
                &every_outer_gate_open(),
                false
            ),
            (PruneVerdict::Protected, PruneAction::LeftAlone)
        );
        assert!(
            !leaked_hangar_homes().contains(&link),
            "the enumerator must not offer it either"
        );
        assert!(outside.is_dir(), "sanity: the symlink target still exists");
    }

    /// The identity refusal, isolated so that ONLY it can save the subject: a
    /// fabricated home that is under a temp root and records no owner, so every
    /// other guard in the chain says "remove".
    ///
    /// Asserted with `--yes`, and safe to: the subject is this test's own
    /// `TempDir`, so a broken identity check deletes a fixture and goes red
    /// rather than deleting anything real.
    #[test]
    fn prune_refuses_the_home_it_is_told_is_its_own() {
        let root = tempfile::tempdir().expect("tempdir");
        let mine = fabricate_home(root.path(), None);
        assert_eq!(
            prune_home_in(
                &mine,
                &PruneContext::of(
                    ProtectedRoots::of(Some(&mine), None),
                    LiveHomes::answered(&[]),
                    Duration::ZERO,
                ),
                true,
            ),
            (PruneVerdict::Protected, PruneAction::LeftAlone)
        );
        assert!(
            mine.is_dir(),
            "prune removed the home this process resolved"
        );
    }

    /// The real-home refusal, isolated the same way: a home that is under a
    /// temp root, records no owner, and is not this process's own, so ONLY the
    /// `dirs::home_dir()` check can save it.
    ///
    /// It is a separate gate from the temp-root test, not its complement.
    /// `$TMPDIR` is attacker-influenced, and one pointing at an ancestor of the
    /// real home makes every path in that home read as ephemeral: the
    /// enumerator would then offer the user's actual `~/.agents-in-a-box`, and
    /// `--yes` would delete it. Fixtures stand in for both roots, so a broken
    /// guard deletes a `TempDir` and goes red instead of deleting a real home.
    #[test]
    fn prune_refuses_a_home_under_the_real_home_dir() {
        // The fixture stands in for `$HOME` itself, so its `.agents-in-a-box`
        // has the exact shape prune approves and every other gate passes it.
        let real_home = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(real_home.path(), None);
        let gates = |protected: ProtectedRoots| {
            PruneContext::of(protected, LiveHomes::answered(&[]), Duration::ZERO)
        };

        // Without the guard this home is plain litter, which is what makes the
        // refusal below attributable to the guard and nothing else.
        assert_eq!(
            prune_home_in(&home, &gates(ProtectedRoots::of(None, None)), false),
            (PruneVerdict::NoOwner, PruneAction::Reported),
            "fixture is not the shape this test needs"
        );

        assert_eq!(
            prune_home_in(
                &home,
                &gates(ProtectedRoots::of(None, Some(real_home.path()))),
                true
            ),
            (PruneVerdict::Protected, PruneAction::LeftAlone)
        );
        assert!(home.is_dir(), "prune removed a home under the real $HOME");
    }

    /// The same refusal against the home this process ACTUALLY resolved, which
    /// also proves the enumerator never offers it.
    ///
    /// Asserted in DRY RUN, deliberately. This is the one test whose subject is
    /// the developer's live hangar home, so it must stay harmless even when the
    /// code under test is wrong: passing `true` here makes the test's safety
    /// depend on the very guard it is checking, and a broken guard then deletes
    /// that home for real instead of going red. The isolated case above is what
    /// proves the guard itself.
    #[test]
    fn prune_never_touches_the_home_this_process_resolved() {
        let Ok(mine) = ainb_hangar_daemon::hangar_dir() else {
            return;
        };
        assert_eq!(
            prune_home_in(&mine, &every_outer_gate_open(), false),
            (PruneVerdict::Protected, PruneAction::LeftAlone)
        );
        let listed = leaked_hangar_homes();
        assert!(
            !listed.contains(&mine),
            "prune listed its own home: {}",
            mine.display()
        );
    }

    /// The polarity that separates a lock arbiter from a killer: `ps` failing to
    /// answer is not proof of ownership, so it never yields a signalable
    /// verdict, whichever record named the pid.
    ///
    /// `single_instance::holder_is_live_daemon` answers YES in exactly this
    /// case, and is right to: there it decides whether to steal a lock, where a
    /// false negative puts two daemons on one home. Reusing that answer to
    /// authorize a SIGKILL is what inverts it into a hazard.
    #[test]
    fn prune_never_confirms_a_pid_the_process_table_cannot_identify() {
        let unknown = |_: u32| PidState::Unknown;
        for (lock, pidfile) in [
            (OwnerRecord::Pid(7), OwnerRecord::Pid(7)),
            (OwnerRecord::Pid(7), OwnerRecord::Absent),
            (OwnerRecord::Absent, OwnerRecord::Pid(7)),
        ] {
            assert_eq!(
                prune_verdict_from(lock, pidfile, &unknown),
                PruneVerdict::Unconfirmed(7),
                "lock {lock:?}, pid file {pidfile:?}"
            );
        }
    }

    /// The same polarity one layer down, where the process table is actually
    /// read: an argv `ps` could not produce is UNKNOWN even when the
    /// lock-arbitration predicate beside it fails open and says yes.
    #[test]
    fn an_unreadable_process_table_is_never_proof_of_a_daemon() {
        assert_eq!(pid_state_from_argv(None, || true), PidState::Unknown);
        // And a positive argv still needs that predicate to agree.
        let daemon = "/opt/homebrew/bin/ainb-hangar-daemon";
        assert_eq!(pid_state_from_argv(Some(daemon), || true), PidState::Daemon);
        assert_eq!(
            pid_state_from_argv(Some(daemon), || false),
            PidState::NotADaemon
        );
        // A self-exec'd cargo test binary wears the daemon argv without being
        // one, and prune signals nothing on shape alone.
        assert_eq!(
            pid_state_from_argv(
                Some("/repo/target/debug/deps/ainb-9f hangar daemon run"),
                || { true }
            ),
            PidState::NotADaemon
        );
    }

    /// The path the guard approved is the path that gets deleted.
    ///
    /// `prunable_in` resolves the home once; re-deriving it at delete time would
    /// resolve every component a second time, and under a world-writable temp
    /// root a symlink swapped in between the two sends the delete somewhere the
    /// guard never approved. Reaching one temp-root home through a symlink left
    /// in another makes the difference observable: the RESOLVED directory is the
    /// leaked home, and it is what must go.
    #[test]
    fn prune_removes_the_exact_path_it_validated() {
        let leaked = tempfile::tempdir().expect("tempdir");
        let resolved = fabricate_home(leaked.path(), None);
        let decoy = tempfile::tempdir().expect("tempdir");
        let reached_by = decoy.path().join(ainb_hangar_core::paths::HANGAR_DIR);
        std::os::unix::fs::symlink(&resolved, &reached_by).expect("symlink into the temp root");

        assert_eq!(
            prune_home_in(&reached_by, &every_outer_gate_open(), true),
            (PruneVerdict::NoOwner, PruneAction::Removed)
        );
        assert!(
            !resolved.exists(),
            "prune deleted something other than the path it validated"
        );
    }

    /// The shape `--yes` may delete is exactly `<temp root>/<one component>/
    /// .agents-in-a-box`, and nothing else that merely LIVES under a temp root.
    ///
    /// A prefix test ("resolves somewhere under a temp root") approves the temp
    /// root itself, every sibling tree beside a leaked home, and every tree a
    /// symlink resolves into, so one symlink turns `--yes` into a
    /// `remove_dir_all` of a whole temp tree. Each subject below is a real
    /// directory this test created, so a guard that regressed deletes a fixture
    /// and goes red rather than deleting anything real.
    #[test]
    fn prune_refuses_everything_but_the_exact_leaked_home_shape() {
        let root = tempfile::tempdir().expect("tempdir");
        let nested = root.path().join("session");
        std::fs::create_dir_all(&nested).expect("mkdir nested");
        let too_deep = fabricate_home(&nested, None);
        let wrong_leaf = root.path().join("hangar-home");
        std::fs::create_dir_all(&wrong_leaf).expect("mkdir wrong leaf");
        let file_root = tempfile::tempdir().expect("tempdir");
        let a_file = file_root.path().join(ainb_hangar_core::paths::HANGAR_DIR);
        std::fs::write(&a_file, "not a home").expect("write file");

        for (subject, why) in [
            (
                root.path().to_path_buf(),
                "the temp-dir entry a leaked home sits IN is not a home",
            ),
            (
                std::env::temp_dir(),
                "the temp root itself is the whole temp tree",
            ),
            (
                too_deep,
                "two levels down is somebody else's tree, not a leaked home",
            ),
            (wrong_leaf, "a directory not named .agents-in-a-box"),
            (a_file, "a FILE with the right name is not a home"),
        ] {
            assert_eq!(
                prune_home_in(&subject, &every_outer_gate_open(), false),
                (PruneVerdict::Protected, PruneAction::LeftAlone),
                "{} was approved: {why}",
                subject.display()
            );
            assert!(subject.exists(), "{}", subject.display());
        }
    }

    /// The post-resolution symlink re-check, driven directly.
    ///
    /// `canonicalize` answers about the moment it ran, and temp roots are
    /// world-writable, so an unprivileged local process can swap a component for
    /// a symlink in the window between the guard's answer and
    /// `remove_dir_all`. Re-reading every component with `symlink_metadata` is
    /// what turns that swap into a refusal, and it is asserted here rather than
    /// through `prunable_in` because no path `canonicalize` returns can carry a
    /// symlink component in the first place: the race is exactly what the
    /// re-check exists for, and only the re-check can be shown it.
    #[test]
    fn a_symlinked_component_is_refused_not_followed() {
        let root = tempfile::tempdir().expect("tempdir");
        let real = root.path().join("real");
        std::fs::create_dir_all(real.join(ainb_hangar_core::paths::HANGAR_DIR))
            .expect("mkdir real home");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink parent");

        assert!(
            has_symlink_component(&link.join(ainb_hangar_core::paths::HANGAR_DIR)),
            "a swapped parent component must be caught"
        );
        assert!(
            has_symlink_component(&root.path().join("gone").join("x")),
            "a component we cannot stat is an unknown, and prune fails closed"
        );
        let clean = std::fs::canonicalize(real.join(ainb_hangar_core::paths::HANGAR_DIR))
            .expect("canonicalize");
        assert!(
            !has_symlink_component(&clean),
            "a fully resolved path has nothing to refuse: {}",
            clean.display()
        );
    }

    /// The rest of the decision table, driven directly so every row is pinned
    /// without a process or a filesystem in the way.
    #[test]
    fn prune_verdict_table() {
        let daemon = |_: u32| PidState::Daemon;
        let dead = |_: u32| PidState::Dead;
        let stranger = |_: u32| PidState::NotADaemon;
        use OwnerRecord::{Absent, Pid, Unreadable};

        // Only the lock authorizes a signal.
        assert_eq!(
            prune_verdict_from(Pid(9), Absent, &daemon),
            PruneVerdict::LiveDaemon(9)
        );
        assert_eq!(
            prune_verdict_from(Absent, Pid(9), &daemon),
            PruneVerdict::Unconfirmed(9)
        );
        // A live process that is positively not a daemon keeps its home either
        // way: the pid was recycled, and the claim is all we have.
        assert_eq!(
            prune_verdict_from(Pid(9), Absent, &stranger),
            PruneVerdict::Stranger(9)
        );
        assert_eq!(
            prune_verdict_from(Absent, Pid(9), &stranger),
            PruneVerdict::Stranger(9)
        );
        // Nothing live named by either record.
        assert_eq!(
            prune_verdict_from(Pid(9), Pid(9), &dead),
            PruneVerdict::DeadOwner(9)
        );
        assert_eq!(
            prune_verdict_from(Absent, Absent, &dead),
            PruneVerdict::NoOwner
        );
        // An unreadable record is not evidence that nothing is running, so it
        // outranks every removal verdict, including a dead pid beside it.
        assert_eq!(
            prune_verdict_from(Unreadable, Absent, &dead),
            PruneVerdict::Unreadable
        );
        assert_eq!(
            prune_verdict_from(Absent, Unreadable, &dead),
            PruneVerdict::Unreadable
        );
        assert_eq!(
            prune_verdict_from(Unreadable, Pid(9), &dead),
            PruneVerdict::Unreadable
        );
    }

    /// Under `--yes`, EVERY verdict that names a live pid keeps its home. The
    /// row-level tests above each prove one shape against a real process; this
    /// pins the whole column, so a new live-owner verdict cannot be added
    /// straight into the removal branch.
    #[test]
    fn no_live_owner_verdict_is_ever_removable() {
        for verdict in [
            PruneVerdict::LiveDaemon(9),
            PruneVerdict::Stranger(9),
            PruneVerdict::Unconfirmed(9),
            PruneVerdict::Unreadable,
            PruneVerdict::Protected,
        ] {
            assert!(!verdict.is_removable(), "{verdict:?}");
        }
        assert!(PruneVerdict::NoOwner.is_removable());
        assert!(PruneVerdict::DeadOwner(9).is_removable());
    }

    /// The summary counts removals and left-alone homes separately, and says
    /// how many of the latter have a live owner.
    ///
    /// The count it replaced summed the two into "acted on N home(s)", which
    /// read as "N daemons dealt with" - a claim the verb cannot make now that
    /// it signals nothing. Fed from `record` rather than hand-set fields, so
    /// the summary and the per-home decisions cannot drift apart.
    #[test]
    fn prune_summary_separates_removals_from_homes_left_alone() {
        let mut tally = PruneTally::default();
        tally.record(&PruneVerdict::NoOwner, &PruneAction::Removed);
        tally.record(&PruneVerdict::DeadOwner(4), &PruneAction::Removed);
        tally.record(&PruneVerdict::Unreadable, &PruneAction::Removed);
        tally.record(&PruneVerdict::LiveDaemon(7), &PruneAction::LeftAlone);
        tally.record(&PruneVerdict::Stranger(8), &PruneAction::LeftAlone);
        tally.record(&PruneVerdict::Unreadable, &PruneAction::LeftAlone);

        assert_eq!(
            describe_summary(&tally, true),
            "3 home(s) removed, 3 left alone (2 with a live owner)"
        );

        let mut dry = PruneTally::default();
        dry.record(&PruneVerdict::NoOwner, &PruneAction::Reported);
        dry.record(&PruneVerdict::LiveDaemon(7), &PruneAction::LeftAlone);
        assert_eq!(
            describe_summary(&dry, false),
            "1 home(s) would be removed, 1 left alone (1 with a live owner); \
             re-run with --yes to remove them"
        );

        // Nothing removable: no nudge to re-run, and still no claim of action.
        let mut kept = PruneTally::default();
        kept.record(&PruneVerdict::Unreadable, &PruneAction::LeftAlone);
        assert_eq!(
            describe_summary(&kept, false),
            "0 home(s) would be removed, 1 left alone"
        );
    }

    /// No summary this verb can print may describe a process being dealt with.
    ///
    /// Asserted over the whole cross product rather than the three sentences
    /// above, because the hazard is a future wording change, not today's.
    #[test]
    fn no_prune_summary_can_imply_a_signal() {
        for verdict in [
            PruneVerdict::NoOwner,
            PruneVerdict::DeadOwner(4),
            PruneVerdict::LiveDaemon(7),
            PruneVerdict::Stranger(8),
            PruneVerdict::Unconfirmed(9),
            PruneVerdict::Unreadable,
            PruneVerdict::Protected,
        ] {
            for action in [
                PruneAction::Removed,
                PruneAction::Reported,
                PruneAction::LeftAlone,
                PruneAction::RemoveFailed("denied".to_string()),
            ] {
                let mut tally = PruneTally::default();
                tally.record(&verdict, &action);
                for applied in [false, true] {
                    let line = describe_summary(&tally, applied);
                    for banned in ["stop", "kill", "signal", "acted on", "daemon"] {
                        assert!(
                            !line.contains(banned),
                            "{line:?} claims {banned:?} for {verdict:?}/{action:?}"
                        );
                    }
                }
            }
        }
    }

    /// The enumerator finds the `<temp root>/*/.agents-in-a-box` shape a leaked
    /// run leaves. Read-only: it classifies nothing and removes nothing.
    #[test]
    fn prune_enumerates_hangar_homes_one_level_under_a_temp_root() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        assert!(
            leaked_hangar_homes().contains(&home),
            "{} should be listed",
            home.display()
        );
        assert!(home.is_dir(), "listing must not remove anything");
    }

    // ──────────────────────────────────────────────────────────────────────
    // The two gates that separate a LIVE ephemeral run from a leaked one.
    //
    // Since #784 no daemon is ever spawned into an ephemeral home, so a running
    // session leaves no ownership lock and no pid file: to the record-based
    // decision table above, a home in active use and a home abandoned last week
    // are the same `NoOwner` litter. These are what tell them apart.
    // ──────────────────────────────────────────────────────────────────────

    /// A home a live process is running in is left alone, with `--yes`.
    ///
    /// Driven through the REAL process table against a child this test spawned,
    /// because the wiring from `ps` to the verdict is the part that can rot.
    /// The child is held as a `Child` handle for its whole life and never
    /// signalled by pid; prune signals nothing at all.
    #[test]
    fn prune_leaves_a_home_a_live_process_is_running_in() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);

        // Baseline: with nothing running in it, this home is removable litter.
        // That is what makes the refusal below attributable to this gate.
        assert_eq!(
            prune_home_in(&home, &every_outer_gate_open(), false),
            (PruneVerdict::NoOwner, PruneAction::Reported),
            "fixture is not the shape this test needs"
        );

        let live = OwnedChild::spawn_with_home(root.path());
        let table = LiveHomes::from_process_table();
        // The process table must have ANSWERED, and named this child's home.
        // Without both, the assertion below is satisfied by the fail-closed
        // path, which is what a `ps` read that never completes looks like: every
        // home reported in use, and the gate inert.
        assert!(
            table
                .0
                .as_ref()
                .is_some_and(|homes| homes
                    .contains(&std::fs::canonicalize(root.path()).expect("canonicalize"))),
            "ps did not report the child's $HOME; the gate would be fail-closed, \
             not working"
        );
        let ctx = PruneContext::of(ProtectedRoots::current(), table, Duration::ZERO);
        assert_eq!(
            prune_home_in(&home, &ctx, true),
            (PruneVerdict::InUse, PruneAction::LeftAlone)
        );
        assert!(
            home.is_dir(),
            "prune deleted a home out from under a live run"
        );
        assert!(live.alive(), "prune must never signal anything");
    }

    /// A process table that cannot answer removes NOTHING.
    ///
    /// The polarity that matters: a missing answer is not evidence that nothing
    /// is running, and this is the gate that stands between `--yes` and a live
    /// session's home. Failing open here deletes it.
    #[test]
    fn an_unreadable_process_table_leaves_every_home_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        let ctx = PruneContext::of(
            ProtectedRoots::current(),
            LiveHomes::unreadable(),
            Duration::ZERO,
        );
        assert_eq!(
            prune_home_in(&home, &ctx, true),
            (PruneVerdict::InUse, PruneAction::LeftAlone)
        );
        assert!(home.is_dir());
    }

    /// Either variable identifies the same home, and neither matches a
    /// neighbour that merely shares a prefix.
    #[test]
    fn a_live_home_is_matched_by_either_environment_variable() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        let resolved = std::fs::canonicalize(&home).expect("canonicalize");
        let sibling = root.path().join("sibling");

        assert!(
            LiveHomes::answered(&[root.path()]).claims(&resolved),
            "$HOME names the PARENT of the hangar home"
        );
        assert!(
            LiveHomes::answered(&[resolved.as_path()]).claims(&resolved),
            "$AINB_HANGAR_HOME names the home itself"
        );
        assert!(
            !LiveHomes::answered(&[sibling.as_path()]).claims(&resolved),
            "an unrelated path must not keep this home"
        );
        assert!(!LiveHomes::answered(&[]).claims(&resolved));
    }

    /// The `ps` dump parse: both variables, raw and resolved, and nothing else.
    #[test]
    fn the_process_table_parse_reads_both_home_variables_only() {
        let homes = home_paths_in_process_table(
            "1234 /bin/sleep 120 HOME=/tmp/bj.Q9x7fk PATH=/usr/bin \
             AINB_HANGAR_HOME=/tmp/other/.agents-in-a-box HOMEBREW_PREFIX=/opt/homebrew HOME=",
        );
        assert!(homes.contains(std::path::Path::new("/tmp/bj.Q9x7fk")));
        assert!(homes.contains(std::path::Path::new("/tmp/other/.agents-in-a-box")));
        assert!(
            !homes.contains(std::path::Path::new("/opt/homebrew")),
            "a variable that merely ENDS in HOME is not a hangar home"
        );
        assert!(
            !homes.contains(std::path::Path::new("")),
            "an empty value names nothing"
        );
        assert!(
            !homes.contains(std::path::Path::new("/usr/bin")),
            "PATH is not a home"
        );
    }

    /// A `ps` that exits 0 while showing no environments has not ANSWERED.
    ///
    /// The gate could previously only fail closed on a `ps` it was unable to
    /// RUN. A `ps` that runs, succeeds and prints no environment at all (a
    /// hardened kernel, a container with no `/proc`, a busybox `ps` that
    /// ignores `e`) was read as a complete answer naming no homes, which made
    /// every home on the machine report as free.
    #[test]
    fn a_ps_that_shows_no_environments_is_not_an_answer() {
        let dump = "  PID TTY           TIME CMD\n\
                    1 ??         0:12.34 /sbin/launchd\n\
                 4321 ??         0:00.01 /bin/sleep 120\n";
        assert!(
            home_paths_in_process_table(dump).is_empty(),
            "the fixture must name no homes, or it proves nothing"
        );
        assert!(
            process_table_homes(dump).is_none(),
            "no HOME= anywhere is `ps` declining, not an empty machine"
        );

        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        let ctx = PruneContext::of(
            ProtectedRoots::current(),
            LiveHomes(process_table_homes(dump)),
            Duration::ZERO,
        );
        assert_eq!(
            prune_home_in(&home, &ctx, true),
            (PruneVerdict::InUse, PruneAction::LeftAlone)
        );
        assert!(home.is_dir(), "a `ps` that answered nothing deleted a home");

        // A dump that DOES carry an environment is still an answer, so the
        // check cannot be satisfied by refusing every dump there is.
        let with_env = format!("{dump}  99 ??         0:00.01 /bin/sleep HOME=/tmp/x\n");
        assert!(process_table_homes(&with_env).is_some());
    }

    /// A `$HOME` containing a space survives the parse.
    ///
    /// `ps` prints the environment unquoted, so splitting on whitespace
    /// recorded `/tmp/my` for a `HOME=/tmp/my home dir`. The truncated prefix
    /// matched no candidate, the live run read as abandoned, and `--yes`
    /// deleted the home it named.
    #[test]
    fn the_process_table_parse_keeps_a_home_containing_a_space() {
        let homes = home_paths_in_process_table(
            "4321 ?? 0:00.01 /bin/sleep 120 HOME=/tmp/my home dir PATH=/usr/bin SHELL=/bin/zsh",
        );
        assert!(
            homes.contains(std::path::Path::new("/tmp/my home dir")),
            "a value runs to the next KEY= boundary, not the next space: {homes:?}"
        );
        assert!(
            !homes.contains(std::path::Path::new("/tmp/my")),
            "the truncated prefix must never be recorded as a home"
        );
        // The last assignment on a line runs to the end of it.
        let trailing = home_paths_in_process_table(
            "1 ?? 0:00.01 /bin/sh AINB_HANGAR_HOME=/tmp/a b/.agents-in-a-box",
        );
        assert!(
            trailing.contains(std::path::Path::new("/tmp/a b/.agents-in-a-box")),
            "{trailing:?}"
        );
    }

    /// Set `path`'s mtime back by `age`, so the age gate can be driven against a
    /// real timestamp rather than a shifted clock.
    fn backdate(path: &std::path::Path, age: Duration) {
        let when = std::time::SystemTime::now() - age;
        let times = std::fs::FileTimes::new().set_accessed(when).set_modified(when);
        std::fs::File::open(path)
            .expect("open for set_times")
            .set_times(times)
            .expect("backdate");
    }

    const A_DAY: Duration = Duration::from_secs(60 * 60 * 24);

    /// A home touched inside the threshold is skipped, and the same home is
    /// removed once it is old enough.
    ///
    /// Both halves are needed: a gate that always skips would pass the first
    /// assertion alone, and prune would then never remove anything again.
    #[test]
    fn prune_skips_a_home_touched_more_recently_than_the_threshold() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        let gate = || PruneContext::of(ProtectedRoots::current(), LiveHomes::answered(&[]), A_DAY);

        assert_eq!(
            prune_home_in(&home, &gate(), true),
            (PruneVerdict::Recent, PruneAction::LeftAlone),
            "a home written a moment ago belongs to a run that is still going"
        );
        assert!(home.is_dir());

        backdate(&home.join("hangar"), A_DAY * 2);
        backdate(&home, A_DAY * 2);
        assert_eq!(
            prune_home_in(&home, &gate(), true),
            (PruneVerdict::NoOwner, PruneAction::Removed),
            "an old home is what this verb exists to remove"
        );
        assert!(!home.exists());
    }

    /// The `hangar/` subdir counts, not just the home dir.
    ///
    /// It is the half a live session keeps rewriting (sockets, locks, logs),
    /// while the home dir's own mtime only moves when a top-level entry appears
    /// or goes. Reading the home dir alone would age out a session that is very
    /// much alive.
    #[test]
    fn the_age_gate_reads_the_hangar_subdir_too() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        backdate(&home, A_DAY * 2);

        assert_eq!(
            prune_home_in(
                &home,
                &PruneContext::of(ProtectedRoots::current(), LiveHomes::answered(&[]), A_DAY),
                true
            ),
            (PruneVerdict::Recent, PruneAction::LeftAlone),
            "a fresh hangar/ is a live session, whatever the home dir's mtime says"
        );
        assert!(home.is_dir());
    }

    /// A file INSIDE `hangar/` is the sign of life both directory mtimes miss.
    ///
    /// POSIX moves a directory's mtime when an entry appears, is renamed or
    /// goes, never when a file already inside it is written. A daemon rewriting
    /// `daemon.heartbeat` every few seconds therefore leaves the home dir and
    /// `hangar/` both looking untouched, and a gate that read only those two
    /// aged out a continuously active home and let `--yes` delete it.
    #[test]
    fn the_age_gate_reads_the_files_inside_the_hangar_dir() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        let heartbeat = home.join("hangar").join("daemon.heartbeat");
        std::fs::write(&heartbeat, b"alive").expect("write heartbeat");
        // Exactly the shape POSIX produces: the file inside is seconds old and
        // both directories around it look a couple of days stale.
        backdate(&home.join("hangar"), A_DAY * 2);
        backdate(&home, A_DAY * 2);

        let gate = || PruneContext::of(ProtectedRoots::current(), LiveHomes::answered(&[]), A_DAY);
        assert_eq!(
            prune_home_in(&home, &gate(), true),
            (PruneVerdict::Recent, PruneAction::LeftAlone),
            "a heartbeat written seconds ago is a run that is still going"
        );
        assert!(home.is_dir(), "prune deleted a home being written to");

        // And the gate lets go once nothing inside is fresh either, so the
        // assertion above cannot be satisfied by a gate that keeps everything.
        backdate(&heartbeat, A_DAY * 2);
        backdate(&home.join("hangar"), A_DAY * 2);
        backdate(&home, A_DAY * 2);
        assert_eq!(
            prune_home_in(&home, &gate(), true),
            (PruneVerdict::NoOwner, PruneAction::Removed),
            "an old home is what this verb exists to remove"
        );
        assert!(!home.exists());
    }

    /// A home whose records name a LIVE daemon says so, even while the age gate
    /// would keep it anyway.
    ///
    /// Not a safety property: both answers keep the home. It is the only
    /// sentence that carries the pid and the literal command that stops it, and
    /// a live daemon rewriting its heartbeat keeps its own home permanently
    /// inside the age window, so answering `Recent` first would withhold that
    /// remedy from every real leaked daemon there is.
    #[test]
    fn a_live_recorded_owner_outranks_the_age_gate() {
        let root = tempfile::tempdir().expect("tempdir");
        let daemon = OwnedChild::spawn(&daemon_shaped_sleep(root.path()));
        let home = fabricate_home(root.path(), None);
        record_lock_owner(&home, daemon.pid);

        // Everything here was written seconds ago, so the age gate on its own
        // would answer `Recent`.
        let ctx = PruneContext::of(ProtectedRoots::current(), LiveHomes::answered(&[]), A_DAY);
        assert_eq!(
            prune_home_in(&home, &ctx, true),
            (PruneVerdict::LiveDaemon(daemon.pid), PruneAction::LeftAlone)
        );
        assert!(home.is_dir());
        assert!(daemon.alive(), "prune must never signal anything");
    }

    /// A `hangar/` that cannot be listed keeps the home.
    ///
    /// It hides exactly the files the gate above exists to read, so it is an
    /// unknown, and every unknown here fails closed.
    #[test]
    fn a_hangar_dir_that_cannot_be_listed_keeps_the_home() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        let hangar = home.join("hangar");
        backdate(&hangar, A_DAY * 2);
        backdate(&home, A_DAY * 2);
        let restore = std::fs::metadata(&hangar).expect("stat hangar dir").permissions();
        std::fs::set_permissions(&hangar, std::fs::Permissions::from_mode(0o000))
            .expect("make hangar dir unlistable");

        let verdict = prune_home_in(
            &home,
            &PruneContext::of(ProtectedRoots::current(), LiveHomes::answered(&[]), A_DAY),
            false,
        );
        let still_listable = std::fs::read_dir(&hangar).is_ok();
        std::fs::set_permissions(&hangar, restore).expect("restore hangar dir");

        if still_listable {
            // Only a caller that ignores the permission bit (root) reaches
            // here. The fixture is then not the shape this test needs, and
            // asserting on it would prove nothing.
            return;
        }
        assert_eq!(verdict, (PruneVerdict::Recent, PruneAction::LeftAlone));
        assert!(home.is_dir());
    }

    /// `--older-than 0` is the escape hatch, and it is the ONLY thing that turns
    /// the age gate off.
    #[test]
    fn a_zero_threshold_disables_the_age_gate() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        assert_eq!(
            prune_home_in(&home, &every_outer_gate_open(), false),
            (PruneVerdict::NoOwner, PruneAction::Reported)
        );
    }

    // ──────────────────────────────────────────────────────────────────────
    // The verb body: the `--yes` -> `apply` threading IS the dry-run promise.
    // ──────────────────────────────────────────────────────────────────────

    /// A dry run touches nothing; `--yes` removes the removable homes and only
    /// those. Driven over the same three fabricated homes, in the same order,
    /// so the two runs are comparable.
    #[test]
    fn the_prune_verb_removes_nothing_without_apply() {
        let removable_root = tempfile::tempdir().expect("tempdir");
        let removable = fabricate_home(removable_root.path(), None);
        let live_root = tempfile::tempdir().expect("tempdir");
        let daemon = OwnedChild::spawn(&daemon_shaped_sleep(live_root.path()));
        let owned = fabricate_home(live_root.path(), None);
        record_lock_owner(&owned, daemon.pid);
        let recent_root = tempfile::tempdir().expect("tempdir");
        let recent = fabricate_home(recent_root.path(), None);

        let homes = vec![removable.clone(), owned.clone(), recent.clone()];
        // Only `recent` stays inside the age window, so each home in this run is
        // kept (or removed) for a different reason.
        let gates = || PruneContext::of(ProtectedRoots::current(), LiveHomes::answered(&[]), A_DAY);
        backdate(&removable.join("hangar"), A_DAY * 2);
        backdate(&removable, A_DAY * 2);
        backdate(&owned.join("hangar"), A_DAY * 2);
        backdate(&owned, A_DAY * 2);

        let dry = prune_all(&homes, &gates(), false);
        assert_eq!(
            dry.tally,
            PruneTally {
                removed: 1,
                left_alone: 2,
                live_owner: 1,
                in_use: 0,
                recent: 1,
            }
        );
        assert_eq!(
            dry.summary,
            "1 home(s) would be removed, 2 left alone (1 with a live owner, 1 too recent); \
             re-run with --yes to remove them"
        );
        assert_eq!(dry.lines.len(), 3);
        for home in &homes {
            assert!(home.is_dir(), "a dry run removed {}", home.display());
        }

        let applied = prune_all(&homes, &gates(), true);
        assert_eq!(
            applied.tally, dry.tally,
            "--yes cannot widen what is removable"
        );
        assert!(!removable.exists(), "the removable home survived --yes");
        assert!(owned.is_dir(), "--yes removed a home with a live owner");
        assert!(
            recent.is_dir(),
            "--yes removed a home inside the age window"
        );
        assert!(daemon.alive(), "prune must never signal anything");
    }

    /// An empty machine gets the sentence the operator sees most often, and no
    /// per-home lines to go with it.
    #[test]
    fn the_prune_verb_says_so_when_there_is_nothing_to_report() {
        let report = prune_all(&[], &every_outer_gate_open(), true);
        assert_eq!(report.lines, Vec::<String>::new());
        assert_eq!(report.summary, "no hangar homes under the system temp dir");
        assert_eq!(report.tally, PruneTally::default());
    }

    /// Every `describe_action` sentence, including the two that print nothing.
    ///
    /// The action suffix is what tells an operator whether a home is still on
    /// disk, so a `RemoveFailed` that rendered as "removed" (or as nothing at
    /// all) would report a deletion that never happened.
    #[test]
    fn describe_action_says_only_what_apply_actually_did() {
        assert_eq!(describe_action(&PruneAction::Reported), None);
        assert_eq!(describe_action(&PruneAction::LeftAlone), None);
        assert_eq!(
            describe_action(&PruneAction::Removed),
            Some("removed".to_string())
        );
        assert_eq!(
            describe_action(&PruneAction::RemoveFailed("Permission denied".to_string())),
            Some("could not remove: Permission denied".to_string())
        );
    }

    /// One line per home: the path, the verdict, and the action if there was
    /// one.
    ///
    /// The exact text, because these lines ARE the verb's output. A dry run
    /// carries no arrow at all, which is what distinguishes "this would be
    /// removed" from "this was".
    #[test]
    fn each_home_line_is_the_path_the_verdict_and_the_action() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);

        let dry = prune_all(std::slice::from_ref(&home), &every_outer_gate_open(), false);
        assert_eq!(
            dry.lines,
            vec![format!("{} - stale home, no owner", home.display())]
        );
        assert!(home.is_dir());

        let applied = prune_all(std::slice::from_ref(&home), &every_outer_gate_open(), true);
        assert_eq!(
            applied.lines,
            vec![format!(
                "{} - stale home, no owner -> removed",
                home.display()
            )]
        );
        assert!(!home.exists());
    }

    /// A removal that FAILS is reported as such and counted with the homes left
    /// alone.
    ///
    /// The one action no other test produces, and the one whose miscounting
    /// would be worst: the directory is still on disk, so tallying it as a
    /// removal would have the summary claim a cleanup that did not happen.
    /// Produced for real, by taking write permission off the directory the home
    /// sits in so the final `rmdir` cannot succeed.
    #[test]
    fn a_removal_that_fails_is_reported_and_counted_as_left_alone() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = fabricate_home(root.path(), None);
        let restore = std::fs::metadata(root.path()).expect("stat temp root").permissions();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o555))
            .expect("make the home's parent read-only");

        let report = prune_all(std::slice::from_ref(&home), &every_outer_gate_open(), true);

        std::fs::set_permissions(root.path(), restore).expect("restore temp root");
        if !home.exists() {
            // Only a caller that ignores the permission bit (root) reaches
            // here. The fixture is then not the shape this test needs.
            return;
        }
        assert_eq!(
            report.tally,
            PruneTally {
                removed: 0,
                left_alone: 1,
                live_owner: 0,
                in_use: 0,
                recent: 0,
            },
            "a failed removal is not a removal"
        );
        let expected = format!(
            "{} - stale home, no owner -> could not remove: ",
            home.display()
        );
        assert!(
            report.lines[0].starts_with(&expected),
            "expected a line starting {expected:?}, got {:?}",
            report.lines[0]
        );
        assert_eq!(
            report.summary, "0 home(s) removed, 1 left alone",
            "the summary must not imply the home is gone"
        );
    }

    /// Both protected roots resolve on the machine this suite runs on.
    ///
    /// Every other refusal test feeds `ProtectedRoots::of` a fixture, so a
    /// `current()` that quietly returned `None` for both fields would leave the
    /// real guard inert with a green suite: `refuses` answers `false` for a
    /// `None` root, by design.
    #[test]
    fn the_real_protected_roots_both_resolve() {
        let roots = ProtectedRoots::current();
        assert!(
            roots.mine.is_some(),
            "this process could not resolve its own hangar home, so prune would \
             not recognise it"
        );
        assert!(
            roots.real_home.is_some(),
            "dirs::home_dir() answered nothing, so the real-$HOME refusal is inert"
        );
    }

    /// The roots that decide what `--yes` may ever look at.
    ///
    /// This list is the outer boundary of the whole verb: a root added here
    /// widens what can be enumerated and deleted, and a root that contains
    /// everything (`TMPDIR=/`) would offer the entire filesystem, which is why
    /// a parentless root is dropped.
    #[test]
    fn the_temp_roots_are_the_system_temp_dir_and_the_fixed_ones() {
        let roots = super::temp_roots();
        for expected in [
            std::env::temp_dir(),
            std::path::PathBuf::from("/tmp"),
            std::path::PathBuf::from("/var/tmp"),
        ] {
            assert!(
                roots.contains(&expected),
                "{} is missing",
                expected.display()
            );
        }
        for root in &roots {
            assert!(
                !contains_the_whole_filesystem(root),
                "{} contains the whole filesystem",
                root.display()
            );
        }
    }

    /// The guard is asked of the RESOLVED root, because that is one of the two
    /// forms membership is decided with.
    ///
    /// `is_under_temp_dir` answers `true` when the raw path is under the root
    /// as written OR the resolved path is under the resolved root. A root whose
    /// raw form has a parent but whose resolved form is `/` therefore passed a
    /// raw-only check while matching every path on the machine, which is the
    /// `TMPDIR=/` catastrophe wearing one extra component.
    #[test]
    fn a_temp_root_that_resolves_to_the_filesystem_root_is_dropped() {
        let disguised = std::path::Path::new("/..");
        assert!(
            disguised.parent().is_some(),
            "the raw form has a parent, which is exactly why a raw-only check \
             lets this root through"
        );
        assert_eq!(
            std::fs::canonicalize(disguised).expect("canonicalize /.."),
            std::path::Path::new("/"),
            "the form membership uses IS the filesystem root"
        );
        assert!(contains_the_whole_filesystem(disguised));

        // And an ordinary temp root is still kept, so the guard cannot be
        // satisfied by dropping everything.
        assert!(!contains_the_whole_filesystem(&std::env::temp_dir()));
        assert!(!contains_the_whole_filesystem(std::path::Path::new("/tmp")));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    //! Pid ownership, stop decisions and launch resolution for the daemon, moved
    //! here with the lifecycle code they exercise.

    use super::*;

    /// A real process we own that holds `socket` open: the listener is bound here
    /// and handed to a `sleep` child as its stdin, so `lsof` reports the child
    /// holding that bound name, the same evidence a real daemon leaves. Killed by
    /// its exact pid on drop; never by name.
    struct SocketDecoy {
        _listener: std::os::unix::net::UnixListener,
        child: std::process::Child,
    }

    impl SocketDecoy {
        fn holding(socket: &std::path::Path) -> Self {
            let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind decoy");
            let handed = listener.try_clone().expect("clone decoy socket");
            let child = std::process::Command::new("/bin/sleep")
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

    impl Drop for SocketDecoy {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    /// `stop` reads a pid out of a file and used to SIGTERM it on sight. A daemon
    /// that died without cleaning up leaves that pid free to be recycled by an
    /// unrelated process, which then eats the signal. Ownership must be proved
    /// against this home's `hangar.sock` first.
    #[test]
    fn stop_refuses_a_pid_that_does_not_hold_this_homes_socket() {
        let mine = tempfile::tempdir().expect("tempdir");
        let theirs = tempfile::tempdir().expect("tempdir");
        let socket = mine.path().join("hangar.sock");
        let pid_path = mine.path().join("hangar").join("daemon.pid");
        std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();

        // A live process that holds nothing: the recycled-pid case.
        let stranger = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn stranger");
        let stranger_pid = stranger.id();
        struct KillOnDrop(std::process::Child);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let _cleanup = KillOnDrop(stranger);
        std::fs::write(&pid_path, format!("{stranger_pid}\n")).unwrap();
        assert_eq!(
            stop_decision(&pid_path, &socket),
            StopDecision::Unproven(stranger_pid)
        );

        // A live daemon of a DIFFERENT hangar home: also never ours to signal.
        let foreign = SocketDecoy::holding(&theirs.path().join("hangar.sock"));
        std::fs::write(&pid_path, format!("{}\n", foreign.pid())).unwrap();
        assert_eq!(
            stop_decision(&pid_path, &socket),
            StopDecision::Unproven(foreign.pid())
        );
    }

    /// The other half: the guard must not make `stop` useless. The process holding
    /// THIS home's socket is signalled, and the dead/absent cases still resolve.
    #[test]
    fn stop_signals_the_pid_holding_this_homes_socket() {
        let home = tempfile::tempdir().expect("tempdir");
        let socket = home.path().join("hangar.sock");
        let pid_path = home.path().join("hangar").join("daemon.pid");
        std::fs::create_dir_all(pid_path.parent().unwrap()).unwrap();

        assert_eq!(stop_decision(&pid_path, &socket), StopDecision::NotRecorded);

        let daemon = SocketDecoy::holding(&socket);
        std::fs::write(&pid_path, format!("{}\n", daemon.pid())).unwrap();
        assert_eq!(
            stop_decision(&pid_path, &socket),
            StopDecision::Signal(OwnedPid(daemon.pid()))
        );

        // A recorded pid that is not running stays a stale-file cleanup, not a
        // signal, the ownership proof never even runs.
        let mut gone = std::process::Command::new("true").spawn().expect("spawn true");
        let dead_pid = gone.id();
        gone.wait().expect("wait true");
        std::fs::write(&pid_path, format!("{dead_pid}\n")).unwrap();
        assert_eq!(
            stop_decision(&pid_path, &socket),
            StopDecision::Stale(dead_pid)
        );
    }

    /// A process we do not own still counts as running.
    ///
    /// `kill(pid, 0)` answers `EPERM` for another user's process; reading that as
    /// "not running" (the prior behaviour) would let the CLI treat a daemon
    /// owned by another account as absent and spawn a duplicate against the same
    /// home. PID 1 exists on every unix and is owned by root.
    #[test]
    fn a_process_we_may_not_signal_still_counts_as_running() {
        assert!(pid_is_running(1), "pid 1 must read as running");
    }

    /// The ownership lock outranks the pid file, and "owner" means PROVED.
    ///
    /// The proof is the same one `stop` demands: the pid must hold this home's
    /// socket. An earlier version of this test seeded its OWN pid, which is why
    /// it could not catch the hole review found, the CLI credited any process
    /// running `ainb` as the home's daemon, so a recycled pid landing on the
    /// user's TUI wedged the home with zero daemons.
    #[test]
    fn the_ownership_lock_outranks_the_pid_file() {
        let home = tempfile::tempdir().expect("tmpdir");
        let hangar = home.path().join("hangar");
        std::fs::create_dir_all(&hangar).expect("mkdir");
        let socket = home.path().join("hangar.sock");
        let daemon = SocketDecoy::holding(&socket);

        // Lock names the socket holder, pid file names a dead process.
        std::fs::write(hangar.join("daemon.pid"), "424242\n").expect("write pid file");
        std::fs::write(hangar.join("daemon.lock"), daemon.pid().to_string()).expect("write lock");
        assert_eq!(running_daemon_pid_in(home.path()), Some(daemon.pid()));

        // Fallback: no lock at all (a daemon from before the lock shipped) still
        // resolves through the pid file, so an upgrade does not make a running
        // daemon invisible.
        std::fs::remove_file(hangar.join("daemon.lock")).expect("drop lock");
        std::fs::write(hangar.join("daemon.pid"), format!("{}\n", daemon.pid()))
            .expect("rewrite pid");
        assert_eq!(running_daemon_pid_in(home.path()), Some(daemon.pid()));

        // Neither file names a live process -> nobody owns the home.
        std::fs::write(hangar.join("daemon.pid"), "424242\n").expect("rewrite pid");
        assert_eq!(running_daemon_pid_in(home.path()), None);
    }

    /// A live process that is merely OURS is not this home's daemon.
    ///
    /// The regression under test: `ainb` and a self-exec'd daemon share an
    /// executable (`ainb hangar daemon run`), so an identity check written in
    /// terms of "runs our binary" credits the TUI. A recycled pid in the lock
    /// then reads as a running daemon and the autostart declines forever.
    #[test]
    fn our_own_process_is_not_this_homes_daemon() {
        let home = tempfile::tempdir().expect("tmpdir");
        let hangar = home.path().join("hangar");
        std::fs::create_dir_all(&hangar).expect("mkdir");

        // This test binary is live, is "an ainb process", and holds no socket.
        std::fs::write(hangar.join("daemon.lock"), std::process::id().to_string())
            .expect("write lock");
        assert_eq!(
            running_daemon_pid_in(home.path()),
            None,
            "a live non-daemon in the lock must not be read as the home's owner"
        );
    }

    /// A self-exec'd cargo TEST binary must not be credited as this home's daemon.
    ///
    /// The incident: a test run left `.../target/debug/deps/ainb-<hash> hangar
    /// daemon run` detached against the user's REAL home and writing its
    /// `daemon.pid`. It matched the argv fallback on shape, so
    /// `ensure_hangar_daemon` read the home as already served and started
    /// nothing, while the socket it never bound sat orphaned. Every Codex
    /// resume then failed with ECONNREFUSED, surfaced to the user as "Codex
    /// remote control unavailable".
    ///
    /// Asserted on `argv_credits_a_daemon` rather than end to end: this test
    /// binary's own argv does not contain the `hangar daemon run` token window,
    /// so seeding our pid would pass with or without the guard.
    #[test]
    fn a_cargo_test_binary_is_never_credited_as_this_homes_daemon() {
        let test_binary = "/w/ainb-tui/target/debug/deps/ainb-39a2b880848dbfd5 hangar daemon run";

        // Shape recognition alone still says "daemon". That is the trap.
        assert!(
            ainb_hangar_daemon::single_instance::is_hangar_daemon_args(test_binary),
            "argv shape matches, which is why the extra guard exists"
        );
        assert!(
            !argv_credits_a_daemon(test_binary),
            "a target/**/deps/ binary must never be credited as the home's daemon"
        );

        // The shapes that must keep counting, or autostart would spawn a second
        // daemon on top of a live one.
        for live in [
            "/opt/homebrew/bin/ainb hangar daemon run",
            "/w/ainb-tui/target/debug/ainb hangar daemon run",
            "/opt/homebrew/bin/ainb-hangar-daemon",
        ] {
            assert!(argv_credits_a_daemon(live), "must still credit: {live}");
        }

        // And the ordinary CLI verbs stay uncredited.
        assert!(!argv_credits_a_daemon(
            "/opt/homebrew/bin/ainb hangar daemon status"
        ));
        assert!(!argv_credits_a_daemon(""));
    }

    #[test]
    fn daemon_upgrade_required_only_for_a_strictly_newer_release() {
        assert!(daemon_upgrade_required(Some("1.20.0"), "1.20.2"));
        assert!(!daemon_upgrade_required(Some("1.20.2"), "1.20.2"));
        assert!(!daemon_upgrade_required(Some("1.20.3"), "1.20.2"));
        assert!(!daemon_upgrade_required(Some("debug"), "1.20.2"));
        assert!(!daemon_upgrade_required(None, "1.20.2"));
    }

    #[test]
    fn homebrew_daemon_version_accepts_only_a_cellar_release_path() {
        assert_eq!(
            homebrew_daemon_version(
                "/opt/homebrew/Cellar/ainb/1.20.0/libexec/ainb hangar daemon run"
            ),
            Some("1.20.0".to_string())
        );
        assert_eq!(
            homebrew_daemon_version("/opt/homebrew/bin/ainb hangar daemon run"),
            None
        );
        assert_eq!(
            homebrew_daemon_version(
                "/opt/homebrew/Cellar/ainb/debug/libexec/ainb hangar daemon run"
            ),
            None
        );
    }

    /// The freshness predicate: a sibling at least as new as `ainb` is fresh; an
    /// older sibling is stale; an unreadable mtime degrades to trusting it.
    #[test]
    fn sibling_daemon_freshness_prefers_a_sibling_no_older_than_ainb() {
        use std::time::{Duration, SystemTime};
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        let newer = base + Duration::from_secs(3600);
        // Sibling newer or equal → fresh.
        assert!(sibling_daemon_is_fresh(Some(newer), Some(base)));
        assert!(sibling_daemon_is_fresh(Some(base), Some(base)));
        // Sibling older than ainb → stale.
        assert!(!sibling_daemon_is_fresh(Some(base), Some(newer)));
        // Either mtime unknown → degrade to trusting the sibling.
        assert!(sibling_daemon_is_fresh(None, Some(base)));
        assert!(sibling_daemon_is_fresh(Some(base), None));
    }

    /// Set a file's mtime to an absolute instant (Rust 1.75+ `set_modified`).
    fn set_mtime(path: &std::path::Path, when: std::time::SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    /// The build-skew guard: with a sibling `ainb-hangar-daemon` OLDER than the
    /// spawning `ainb` present, `start`'s launch resolution must NOT run it, it
    /// falls back to self-exec (`ainb hangar daemon run`, the fresh embedded
    /// daemon) instead of the stale sibling. This is the regression that let
    /// pre-#441 daemon code keep serving after a plain `cargo build`.
    #[test]
    fn resolve_daemon_launch_skips_a_sibling_older_than_ainb() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ainb");
        let sibling = dir.path().join("ainb-hangar-daemon");
        std::fs::write(&exe, b"ainb").unwrap();
        std::fs::write(&sibling, b"daemon").unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        // Sibling built BEFORE ainb, the stale-build signature.
        set_mtime(&sibling, base);
        set_mtime(&exe, base + Duration::from_secs(3600));

        let (bin, args) = resolve_daemon_launch_for(Some(&exe));
        assert_eq!(
            args,
            vec!["hangar", "daemon", "run"],
            "a sibling older than ainb must not be launched"
        );
        assert_eq!(bin, exe, "self-exec must run this very ainb binary");
    }

    /// The companion: a sibling at least as new as `ainb` IS launched directly
    /// (empty argv), so the freshness guard doesn't defeat the standalone binary
    /// on a clean co-build.
    #[test]
    fn resolve_daemon_launch_uses_a_sibling_no_older_than_ainb() {
        use std::time::{Duration, SystemTime};
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("ainb");
        let sibling = dir.path().join("ainb-hangar-daemon");
        std::fs::write(&exe, b"ainb").unwrap();
        std::fs::write(&sibling, b"daemon").unwrap();
        let base = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        set_mtime(&exe, base);
        set_mtime(&sibling, base + Duration::from_secs(3600));

        let (bin, args) = resolve_daemon_launch_for(Some(&exe));
        assert!(
            args.is_empty(),
            "a fresh sibling should be launched directly"
        );
        assert_eq!(bin, sibling);
    }
}
