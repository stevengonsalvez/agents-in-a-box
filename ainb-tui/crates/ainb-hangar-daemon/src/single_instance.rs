//! One daemon per hangar home, enforced before anything else exists.
//!
//! Everything [`crate::boot`] builds — the SQLite store, the claim loop, the
//! sweepers, the control socket — assumes sole ownership of one hangar home. Two
//! daemons on one home is not a degraded mode: `rpc::bind` unlinks the incumbent's
//! socket (leaving it `accept()`ing an inode no client can reach) while both claim
//! loops and both sweepers race the same database.
//!
//! The guard that used to stand in for this was a pidfile written at the END of
//! boot and read by a different process BEFORE spawning — a check-then-act with a
//! window spanning a dozen subsystem initialisations, and a `std::fs::write` that
//! cannot fail, so a losing daemon never learned it had lost. This module replaces
//! it with [`crate::beads_adapter::lock::BdLock`], the crate's existing hardened
//! cross-process mutex, taken as the first statement of `boot`.
//!
//! # Why the holder check is not just `kill(pid, 0)`
//!
//! `<hangar_home>/hangar/daemon.lock` outlives reboots: a SIGKILLed (or
//! power-cut) daemon leaves the file naming its pid, and after the next boot that
//! pid very likely belongs to some unrelated process. A liveness-only check would
//! read that as "a daemon owns this home" forever and the daemon would never start
//! again. So the predicate also asks the process table what that pid is running.
//!
//! That check FAILS SAFE in every direction: a `ps` that cannot answer, an argv
//! shape it does not recognise, and a holder running our own executable all
//! RESPECT the holder. Only positive evidence of a different program justifies a
//! steal. The asymmetry is deliberate — declining to boot is loud and
//! recoverable (the log names the file to remove), whereas stealing a live
//! daemon's lock silently recreates the double-hold this module exists to
//! prevent.
//!
//! The kernel offers a primitive with none of this bookkeeping: an `flock` /
//! `F_SETLK` held for the process lifetime is released by the kernel on exit,
//! crash and reboot alike, which removes the recycled-pid problem at the root.
//! This module reuses [`BdLock`] instead because that carries the crate's
//! existing double-hold race tests; the lock-file primitive is the better
//! long-term shape and the identity layer is what it costs to keep the pidfile.

use std::path::{Path, PathBuf};

use crate::beads_adapter::lock::{BdLock, BdLockGuard, LockHeld, pid_alive};

/// Resolve the ownership lock for a hangar home:
/// `<hangar_home>/hangar/daemon.lock`.
///
/// Beside `daemon.pid` rather than replacing it: the pid file remains the
/// discovery/status artifact, while this file is the thing that actually decides
/// who runs.
#[must_use]
pub fn lock_path_in(dir: &Path) -> PathBuf {
    dir.join("hangar").join("daemon.lock")
}

/// The outcome of asking for a hangar home.
#[derive(Debug)]
pub enum Ownership {
    /// This process owns the home. The guard MUST be held for the daemon's whole
    /// lifetime — dropping it early republishes the home as free.
    Acquired(BdLockGuard),
    /// A live hangar daemon already owns the home, named by pid.
    HeldBy(i32),
    /// The lock churned for the whole window without ever being publishable and
    /// without a live holder to name. Another daemon is mid-acquisition; the
    /// correct response is still to decline.
    Contended,
}

/// Claim `dir` for this process, or report who owns it.
///
/// # Errors
///
/// The lock's parent directory could not be created. That is the same
/// `<home>/hangar` directory the store and pid file need, so it is fatal rather
/// than a reason to boot unguarded.
pub fn acquire(dir: &Path) -> std::io::Result<Ownership> {
    match acquire_with(dir, holder_is_live_daemon)? {
        // `Contended` means the path churned for a whole window without ever
        // naming a live holder. That is NOT evidence anyone won it, so a single
        // sample must not be enough to decline: symmetric contenders could all
        // stand down and leave the home with no daemon.
        //
        // Sample twice. A second fail-fast pass costs one window and turns the
        // usual case (someone did win in the meantime) into an honest `HeldBy`.
        //
        // It is deliberately NOT a longer blocking acquire. That was tried and
        // reverted: escalating turned a rare "nobody boots" into an
        // intermittent DOUBLE-hold under the stale-lock stress, which is the
        // failure this whole module exists to prevent and strictly the worse of
        // the two. Two daemons corrupt a home; a home that briefly starts none
        // is fixed by the next invocation.
        Ownership::Contended => acquire_with(dir, holder_is_live_daemon),
        won_or_held => Ok(won_or_held),
    }
}

/// [`acquire`] with an injectable holder predicate, so the decision table is
/// testable without a real daemon in the process list.
fn acquire_with<F>(dir: &Path, holder_is_live: F) -> std::io::Result<Ownership>
where
    F: Fn(i32) -> bool,
{
    match BdLock::new(lock_path_in(dir)).try_acquire_with(holder_is_live) {
        Ok(guard) => Ok(Ownership::Acquired(guard)),
        Err(LockHeld::By(pid)) => Ok(Ownership::HeldBy(pid)),
        Err(LockHeld::Contended) => Ok(Ownership::Contended),
        Err(LockHeld::Io(e)) => Err(e),
    }
}

/// Is `pid` a live process this daemon must respect as the home's owner?
///
/// Three ways to answer yes, and only ONE way to answer no. That asymmetry is
/// the contract [`BdLock::try_acquire_with`] demands: a false negative steals a
/// LIVE holder's lock and puts two daemons on one home, so "not a daemon" is
/// only ever concluded from positive evidence that the holder is a different
/// program.
///
/// 1. `ps` cannot answer → respect (the original fail-safe).
/// 2. The argv is a recognised daemon shape → respect.
/// 3. The argv is unrecognised but is the SAME COMMAND LINE we were invoked
///    with → respect. This covers every daemon whose argv
///    [`is_hangar_daemon_args`] cannot know about: an `AINB_HANGAR_DAEMON_BIN`
///    override pointing at a wrapper or a dev binary, an install path
///    containing a space (`ps` renders argv whitespace-separated, so
///    `/Volumes/My Passport/bin/ainb-hangar-daemon` has a first token of
///    `/Volumes/My`), and an in-process `boot()` inside a cargo test binary,
///    whose argv is `target/debug/deps/<hash>`.
///
/// Without (3) a second daemon judged such an incumbent a stranger, stole its
/// lock, and `rpc::bind` then unlinked its socket — the exact double-daemon
/// incident this module exists to prevent, reintroduced by the guard meant to
/// stop it.
#[must_use]
pub fn holder_is_live_daemon(pid: i32) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    let Some(args) = process_argv(pid) else {
        return true;
    };
    is_hangar_daemon_args(&args) || shares_our_command_line(&args)
}

