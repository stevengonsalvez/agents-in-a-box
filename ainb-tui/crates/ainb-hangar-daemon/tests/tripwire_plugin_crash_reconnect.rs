//! e38.31 tripwire: plugin crash → host recovers, and host death → no orphan.
//!
//! F40 covers the *daemon* dropping under a live plugin (the host observes a
//! socket disconnect). This tripwire covers the two *inverse* directions that
//! F40 leaves untested, driving the REAL `ainb tui` on the Hangar screen in
//! tmux:
//!
//! ```text
//!  ┌──────────┐   spawns   ┌─────────────┐   dials   ┌────────────┐
//!  │ ainb tui │──────────▶│ hangar-tui   │─────────▶│ hangar     │
//!  │  (host)  │◀── render │ (plugin)     │  socket   │ daemon     │
//!  └──────────┘  pipe     └─────────────┘           └────────────┘
//!       │                       ▲                          ▲
//!       │ leg A: kill -9 plugin │ host stays alive,        │ seeded rows
//!       │                       │ re-renders / clean state │
//!       ▼                       │                          │
//!  leg B: kill -9 HOST ───▶ plugin self-exits (stdin EOF /
//!                           parent-death watcher) — no PPID=1 orphan
//! ```
//!
//! ## Leg A — PLUGIN CRASH → HOST RECOVERS
//!
//! Capture the EXACT hangar-tui plugin child pid (its `comm` is the staged
//! plugin's absolute path; its parent is the EXACT `ainb` host binary), then
//! `kill -9` only that pid. The host's `plugin_task::handle_exit` drains the
//! render ledger with a `RuntimeError`, records the failure, and — because the
//! hangar-tui plugin is `lifecycle.spawn = "lazy"` — re-spawns on the next
//! render kick (a nav keystroke marks the screen dirty). POSITIVE: the host
//! either re-spawns the plugin and re-renders the seeded rows OR shows a clean
//! Hangar chrome (not a crash). NEGATIVE (paired): the host process is still
//! alive (it did NOT die with its child) and the pane is NOT a dead terminal.
//!
//! ## Leg B — HOST DEATH → NO ORPHAN
//!
//! With the plugin alive, `kill -9` the HOST. The host never runs its `Drop`,
//! so the macOS process-group `kill(-pgid)` backstop never fires and
//! `kill_on_drop` is bypassed — without a reaping mechanism the plugin would
//! reparent to `launchd` (PPID = 1) and leak. POSITIVE: the captured plugin pid
//! is gone within a bounded poll. NEGATIVE (paired): no process with the EXACT
//! staged-plugin path survives as an orphan.
//!
//! ### What actually reaps the plugin here (verified, not assumed)
//!
//! Two mechanisms can reap a host-spawned plugin on host death, and this
//! end-to-end test exercises their **union** — the observable, user-visible
//! guarantee (no orphan), not either mechanism in isolation:
//!
//! 1. **stdin-EOF graceful exit** (`server::read_loop` breaks on `Ok(None)`).
//!    The host owns the write end of the plugin's stdin pipe; a `SIGKILL`'d host
//!    has its fd table reaped by the kernel, closing that write end, so the
//!    plugin reads EOF and exits. Measured: this fires within ~0.5 s on macOS
//!    even with the watcher disabled — so it dominates the host-spawned case.
//! 2. **parent-death watcher** (`parent_watch`, commit 8494ad0f, macOS) /
//!    **`PR_SET_PDEATHSIG`** (`process::install_leak_guard`, Linux). The
//!    backstop for the case where the host dies WITHOUT closing the plugin's
//!    stdin (e.g. the write end is held open by a third process). A host-spawned
//!    plugin driven through `ainb tui` cannot reach that case from a tmux test
//!    (the host always owns stdin), so the watcher in **isolation** is covered
//!    by [`ainb_plugin_sdk_rust::parent_watch`]'s own unit tests
//!    (`should_exit`) plus the stdin-still-open isolation test in that crate —
//!    NOT by this end-to-end tripwire.
//!
//! The bounded-vanish assertion holds identically on macOS and Linux, so leg B
//! is NOT cfg-skipped: the user-visible guarantee (no orphaned plugin after host
//! death) is the same on every Unix host and worth defending there.
//!
//! ## Harness reuse + HARD RULES
//!
//! Gating (`can_run_tripwire`/`skip`), seeding/spawn (`prepare_pipeline`), and
//! the tmux driver (`TuiSession`, `open_hangar_and_wait_ready`, `poll_capture`,
//! single-char nav, exact-name `kill-session`) are reused from
//! `tripwire_p4_common`. The crash-specific bits — discovering the EXACT plugin
//! child pid via `ps` (never a name/wildcard match), `kill -9` that exact pid
//! via `nix` — live here. SKIP-not-fail when tmux/binaries/plugin are missing.
//! `--test-threads=1` (the runner enforces it) so the killed processes never
//! race a sibling tripwire's tempdir.

