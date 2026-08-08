//! ABOUTME: `#[ignore]`d probes that drive the REAL send-path functions against
//! a REAL Claude Code pane, so the gate's behaviour is measured rather than
//! transliterated.
//!
//! WHY THESE EXIST AND WHY THEY ARE IGNORED. Every earlier round of this fix
//! passed a green unit suite while being broken in production, because a
//! fixture can only ever replay a frame someone already thought to capture. The
//! gate's whole job is a RACE (does a CR reach a pane that has not finished
//! ingesting), and a race is not observable in a still frame. These probes
//! therefore attach to a live `claude` session, drive the same private
//! functions `tmux_send` drives, and report wall-clock timings.
//!
//! They cannot run in CI: they need a logged-in `claude`, they spend API
//! tokens, and their timings are properties of the machine. So every one is
//! `#[ignore]`d and every one refuses to run unless `PROBE_SESSION` names a
//! live tmux target the operator prepared and is watching.
//!
//! Usage:
//!
//! ```text
//! tmux new-session -d -s <name> -x 80 -y 24 -c <throwaway dir>
//! tmux send-keys -t <name> -l -- claude ; tmux send-keys -t <name> Enter
//! PROBE_SESSION=<name> PROBE_EVIDENCE=<dir> AINB_HOME=<scratch> \
//!   cargo test -p ainb-fleet-core --features tmux-tests --lib \
//!   live_probe -- --ignored --nocapture --test-threads=1
//! ```

use std::fmt::Write as _;
use std::time::Instant as WallClock;

use super::{
    Attribution, Baseline, Gate, PasteTally, Verdict, composer_region, composer_region_now,
    composer_state, count_live_needles, forget_baseline, paste_tally, record_baseline,
    region_is_clear, region_live_squeezed, region_shows_payload_tail, region_squeezed,
    send_keys_literal, tmux_press_enter, tmux_send, wait_for_ingest,
};
use crate::fleet::read::tmux_pane::capture_pane_ansi;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

/// The live pane, or `None` when the operator did not prepare one.
fn probe_session() -> Option<String> {
    let name = std::env::var("PROBE_SESSION").ok()?;
    (!name.trim().is_empty()).then_some(name)
}

/// Current-thread runtime, so the probes need no `macros` feature and no
/// multi-threaded scheduler to reason about.
fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("building a probe runtime")
}

/// Persist one labelled capture next to the test output, so the evidence
/// survives the run rather than living only in a scrollback.
fn evidence(label: &str, body: &str) {
    eprintln!("---- EVIDENCE {label} ----\n{body}\n---- END {label} ----");
    if let Ok(dir) = std::env::var("PROBE_EVIDENCE") {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(
            std::path::Path::new(&dir).join(format!("{label}.txt")),
            body,
        );
    }
}

async fn capture(session: &str) -> String {
    capture_pane_ansi(session, 0).await.expect("capturing the probe pane")
}

async fn region_of(session: &str) -> Option<String> {
    composer_region(&capture(session).await)
}