/// Is this argv the SAME INVOCATION as ours — the whole command line, not just
/// the executable?
///
/// Comparing executables alone was a hole: on a Homebrew layout there is no
/// sidecar binary, so the daemon self-execs as `ainb hangar daemon run` and its
/// `current_exe` is plain `ainb` — the same executable the TUI runs. A recycled
/// pid landing on the user's TUI would then be respected as this home's owner,
/// and the daemon would decline to boot for as long as that TUI lived: zero
/// daemons, silently, which is the failure this module exists to prevent turned
/// inside out.
///
/// The full command line separates them (`…/ainb` vs `…/ainb hangar daemon
/// run`) while still covering everything [`is_hangar_daemon_args`] cannot know
/// about — an `AINB_HANGAR_DAEMON_BIN` wrapper, an install path containing a
/// space, an in-process `boot()` in a test binary — because two processes of
/// that kind share one command line.
///
/// An unresolvable argv answers `true` (respect the holder), keeping every
/// unknown on the fail-safe side.
fn shares_our_command_line(args: &str) -> bool {
    our_command_line().is_none_or(|ours| args == ours)
}

/// This process's own argv, rendered the way `ps -o args=` renders it.
fn our_command_line() -> Option<String> {
    let exe = std::env::current_exe().ok()?.to_string_lossy().into_owned();
    let rest: Vec<String> = std::env::args().skip(1).collect();
    let line = if rest.is_empty() {
        exe
    } else {
        format!("{exe} {}", rest.join(" "))
    };
    (!line.trim().is_empty()).then_some(line)
}

/// How long to wait for `ps` before treating the process table as unreadable.
///
/// `holder_is_live_daemon` runs as part of the FIRST statement of `boot`, so an
/// unbounded wait here would hang startup on a wedged `ps` (a stalled disk, a
/// frozen cgroup) — defeating the fail-fast the whole guard is built around. A
/// timeout degrades to the `None` branch, which respects the holder.
const PS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Read one `ps -o` field for `pid`, with a bounded wait.
fn process_ps_field(pid: i32, field: &str) -> Option<String> {
    let field = format!("{field}=");
    let mut child = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", &field])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + PS_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                // Reap our own child by its exact handle so a wedged `ps` is not
                // left behind, then answer "unreadable".
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(_) => return None,
        }
    };
    if !status.success() {
        return None;
    }
    let mut stdout = child.stdout.take()?;
    let mut args = String::new();
    std::io::Read::read_to_string(&mut stdout, &mut args).ok()?;
    let args = args.trim();
    (!args.is_empty()).then(|| args.to_string())
}

/// The full argv of `pid` as one line, or `None` when `ps` cannot answer in
/// time.
///
/// Exported so the CLI can apply the same identity rule to the same lock file —
/// the two halves disagreeing about who owns a home is how a recycled pid ends
/// up being reported as a running daemon, and `SIGTERM`ed by `stop`.
#[must_use]
pub fn process_argv(pid: i32) -> Option<String> {
    process_ps_field(pid, "args")
}

/// Executable path of `pid`, without its command-line arguments.
///
/// This is the stable, compact identity rendered in the Daemons overlay.
///
/// `/proc` is consulted before `ps` because the two platforms mean different
/// things by `comm`. On macOS `ps -o comm=` prints the executable PATH, which is
/// what the overlay wants. On Linux it prints the kernel's process NAME, capped
/// at `TASK_COMM_LEN` — 16 bytes including the NUL, so 15 characters. Our own
/// binary is longer than that, and the overlay rendered the truncation:
///
/// ```text
/// ps -o comm=      ->  ainb_hangar_dae
/// /proc/<pid>/exe  ->  /home/runner/work/.../ainb_hangar_daemon-<hash>
/// ```
///
/// `read_link` fails on macOS (no `/proc`) and for a pid owned by another user
/// without `CAP_SYS_PTRACE`, both of which fall through to the `ps` path that
/// has always served them.
#[must_use]
pub fn process_binary(pid: i32) -> Option<String> {
    if let Ok(exe) = std::fs::read_link(format!("/proc/{pid}/exe")) {
        return Some(exe.to_string_lossy().into_owned());
    }
    process_ps_field(pid, "comm")
}

/// How often the watchdog re-checks that this daemon still owns its home.
///
/// Overridable with `AINB_HANGAR_OWNERSHIP_WATCH_MS` so a tripwire can drive
/// several ticks inside a test budget, mirroring `HANGAR_DAEMON_POLL_MS`.
fn watchdog_interval() -> std::time::Duration {
    interval_from(std::env::var("AINB_HANGAR_OWNERSHIP_WATCH_MS").ok())
}

/// Turn a raw `AINB_HANGAR_OWNERSHIP_WATCH_MS` value into a tick width.
///
/// Split from the env read so the knob's rules are provable without writing to
/// the process environment. That environment is shared by every test in a
/// binary, and a value set around an `await` is read by whatever else happens to
/// run beside it.
fn interval_from(raw: Option<String>) -> std::time::Duration {
    /// The tick is the WIDTH of the window in which two daemons can be live on
    /// one home: the newcomer unlinks the incumbent's socket the moment it
    /// binds, while the incumbent keeps claiming and sweeping until its next
    /// sample. 5s bounds that, at the cost of one file read per tick.
    const DEFAULT: std::time::Duration = std::time::Duration::from_secs(5);

    raw.and_then(|raw| raw.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(DEFAULT, std::time::Duration::from_millis)
}

/// What one watchdog sample saw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Held {
    /// The lock still names us. Nothing to do.
    Ours,
    /// The lock is absent or unreadable. NOT proof we lost it — a steal may be
    /// mid-flight, and a daemon that exited on a transient read failure would be
    /// a worse bug than the one this watches for.
    Unknown,
    /// Another pid holds the lock and is a live daemon: it owns the home now.
    Lost(i32),
    /// Another pid is recorded but is not a live daemon — debris, not an owner.
    Stale(i32),
}