#![allow(clippy::duration_suboptimal_units)] // `from_secs(N)` reads fine as a poll budget.

// Reuse the P4.9 harness for gating + the seed/spawn + tmux driver. `#[path]`
// (not `mod`) so Cargo does not compile the helper file as its own test binary.
#[path = "tripwire_p4_common.rs"]
mod common;

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use common::{
    READY_MARKER, TuiSession, ainb_bin, can_run_tripwire, prepare_pipeline, skip, staged_plugin,
};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

#[test]
fn plugin_crash_host_recovers_and_host_death_leaves_no_orphan() {
    if !can_run_tripwire() {
        skip("plugin crash / host reconnect + parent-death watcher tripwire");
        return;
    }

    let pipe = prepare_pipeline();
    let host_bin = ainb_bin().expect("gated by can_run_tripwire");
    let plugin_bin = staged_plugin().expect("gated by can_run_tripwire");

    let sess = TuiSession::spawn(&host_bin, pipe.home());
    let landing = sess
        .open_hangar_and_wait_ready()
        .unwrap_or_else(|| panic!("hangar issue list never rendered:\n{}", sess.capture()));
    assert!(
        landing.contains(READY_MARKER),
        "precondition: seeded rows must render before the crash:\n{landing}"
    );

    // ---- discover the EXACT plugin child pid + its host parent -------------
    // The plugin child's executable path is the staged plugin's absolute path;
    // its parent's executable path is the EXACT `ainb` host binary (both read
    // from `ps … args=`, portable to macOS + Linux — see `ps_rows`). Matching
    // BOTH exact paths uniquely identifies *our* plugin/host pair even when
    // stray `ainb` / plugin processes from other worktrees are alive (recon
    // confirmed they are). We never match by basename or a wildcard.
    let (plugin_pid, host_pid) = poll_for_plugin_and_host(
        &plugin_bin,
        &host_bin,
        Instant::now() + Duration::from_secs(20),
    )
    .expect("hangar-tui plugin child of our ainb host never appeared in `ps`");
    assert!(plugin_pid > 0 && host_pid > 0, "real pids expected");

    // =======================================================================
    // LEG A — PLUGIN CRASH → HOST RECOVERS
    // =======================================================================
    kill_exact(plugin_pid).expect("kill -9 the exact plugin child pid");

    // POSITIVE: the host recovers. The hangar-tui plugin is `lazy`, so the host
    // re-spawns on the next render kick — drive a couple of nav keystrokes to
    // mark the screen dirty, then accept EITHER a re-rendered seeded row OR a
    // clean Hangar chrome frame (a clean disconnected/error state). Re-send the
    // nav key each poll (a single key can be dropped on a busy pane), bounded.
    let recovered = poll_nav_until(&sess, "1", Instant::now() + Duration::from_secs(45), |c| {
        c.contains(READY_MARKER) || common::hangar_chrome_visible(c)
    });
    let recovered = recovered.unwrap_or_else(|| {
        panic!(
            "host never recovered the Hangar screen after the plugin crash \
             (no re-render and no clean chrome):\n{}",
            sess.capture()
        )
    });
    // POSITIVE marker present (rows back, or at least clean plugin chrome).
    assert!(
        recovered.contains(READY_MARKER) || common::hangar_chrome_visible(&recovered),
        "post-crash pane is neither re-rendered rows nor clean chrome:\n{recovered}"
    );

    // NEGATIVE (paired): the HOST process did NOT die with its child — it is
    // still alive. A host that crashed when the plugin died is a real bug.
    assert!(
        pid_alive(host_pid),
        "host process (pid {host_pid}) died when the plugin crashed — the host \
         must survive a plugin subprocess crash, not die with it"
    );
    // NEGATIVE (paired): the pane is NOT a dead/empty terminal frozen on
    // nothing — the persistent Hangar tab strip must still be painted (the host
    // is still drawing frames, not wedged on a corpse).
    assert!(
        common::hangar_chrome_visible(&recovered),
        "pane lost the Hangar chrome after the crash (frozen / dead terminal):\n{recovered}"
    );

    // =======================================================================
    // LEG B — HOST DEATH → NO ORPHAN (stdin-EOF / parent-death watcher union)
    // =======================================================================
    // Re-discover the (possibly re-spawned) live plugin child — leg A may have
    // re-spawned it under a NEW pid, so capture the current one. The hangar-tui
    // plugin is `lazy`, so if leg A returned on clean chrome (no rows) the
    // respawn may not have fired yet; KEEP nudging with nav keys while polling
    // so the lazy respawn is actually triggered. We assert a LIVE plugin existed
    // at host-kill time so the no-orphan check below is meaningful, not vacuous
    // (a dead plugin trivially can't orphan).
    let live_plugin = nudge_until_plugin_live(
        &sess,
        &plugin_bin,
        &host_bin,
        Instant::now() + Duration::from_secs(30),
    );
    assert!(
        live_plugin.is_some(),
        "no live hangar-tui child existed after leg A's recovery — leg B's \
         no-orphan check would be vacuous; expected the lazy plugin to have \
         re-spawned under the host before the host kill"
    );
    let plugin_pid = live_plugin.expect("asserted Some above");

    // Confirm it is genuinely ALIVE the instant before we kill the host, so the
    // subsequent alive→gone transition is a real observation, not a no-op.
    assert!(
        pid_alive(plugin_pid),
        "re-discovered plugin pid {plugin_pid} was already dead before host kill"
    );

    // Kill the HOST — never runs Drop, so the macOS process-group kill backstop
    // never fires and `kill_on_drop` is bypassed. The plugin must self-exit via
    // the stdin-EOF path (host's stdin write-end closed on fd-table reap) or, in
    // the stdin-still-open case, the parent-death watcher / PR_SET_PDEATHSIG.
    kill_exact(host_pid).expect("kill -9 the exact host pid");

    // POSITIVE: the captured plugin pid is gone within a bounded poll.
    let gone = poll_until_pid_gone(plugin_pid, Instant::now() + Duration::from_secs(15));
    assert!(
        gone,
        "plugin pid {plugin_pid} survived host death — the no-orphan guarantee \
         (stdin-EOF exit + parent-death watcher / PR_SET_PDEATHSIG) failed"
    );

    // NEGATIVE (paired), exhaustive: no process with the EXACT staged-plugin
    // path survives as an orphan of THIS run. Scoped to the exact absolute path
    // (this worktree's `dist/plugins/hangar-tui/hangar-tui`) so stray plugins
    // from other worktrees can never make this red. Bounded poll for the
    // reaping mechanism to land.
    let no_orphan = poll_until_no_exact_proc(&plugin_bin, Instant::now() + Duration::from_secs(15));
    assert!(
        no_orphan,
        "an orphaned hangar-tui plugin ({}) survived host death — the no-orphan \
         guarantee failed to reap it:\n{}",
        plugin_bin.display(),
        list_exact_procs(&plugin_bin)
    );
}