/// Wait until `predicate` holds for the composer region, or give up.
async fn wait_until<F>(session: &str, budget_ms: u64, predicate: F) -> Option<String>
where
    F: Fn(&str) -> bool,
{
    let start = WallClock::now();
    loop {
        if let Some(region) = region_of(session).await {
            if predicate(&region) {
                return Some(region);
            }
        }
        if u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) >= budget_ms {
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Wait for the model to finish the turn.
///
/// "Composer is clear" is NOT enough on its own: claude clears the composer the
/// instant it accepts a turn and then spends minutes working. The reliable
/// signal at 80x24 is that the WHOLE pane stops repainting, because a working
/// claude animates a spinner (`✢ Boogieing…`) several times a second. Measured
/// on the live pane: an idle pane holds a byte-identical capture for tens of
/// seconds, a working one never holds one for two.
async fn wait_for_idle(session: &str, budget_ms: u64) -> bool {
    let start = WallClock::now();
    let mut previous: Option<String> = None;
    let mut still = 0;
    loop {
        let pane = capture_pane_ansi(session, 0).await.ok();
        still = if pane.is_some() && pane == previous {
            still + 1
        } else {
            0
        };
        previous = pane;
        let clear = region_of(session).await.is_some_and(|region| region_is_clear(&region));
        if still >= 5 && clear {
            return true;
        }
        if u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX) >= budget_ms {
            return false;
        }
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
}

/// Bring the pane to a known-good state before a probe: nothing parked in the
/// composer, no turn in flight.
async fn prepare(session: &str, budget_ms: u64) {
    clear_composer(session).await;
    assert!(
        wait_for_idle(session, budget_ms).await,
        "the pane never went idle"
    );
}

/// What the gate did, and how long it took from the write.
struct GateRun {
    baseline: Baseline,
    gate: Gate,
    /// Milliseconds between the literal write and the gate returning, i.e.
    /// exactly the window before `tmux_send` presses Enter.
    write_to_gate_ms: u128,
    /// The composer as it stood when the gate opened.
    region_at_gate: Option<String>,
}

/// Replay `tmux_send`'s prologue (baseline, record, one literal write, gate)
/// with a stopwatch on it, WITHOUT pressing Enter.
///
/// This is the real code path: the same `composer_region_now`, the same
/// `Baseline::of`, the same `send_keys_literal`, the same `wait_for_ingest`.
/// Splitting the Enter off is what makes the gate's timing observable, and it
/// leaves the payload parked so the parked-composer predicates can be driven on
/// a genuinely parked pane.
async fn probe_gate(session: &str, payload: &str) -> GateRun {
    let before = composer_region_now(session).await.expect("pane readable before the write");
    let baseline = Baseline::of(before.as_deref(), payload);
    record_baseline(session, baseline.pastes);
    let started = WallClock::now();
    send_keys_literal(session, payload).await.expect("literal write");
    let gate = wait_for_ingest(session, payload, &baseline).await;
    let write_to_gate_ms = started.elapsed().as_millis();
    let region_at_gate = region_of(session).await;
    GateRun {
        baseline,
        gate,
        write_to_gate_ms,
        region_at_gate,
    }
}

// ---------------------------------------------------------------------------
// payloads
// ---------------------------------------------------------------------------

/// The fixed tail every probe payload ends in.
///
/// Byte-identical across "ticks" on purpose: that is the property the ATC
/// heartbeat has (`build_heartbeat_with_ledger` appends the same closing
/// instruction to every nudge) and the property that makes a tail needle unable
/// to tell tick N from tick N-1, or from a dim ghost of tick N-1.
const PROBE_TAIL: &str = "This is a send-path probe, not a real nudge: reply with the single word ACK and do nothing \
else.";

/// A heartbeat-shaped, harmless payload of roughly `rows` body lines.
///
/// Shaped after `ainb-core/src/fleet/atc/heartbeat.rs::build_heartbeat_with_ledger`
/// (bracketed marker, per-kind counts, one line per blocked session, a closing
/// instruction) so the on-screen rendering and the tail-needle behaviour match a
/// real nudge. The instruction itself is replaced by [`PROBE_TAIL`] so a live
/// model does no work.
fn heartbeat_shaped(tick: u64, rows: usize) -> String {
    let mut out = format!(
        "[HEARTBEAT {tick}] {rows} session(s) need attention - ERR 1 . ASK 0 . IDLE {rows} . WAIT 0\n"
    );
    for index in 0..rows {
        let _ = writeln!(
            out,
            "- [IDLE] probe-session-{index:03} - idle {index}m, no owner, awaiting the coalesce guard"
        );
    }
    out.push_str(PROBE_TAIL);
    out
}

/// The short nudge, matching the hangar daemon's `build_nudge` shape (~130
/// bytes), which renders as RAW TEXT at 80x24 and is therefore the payload the
/// tail needle actually carries.
fn short_nudge(tick: u64) -> String {
    format!("[HEARTBEAT {tick}] fleet quiet - 0 sessions need attention. {PROBE_TAIL}")
}

/// Grow a heartbeat-shaped payload to at least `target` bytes.
fn sized(tick: u64, target: usize) -> String {
    let mut rows = 1;
    loop {
        let body = heartbeat_shaped(tick, rows);
        if body.len() >= target || rows > 400 {
            return body;
        }
        rows += 1;
    }
}

// ---------------------------------------------------------------------------
// THE CASE THAT MATTERS MOST: the ghost-residue race
// ---------------------------------------------------------------------------

/// Discard whatever is sitting in the composer WITHOUT submitting it.
///
/// `C-u` kills to the start of the current WRAPPED row, not the whole buffer,
/// so a parked payload spanning seven rows needs seven of them. Measured on the
/// live pane; a single press left the payload in place and the next probe then
/// began against a dirty composer.
async fn clear_composer(session: &str) {
    for _ in 0..40 {
        if region_of(session).await.is_some_and(|region| region_is_clear(&region)) {
            return;
        }
        let _ = std::process::Command::new("tmux")
            .args(["send-keys", "-t", session, "C-u"])
            .status();
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
}

/// Run the race against whatever residue is currently in `region`, and report
/// the milliseconds between the literal write and the gate opening.
///
/// `region` must already satisfy the trap's precondition: it reads as a CLEAR
/// composer (so a caller would say "the last send was accepted"), and yet a
/// bare contains-the-needle gate fires on it. The payload is derived FROM the
/// residue precisely so those two things are true at once, which is the whole
/// of CRITICAL-1: HEAD cannot tell "my payload is on screen" from "something
/// that looks like my payload was already on screen".
async fn race_against(session: &str, label: &str, residue: &str, payload: &str) -> GateRun {
    evidence(&format!("{label}-residue"), residue);
    assert!(
        region_shows_payload_tail(residue, payload),
        "{label}: precondition failed, a bare contains-the-needle gate does NOT fire on this \
residue, so the race is not being reproduced"
    );
    let baseline = Baseline::of(Some(residue), payload);
    eprintln!(
        "{label}: HEAD-gate-fires-on-residue=true baseline_live_needles={} baseline_pastes={:?}",
        baseline.needles, baseline.pastes
    );
    assert!(
        !super::ingest_observed(residue, payload, &baseline, super::PLACEHOLDER_SETTLE),
        "{label}: THE P0, the gate opened on residue that was on screen BEFORE the write"
    );

    let run = probe_gate(session, payload).await;
    evidence(
        &format!("{label}-region-at-gate"),
        run.region_at_gate.as_deref().unwrap_or("<no composer>"),
    );
    eprintln!(
        "{label}: gate={:?} WRITE_TO_ENTER={}ms baseline_needles={}",
        run.gate, run.write_to_gate_ms, run.baseline.needles
    );
    assert_eq!(
        run.gate,
        Gate::PayloadVisible,
        "{label}: the payload never became visible"
    );
    let at_gate = run.region_at_gate.clone().expect("composer readable at gate");
    // The substantive assertion, and it is the REAL gate predicate rather than
    // a restatement of it: at the moment Enter would be pressed, the composer
    // must show a CHANGE against the pre-write baseline.
    assert!(
        super::ingest_observed(&at_gate, payload, &run.baseline, super::PLACEHOLDER_SETTLE),
        "{label}: the gate opened while the composer still showed nothing but the residue"
    );
    let needle = super::payload_needle(payload).expect("payload has a needle");
    eprintln!(
        "{label}: at gate live_needles={} (baseline {}) pastes={:?} (baseline {:?})",
        region_live_squeezed(&at_gate).matches(needle.as_str()).count(),
        run.baseline.needles,
        paste_tally(&at_gate),
        run.baseline.pastes
    );
    run
}

/// [`super::INGEST_POLL`] in milliseconds, so the trace below counts the same
/// stillness the real gate counts.
const INGEST_POLL_MS: u64 = 120;

/// One sample of the HEAD predicate and the fixed one, taken from the SAME
/// live capture.
struct Frame {
    at_ms: u128,
    head_fires: bool,
    live_needles: usize,
    pastes: PasteTally,
    /// Consecutive identical captures at this point, i.e. what the placeholder
    /// clause's settle counter held. Printed because a settle that never
    /// arrives is indistinguishable from a slow ingest without it.
    settled: u32,
}

/// Write `payload` and then sample both gates at ~10ms until the FIXED gate's
/// change-against-baseline condition is first satisfied.
///
/// This is the measurement that separates HEAD from the fix, and neither
/// predicate is reimplemented here: `region_shows_payload_tail` is literally
/// the clause HEAD's `wait_for_ingest` returned `PayloadVisible` on, and
/// `ingest_observed` is literally the clause the fixed one returns on.
async fn trace_gates(session: &str, label: &str, payload: &str, baseline: &Baseline) -> Vec<Frame> {
    let started = WallClock::now();
    send_keys_literal(session, payload).await.expect("literal write");
    let mut frames = Vec::new();
    let mut settled = 0_u32;
    let mut previous: Option<String> = None;
    loop {
        let at_ms = started.elapsed().as_millis();
        let Some(region) = region_of(session).await else {
            continue;
        };
        settled = if previous.as_ref() == Some(&region) {
            settled + 1
        } else {
            0
        };
        previous = Some(region.clone());
        let observed = super::ingest_observed(&region, payload, baseline, settled);
        frames.push(Frame {
            at_ms,
            head_fires: region_shows_payload_tail(&region, payload),
            live_needles: super::count_live_needles(&region, payload),
            pastes: paste_tally(&region),
            settled,
        });
        if observed || at_ms > 12_000 {
            break;
        }
        // The REAL poll interval, so `settled` counts the same stillness the
        // real gate counts and the reported timings are the real ones.
        tokio::time::sleep(std::time::Duration::from_millis(INGEST_POLL_MS)).await;
    }
    let head_first = frames.iter().find(|f| f.head_fires).map(|f| f.at_ms);
    let fixed_at = frames.last().map_or(0, |f| f.at_ms);
    eprintln!(
        "{label}: HEAD would press Enter at {head_first:?}ms, the fixed gate at {fixed_at}ms"
    );
    for frame in &frames {
        eprintln!(
            "  t={:>6}ms head_fires={} live_needles={} settled={} pastes={:?}",
            frame.at_ms, frame.head_fires, frame.live_needles, frame.settled, frame.pastes
        );
    }
    frames
}

/// THE GHOST-RESIDUE RACE against a REAL dim ghost.
///
/// Claude Code paints a DIM line into an EMPTY composer, and in `capture-pane
/// -p` that is byte-identical to freshly typed text. So a composer that a
/// caller reads as CLEAR ("the previous send was accepted") simultaneously
/// answers YES to `region_shows_payload_tail`, and HEAD's gate fires on the
/// first poll, ~20ms after the write, straight into the measured CR-fusion
/// window.
///
/// WHICH DIM LINE, and why not the suggestion ghost. claude emits the
/// suggestion ghost on its own schedule: measured on this host over an hour of
/// driving the pane, it appeared twice and could not be provoked (none in 180s
/// of polling after a turn, none from typing-then-clearing, none from a
/// repetitive prompt). A probe cannot wait on that, so this one uses the dim
/// line that IS deterministic, `Press up to edit queued messages`, which claude
/// paints into the composer of a BUSY session that has accepted a send. Same
/// trap, same place, same renderer: dim, needle-carrying, and verdict-CLEAR.
/// [`live_suggestion_ghost_race`] runs the identical body against a real
/// suggestion ghost when one happens to be up, and
/// [`live_captured_ghost_frame_reads_submitted`] pins a captured one.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_dim_residue_race() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        // Make the session busy, then send into it: claude accepts the send and
        // paints the dim `Press up to edit queued messages` into the composer.
        let busy = format!("Count silently to 40, then {PROBE_TAIL}");
        tmux_send(&session, &busy).await.expect("the busy-maker must be delivered");
        let queued = short_nudge(1_700_000_000_050);
        tmux_send(&session, &queued).await.expect("the queued send must be accepted");

        let residue = wait_until(&session, 30_000, |region| {
            region_is_clear(region) && !region_squeezed(region).is_empty()
        })
        .await
        .expect("no dim residue ever appeared in the composer");

        // The payload is derived from the residue on purpose: this is the exact
        // condition CRITICAL-1 describes, where the composer ALREADY holds the
        // needle before a single byte of ours goes out.
        let payload = region_squeezed(&residue);
        race_against(&session, "DIM-RESIDUE-RACE", &residue, &payload).await;
        clear_composer(&session).await;
        forget_baseline(&session);
        assert!(
            wait_for_idle(&session, 240_000).await,
            "the busy turn never finished"
        );
    });
}