/// Classify the lock file's current contents against our own pid.
///
/// Pure but for the `holder_is_live` probe, so every branch is testable.
fn sample<F>(lock: &Path, mine: i32, holder_is_live: F) -> Held
where
    F: Fn(i32) -> bool,
{
    let Some(holder) = std::fs::read_to_string(lock)
        .ok()
        .and_then(|text| text.trim().parse::<i32>().ok())
    else {
        return Held::Unknown;
    };
    if holder == mine {
        Held::Ours
    } else if holder_is_live(holder) {
        Held::Lost(holder)
    } else {
        Held::Stale(holder)
    }
}

/// How many CONSECUTIVE [`Held::Unknown`] samples the watchdog tolerates before
/// it does anything about them.
///
/// Three ticks of agreement, 15s at the default interval. One unreadable sample
/// is what a steal mid-flight, a snapshotting filesystem or a momentarily busy
/// disk looks like, and standing a HEALTHY daemon down on one of those would be
/// a worse bug than the orphan this escalation exists to stop. Three in a row,
/// with the run reset by any readable sample, is evidence rather than noise.
const UNKNOWN_ESCALATE_AFTER: u32 = 3;

/// How many CONSECUTIVE failed republishes the watchdog tolerates before it
/// reads them as a home that is no longer there.
///
/// A failed write is an ESCALATION STEP, not a verdict. `republish` returns
/// [`Republish::Failed`] for a deleted home, but equally for a full disk, an
/// I/O error, an `EPERM` while a directory is momentarily unwritable, or an
/// `ENFILE` under load. Standing a healthy daemon down on the first of those
/// contradicts the rule the [`Held::Unknown`] ladder above is built on: one
/// transient failure must never stand a healthy daemon down.
///
/// So a failure keeps the daemon serving and leaves the miss run intact, which
/// makes the NEXT tick retry the write. Only a full run of consecutive
/// failures, with the counter reset by any republish that lands or finds the
/// path occupied and by any readable sample, is evidence rather than noise.
///
/// Separate from [`UNKNOWN_ESCALATE_AFTER`] because the two count different
/// things (samples that could not be READ versus writes that could not LAND),
/// and a future tuning of one must not silently move the other.
const REPUBLISH_ESCALATE_AFTER: u32 = 3;

/// Why the ownership watchdog stopped watching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Another live daemon owns this home now, named by pid.
    Lost(i32),
    /// The lock could not be restored, because the home itself is gone: deleted
    /// underneath a daemon that was serving it, which is what an ephemeral
    /// `$HOME` does the moment its harness exits.
    ///
    /// A distinct outcome rather than a sentinel pid: the two say opposite
    /// things about the sessions this daemon supervises. A successor adopts
    /// them; a deleted home leaves nobody to.
    HomeGone,
}

/// What happened when the watchdog tried to put its own pid back on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Republish {
    /// The lock names us again.
    Published,
    /// Some other file already occupies the path, so we published nothing.
    Occupied,
    /// The pid could not be written into the home at all.
    Failed,
}

/// Put `pid` back into `lock`, but only while nothing else occupies the path.
///
/// Publication is the same shape as [`BdLock`]'s: the pid is written into a
/// private temp file in the same directory and then `hard_link`ed into place.
/// The link is atomic and answers `EEXIST` while ANY file is there, so a
/// successor that published between our sample and this call is never
/// clobbered. It keeps the home, and our next sample reads [`Held::Lost`] and
/// stands us down. A blind overwrite here would put two daemons on one home,
/// which is the failure this module exists to prevent.
///
/// The parent directory is deliberately NOT created. A `create_dir_all` would
/// resurrect a deleted home one empty directory at a time and the watchdog
/// would never learn the home was gone, which is the only question this call is
/// asked to answer.
fn republish(lock: &Path, pid: i32) -> Republish {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let Some(dir) = lock.parent() else {
        return Republish::Failed;
    };
    let tmp = dir.join(format!(".daemon.lock.republish.{pid}"));
    // `create_new`, never create+truncate: the staging path is predictable, and
    // a symlink planted there by another local user would otherwise be followed
    // and its target truncated by a daemon that may be more privileged. A
    // leftover from a killed republish fails the create, which reads as one
    // failed attempt. Unlink first so a leftover from a killed republish cannot
    // wedge every future attempt: removing a symlink removes the link, never
    // its target, so this is safe against the same planted link.
    let _ = std::fs::remove_file(&tmp);
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut file| file.write_all(pid.to_string().as_bytes()));
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Republish::Failed;
    }
    let linked = std::fs::hard_link(&tmp, lock);
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => Republish::Published,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Republish::Occupied,
        Err(_) => Republish::Failed,
    }
}

/// What the watchdog should do after folding in one sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Keep serving.
    Serve,
    /// The run of misses is long enough to act on: try to put our pid back.
    Republish,
    /// Stop serving this home.
    StandDown(Outcome),
}

/// The run of consecutive [`Held::Unknown`] samples behind the current decision.
///
/// A struct rather than a bare counter, because "we already tried to repair this
/// outage" is the second half of the escalation: the first full run of misses
/// buys a republish, and only a SECOND full run after that republish reported
/// success is read as a home that is no longer there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Misses {
    /// How many samples in a row have been [`Held::Unknown`].
    run: u32,
    /// Did a republish during this outage report that it published?
    republished: bool,
    /// How many republishes in a row have failed to write at all.
    ///
    /// The second escalation ladder, counted separately from `run` because a
    /// failed write is not a failed read. Reset by any republish that lands or
    /// finds the path occupied, and by any readable sample (which resets the
    /// whole struct).
    failed_republishes: u32,
}