/// Discover the EXACT live plugin child pid and its host parent pid by exact
/// executable-path matching, polling until both appear or `deadline` passes.
///
/// Returns `(plugin_pid, host_pid)` for the plugin whose executable path is
/// exactly `plugin_bin` AND whose parent's executable path is exactly
/// `host_bin` — the unambiguous identity of *our* host/plugin pair, robust to
/// stray `ainb` / plugin processes from other worktrees (matched by full path,
/// never a basename or wildcard).
fn poll_for_plugin_and_host(
    plugin_bin: &Path,
    host_bin: &Path,
    deadline: Instant,
) -> Option<(u32, u32)> {
    loop {
        if let Some(pair) = find_plugin_and_host(plugin_bin, host_bin) {
            return Some(pair);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// One-shot `ps` scan for the `(plugin_pid, host_pid)` pair (see
/// [`poll_for_plugin_and_host`]). Builds a pid→exe map, then finds a plugin row
/// whose exe `== plugin_bin` and whose `ppid`'s exe `== host_bin`.
fn find_plugin_and_host(plugin_bin: &Path, host_bin: &Path) -> Option<(u32, u32)> {
    let plugin_str = plugin_bin.to_string_lossy();
    let host_str = host_bin.to_string_lossy();

    let rows = ps_rows();
    // pid -> exe, for resolving a candidate plugin's parent exe.
    let exe_of: std::collections::HashMap<u32, String> =
        rows.iter().map(|r| (r.pid, r.exe.clone())).collect();

    for r in &rows {
        if r.exe != *plugin_str {
            continue;
        }
        // Confirm the parent is OUR exact host binary.
        if exe_of.get(&r.ppid).is_some_and(|c| *c == *host_str) {
            return Some((r.pid, r.ppid));
        }
    }
    None
}

/// A single `ps` row: pid, ppid, and the process's executable path (the leading
/// `argv[0]` token of its command line).
struct PsRow {
    pid: u32,
    ppid: u32,
    exe: String,
}

/// Snapshot of all processes as `(pid, ppid, exe)` rows via
/// `ps -axo pid=,ppid=,args=`, where `exe` is the leading `argv[0]` token of the
/// command line.
///
/// ## Why `args=` (not `comm=`) — cross-platform path matching
///
/// We need the EXACT absolute executable path to disambiguate our plugin/host
/// from stray ones in other worktrees. `comm` is NOT portable for this: on macOS
/// `comm` is the full path, but on **Linux** `comm` is `/proc/<pid>/comm`,
/// truncated to 15 chars with no directory — `hangar-tui`, useless for an
/// absolute-path compare. `args` (the full command line) starts with the path
/// the process was invoked as on BOTH platforms: the host launches
/// `exec '<abs ainb>' tui` and spawns the plugin via `Command::new(<abs path>)`,
/// so `argv[0]` is the absolute path on macOS and Linux alike. We take the first
/// whitespace-delimited token of `args` as the executable path (our binaries
/// have no spaces in their paths, and the host's only trailing arg is ` tui`).
///
/// Returns an empty vec on any `ps` failure (callers poll, so a transient miss
/// is harmless).
fn ps_rows() -> Vec<PsRow> {
    let out = match Command::new("ps").args(["-axo", "pid=,ppid=,args="]).output() {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    let text = String::from_utf8_lossy(&out);
    let mut rows = Vec::new();
    for line in text.lines() {
        // `pid ppid args…` — split off the first two integer columns, then take
        // the leading token of the remaining command line as the executable.
        let mut it = line.trim_start().splitn(3, char::is_whitespace);
        let (Some(pid), Some(ppid), Some(args)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let (Ok(pid), Ok(ppid)) = (pid.parse::<u32>(), ppid.parse::<u32>()) else {
            continue;
        };
        let exe = args.trim().split(char::is_whitespace).next().unwrap_or("").to_string();
        rows.push(PsRow { pid, ppid, exe });
    }
    rows
}

/// `kill -9` exactly `pid` (never a name/wildcard match). Reaping is not needed:
/// the host pid is owned by the tmux pane shell, and the plugin pid was spawned
/// by the host — neither is a child of this test process.
fn kill_exact(pid: u32) -> nix::Result<()> {
    kill(
        Pid::from_raw(i32::try_from(pid).expect("pid fits i32")),
        Signal::SIGKILL,
    )
}

/// `true` while `pid` is alive (signal 0 probe via `kill -0`).
fn pid_alive(pid: u32) -> bool {
    kill(
        Pid::from_raw(i32::try_from(pid).expect("pid fits i32")),
        None,
    )
    .is_ok()
}

/// Poll until `pid` is gone (`kill -0` fails) or `deadline` passes. Returns
/// `true` once the pid has vanished.
fn poll_until_pid_gone(pid: u32, deadline: Instant) -> bool {
    loop {
        if !pid_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Poll until NO process has its executable path `== bin` (the exact
/// staged-plugin path) or `deadline` passes. Returns `true` once none remain.
fn poll_until_no_exact_proc(bin: &Path, deadline: Instant) -> bool {
    let bin_str = bin.to_string_lossy().into_owned();
    loop {
        let any = ps_rows().iter().any(|r| r.exe == bin_str);
        if !any {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Render the surviving rows whose executable path `== bin` for a failure
/// message.
fn list_exact_procs(bin: &Path) -> String {
    let bin_str = bin.to_string_lossy().into_owned();
    ps_rows()
        .iter()
        .filter(|r| r.exe == bin_str)
        .map(|r| format!("  pid={} ppid={} exe={}", r.pid, r.ppid, r.exe))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Re-send single-char nav `key` (no `Enter`, per HARD RULE 3) every poll until
/// `pred` holds on the captured pane or `deadline` passes. Used after the plugin
/// crash to mark the lazy screen dirty so the host re-spawns + re-renders.
fn poll_nav_until(
    sess: &TuiSession,
    key: &str,
    deadline: Instant,
    pred: impl Fn(&str) -> bool,
) -> Option<String> {
    loop {
        sess.send_key(key);
        if let Some(c) = sess.poll_capture(Instant::now() + Duration::from_millis(1500), &pred) {
            return Some(c);
        }
        if Instant::now() >= deadline {
            return None;
        }
    }
}

/// Drive nav keystrokes (alternating tabs to keep marking the screen dirty)
/// while polling for a LIVE plugin child of our host, until one appears or
/// `deadline` passes. Returns its pid.
///
/// The hangar-tui plugin is `lazy`: after the leg-A crash it only re-spawns on a
/// render kick, which a nav keystroke triggers. A single nudge can be dropped on
/// a busy pane or land while the backoff timer is still running, so we keep
/// nudging on every poll iteration rather than relying on one keystroke.
fn nudge_until_plugin_live(
    sess: &TuiSession,
    plugin_bin: &Path,
    host_bin: &Path,
    deadline: Instant,
) -> Option<u32> {
    // Alternate two top-level tab keys so the screen is reliably re-marked dirty
    // each iteration (re-pressing the SAME key on an already-current tab can be a
    // no-op for the dirty flag on some frames).
    let keys = ["1", "2"];
    let mut i = 0usize;
    loop {
        sess.send_key(keys[i % keys.len()]);
        i += 1;
        if let Some((pid, _)) = find_plugin_and_host(plugin_bin, host_bin) {
            return Some(pid);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}