/// THE GHOST-RESIDUE RACE against claude's own SUGGESTION ghost.
///
/// Claude Code paints a dim suggested next prompt into an idle, EMPTY composer
/// (`ESC[39m ❯ NBSP ESC[2m <suggestion> ESC[0m`). It is emitted on claude's own
/// schedule, so this probe does not try to provoke one: it requires the
/// composer to be holding one already and skips otherwise. Run it while a
/// ghost is up.
#[test]
#[ignore = "needs a live claude pane whose composer is CURRENTLY showing a ghost"]
fn live_suggestion_ghost_race() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        let Some(ghost) = region_of(&session).await else {
            panic!("no composer on the probe pane");
        };
        assert!(
            region_is_clear(&ghost) && !region_squeezed(&ghost).is_empty(),
            "the composer is not showing a ghost right now, so there is nothing to race against"
        );
        let payload = region_squeezed(&ghost);
        race_against(&session, "SUGGESTION-GHOST-RACE", &ghost, &payload).await;
        clear_composer(&session).await;
        forget_baseline(&session);
    });
}

/// A REAL captured suggestion-ghost frame, driven through the real classifier.
///
/// `PROBE_GHOST_PANE` names a verbatim `capture-pane -e` of a claude pane whose
/// composer is holding a ghost. The frame is real; only the timing is not, so
/// this is the regression check on round 2's win (a ghost reads SUBMITTED)
/// rather than a race measurement.
#[test]
#[ignore = "needs PROBE_GHOST_PANE naming a captured ghost frame"]
fn live_captured_ghost_frame_reads_submitted() {
    let Ok(path) = std::env::var("PROBE_GHOST_PANE") else {
        eprintln!("PROBE_GHOST_PANE unset, skipping");
        return;
    };
    let pane = std::fs::read_to_string(&path).expect("reading the captured ghost frame");
    let region = composer_region(&pane).expect("the captured frame has a composer");
    evidence("captured-ghost-region", &region);
    let text = region_squeezed(&region);
    assert!(
        !text.is_empty(),
        "the captured frame's composer is empty, so it holds no ghost"
    );

    eprintln!(
        "CAPTURED GHOST: squeezed={text:?} live={:?} clear={} state={:?}",
        region_live_squeezed(&region),
        region_is_clear(&region),
        composer_state(&pane, Attribution::Payload(&text, Baseline::default()))
    );
    assert!(
        region_is_clear(&region),
        "a ghost must read as a CLEAR composer"
    );
    assert!(
        region_live_squeezed(&region).is_empty(),
        "a ghost is entirely DIM, so it must contribute no live content"
    );
    assert_eq!(
        composer_state(&pane, Attribution::Payload(&text, Baseline::default())),
        Verdict::Submitted,
        "a ghost of our own payload must read SUBMITTED, not PENDING"
    );
    // And the gate must not open on it, at any settle count.
    let baseline = Baseline::of(Some(&region), &text);
    assert_eq!(baseline.needles, 0, "the ghost's needle is DIM, not live");
    for settled in 0..=super::PLACEHOLDER_SETTLE + 2 {
        assert!(
            !super::ingest_observed(&region, &text, &baseline, settled),
            "THE P0: the gate opened on a ghost that was on screen before the write"
        );
    }
    assert!(
        !composer_state(&pane, Attribution::Precheck(Some(PasteTally::default())))
            .eq(&Verdict::Pending),
        "the ATC coalesce guard must not blind-Enter a ghost"
    );
}

