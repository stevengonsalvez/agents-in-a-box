//! P8.7 e2e tripwire 2 — the daemon-health pane (`D`) renders a throughput
//! sparkline whose failure band shows red.
//!
//! ```text
//!  seed P4 fixture ──▶ bump agent cap + enqueue N tasks (1 fails, rest pass)
//!         │
//!         ▼
//!  ainb-hangar-daemon (CLAIM ENABLED, fake-claude) ── runs FSM ──▶
//!         record_completed / record_failed  ──▶ throughput ring
//!         ▲                                              │
//!  ainb tui (tmux) ──`g`──▶ Hangar ──`D`──▶ daemon-health pane
//!                                            │
//!         poll_capture: ▁▂▃▄▅▆▇█ block chars + red SGR near the failure band
//! ```
//!
//! The sparkline's data is the daemon's **in-memory** throughput ring (P8.5),
//! which is populated only by the FSM finalize path on real task completion. So
//! this drives the data the REAL way: a claim-enabled daemon executes a batch of
//! seeded tasks through a deterministic fake-`claude`
//! ([`fake_claude_mixed`](common::fake_claude_mixed)) — the first run fails
//! (`record_failed` → the sparkline's red band), the rest succeed
//! (`record_completed` → green) — then the `D` screen snapshots the ring.
//!
//! Assertions (positive + negative, per the `tmux-ui-tripwire` skill):
//!   * POSITIVE: at least one of the eight unicode block glyphs `▁▂▃▄▅▆▇█` is on
//!     the pane (the sparkline rendered a non-empty column), AND the pane chrome
//!     (`Daemon health`, `throughput (60s):`) is present.
//!   * POSITIVE: an escaped (`-e`) capture carries a truecolor **red** SGR for
//!     the sparkline's failure accent (`38;2;220;120;100`). If no red escape is
//!     observed (terminal/tmux escape-emission differences) the assertion softens
//!     to "block chars + health chrome rendered" — documented below.
//!   * NEGATIVE: the pane is NOT the Hangar issue-list landing screen (proves the
//!     `D` switch actually happened).
//!
//! SKIP (never fail) when tmux / binaries / the staged plugin are absent.

#![allow(clippy::duration_suboptimal_units)] // `from_secs` reads fine as a budget.

#[path = "tripwire_p4_common.rs"]
mod common;

use std::time::{Duration, Instant};

use common::{
    READY_MARKER, TuiSession, ainb_bin, can_run_tripwire, enqueue_tasks, fake_claude_mixed,
    hangar_chrome_visible, prepare_pipeline_with, set_agent_concurrency, skip, wait_for_terminal,
};

/// The eight vertical partial-block glyphs the dual-dim sparkline paints with
/// (`widgets::sparkline_dual::BARS`). At least one must appear once the ring has
/// throughput.
const BLOCK_GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The sparkline's red failure accent, as a truecolor SGR escape. `SPARK_RED` is
/// `Color::rgb(220, 120, 100)` (see `widgets::sparkline_dual`), so a `-e` capture
/// of a column with a failure proportion carries this foreground sequence.
const RED_SGR: &str = "38;2;220;120;100";

/// How many tasks to seed, and how many of them fail (the first one).
const SEED_TASKS: usize = 6;
const FAIL_FIRST: u32 = 1;