impl Misses {
    /// Fold one sample in and say what to do about it.
    ///
    /// Pure, so the whole escalation is testable without a clock, a filesystem
    /// or a second daemon.
    fn observe(&mut self, sample: Held) -> Step {
        match sample {
            // Any readable answer ends the outage, a stale holder included: the
            // file was there to read, so the home is too.
            Held::Ours | Held::Stale(_) => {
                *self = Self::default();
                Step::Serve
            }
            Held::Lost(pid) => {
                *self = Self::default();
                Step::StandDown(Outcome::Lost(pid))
            }
            Held::Unknown => {
                self.run += 1;
                if self.run < UNKNOWN_ESCALATE_AFTER {
                    Step::Serve
                } else if self.republished {
                    // A whole second run of misses after a republish that
                    // reported success: our pid is not staying on disk, so the
                    // directory we wrote it into is not really there any more.
                    Step::StandDown(Outcome::HomeGone)
                } else {
                    Step::Republish
                }
            }
        }
    }

    /// Fold in the result of the republish [`Step::Republish`] asked for,
    /// answering with the outcome to stand down on, if any.
    const fn after_republish(&mut self, outcome: Republish) -> Option<Outcome> {
        match outcome {
            Republish::Published => {
                self.run = 0;
                self.republished = true;
                self.failed_republishes = 0;
                None
            }
            // Something else holds the path. That is not a missing home: the
            // next sample names the holder and the ordinary `Lost` / `Stale`
            // arms take it from there. Debris that stays unparseable keeps
            // landing here, one harmless write attempt per run of misses, which
            // is the right price for never standing down a daemon whose home is
            // demonstrably still on disk.
            Republish::Occupied => {
                self.run = 0;
                self.failed_republishes = 0;
                None
            }
            // We could not write into the home's own directory. That is the
            // shape a deleted home takes, but it is ALSO the shape of a full
            // disk, an I/O error and a directory that is momentarily
            // unwritable, so it escalates rather than deciding.
            //
            // The miss run is deliberately left alone: it is already at or past
            // `UNKNOWN_ESCALATE_AFTER`, so the next [`Held::Unknown`] sample
            // asks for another republish and the retries land one tick apart.
            // Only a full run of them stands the daemon down.
            Republish::Failed => {
                self.failed_republishes += 1;
                if self.failed_republishes >= REPUBLISH_ESCALATE_AFTER {
                    Some(Outcome::HomeGone)
                } else {
                    None
                }
            }
        }
    }
}

/// Watch that this daemon still owns `dir`, resolving with the reason it stopped
/// owning it.
///
/// The layer that does not depend on any exit path running. A daemon can only
/// lose a lock it holds by something outside its control — an operator deleting
/// the file, a predicate misfire, a home restored from a backup — and two
/// daemons on one home is the failure this whole module exists to prevent, so
/// the right response is to stand down rather than race.
///
/// [`Held::Unknown`] is the sample that used to be logged forever. A home
/// deleted underneath a running daemon (an ephemeral `$HOME`, a cleaned-up
/// scratch directory) reads that way on EVERY tick, and the daemon served an
/// address nothing could reach until something killed it. It now escalates:
/// after [`UNKNOWN_ESCALATE_AFTER`] consecutive misses the daemon republishes
/// its own pid, and stands down with [`Outcome::HomeGone`] if the lock is still
/// missing a full run of misses after a republish that landed, or if
/// [`REPUBLISH_ESCALATE_AFTER`] consecutive republishes cannot be written at
/// all.
///
/// Both counters are reset by any readable sample, and the write counter also
/// by any republish that lands or finds the path occupied. So neither one
/// transient read failure nor one transient write failure can ever stand a
/// healthy daemon down — a full disk, an I/O blip or a momentarily unwritable
/// directory is logged and retried on the next tick.
///
/// Scoped to THIS home by construction: it reads one file, writes one file, and
/// signals no one. That is the difference between this and an argv-matching
/// reaper, which cannot tell which home a process serves and would kill daemons
/// belonging to others.
pub async fn watch_ownership(dir: &Path) -> Outcome {
    watch_ownership_with(dir, watchdog_interval()).await
}

/// [`watch_ownership`] with the tick width passed in rather than read from the
/// environment.
///
/// The env knob is resolved ONCE, by the caller above, so this loop owns no
/// process-global state. A test drives a whole escalation ladder in
/// milliseconds by argument; a test that instead set
/// `AINB_HANGAR_OWNERSHIP_WATCH_MS` around its `await` would be mutating an
/// environment its sibling tests read concurrently, which is a data race that
/// shows up as flakiness somewhere else entirely.
async fn watch_ownership_with(dir: &Path, interval: std::time::Duration) -> Outcome {
    let lock = lock_path_in(dir);
    let mine = i32::try_from(std::process::id()).unwrap_or(-1);
    let mut misses = Misses::default();
    loop {
        tokio::time::sleep(interval).await;
        let seen = sample(&lock, mine, holder_is_live_daemon);
        if let Held::Stale(pid) = seen {
            tracing::warn!(
                lock = %lock.display(),
                holder = pid,
                "hangar ownership lock names a stale pid; continuing to serve"
            );
        }
        match misses.observe(seen) {
            Step::Serve => {
                // Not proof of loss. If the home really is free, the next daemon
                // to take it turns this into `Lost` on a later tick — one tick of
                // overlap, versus an exit on a transient read.
                if seen == Held::Unknown {
                    tracing::warn!(
                        lock = %lock.display(),
                        misses = misses.run,
                        "hangar ownership lock is missing; continuing to serve"
                    );
                }
            }
            Step::Republish => {
                let outcome = republish(&lock, mine);
                let run = misses.run;
                let stand_down = misses.after_republish(outcome);
                if outcome == Republish::Failed {
                    // Every failed write is logged with its attempt count, so a
                    // sustained outage is visible in the log BEFORE the run is
                    // long enough to stand the daemon down.
                    tracing::warn!(
                        lock = %lock.display(),
                        misses = run,
                        attempt = misses.failed_republishes,
                        limit = REPUBLISH_ESCALATE_AFTER,
                        "hangar ownership lock could not be republished; \
                         continuing to serve and retrying on the next tick"
                    );
                } else {
                    tracing::warn!(
                        lock = %lock.display(),
                        misses = run,
                        outcome = ?outcome,
                        "hangar ownership lock has been missing for several ticks; \
                         republished this daemon's pid"
                    );
                }
                if let Some(stand_down) = stand_down {
                    tracing::error!(
                        lock = %lock.display(),
                        attempts = misses.failed_republishes,
                        "hangar home refused this daemon's ownership lock on every attempt \
                         in a row; standing down"
                    );
                    return stand_down;
                }
            }
            Step::StandDown(Outcome::HomeGone) => {
                tracing::error!(
                    lock = %lock.display(),
                    "hangar ownership lock stayed missing after a republish; standing down"
                );
                return Outcome::HomeGone;
            }
            Step::StandDown(outcome) => return outcome,
        }
    }
}