/// THE OTHER HALF OF CRITICAL-1, and the one the ATC actually hits: tick N-1 is
/// still PARKED in the composer (live, not dim) when tick N is written.
///
/// Every ATC heartbeat ends in the same fixed literal, so tick N and tick N-1
/// have byte-identical tail needles and a `contains` can never tell them apart.
/// HEAD's gate therefore fires on the residue on the first poll; the fixed gate
/// must wait until the needle count RISES, which is the truthful state (both
/// bodies are then in the composer).
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_parked_previous_tick_race() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        let previous = short_nudge(1_700_000_000_001);
        let next = short_nudge(1_700_000_000_002);
        assert_ne!(previous, next, "different ticks, different bodies");
        assert_eq!(
            super::payload_needle(&previous),
            super::payload_needle(&next),
            "precondition: the tail needle cannot discriminate two ticks"
        );

        // Park tick N-1: written, never submitted. This is the measured
        // failure mode, not a contrivance.
        send_keys_literal(&session, &previous).await.expect("parking tick N-1");
        let residue = wait_until(&session, 15_000, |region| {
            region_shows_payload_tail(region, &previous)
        })
        .await
        .expect("tick N-1 never rendered in the composer");

        let baseline = Baseline::of(Some(&residue), &next);
        assert_eq!(
            baseline.needles, 1,
            "the parked tick is LIVE, so it must contribute exactly one live needle"
        );
        assert!(
            !super::ingest_observed(&residue, &next, &baseline, super::PLACEHOLDER_SETTLE),
            "THE P0: the gate opened on the PREVIOUS tick's residue"
        );

        race_against(&session, "PARKED-TICK-RACE", &residue, &next).await;
        clear_composer(&session).await;
        forget_baseline(&session);
    });
}