#[test]
fn daemon_health_sparkline_renders_with_red_failure_band() {
    if !can_run_tripwire() {
        skip("daemon-health sparkline tripwire");
        return;
    }

    // 1. Seed the P4 fixture + spawn a CLAIM-ENABLED daemon driving a fake-claude
    //    that fails its first run then succeeds. `HANGAR_DAEMON_POLL_MS=200`
    //    drains the queue promptly.
    let home_holder = tempfile::tempdir().expect("fake-claude dir");
    let fake_claude = fake_claude_mixed(home_holder.path(), FAIL_FIRST);
    let pipeline = prepare_pipeline_with(&[
        ("HANGAR_DAEMON_RUNTIME_ID", "runtime-1"),
        (
            "HANGAR_CLAUDE_PATH",
            fake_claude.to_str().expect("utf-8 path"),
        ),
        ("HANGAR_DAEMON_POLL_MS", "200"),
    ]);

    // 2. Free the fixture's running slot + raise the agent's concurrency, then
    //    enqueue the batch the daemon will claim and finalise.
    set_agent_concurrency(pipeline.home(), 10);
    enqueue_tasks(pipeline.home(), "health", SEED_TASKS);

    // 3. Wait for the daemon to finalise the whole batch (driving the ring).
    let terminal = wait_for_terminal(
        pipeline.home(),
        "health",
        SEED_TASKS,
        Instant::now() + Duration::from_secs(40),
    );
    assert_eq!(
        terminal, SEED_TASKS,
        "daemon should finalise all {SEED_TASKS} seeded tasks (got {terminal}); \
         the throughput ring needs them to drive the sparkline"
    );

    // 4. Launch the TUI under the same $HOME, open Hangar, then the `D` pane.
    let bin = ainb_bin().expect("gated by can_run_tripwire");
    let (session, landing) = TuiSession::launch_to_hangar(&bin, pipeline.home());
    assert!(
        landing.contains(READY_MARKER),
        "expected the Hangar issue-list landing before pressing D:\n{landing}"
    );

    // Press `D` (single-char nav, no Enter) until the health pane chrome is on
    // screen with at least one sparkline block glyph.
    let deadline = Instant::now() + Duration::from_secs(45);
    let pane = session
        .switch_tab_until("D", deadline, |c| {
            hangar_chrome_visible(c)
                && c.contains("Daemon health")
                && c.contains("throughput (60s):")
                && BLOCK_GLYPHS.iter().any(|g| c.contains(*g))
        })
        .unwrap_or_else(|| {
            panic!(
                "daemon-health sparkline never rendered block glyphs:\n{}",
                session.capture()
            )
        });

    // POSITIVE: the health chrome + at least one block glyph.
    assert!(
        pane.contains("Daemon health"),
        "daemon-health pane title missing:\n{pane}"
    );
    assert!(
        pane.contains("throughput (60s):"),
        "throughput sparkline header missing:\n{pane}"
    );
    let block_count = BLOCK_GLYPHS.iter().filter(|g| pane.contains(**g)).count();
    assert!(
        block_count >= 1,
        "expected at least one sparkline block glyph {BLOCK_GLYPHS:?}:\n{pane}"
    );

    // NEGATIVE: no longer the issue-list landing (the `D` switch happened).
    assert!(
        !pane.contains(READY_MARKER),
        "still on the issue list after pressing D (no screen switch):\n{pane}"
    );

    // POSITIVE (with documented softening): an escaped capture should carry the
    // sparkline's red failure-band SGR. tmux's `-e` escape emission is faithful
    // on the terminals we target (Term.app / iTerm2 / Linux), but to keep the
    // tripwire robust across terminals that normalise colours we treat the red
    // escape as a STRONG assertion only when ANY SGR escape is present in the
    // capture; if `-e` produced no escapes at all (a terminal that strips them)
    // we fall back to the block-glyph + chrome proof above and log the soften.
    let escaped = session.capture_escaped();
    let has_any_sgr = escaped.contains("\u{1b}[");
    if has_any_sgr {
        assert!(
            escaped.contains(RED_SGR),
            "the failure-spike column must paint a red SGR ({RED_SGR}) in the \
             escaped capture:\n{escaped}"
        );
    } else {
        eprintln!(
            "SOFTEN: tmux `capture-pane -e` produced no SGR escapes on this \
             terminal; asserting block glyphs + health chrome only (the red \
             failure-band escape could not be observed)."
        );
    }

    drop(session);
    drop(pipeline);
    drop(home_holder);
}