/// Does this argv line belong to a hangar daemon?
///
/// Two shapes launch one (`cli::hangar::resolve_daemon_launch`): the sidecar
/// binary `ainb-hangar-daemon`, and `ainb` self-exec'ing as
/// `ainb hangar daemon run` when the sidecar is absent or stale.
///
/// Matching is exact-token, not substring: `ainb hangar daemon status` and
/// `ainb hangar daemon stop` are ordinary CLI invocations that must never be
/// mistaken for a running daemon, and a bare `ainb` (the TUI) must not either.
#[must_use]
pub fn is_hangar_daemon_args(args: &str) -> bool {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    let sidecar = tokens
        .first()
        .map(Path::new)
        .and_then(Path::file_name)
        .is_some_and(|name| name == "ainb-hangar-daemon");
    sidecar || tokens.windows(3).any(|w| w == ["hangar", "daemon", "run"])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tick the loop-driving tests run at, reached only by ARGUMENT.
    ///
    /// Short enough that a whole escalation ladder fits in a test, and set
    /// without touching `AINB_HANGAR_OWNERSHIP_WATCH_MS`: that variable is
    /// process-global, so holding a value in it across an `await` races every
    /// sibling test in this binary that reads the environment.
    const TEST_TICK: std::time::Duration = std::time::Duration::from_millis(20);

    #[test]
    fn process_binary_reports_current_executable_path() {
        let pid = i32::try_from(std::process::id()).unwrap();
        let path = process_binary(pid).expect("ps should report this test binary");
        assert!(std::path::Path::new(&path).is_absolute(), "got {path}");
    }

    #[test]
    fn sidecar_and_self_exec_argv_are_recognised() {
        for args in [
            "/opt/homebrew/bin/ainb-hangar-daemon",
            "ainb-hangar-daemon",
            "target/debug/ainb-hangar-daemon --once",
            "/Users/x/.cargo/bin/ainb hangar daemon run",
            "ainb hangar daemon run",
        ] {
            assert!(is_hangar_daemon_args(args), "should match: {args}");
        }
    }

    /// The false positives that matter: a mismatch here either wedges every
    /// future boot (an ordinary CLI process mistaken for the owner) or, on the
    /// `stop --all` side, signals something that is not a daemon at all.
    #[test]
    fn cli_verbs_the_tui_and_strangers_are_not_daemons() {
        for args in [
            "/opt/homebrew/bin/ainb",
            "ainb",
            "ainb hangar daemon status",
            "ainb hangar daemon stop --all",
            "ainb hangar daemon restart",
            "ainb run --agent claude",
            "/bin/sleep 30",
            "grep ainb-hangar-daemon",
            "",
        ] {
            assert!(!is_hangar_daemon_args(args), "should not match: {args}");
        }
    }

    #[test]
    fn the_lock_lives_beside_the_pid_file() {
        let dir = Path::new("/tmp/hangar-home");
        assert_eq!(
            lock_path_in(dir),
            Path::new("/tmp/hangar-home/hangar/daemon.lock")
        );
    }

    #[test]
    fn a_free_home_is_acquired() {
        let dir = tempfile::tempdir().expect("tmpdir");
        match acquire(dir.path()).expect("acquire") {
            Ownership::Acquired(_) => {}
            other => panic!("expected a free home to be acquired, got {other:?}"),
        }
    }

    /// The decisive case: a second daemon must be told who owns the home rather
    /// than booting alongside it.
    #[test]
    fn a_home_owned_by_a_live_daemon_is_declined() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let held = acquire(dir.path()).expect("first acquire");
        assert!(matches!(held, Ownership::Acquired(_)));

        // Our own pid holds it, and the predicate is told we are a daemon.
        match acquire_with(dir.path(), |_| true).expect("second acquire") {
            Ownership::HeldBy(pid) => {
                assert_eq!(pid, i32::try_from(std::process::id()).unwrap());
            }
            other => panic!("expected the home to be declined, got {other:?}"),
        }
        drop(held);
    }

    /// A lock left by a `SIGKILL`ed daemon whose pid has since been recycled onto
    /// an unrelated live process must be reclaimable, or the daemon can never
    /// boot again after a reboot.
    #[test]
    fn a_lock_naming_a_live_non_daemon_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = lock_path_in(dir.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");

        // A genuinely foreign live process — the shape a recycled pid takes
        // after a reboot. Our OWN pid would not do: this test binary is the
        // executable a daemon would be running, so it is respected by design
        // (see `a_holder_running_our_own_binary_is_respected_whatever_its_argv`).
        // Killed by its exact pid on drop.
        let mut stranger = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn /bin/sleep");
        std::fs::write(&path, stranger.id().to_string()).expect("seed lock");

        let outcome = acquire(dir.path()).expect("acquire");
        let _ = stranger.kill();
        let _ = stranger.wait();

        match outcome {
            Ownership::Acquired(_) => {}
            other => panic!("expected a recycled-pid lock to be reclaimed, got {other:?}"),
        }
    }

    /// The fail-safe that stops the guard from recreating the incident.
    ///
    /// A daemon whose argv `is_hangar_daemon_args` cannot recognise — a wrapper
    /// via `AINB_HANGAR_DAEMON_BIN`, an install path with a space, an
    /// in-process `boot()` in a test binary — must still be respected, or a
    /// second daemon steals its lock and both end up serving one home.
    #[test]
    fn a_holder_sharing_our_command_line_is_respected_whatever_its_argv() {
        let ours = our_command_line().expect("our own command line");

        assert!(shares_our_command_line(&ours), "our exact invocation");
        assert!(
            !is_hangar_daemon_args(&ours),
            "this test binary's argv is exactly the unrecognised shape under test"
        );
        assert!(
            holder_is_live_daemon(i32::try_from(std::process::id()).unwrap()),
            "our own live process must never be judged a stranger"
        );
    }

    /// The hole this replaced an executable-only check to close.
    ///
    /// On a Homebrew layout the daemon self-execs as `ainb hangar daemon run`,
    /// so its executable IS the TUI's executable. Crediting a bare `ainb` as
    /// this home's daemon would let a recycled pid landing on the user's TUI
    /// wedge the home with zero daemons for as long as that TUI lives.
    #[test]
    fn our_executable_running_a_different_command_line_is_not_us() {
        let exe = std::env::current_exe().expect("current_exe");
        let exe = exe.to_string_lossy().into_owned();

        // Same binary, different invocation — e.g. the TUI next to a self-exec'd
        // daemon. Not us, and not a recognised daemon shape either.
        assert!(!shares_our_command_line(&exe) || std::env::args().len() == 1);
        assert!(!shares_our_command_line(&format!("{exe} tui")));
        assert!(!is_hangar_daemon_args(&exe));
    }

    /// The other half: a genuinely different program is not protected, or a
    /// recycled pid would wedge the home forever.
    #[test]
    fn a_different_executable_is_not_ours() {
        assert!(!shares_our_command_line("/bin/sleep 30"));
        assert!(!shares_our_command_line("/usr/bin/vim"));
    }

    /// The watchdog's whole decision table. The dangerous cell is `Unknown`:
    /// reading an absent or unreadable file as "we lost it" would let a
    /// transient failure shut down a healthy daemon.
    #[test]
    fn the_watchdog_stands_down_only_for_a_live_successor() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = dir.path().join("daemon.lock");
        let mine = 4242;

        assert_eq!(sample(&lock, mine, |_| true), Held::Unknown, "absent");

        std::fs::write(&lock, "not-a-pid").expect("write");
        assert_eq!(sample(&lock, mine, |_| true), Held::Unknown, "unreadable");

        std::fs::write(&lock, mine.to_string()).expect("write");
        assert_eq!(sample(&lock, mine, |_| true), Held::Ours);

        std::fs::write(&lock, "9001").expect("write");
        assert_eq!(sample(&lock, mine, |_| true), Held::Lost(9001));
        assert_eq!(sample(&lock, mine, |_| false), Held::Stale(9001));
    }

    /// The non-negotiable half of the escalation: a run of misses that is broken
    /// by ANY readable sample starts again from zero, so one transient read
    /// failure can never stand a healthy daemon down.
    #[test]
    fn a_broken_run_of_misses_never_escalates() {
        let mut misses = Misses::default();

        assert_eq!(misses.observe(Held::Unknown), Step::Serve, "first miss");
        assert_eq!(misses.observe(Held::Unknown), Step::Serve, "second miss");
        assert_eq!(misses.observe(Held::Ours), Step::Serve, "the lock is back");
        // The two misses before it must count for nothing now.
        assert_eq!(misses.observe(Held::Unknown), Step::Serve);
        assert_eq!(misses.observe(Held::Unknown), Step::Serve);
        assert_eq!(
            misses,
            Misses {
                run: 2,
                republished: false,
                failed_republishes: 0,
            }
        );

        // A stale holder is a READ that succeeded, so it resets the run too.
        assert_eq!(misses.observe(Held::Stale(9001)), Step::Serve);
        assert_eq!(misses, Misses::default());
    }

    /// The escalation itself: the Nth consecutive miss asks for a republish, and
    /// nothing before it does.
    #[test]
    fn a_full_run_of_misses_asks_for_a_republish() {
        let mut misses = Misses::default();
        for tick in 1..UNKNOWN_ESCALATE_AFTER {
            assert_eq!(misses.observe(Held::Unknown), Step::Serve, "miss {tick}");
        }
        assert_eq!(misses.observe(Held::Unknown), Step::Republish);
    }

    /// A live successor still wins immediately, whatever the miss counter says:
    /// the `Lost` arm is untouched by the escalation.
    #[test]
    fn a_live_successor_still_stands_us_down_at_once() {
        let mut misses = Misses::default();
        assert_eq!(misses.observe(Held::Unknown), Step::Serve);
        assert_eq!(
            misses.observe(Held::Lost(9001)),
            Step::StandDown(Outcome::Lost(9001))
        );
    }

    /// A republish that lands buys the home another run of misses, and only the
    /// SECOND full run stands the daemon down.
    #[test]
    fn only_a_second_run_of_misses_after_a_republish_stands_us_down() {
        let mut misses = Misses::default();
        for _ in 0..UNKNOWN_ESCALATE_AFTER {
            misses.observe(Held::Unknown);
        }
        assert_eq!(misses.after_republish(Republish::Published), None);

        for tick in 1..UNKNOWN_ESCALATE_AFTER {
            assert_eq!(misses.observe(Held::Unknown), Step::Serve, "miss {tick}");
        }
        assert_eq!(
            misses.observe(Held::Unknown),
            Step::StandDown(Outcome::HomeGone)
        );
    }

    /// The half of the write ladder that must never regress: ONE failed
    /// republish is an I/O outage, a full disk or a momentarily unwritable
    /// directory, and standing a healthy daemon down on it contradicts the rule
    /// the whole escalation is built on.
    ///
    /// Every attempt before the last answers `None`, and the miss run is left
    /// intact so the next tick retries the write rather than waiting out
    /// another full run of misses first.
    #[test]
    fn a_single_failed_republish_never_stands_us_down() {
        let mut misses = Misses::default();
        for _ in 0..UNKNOWN_ESCALATE_AFTER {
            misses.observe(Held::Unknown);
        }

        for attempt in 1..REPUBLISH_ESCALATE_AFTER {
            assert_eq!(
                misses.after_republish(Republish::Failed),
                None,
                "failed republish {attempt} of {REPUBLISH_ESCALATE_AFTER} stood us down"
            );
            assert_eq!(misses.failed_republishes, attempt);
            // The run stays past the threshold, so the very next miss retries
            // the write instead of restarting the read ladder.
            assert_eq!(
                misses.observe(Held::Unknown),
                Step::Republish,
                "a failed write must be retried on the next tick"
            );
        }
    }

    /// A write that lands, or finds the path occupied, ends the outage: the
    /// failure counter starts again from zero, so a run of failures has to be
    /// consecutive to mean anything.
    #[test]
    fn a_republish_that_lands_resets_the_failure_run() {
        for recovery in [Republish::Published, Republish::Occupied] {
            let mut misses = Misses::default();
            for _ in 0..UNKNOWN_ESCALATE_AFTER {
                misses.observe(Held::Unknown);
            }
            for _ in 1..REPUBLISH_ESCALATE_AFTER {
                assert_eq!(misses.after_republish(Republish::Failed), None);
                misses.observe(Held::Unknown);
            }

            assert_eq!(misses.after_republish(recovery), None, "{recovery:?}");
            assert_eq!(misses.failed_republishes, 0, "{recovery:?}");
        }
    }

    /// A readable sample resets the failure run too, since the whole struct is
    /// cleared: the home answered, so nothing before it counts.
    #[test]
    fn a_readable_sample_resets_the_failure_run() {
        let mut misses = Misses::default();
        for _ in 0..UNKNOWN_ESCALATE_AFTER {
            misses.observe(Held::Unknown);
        }
        assert_eq!(misses.after_republish(Republish::Failed), None);

        assert_eq!(misses.observe(Held::Ours), Step::Serve);
        assert_eq!(misses, Misses::default());
    }

    /// ...and the stand-down is still REACHABLE: a home that refuses every
    /// write in a row is gone, and the daemon must stop serving it.
    #[test]
    fn a_full_run_of_failed_republishes_is_a_missing_home() {
        let mut misses = Misses::default();
        for _ in 0..UNKNOWN_ESCALATE_AFTER {
            misses.observe(Held::Unknown);
        }
        for _ in 1..REPUBLISH_ESCALATE_AFTER {
            assert_eq!(misses.after_republish(Republish::Failed), None);
            misses.observe(Held::Unknown);
        }
        assert_eq!(
            misses.after_republish(Republish::Failed),
            Some(Outcome::HomeGone)
        );
    }

    /// The invariant the whole ladder rests on, stated directly: while the lock
    /// file is present and parses as a pid, NO number of ticks can produce
    /// [`Outcome::HomeGone`]. Every readable sample either keeps us serving or
    /// hands the home to a named live successor, and none of them ever reaches
    /// a republish.
    #[test]
    fn a_readable_lock_can_never_produce_a_missing_home() {
        for sample in [Held::Ours, Held::Stale(9001), Held::Lost(9001)] {
            let mut misses = Misses::default();
            for tick in 0..UNKNOWN_ESCALATE_AFTER * 4 {
                match misses.observe(sample) {
                    Step::Serve => {}
                    Step::StandDown(Outcome::Lost(pid)) => assert_eq!(pid, 9001),
                    other => panic!("tick {tick} on {sample:?} produced {other:?}"),
                }
            }
        }
    }

    /// An occupied path is a home that is very much still there, so it resets
    /// the run rather than standing anything down.
    #[test]
    fn an_occupied_lock_path_is_not_a_missing_home() {
        let mut misses = Misses::default();
        for _ in 0..UNKNOWN_ESCALATE_AFTER {
            misses.observe(Held::Unknown);
        }
        assert_eq!(misses.after_republish(Republish::Occupied), None);
        assert_eq!(misses, Misses::default());
    }

    /// The repair itself, against a real directory: our pid goes back on disk
    /// and the very next sample reads it as ours.
    #[test]
    fn a_republish_into_a_live_home_restores_our_pid() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = lock_path_in(dir.path());
        std::fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");

        assert_eq!(republish(&lock, 4242), Republish::Published);
        assert_eq!(std::fs::read_to_string(&lock).expect("read"), "4242");
        assert_eq!(sample(&lock, 4242, |_| true), Held::Ours);
    }

    /// The clobber guard: a successor that published while we were counting
    /// misses keeps the home. Overwriting it here would put two daemons on one
    /// home, which is the failure this whole module exists to prevent.
    #[test]
    fn a_republish_never_overwrites_another_holder() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = lock_path_in(dir.path());
        std::fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");
        std::fs::write(&lock, "9001").expect("seed lock");

        assert_eq!(republish(&lock, 4242), Republish::Occupied);
        assert_eq!(
            std::fs::read_to_string(&lock).expect("read"),
            "9001",
            "the incumbent's pid must survive"
        );
    }

    /// The deleted home, at the filesystem level: with no directory to write
    /// into there is nothing left to serve.
    #[test]
    fn a_republish_into_a_deleted_home_fails() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = lock_path_in(dir.path());
        std::fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");
        std::fs::remove_dir_all(dir.path()).expect("delete the home");

        assert_eq!(republish(&lock, 4242), Republish::Failed);
    }

    /// The production knob, without touching the process environment: a
    /// missing, unparseable or zero value falls back to the default rather than
    /// spinning the watchdog at no interval at all.
    #[test]
    fn the_watch_interval_knob_falls_back_to_the_default() {
        let default = interval_from(None);
        assert!(
            default >= std::time::Duration::from_secs(1),
            "the default must be a real production tick, got {default:?}"
        );
        assert_eq!(
            interval_from(Some("20".to_string())),
            std::time::Duration::from_millis(20)
        );
        assert_eq!(interval_from(Some("0".to_string())), default, "zero");
        assert_eq!(interval_from(Some("nonsense".to_string())), default);
        assert_eq!(interval_from(Some(String::new())), default, "empty");
    }

    /// The whole loop: a home deleted underneath a serving daemon stands it
    /// down instead of logging forever.
    ///
    /// The clock is paused, so the ticks are counted rather than waited for and
    /// the elapsed VIRTUAL time is exact. That pins two things at once: the
    /// stand-down lands on the full run of misses and no sooner, and the loop
    /// ticks at the interval it was GIVEN — a loop that reached for the env knob
    /// instead would take 15 seconds to get here.
    #[tokio::test(start_paused = true)]
    async fn a_deleted_home_stands_the_watchdog_down() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = lock_path_in(dir.path());
        std::fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");
        std::fs::write(&lock, std::process::id().to_string()).expect("seed lock");
        std::fs::remove_dir_all(dir.path()).expect("delete the home");

        let started = tokio::time::Instant::now();
        let outcome = watch_ownership_with(dir.path(), TEST_TICK).await;

        assert_eq!(outcome, Outcome::HomeGone);
        assert_eq!(
            started.elapsed(),
            TEST_TICK * (UNKNOWN_ESCALATE_AFTER + REPUBLISH_ESCALATE_AFTER - 1),
            "the stand-down must cost a full run of misses plus a full run of \
             failed republishes, at the given tick"
        );
    }

    /// The write-side transient guarantee at the LOOP level: a home that cannot
    /// be written into for a tick or two, and then can be, keeps its daemon.
    ///
    /// The outage is modelled by the lock's parent directory being absent —
    /// `republish` deliberately never creates it — which is exactly what a
    /// momentarily unwritable directory, a full disk or an I/O error look like
    /// from here: [`Republish::Failed`]. The directory then reappears with our
    /// pid in it, the way an outage ends.
    ///
    /// Before the failure ladder existed this stood the daemon down on the very
    /// first failed write, at tick [`UNKNOWN_ESCALATE_AFTER`].
    #[tokio::test(start_paused = true)]
    async fn a_transient_write_outage_never_stands_the_watchdog_down() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = lock_path_in(dir.path());
        // The home exists; the `hangar` directory inside it does not, so every
        // sample is `Unknown` and every republish is `Failed`.
        assert!(!lock.exists());

        let restored = lock.clone();
        tokio::spawn(async move {
            // Half a tick after the FIRST republish attempt, so exactly one
            // write has failed when the home comes back.
            tokio::time::sleep(TEST_TICK * UNKNOWN_ESCALATE_AFTER + TEST_TICK / 2).await;
            std::fs::create_dir_all(restored.parent().expect("parent")).expect("mkdir");
            std::fs::write(&restored, std::process::id().to_string()).expect("restore lock");
        });

        // Long enough for several more full ladders after the outage ends.
        let budget = TEST_TICK * (UNKNOWN_ESCALATE_AFTER + REPUBLISH_ESCALATE_AFTER) * 4;
        let outcome =
            tokio::time::timeout(budget, watch_ownership_with(dir.path(), TEST_TICK)).await;

        assert!(
            outcome.is_err(),
            "one failed republish stood a healthy daemon down: {outcome:?}"
        );
    }

    /// ...and the other direction, which is the one that must never regress: a
    /// daemon whose lock is intact keeps serving through more ticks than the
    /// whole escalation ladder needs.
    #[tokio::test(start_paused = true)]
    async fn a_healthy_home_never_stands_the_watchdog_down() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let lock = lock_path_in(dir.path());
        std::fs::create_dir_all(lock.parent().expect("parent")).expect("mkdir");
        std::fs::write(&lock, std::process::id().to_string()).expect("seed lock");

        // Half a tick past two full ladders, so the budget can never expire on
        // the same virtual instant as a tick and hide one.
        let budget = TEST_TICK * (UNKNOWN_ESCALATE_AFTER * 2 + 2) + TEST_TICK / 2;
        let outcome =
            tokio::time::timeout(budget, watch_ownership_with(dir.path(), TEST_TICK)).await;

        assert!(outcome.is_err(), "a healthy daemon stood down: {outcome:?}");
    }

    /// The transient-miss guarantee at the LOOP level, not just in
    /// [`Misses::observe`]: samples that never become readable still cannot
    /// stand a daemon down while the home is demonstrably on disk.
    ///
    /// A directory at the lock path is unreadable as a pid, so every sample is
    /// [`Held::Unknown`], and unlinkable-into, so every republish is
    /// [`Republish::Occupied`] — which resets the run and keeps
    /// [`Outcome::HomeGone`] unreachable however many ladders pass.
    #[tokio::test(start_paused = true)]
    async fn misses_over_a_home_that_is_still_there_never_stand_us_down() {
        let dir = tempfile::tempdir().expect("tmpdir");
        std::fs::create_dir_all(lock_path_in(dir.path())).expect("mkdir");

        let budget = TEST_TICK * (UNKNOWN_ESCALATE_AFTER * 4) + TEST_TICK / 2;
        let outcome =
            tokio::time::timeout(budget, watch_ownership_with(dir.path(), TEST_TICK)).await;

        assert!(
            outcome.is_err(),
            "a home still on disk stood the daemon down: {outcome:?}"
        );
    }

    /// The fail-safe direction: an unreadable process table must NOT be read as
    /// "the holder is gone".
    #[test]
    fn an_unanswerable_process_table_respects_the_holder() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let held = acquire(dir.path()).expect("first acquire");
        // `holder_is_live_daemon` with `ps_args` returning None reduces to
        // `pid_alive`, which is what this predicate models.
        match acquire_with(dir.path(), pid_alive).expect("second acquire") {
            Ownership::HeldBy(_) => {}
            other => panic!("expected the holder to be respected, got {other:?}"),
        }
        drop(held);
    }
}