/// THE MEASUREMENT THAT SEPARATES HEAD FROM THE FIX.
///
/// A short tick N-1 is parked (so its needle IS on screen and HEAD's clause is
/// satisfied from the very first frame) and a LARGE tick N is then written, so
/// its ingest takes long enough for the difference to be visible in wall-clock
/// time. HEAD presses Enter into the still-draining pane, which is the measured
/// CR-fusion condition and the whole P0. The fixed gate must wait until the
/// screen actually CHANGES against the pre-write baseline.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_head_fires_early_where_the_fix_waits() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        let previous = short_nudge(1_700_000_000_010);
        let next = sized(1_700_000_000_011, 2400);
        assert_eq!(
            super::payload_needle(&previous),
            super::payload_needle(&next),
            "precondition: the tail needle cannot discriminate the two ticks"
        );

        send_keys_literal(&session, &previous).await.expect("parking tick N-1");
        let residue = wait_until(&session, 15_000, |region| {
            region_shows_payload_tail(region, &previous)
        })
        .await
        .expect("tick N-1 never rendered");
        evidence("head-vs-fix-residue", &residue);
        let baseline = Baseline::of(Some(&residue), &next);
        assert!(
            region_shows_payload_tail(&residue, &next),
            "precondition: HEAD's clause is already satisfied by the residue"
        );

        let frames = trace_gates(&session, "HEAD-VS-FIX", &next, &baseline).await;
        let first_head = frames
            .iter()
            .find(|frame| frame.head_fires)
            .expect("HEAD's clause never fired, so the race is not being reproduced");
        let fixed = frames.last().expect("at least one frame");
        eprintln!(
            "HEAD-VS-FIX: payload={}B HEAD_enter_at={}ms FIXED_enter_at={}ms",
            next.len(),
            first_head.at_ms,
            fixed.at_ms
        );
        assert_eq!(
            first_head.live_needles, baseline.needles,
            "HEAD's clause fired on a frame that carried NEW live evidence, so this frame does \
not show the trap"
        );
        assert!(
            fixed.at_ms > first_head.at_ms,
            "THE P0: the fixed gate opened no later than HEAD did, i.e. it also fired on the \
residue"
        );
        let after = region_of(&session).await.unwrap_or_default();
        evidence("head-vs-fix-region-at-fixed-gate", &after);
        clear_composer(&session).await;
    });
}

// ---------------------------------------------------------------------------
// the rest of the settled cases
// ---------------------------------------------------------------------------

/// A payload whose TAIL NEEDLE contains a `>` that starts its own rendered row
/// (a markdown blockquote) must submit, and must not be abandoned before Enter.
///
/// THE MEDIUM-3 CASE, and the payload is built so it actually exercises it. The
/// old `composer_cells` ran its prompt-glyph skip on EVERY row, so a row whose
/// first character was `>` / `❯` / `›` lost that character from the region while
/// [`payload_needle`] kept it. The needle then could not be found in a composer
/// that visibly held it, `wait_for_ingest` timed out, and `tmux_send` returned
/// `Err` WITHOUT EVER PRESSING ENTER. For that to bite, the `>` must be inside
/// the last `NEEDLE_CHARS` non-whitespace characters, which is why the quoted
/// line is the LAST line and is short.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_blockquote_tail_submits() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        let payload = "[HEARTBEAT 1700000000600] a relayed reply follows, quoted. Do not act on \
the quoted line; it is send-path probe text, not a real nudge.\n> reply with the single word ACK."
            .to_string();
        let needle = super::payload_needle(&payload).expect("payload has a needle");
        assert!(
            needle.contains('>'),
            "precondition: the blockquote glyph must be INSIDE the tail needle, or this payload \
does not exercise the defect (needle was {needle:?})"
        );

        // Park it first, so the region can be inspected while the payload is
        // provably on screen.
        send_keys_literal(&session, &payload)
            .await
            .expect("parking the blockquote payload");
        let region = wait_until(&session, 15_000, |region| !region_is_clear(region))
            .await
            .expect("the blockquote payload never rendered");
        evidence("blockquote-parked-region", &region);
        assert!(
            region
                .lines()
                .any(|row| row.trim_start_matches(['│', ' ', '\t']).starts_with('>')),
            "precondition: no rendered row starts with the blockquote glyph"
        );
        assert!(
            region_shows_payload_tail(&region, &payload),
            "THE MEDIUM-3 DEFECT: the composer visibly holds the payload but the needle cannot \
be found in the region, so the gate would time out and Enter would never be pressed"
        );
        clear_composer(&session).await;

        let started = WallClock::now();
        let result = tmux_send(&session, &payload).await;
        let elapsed = started.elapsed().as_millis();
        let after = region_of(&session).await.unwrap_or_default();
        evidence("blockquote-after-send", &after);
        eprintln!("BLOCKQUOTE: ok={} elapsed={elapsed}ms", result.is_ok());
        assert!(
            result.is_ok(),
            "a blockquote payload was not delivered: {result:?}"
        );
        assert!(
            wait_for_idle(&session, 300_000).await,
            "blockquote turn never finished"
        );
    });
}

/// Latency of the healthy path after the pre-write baseline capture was added.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_healthy_latency() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        for (label, payload) in [
            ("short", short_nudge(1_700_000_000_200)),
            ("2400b", sized(1_700_000_000_201, 2400)),
        ] {
            prepare(&session, 180_000).await;
            let started = WallClock::now();
            let result = tmux_send(&session, &payload).await;
            let elapsed = started.elapsed().as_millis();
            eprintln!(
                "LATENCY {label}: bytes={} elapsed={elapsed}ms result={result:?}",
                payload.len()
            );
            assert!(result.is_ok(), "{label} send failed: {result:?}");
            assert!(
                wait_for_idle(&session, 180_000).await,
                "{label} turn never finished"
            );
        }
    });
}

/// A payload parked in the composer (written, never submitted) in the band
/// where the tail needle is blind must classify `Pending`, and a clean send
/// must classify `Submitted`. Driven through the real `composer_state`.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_blind_band_parked_is_pending() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        for target in [2000_usize, 4000, 8000] {
            prepare(&session, 180_000).await;
            let payload = sized(1_700_000_000_300 + target as u64, target);
            let run = probe_gate(&session, &payload).await;
            let pane = capture(&session).await;
            evidence(&format!("blind-band-{target}-parked"), &pane);
            let region = composer_region(&pane).expect("composer readable while parked");
            let verdict = composer_state(&pane, Attribution::Payload(&payload, run.baseline));
            eprintln!(
                "BLIND BAND {target}B: bytes={} gate={:?} write_to_gate={}ms parked_verdict={:?} \
tally={:?} live_needle={} squeezed_needle={}",
                payload.len(),
                run.gate,
                run.write_to_gate_ms,
                verdict,
                paste_tally(&region),
                count_live_needles(&region, &payload),
                region_shows_payload_tail(&region, &payload),
            );
            assert_eq!(
                run.gate,
                Gate::PayloadVisible,
                "{target}B never became visible"
            );
            assert_eq!(
                verdict,
                Verdict::Pending,
                "a PARKED {target}B payload was not classified Pending"
            );

            // Now submit it and re-classify: the same payload must read
            // Submitted once the composer clears.
            tmux_press_enter(&session).await.expect("submitting the parked payload");
            let submitted = wait_until(&session, 30_000, region_is_clear)
                .await
                .expect("composer never cleared after Enter");
            evidence(&format!("blind-band-{target}-submitted"), &submitted);
            let pane = capture(&session).await;
            assert_eq!(
                composer_state(&pane, Attribution::Payload(&payload, run.baseline)),
                Verdict::Submitted,
                "a SUBMITTED {target}B payload was not classified Submitted"
            );
            forget_baseline(&session);
            assert!(
                wait_for_idle(&session, 180_000).await,
                "{target}B turn never finished"
            );
        }
    });
}

/// A human's paste staged in the composer before anyone sent to the pane.
///
/// `pane_has_unsubmitted_input` is what the ATC coalesce guard consults before
/// pressing a LONE Enter, so a `true` here means the guard submits a stranger's
/// text. With no recorded pre-send baseline the placeholder is unattributable
/// and the answer must be `false`.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_human_paste_is_not_flushed() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        // No record: nobody has sent to this pane.
        forget_baseline(&session);

        // FIRST, the raw-text form. A 1760-character human paste rendered as
        // plain text with NO placeholder on this host, which is the same
        // non-monotonic classifier behaviour a 1546-byte payload showed, so the
        // guard is asked about it before the bigger one.
        let small: String =
            std::iter::repeat_n("human pasted stack frame line, not a nudge. ", 40).collect();
        send_keys_literal(&session, &small)
            .await
            .expect("staging the small human paste");
        let raw = wait_until(&session, 15_000, |region| !region_is_clear(region))
            .await
            .expect("the small staged paste never rendered");
        evidence("human-paste-staged-raw", &raw);
        let raw_guard = super::pane_has_unsubmitted_input(&session).await;
        eprintln!(
            "HUMAN PASTE raw ({}B, tally={:?}): pane_has_unsubmitted_input={raw_guard}",
            small.len(),
            paste_tally(&raw)
        );
        assert!(
            !raw_guard,
            "the guard would blind-Enter a human's RAW typed/pasted text"
        );
        clear_composer(&session).await;

        // NOW the form that does draw placeholders.
        let human: String =
            std::iter::repeat_n("human pasted stack frame line, not a nudge. ", 120).collect();
        assert!(
            human.len() > 801,
            "the staged paste must cross the paste threshold"
        );
        send_keys_literal(&session, &human).await.expect("staging the human paste");
        let region = wait_until(&session, 15_000, |region| paste_tally(region).count > 0)
            .await
            .expect("the staged paste never rendered a placeholder");
        evidence("human-paste-staged", &region);

        let unattributed = super::pane_has_unsubmitted_input(&session).await;
        eprintln!(
            "HUMAN PASTE (no record): pane_has_unsubmitted_input={unattributed} tally={:?} \
squeezed={:?}",
            paste_tally(&region),
            region_squeezed(&region)
        );
        assert!(
            !unattributed,
            "THE HIGH-2 HOLE: the ATC guard would blind-Enter a human's staged paste"
        );

        // Same screen, but now with a record that predates it: this IS ours and
        // must be flushable.
        record_baseline(&session, PasteTally::default());
        let ours = super::pane_has_unsubmitted_input(&session).await;
        eprintln!("HUMAN PASTE (with a pre-send record): pane_has_unsubmitted_input={ours}");
        assert!(
            ours,
            "a placeholder that post-dates our own write must be flushable"
        );
        forget_baseline(&session);
        // Leave the pane clean WITHOUT submitting the staged text.
        clear_composer(&session).await;
    });
}

/// A busy target that has ACCEPTED the send renders `Press up to edit queued
/// messages`, dim. That must read `Submitted`, not `Pending`: reading it as
/// pending makes the caller skip the session forever.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_busy_queued_reads_submitted() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        // Give the model a slow but harmless turn, so the next send queues.
        let busy = format!("Count from 1 to 60 out loud, one number per line, then {PROBE_TAIL}");
        tmux_send(&session, &busy).await.expect("the busy-maker must be delivered");
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;

        let queued = heartbeat_shaped(1_700_000_000_400, 2);
        let result = tmux_send(&session, &queued).await;
        let pane = capture(&session).await;
        let region = composer_region(&pane).unwrap_or_default();
        evidence("busy-queued-region", &region);
        eprintln!(
            "BUSY QUEUED: result={result:?} clear={} state={:?}",
            region_is_clear(&region),
            composer_state(&pane, Attribution::Payload(&queued, Baseline::default()))
        );
        assert!(
            result.is_ok(),
            "a queued-but-accepted send was reported as a failure: {result:?}"
        );
        assert!(
            wait_for_idle(&session, 600_000).await,
            "the busy turn never finished"
        );
    });
}

/// THE ONE PATH THAT CAN REPORT `Ok` FOR A PARKED PAYLOAD.
///
/// [`Gate::NoComposer`] presses Enter once and reports `Ok` WITHOUT proof, and
/// `wait_for_ingest` settles there whenever [`composer_region`] returns `None`
/// on two consecutive captures. On a pane with no claude composer that is
/// correct and is parity with `main`, but on a REAL claude pane it would be a
/// false success, so the question this probe answers is empirical: does a real
/// 80x24 claude composer ever become unparseable while a large payload is being
/// ingested into it?
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_a_large_ingest_never_loses_the_composer() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        let payload = sized(1_700_000_000_800, 16_000);
        let started = WallClock::now();
        send_keys_literal(&session, &payload).await.expect("parking a 16000B payload");
        let (mut samples, mut unreadable, mut composerless) = (0_u32, 0_u32, 0_u32);
        while started.elapsed() < std::time::Duration::from_secs(12) {
            samples += 1;
            match capture_pane_ansi(&session, 0).await {
                Err(_) => unreadable += 1,
                Ok(pane) => {
                    if composer_region(&pane).is_none() {
                        composerless += 1;
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        eprintln!(
            "LARGE INGEST: {}B samples={samples} capture_failures={unreadable} \
composer_unparseable={composerless}",
            payload.len()
        );
        let parked = region_of(&session).await.expect("composer readable after the ingest");
        evidence("large-ingest-parked", &parked);
        assert_eq!(
            composer_state(
                &capture(&session).await,
                Attribution::Payload(&payload, Baseline::default())
            ),
            Verdict::Pending,
            "a PARKED 16000B payload must classify Pending"
        );
        assert_eq!(
            composerless, 0,
            "the composer became unparseable during the ingest, which is the one way a PARKED \
payload can still be reported Ok (Gate::NoComposer)"
        );
        clear_composer(&session).await;
    });
}

/// A LARGE payload into a BUSY claude: the shape that produced a spurious
/// `Err` for a payload that had in fact been delivered.
///
/// The old loop re-ran the whole 8s ingest gate on every Enter retry. A busy
/// pane that takes the turn but has not repainted at the 700ms check reads
/// `Pending`, the loop retried, and the gate then could not observe the payload
/// PRECISELY because it had gone through, so it timed out and reported
/// `SendFailure::Written`. The ATC gates `commit_delivery_on_send` on
/// `is_ok()`, so that false failure leaves its completions inbox undrained.
///
/// The transcript, not the screen, decides whether it was delivered.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_busy_large_payload_is_delivered_not_erred() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        let busy = format!("Count silently from 1 to 400, then {PROBE_TAIL}");
        tmux_send(&session, &busy).await.expect("the busy-maker must be delivered");
        tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

        let payload = sized(1_700_000_000_700, 8000);
        let started = WallClock::now();
        let result = tmux_send(&session, &payload).await;
        let elapsed = started.elapsed().as_millis();
        eprintln!(
            "BUSY LARGE: {}B ok={} elapsed={elapsed}ms",
            payload.len(),
            result.is_ok()
        );
        if let Err(error) = &result {
            eprintln!("  error: {error:#}");
        }
        assert!(
            wait_for_idle(&session, 900_000).await,
            "the busy turn never finished"
        );

        let exact = transcript_user_turns()
            .iter()
            .filter(|turn| turn.as_str() == payload.as_str())
            .count();
        eprintln!("BUSY LARGE: byte_exact_turns={exact}");
        assert_eq!(
            exact, 1,
            "the payload must be accepted exactly once, not {exact} times"
        );
        assert!(
            result.is_ok(),
            "A DELIVERED PAYLOAD WAS REPORTED AS A FAILURE: {result:?}"
        );
    });
}

/// Every user turn recorded in the session's own JSONL transcript.
///
/// The SCREEN is not proof of delivery for this case: a payload can be on it
/// and unsubmitted, or submitted and scrolled away. The transcript is what
/// claude actually accepted as a turn, so byte-exactness is checked there.
fn transcript_user_turns() -> Vec<String> {
    let Ok(path) = std::env::var("PROBE_TRANSCRIPT") else {
        panic!("PROBE_TRANSCRIPT must name the session's .jsonl transcript");
    };
    let raw = std::fs::read_to_string(path).expect("reading the transcript");
    raw.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|row| row["type"] == "user")
        .filter_map(|row| {
            let content = &row["message"]["content"];
            content.as_str().map(str::to_owned).or_else(|| {
                content.as_array().map(|parts| {
                    parts.iter().filter_map(|part| part["text"].as_str()).collect::<String>()
                })
            })
        })
        .collect()
}

/// TWO CONSECUTIVE HEARTBEATS BACK TO BACK INTO A BUSY TARGET.
///
/// The pair is what makes this hard: the two ticks have byte-identical tail
/// needles, and the target is busy, which is the measured condition under which
/// a CR fuses onto the payload's final read and is swallowed. Both bodies must
/// arrive INTACT and BYTE EXACT, and no send may report `Ok` for a payload the
/// session never accepted as a turn.
#[test]
#[ignore = "needs a live claude pane named by PROBE_SESSION"]
fn live_two_heartbeats_into_a_busy_target() {
    let Some(session) = probe_session() else {
        eprintln!("PROBE_SESSION unset, skipping");
        return;
    };
    rt().block_on(async {
        prepare(&session, 180_000).await;
        let before = transcript_user_turns().len();

        let busy = format!("Count silently from 1 to 400, then {PROBE_TAIL}");
        tmux_send(&session, &busy).await.expect("the busy-maker must be delivered");
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

        let first = heartbeat_shaped(1_700_000_000_500, 3);
        let second = heartbeat_shaped(1_700_000_000_501, 3);
        assert_eq!(
            super::payload_needle(&first),
            super::payload_needle(&second),
            "precondition: the two ticks share a byte-identical tail needle"
        );

        let started = WallClock::now();
        let first_result = tmux_send(&session, &first).await;
        let first_ms = started.elapsed().as_millis();
        let started = WallClock::now();
        let second_result = tmux_send(&session, &second).await;
        let second_ms = started.elapsed().as_millis();
        eprintln!(
            "BACK TO BACK: tick1 {}B ok={} in {first_ms}ms | tick2 {}B ok={} in {second_ms}ms",
            first.len(),
            first_result.is_ok(),
            second.len(),
            second_result.is_ok()
        );
        if let Err(error) = &first_result {
            eprintln!("  tick1 error: {error:#}");
        }
        if let Err(error) = &second_result {
            eprintln!("  tick2 error: {error:#}");
        }

        assert!(
            wait_for_idle(&session, 900_000).await,
            "the busy turn never finished"
        );
        let turns = transcript_user_turns();
        evidence(
            "back-to-back-transcript-tail",
            &turns[before.min(turns.len())..].join("\n---\n"),
        );
        for (label, payload, result) in [
            ("tick1", &first, &first_result),
            ("tick2", &second, &second_result),
        ] {
            let exact = turns.iter().filter(|turn| turn.as_str() == payload.as_str()).count();
            eprintln!("  {label}: ok={} byte_exact_turns={exact}", result.is_ok());
            if result.is_ok() {
                assert_eq!(
                    exact, 1,
                    "{label} was reported Ok but the session recorded it {exact} times, so the \
send path either lost it or delivered it twice"
                );
            }
        }
        assert!(
            first_result.is_ok(),
            "tick1 was not delivered: {first_result:?}"
        );
        assert!(
            second_result.is_ok(),
            "tick2 was not delivered: {second_result:?}"
        );
    });
}
