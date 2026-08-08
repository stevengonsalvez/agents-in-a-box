// ABOUTME: tmux send-keys + has-session wrappers. The payload goes out as ONE
// literal write, Enter is pressed only once the payload is observed in the
// composer, and the send is then verified against the pane before it is
// reported as delivered.
//
// Uses `-l` literal mode for the text to prevent prompt-injection via
// shell metacharacters; sends Enter as a key event afterwards.
//
// The payload is passed AFTER a `--` end-of-options terminator: without it a
// `text` that begins with `-` (e.g. an interview option label `-y` / `--no`, or
// a `fleet broadcast` prompt) is parsed by tmux as a flag rather than literal
// keys, silently dropping (or corrupting) the send.
//
// ---------------------------------------------------------------------------
// WHAT WAS MEASURED (claude 2.1.224 / tmux 3.6a / macOS; captures kept in
// `pane_fixtures/`, provenance in the test module below)
// ---------------------------------------------------------------------------
//
//   * A macOS PTY delivers at most 1022 bytes per `read()`, so one
//     `send-keys -l` of N bytes arrives as ceil(N/1022) reads.
//   * Claude Code classifies a single read of >=801 chars as a PASTE and draws
//     one `[Pasted text #N]` placeholder for it. Newlines are irrelevant.
//   * Whether the trailing Enter submits depends on whether the CR lands in its
//     OWN read. On an idle target it does (a 1-byte read a few ms later) and
//     the turn is submitted. On a busy target the CR is appended to bytes still
//     in flight, arrives INSIDE the payload's final read, and is consumed as a
//     literal newline inside the paste buffer, so nothing is submitted. The
//     composer then shows `[Pasted text #N +1 lines]`, where `+1 lines` IS the
//     swallowed Enter.
//   * Retrying Enter into a still-draining pane does not help: Enters at +1.0s,
//     +1.7s and +3.9s all arrived fused onto the same final read as `\r\r\r`.
//     There is no safe fixed delay; the wait must be OBSERVED, and it must be
//     observed ON THE PAYLOAD, since an untouched composer looks identical to
//     one that finished ingesting.
//   * A delay sweep over 900..16000 bytes showed a single unchunked write
//     submits cleanly at every size once the Enter is not fused. Chunking the
//     payload under the paste threshold therefore buys nothing the ingest gate
//     does not already buy, and it costs the `[Pasted text #N]` placeholder
//     that [`pane_has_unsubmitted_input`] (the ATC coalesce guard) relies on.
//     So: ONE write, exactly as `main` does.
//
// ---------------------------------------------------------------------------
// WHY THE VERIFICATION LOOKS AT EMPTINESS, NOT AT THE PAYLOAD
// ---------------------------------------------------------------------------
//
// `ainb run` creates every session at 80x24, and THE COMPOSER VIEWPORT IS
// TAIL-ANCHORED: once the content overflows, the head scrolls out of view and
// is not recoverable from the capture. Measured live at 80x24: a 1546-byte
// heartbeat-shaped payload rendered as 7 rows / 382 visible characters with the
// leading `[HEARTBEAT` marker OFF-SCREEN and no placeholder anywhere.
//
// Two consequences drive the whole design:
//
//   1. A needle taken from the payload's HEAD is invisible exactly when the bug
//      fires, so the primary post-send signal is that the composer is CLEAR,
//      and any needle is taken from the payload's TAIL.
//   2. "Clear" has to be DIM-aware. An idle Claude Code renders a dim ghost of
//      the previous prompt inside an EMPTY composer, and a BUSY one renders a
//      dim `Press up to edit queued messages` after it has ACCEPTED the send.
//      In plain `capture-pane -p` both are byte-identical in shape to real
//      typed text; `-e` keeps the `ESC [ 2 m` that tells them apart.

// ---------------------------------------------------------------------------
// WHY EVERY SIGNAL IS A CHANGE AGAINST A PRE-WRITE BASELINE
// ---------------------------------------------------------------------------
//
// "Does the composer contain something that looks like my payload" is NOT the
// same question as "did my payload arrive", and answering the first one is how
// the P0 comes back:
//
//   * EVERY ATC heartbeat ends in the same fixed literal, so the last
//     [`NEEDLE_CHARS`] characters of heartbeat N are byte-identical to those of
//     heartbeat N-1. A needle can never tell two of them apart.
//   * Claude Code renders a DIM ghost of the previous prompt inside an EMPTY
//     composer, so a pane that just accepted our text still SHOWS our text.
//   * A human pasting more than one PTY read's worth of characters draws the
//     same `[Pasted text #N]` a machine nudge does.
//
// So the ingest gate captures the composer BEFORE the write and accepts only a
// CHANGE against it: a LIVE (non-dim) occurrence of the needle that was not
// there before, or a paste placeholder that was not there before. Both counts
// are taken from the same [`composer_cells`] rendering, so "dim ghost" and
// "someone else's paste" are subtracted rather than credited to us.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use tokio::process::Command;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::time::{Instant, sleep};

use crate::fleet::read::tmux_pane::capture_pane_ansi;

/// Trailing non-whitespace characters of the payload used to recognise it
/// parked in the composer.
///
/// The TAIL, never the head. The composer viewport is tail-anchored, so the
/// tail is the one part of an arbitrarily long payload that is guaranteed to be
/// on screen. It is also the right end for the partial-submit symptom, where
/// the head is accepted and the tail is left stranded.
const NEEDLE_CHARS: usize = 32;

/// How often to re-capture the pane while waiting for the payload to appear.
const INGEST_POLL: Duration = Duration::from_millis(120);
/// Hard cap on the ingest gate. Measured ingest for 2400- and 8000-char
/// payloads was 0.53-0.55s, so this is ~15x headroom, not a tuning knob.
const INGEST_TIMEOUT: Duration = Duration::from_secs(8);
/// Consecutive UNCHANGED captures the placeholder branch needs before it will
/// call the ingest finished.
///
/// Sized against the measurement, not guessed. An 8000-byte payload ingested in
/// 0.53-0.55s over 8 PTY reads, i.e. ~66ms of screen change per read, and the
/// FIRST placeholder is drawn after read 1 of 8. At [`INGEST_POLL`] = 120ms a
/// single stall longer than one poll already produces two identical captures
/// with ~7KB still queued, which is why one quiet poll was not a settle at all.
/// Five consecutive unchanged captures is 600ms of stillness, which is longer
/// than the whole measured ingest of the largest payload we measured and ~9x
/// the measured per-read gap, so a pane that is still draining cannot reach it.
///
/// Why a quiet window rather than counting placeholders up to an expected
/// number: the placeholder count is NOT a function of the payload size. A PTY
/// read is capped at 1022 bytes but may be shorter, and Claude Code only draws
/// a placeholder for a read of >=801 characters, so the same 1546-byte payload
/// rendered as raw text with NO placeholder on four consecutive captures. Size
/// gives a sound UPPER bound only ([`paste_capacity`]), never a target.
const PLACEHOLDER_SETTLE: u32 = 5;
/// Wait after each Enter before checking whether the composer cleared.
const SUBMIT_CHECK_DELAY: Duration = Duration::from_millis(700);
/// Total Enter attempts (the first plus retries).
const ENTER_ATTEMPTS: u32 = 4;
/// Backoff between Enter attempts.
const RETRY_BACKOFF: Duration = Duration::from_secs(1);
/// Re-reads of an UNVERIFIED composer before the send gives up on proving
/// anything, and the gap between them.
const VERDICT_SETTLE_POLLS: u32 = 6;
const VERDICT_SETTLE_POLL: Duration = Duration::from_millis(250);
/// Attempts at reading the composer BEFORE the write. A pane that cannot be
/// read at all is not written to, so the router may still reach it elsewhere.
const BASELINE_CAPTURE_TRIES: u32 = 3;
const BASELINE_CAPTURE_RETRY: Duration = Duration::from_millis(50);

/// Characters a single PTY read must carry for Claude Code to classify it as a
/// paste and draw one `[Pasted text #N]` for it.
const PASTE_READ_CHARS: usize = 801;

/// How long a recorded pre-send paste tally stays usable.
///
/// Bounds the damage of a record that was never cleared (the process died
/// between the write and the verdict): after this, the payload-less pre-check
/// is back to having no baseline and stops attributing placeholders to us.
const BASELINE_RECORD_TTL_MS: u128 = 10 * 60 * 1000;

/// The squeezed form of Claude Code's paste placeholder. Matched against a
/// whitespace-stripped region so a placeholder that wrapped across two rows
/// still matches.
const PLACEHOLDER_MARK: &str = "[Pastedtext";
/// The ATC heartbeat's own marker, squeezed. Distinctive enough to be evidence
/// of an unsubmitted nudge with no payload in hand.
const HEARTBEAT_MARK: &str = "[HEARTBEAT";

/// Why a [`tmux_send`] failed, at the granularity the router needs.
///
/// The distinction is load-bearing: a payload that reached the pane must NOT be
/// re-sent over a second transport (it would arrive twice as soon as the
/// composer flushes), while a payload that was never written must be allowed to
/// fall back, or a pane that dies between the liveness check and the first
/// write is delivered by no transport at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SendFailure {
    /// Not one byte reached the pane: the `send-keys` invocation itself failed,
    /// so tmux never handed the payload to the pane. Another transport may
    /// safely deliver the same text.
    NothingWritten,
    /// The text is in the pane, but the turn was not submitted, or we could not
    /// prove that it was. Nothing else may send the same text.
    Written,
}

/// A [`tmux_send`] failure carrying [`SendFailure`], so callers branch on a
/// type rather than on the wording of a message.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SendError {
    failure: SendFailure,
    detail: String,
}

impl SendError {
    fn nothing_written(detail: impl Into<String>) -> Self {
        Self {
            failure: SendFailure::NothingWritten,
            detail: detail.into(),
        }
    }

    fn written(detail: impl Into<String>) -> Self {
        Self {
            failure: SendFailure::Written,
            detail: detail.into(),
        }
    }

    /// Whether the payload reached the pane.
    pub const fn failure(&self) -> SendFailure {
        self.failure
    }
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for SendError {}

/// Per-pane send locks.
///
/// Two senders overlapping on one pane produce a single read shaped
/// `[tail of A][CR][head of B]`: Claude submits at the embedded CR (so A is
/// acted on) and the head of B is left parked in the now-empty composer. That
/// is the measured "partially submitted" symptom, and only mutual exclusion
/// across the whole write + Enter + verify sequence prevents it.
///
/// In-process only: a send issued by a different `ainb` process (e.g. the ATC
/// daemon while a CLI broadcast runs) can still interleave.
static SEND_LOCKS: LazyLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn lock_pane(tmux_session: &str) -> OwnedMutexGuard<()> {
    let lock = {
        let mut locks = SEND_LOCKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            locks
                .entry(tmux_session.to_string())
                .or_insert_with(|| Arc::new(AsyncMutex::new(()))),
        )
    };
    lock.lock_owned().await
}

/// Build the `tmux send-keys` argv for a literal payload. The `--` terminator
/// sits between the flags and `text` so a `-`-prefixed payload (e.g. an
/// interview option label like `-y`/`--no`, or a broadcast prompt) is parsed as
/// literal keys, not a tmux flag.
const fn send_keys_literal_args<'a>(tmux_session: &'a str, text: &'a str) -> [&'a str; 6] {
    ["send-keys", "-t", tmux_session, "-l", "--", text]
}

fn picker_key_args<'a>(tmux_session: &'a str, key: &'a str) -> Option<[&'a str; 4]> {
    let allowed = matches!(
        key,
        "0" | "1"
            | "2"
            | "3"
            | "4"
            | "5"
            | "6"
            | "7"
            | "8"
            | "9"
            | "Enter"
            | "Up"
            | "Down"
            | "Left"
            | "Right"
            | "Space"
            | "Tab"
            | "BTab"
    );
    allowed.then_some(["send-keys", "-t", tmux_session, key])
}

async fn send_keys_literal(tmux_session: &str, text: &str) -> Result<()> {
    let status = Command::new("tmux")
        .args(send_keys_literal_args(tmux_session, text))
        .status()
        .await
        .context("invoking tmux send-keys (literal)")?;
    if !status.success() {
        anyhow::bail!("tmux send-keys -l exited {status}");
    }
    Ok(())
}

/// What the pane says about a payload we just wrote.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Verdict {
    /// The composer is clear: the turn was accepted.
    Submitted,
    /// The payload (or a paste placeholder for it) is still in the composer.
    Pending,
    /// The pane could not be read, has no composer to read, or holds content we
    /// cannot attribute. NOTHING may be claimed either way.
    Unverified,
}

/// The paste placeholders visible in a composer.
///
/// Both fields are needed. `count` is the obvious one and is what a human's
/// staged paste inflates; `max_index` is the `#N` Claude Code stamps into each
/// placeholder, which MATTERS because the composer viewport is tail-anchored:
/// an early placeholder scrolls out of view and the count goes DOWN while the
/// pane is gaining pastes. The index does not, so growth is recognised either
/// way.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct PasteTally {
    count: usize,
    max_index: u64,
}

impl PasteTally {
    /// Did the pane gain a placeholder relative to `base`?
    ///
    /// A bare "is a placeholder present" is exactly the HIGH-2 hole: a human's
    /// pasted stack trace draws the same glyphs a machine nudge does, so only
    /// an INCREASE over what was on screen before our write can be ours.
    const fn grew_beyond(self, base: Self) -> bool {
        self.max_index > base.max_index || self.count > base.count
    }
}

/// What the composer already held immediately BEFORE our write.
///
/// Everything the ingest gate accepts is measured against this, so residue (a
/// dim ghost of the previous prompt, a previous heartbeat with an identical
/// tail, a human's staged paste) cannot be read as our payload arriving.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct Baseline {
    /// LIVE (non-dim) occurrences of this payload's own needle. Live-only is
    /// load-bearing: Claude Code's ghost suggestion is a DIM copy of the
    /// previous prompt, so counting it would make the ghost's disappearance
    /// (as our text replaces it) look like no change at all.
    needles: usize,
    pastes: PasteTally,
}

impl Baseline {
    /// `None` means there was no composer to read, which is a baseline of
    /// nothing rather than an unknown: the gate's composer-less path does not
    /// consult it.
    fn of(region: Option<&str>, payload: &str) -> Self {
        region.map_or_else(Self::default, |region| Self {
            needles: count_live_needles(region, payload),
            pastes: paste_tally(region),
        })
    }
}

/// The state of the ingest gate: whether it is safe to press Enter yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Gate {
    /// The payload is visibly in the composer, so the pane has finished
    /// ingesting it and a CR now lands in its own read.
    PayloadVisible,
    /// This pane has no Claude Code composer to read at all (a non-Claude TUI,
    /// or a bare shell). The write cannot be verified here, but the pane has
    /// stopped changing, so Enter is no more dangerous than it is on `main`.
    NoComposer,
    /// A composer exists and never showed the payload within the timeout.
    /// Enter must NOT be pressed: firing into a pane that has not started
    /// draining is exactly the CR-fusion condition.
    NotObserved,
}

/// What a settled [`Gate`] means for the attempt loop.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum GateAction {
    /// Press Enter, then verify against the pane that the composer cleared.
    /// The normal path, and the only one that can report proven delivery.
    PressAndVerify,
    /// Press Enter ONCE and report `Ok`, without proof.
    PressAndAccept,
    /// Do NOT press Enter, and fail. This is the CR-fusion condition.
    Refuse,
}

/// The gate-to-action mapping, as a pure function so both halves of the
/// contract can be pinned by a test.
///
/// WHY `NoComposer` ACCEPTS. On this path the wire behaviour is provably
/// identical to `main`: one literal write, one Enter, nothing else. There is no
/// Claude composer on the pane, so there is no composer for a CR to fuse into
/// and nothing that can be parked in one. Reporting `Failed` for a pane that
/// behaves exactly as it did on `main` is a reporting regression, not a safety
/// win, and it has teeth: the hangar daemon maps any `Err` to
/// `ActionReceiptStatus::Failed`, and the ATC gates `commit_delivery_on_send` on
/// `send_result.is_ok()`, so a pane that permanently lands here would never have
/// its inbox drained and would be re-sent the same completions on every
/// heartbeat, forever.
///
/// THE RESIDUAL HOLE, stated rather than hidden. A REAL Claude pane that is
/// momentarily unparseable (a boot splash, a full-screen dialog drawn over the
/// composer) also settles on [`Gate::NoComposer`], and is then reported `Ok`
/// with no proof that the turn was submitted. That is PARITY with `main`, not an
/// improvement on it. Closing it needs a distinct delivered-unverified state
/// threaded through `route.rs`, `SendOutcome` and the hangar wire protocol, so
/// that callers can tell "submitted, proven" from "written, unproven" instead of
/// being handed a two-valued result. That is deliberately not built here.
///
/// [`Gate::NotObserved`] keeps returning `Err`, and must: it is the genuine
/// CR-fusion condition on a real Claude pane, where the payload is still
/// draining and an Enter would be swallowed into the paste buffer. This is a
/// narrow exemption for the composer-less pane, NOT a retreat from verification.
const fn gate_action(gate: Gate) -> GateAction {
    match gate {
        Gate::PayloadVisible => GateAction::PressAndVerify,
        Gate::NoComposer => GateAction::PressAndAccept,
        Gate::NotObserved => GateAction::Refuse,
    }
}

/// Write `text` into the session's composer and submit it, verifying against
/// the pane that the composer actually cleared.
///
/// `Ok(())` means "typed AND submitted". Every other case is an `Err` carrying
/// a [`SendError`], whose [`SendFailure`] says whether anything reached the
/// pane. Callers must not treat any `Err` as delivery.
///
/// KNOWN LIMIT, stated rather than hidden: verification is Claude-Code-shaped.
/// A pane with no `─`-delimited composer (a Codex/Gemini/Copilot TUI, a bare
/// shell) is submitted to exactly as `main` does, one write and one Enter, and
/// reported `Ok` WITHOUT proof, because there is nothing on such a pane to prove
/// against. See [`gate_action`] for why that is parity rather than a regression,
/// and for the residual hole it leaves.
pub async fn tmux_send(tmux_session: &str, text: &str) -> Result<()> {
    let _guard = lock_pane(tmux_session).await;

    // THE BASELINE, taken BEFORE a single byte goes out. Without it the gate
    // below can only ask "is something that looks like my payload on screen",
    // which a dim ghost of the previous prompt, a previously parked heartbeat
    // with an identical tail, or a human's staged paste all answer `yes` to.
    //
    // A pane we cannot read is not written to at all: reporting
    // `NothingWritten` here keeps the broker fallback available, where writing
    // first and failing to verify afterwards would suppress it.
    let Some(before) = composer_region_now(tmux_session).await else {
        return Err(SendError::nothing_written(format!(
            "send to {tmux_session} wrote nothing: its pane could not be read before the write, so \
there is no baseline to verify a send against"
        ))
        .into());
    };
    let baseline = Baseline::of(before.as_deref(), text);
    record_baseline(tmux_session, baseline.pastes);

    // ONE literal write, as `main` does: chunking under the paste classifier
    // would suppress the `[Pasted text #N]` placeholder that the ATC coalesce
    // guard reads. A non-zero exit here means tmux never handed the keys to the
    // pane, so no bytes were written and the broker may still try.
    if let Err(error) = send_keys_literal(tmux_session, text).await {
        forget_baseline(tmux_session);
        return Err(SendError::nothing_written(format!(
            "send to {tmux_session} wrote nothing: {error:#}"
        ))
        .into());
    }

    // The gate runs ONCE, before the first Enter. It exists to keep a CR out of
    // a still-draining pane, and once the payload has been observed that
    // condition is over, so the retries below are driven by the VERDICT rather
    // than by re-running the gate. Re-running it was itself a defect: on a
    // retry the payload is gone from the composer precisely when it WAS
    // delivered, so the gate could not observe it, waited out the full
    // INGEST_TIMEOUT and reported a delivered payload as a failure, which left
    // the ATC inbox undrained.
    match gate_action(wait_for_ingest(tmux_session, text, &baseline).await) {
        GateAction::Refuse => {
            return Err(SendError::written(format!(
                "send to {tmux_session} was written but never appeared in the composer within \
{INGEST_TIMEOUT:?}; Enter was NOT pressed, so the payload may still be draining into the pane"
            ))
            .into());
        }
        GateAction::PressAndAccept => {
            // Nothing on this pane can distinguish a swallowed Enter from an
            // accepted one, and a second Enter would submit a second turn, so
            // the retry budget is not spent here: one write, one Enter, exactly
            // as `main` sends. That is also why this reports `Ok` rather than a
            // failure. See [`gate_action`] for the full rationale and for the
            // residual hole (a real Claude pane that is momentarily unparseable
            // is accepted here without proof, which is parity with `main`).
            return match press_enter_retrying(tmux_session).await {
                Ok(()) => {
                    forget_baseline(tmux_session);
                    Ok(())
                }
                Err(error) => Err(SendError::written(format!(
                    "send to {tmux_session} was typed into the composer but Enter failed: {error:#}"
                ))
                .into()),
            };
        }
        GateAction::PressAndVerify => {}
    }

    let mut last = Verdict::Pending;
    let mut pressed = 0_u32;
    for attempt in 1..=ENTER_ATTEMPTS {
        // A transient Enter failure must not defeat the retry budget it exists
        // to absorb, so only the last attempt propagates the error.
        if let Err(error) = tmux_press_enter(tmux_session).await {
            if attempt == ENTER_ATTEMPTS {
                return Err(SendError::written(format!(
                    "send to {tmux_session} was typed into the composer but Enter failed: {error:#}"
                ))
                .into());
            }
            sleep(RETRY_BACKOFF).await;
            continue;
        }
        pressed += 1;
        sleep(SUBMIT_CHECK_DELAY).await;
        last = composer_verdict(tmux_session, text, &baseline).await;
        if last == Verdict::Unverified {
            // One unreadable or mid-redraw frame must not turn a DELIVERED
            // payload into a failure, and the loop will not press Enter again
            // from here, so this bounded re-read is the last chance to
            // recognise the submit.
            last = settle_verdict(tmux_session, text, &baseline).await;
        }
        match last {
            Verdict::Submitted => {
                forget_baseline(tmux_session);
                return Ok(());
            }
            // The composer holds live content we cannot attribute: a human
            // started typing into it. Another Enter would submit THEIR text, so
            // the retry budget stops here rather than being spent on a key
            // press that is unsafe by construction.
            Verdict::Unverified => break,
            Verdict::Pending => {}
        }
        if attempt < ENTER_ATTEMPTS {
            sleep(RETRY_BACKOFF).await;
        }
    }

    // `pressed`, not `ENTER_ATTEMPTS`: the loop stops early when the composer
    // fills with content we cannot attribute, and a message claiming four
    // attempts after one would send the next reader hunting for three key
    // presses that never happened.
    Err(SendError::written(if last == Verdict::Pending {
        format!(
            "send to {tmux_session} was typed into the composer but NOT submitted after \
{pressed} Enter attempts (the payload is still parked in the composer)"
        )
    } else {
        format!(
            "send to {tmux_session} could not be verified after {pressed} Enter attempts \
(the composer could not be read or holds content we cannot attribute, so the payload may be \
parked in it)"
        )
    })
    .into())
}

/// Press Enter in the target session (used both to submit a send and to flush
/// a previously-parked paste).
pub async fn tmux_press_enter(tmux_session: &str) -> Result<()> {
    let enter = Command::new("tmux")
        .args(["send-keys", "-t", tmux_session, "Enter"])
        .status()
        .await
        .context("invoking tmux send-keys Enter")?;
    if !enter.success() {
        anyhow::bail!("tmux send-keys Enter exited {enter}");
    }
    Ok(())
}

/// Route one constrained picker key without converting it to generic text.
pub async fn tmux_send_picker_key(tmux_session: &str, key: &str) -> Result<()> {
    let args = picker_key_args(tmux_session, key)
        .ok_or_else(|| anyhow::anyhow!("unsupported verified picker key: {key}"))?;
    let status = Command::new("tmux")
        .args(args)
        .status()
        .await
        .context("invoking tmux send-keys for verified picker")?;
    if !status.success() {
        anyhow::bail!("tmux verified picker send-keys exited {status}");
    }
    Ok(())
}

/// Press Enter, absorbing a transient tmux failure.
///
/// Used only on the composer-less path, where the verify loop (and with it its
/// own retry handling) does not run.
async fn press_enter_retrying(tmux_session: &str) -> Result<()> {
    for _ in 1..ENTER_ATTEMPTS {
        if tmux_press_enter(tmux_session).await.is_ok() {
            return Ok(());
        }
        sleep(RETRY_BACKOFF).await;
    }
    tmux_press_enter(tmux_session).await
}

async fn composer_verdict(tmux_session: &str, payload: &str, baseline: &Baseline) -> Verdict {
    (capture_pane_ansi(tmux_session, 0).await).map_or(Verdict::Unverified, |pane| {
        composer_state(&pane, Attribution::Payload(payload, *baseline))
    })
}

/// Re-read an UNVERIFIED composer a bounded number of times.
///
/// Returns as soon as the pane says anything definite. `Unverified` is the
/// verdict for a capture that failed and for a frame caught mid-redraw as well
/// as for a human's text, and only the last of those is permanent, so a single
/// sample of it is not a result.
async fn settle_verdict(tmux_session: &str, payload: &str, baseline: &Baseline) -> Verdict {
    for _ in 0..VERDICT_SETTLE_POLLS {
        sleep(VERDICT_SETTLE_POLL).await;
        let verdict = composer_verdict(tmux_session, payload, baseline).await;
        if verdict != Verdict::Unverified {
            return verdict;
        }
    }
    Verdict::Unverified
}

/// Read the pane's composer region now.
///
/// `None` is "the pane could not be read at all", which is NOT the same as
/// `Some(None)`, "the pane was read and has no composer".
async fn composer_region_now(tmux_session: &str) -> Option<Option<String>> {
    for attempt in 1..=BASELINE_CAPTURE_TRIES {
        if let Ok(pane) = capture_pane_ansi(tmux_session, 0).await {
            return Some(composer_region(&pane));
        }
        if attempt < BASELINE_CAPTURE_TRIES {
            sleep(BASELINE_CAPTURE_RETRY).await;
        }
    }
    None
}

/// Poll until the payload is VISIBLY in the composer.
///
/// A "two identical captures" settle cannot do this job: on a busy target that
/// has not started draining, the composer is unchanged (and usually empty),
/// which scores as stable, so the first Enter goes into a still-draining pane,
/// which is the exact CR-fusion condition. The gate therefore keys on the
/// PAYLOAD.
///
/// Two acceptance shapes, because a payload can be invisible as text, and BOTH
/// are measured against `baseline`, never against an empty expectation:
///
///   * a LIVE occurrence of its tail needle that was not on screen before the
///     write, which proves the LAST bytes have landed and is not satisfied by a
///     dim ghost of the same text or by the identical tail of the PREVIOUS
///     heartbeat still parked in the composer;
///   * or a paste placeholder that was not on screen before the write, on a
///     payload long enough to produce one, once the region has stopped changing
///     for [`PLACEHOLDER_SETTLE`] consecutive captures. Measured at 80x24: a
///     900-byte write rendered as a bare `[Pasted text #3]` with no payload text
///     visible at all, so in that band the needle can never appear.
async fn wait_for_ingest(tmux_session: &str, payload: &str, baseline: &Baseline) -> Gate {
    let deadline = Instant::now() + INGEST_TIMEOUT;
    let mut previous: Option<Option<String>> = None;
    let mut settled: u32 = 0;
    let mut saw_composer = false;
    loop {
        if let Ok(pane) = capture_pane_ansi(tmux_session, 0).await {
            let region = composer_region(&pane);
            let unchanged = previous.as_ref() == Some(&region);
            settled = if unchanged { settled + 1 } else { 0 };
            match &region {
                Some(region) => {
                    saw_composer = true;
                    if ingest_observed(region, payload, baseline, settled) {
                        return Gate::PayloadVisible;
                    }
                }
                // No composer to read. Give the pane one quiet round anyway, so
                // a bare pane still gets its Enter the way `main` sent it.
                None if unchanged => return Gate::NoComposer,
                None => {}
            }
            previous = Some(region);
        }
        if Instant::now() >= deadline {
            return if saw_composer {
                Gate::NotObserved
            } else {
                Gate::NoComposer
            };
        }
        sleep(INGEST_POLL).await;
    }
}

/// Has OUR write reached the composer? The ingest gate as a pure function of
/// the region, the payload, the pre-write baseline and how many consecutive
/// captures the region has been identical for.
///
/// Every clause is a CHANGE against `baseline`. `region` alone can never answer
/// this: the ATC appends the same fixed literal to every heartbeat, so two
/// consecutive nudges share a byte-identical needle, and a `[Pasted text #N]` is
/// drawn the same way whether a machine or a human put it there.
fn ingest_observed(region: &str, payload: &str, baseline: &Baseline, settled: u32) -> bool {
    if count_live_needles(region, payload) > baseline.needles {
        return true;
    }
    // A payload too short to fill one paste-classified read cannot have drawn a
    // placeholder, so any placeholder appearing during such a send belongs to
    // someone else and must not gate our Enter.
    paste_capacity(payload) > 0
        && paste_tally(region).grew_beyond(baseline.pastes)
        && settled >= PLACEHOLDER_SETTLE
}

/// The most paste placeholders `payload` could possibly account for.
///
/// Claude Code draws one placeholder per PTY read of >= [`PASTE_READ_CHARS`]
/// characters, so each placeholder consumes at least that many characters of
/// the payload. This is a sound UPPER bound and nothing more: reads are capped
/// at 1022 bytes but may be shorter, which is why a 1546-byte payload rendered
/// with no placeholder at all on four consecutive live captures.
fn paste_capacity(payload: &str) -> usize {
    payload.chars().count() / PASTE_READ_CHARS
}

/// True when the session's visible composer still holds an unsubmitted nudge.
///
/// THE PAYLOAD-LESS PRE-CHECK, deliberately weaker than the post-send
/// verification. The ATC coalesce guard calls this and responds by pressing a
/// LONE Enter, so it must recognise only shapes that cannot be anything but an
/// unsubmitted machine nudge: a paste placeholder or a raw `[HEARTBEAT` line.
/// Text a human is mid-typing must NOT be reported, or the guard submits their
/// half-written prompt. `composer_pending(pane, Some(payload))` is the strict
/// form and is the one [`tmux_send`] uses.
///
/// Capture failure degrades to `false` (assume submitted): this guards an
/// idempotent pre-check, and reporting `true` for a pane that cannot be read at
/// all would skip every subsequent tick forever. The load-bearing verification
/// in [`tmux_send`] does NOT use this function; it fails loudly instead.
///
/// ATTRIBUTION, and its exact limit. A `[Pasted text #N]` on its own says
/// nothing about WHO put it there: a human pasting a stack trace or a diff
/// draws the identical glyphs, and answering it with a lone Enter submits their
/// staged text. So a placeholder counts here only when it POST-DATES the last
/// write [`tmux_send`] made to this pane, which is what
/// [`record_baseline`] leaves behind. With no such record the placeholder is
/// not ours to flush and this reports `false`.
///
/// LIMIT, for the same reason the strict check needs the payload: at 80x24 a
/// parked heartbeat that renders as raw text keeps its `[HEARTBEAT` head OFF
/// SCREEN, so this returns `false` for it. Widening the match to "composer is
/// not empty" is not available here: that is precisely the human-typing case.
pub async fn pane_has_unsubmitted_input(tmux_session: &str) -> bool {
    let baseline = recorded_baseline(tmux_session);
    capture_pane_ansi(tmux_session, 0)
        .await
        .is_ok_and(|pane| composer_pending(&pane, Attribution::Precheck(baseline)))
}

/// The paste tally a pane carried immediately before our last write to it.
///
/// WHY THIS IS ON DISK. The payload-less pre-check needs a pre-send baseline for
/// exactly the same reason the ingest gate does, and it cannot be handed one by
/// its caller: the ATC heartbeat is fired by launchd/systemd as a FRESH `ainb
/// fleet atc heartbeat <name>` process on every tick, so the tick that asks
/// "is the previous nudge still parked" is never the process that sent it. An
/// in-process map would be empty every time it was consulted.
///
/// Deliberately not a store with a schema: one small file per pane, holding the
/// time, the placeholder count and the highest `#N`, written best-effort and
/// ignored once [`BASELINE_RECORD_TTL_MS`] old. Every failure path degrades to
/// "no baseline", which makes the pre-check report `false`, which is the safe
/// direction: a nudge is re-sent (heartbeats are idempotent snapshots) instead
/// of a stranger's composer being submitted.
fn baseline_record_path(tmux_session: &str) -> Option<PathBuf> {
    let base = std::env::var_os("AINB_HOME").map(PathBuf::from).or_else(dirs::home_dir)?;
    // Hashed rather than sanitised: a tmux target can carry `:`, `.` and `/`
    // (`sess:0.0`), and two different targets must never collide onto one file.
    let key = blake3::hash(tmux_session.as_bytes()).to_hex();
    Some(
        base.join(".agents-in-a-box")
            .join("fleet")
            .join("composer-baseline")
            .join(format!("{key}.txt")),
    )
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis())
}

fn record_baseline(tmux_session: &str, pastes: PasteTally) {
    let Some(path) = baseline_record_path(tmux_session) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
        sweep_expired_baselines(dir);
    }
    let _ = std::fs::write(
        &path,
        format!("{} {} {}", now_ms(), pastes.count, pastes.max_index),
    );
}

/// Delete records that are past the TTL.
///
/// A record is cleared when the send is PROVEN delivered, so the ones that
/// survive belong to sends that could not be verified, and their panes die long
/// before the files would. Without this they accumulate one per pane forever;
/// the sweep bounds the directory at the panes written to in the last
/// [`BASELINE_RECORD_TTL_MS`]. Reading an already-expired record is harmless on
/// its own (it is ignored), so every error here is ignored too.
fn sweep_expired_baselines(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = now_ms();
    for entry in entries.flatten() {
        let path = entry.path();
        let expired = std::fs::read_to_string(&path)
            .ok()
            .is_none_or(|raw| parse_baseline_record(&raw, now).is_none());
        if expired {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Drop the record once the composer has been PROVEN clear.
///
/// Load-bearing, not tidiness: a record left behind after a successful send
/// would make a human's later paste look like growth over it, which is the very
/// mis-attribution this whole mechanism exists to prevent.
fn forget_baseline(tmux_session: &str) {
    if let Some(path) = baseline_record_path(tmux_session) {
        let _ = std::fs::remove_file(path);
    }
}

fn recorded_baseline(tmux_session: &str) -> Option<PasteTally> {
    let raw = std::fs::read_to_string(baseline_record_path(tmux_session)?).ok()?;
    parse_baseline_record(&raw, now_ms())
}

fn parse_baseline_record(raw: &str, now: u128) -> Option<PasteTally> {
    let mut fields = raw.split_whitespace();
    let written: u128 = fields.next()?.parse().ok()?;
    let count: usize = fields.next()?.parse().ok()?;
    let max_index: u64 = fields.next()?.parse().ok()?;
    (now.saturating_sub(written) <= BASELINE_RECORD_TTL_MS)
        .then_some(PasteTally { count, max_index })
}

/// A full-width horizontal rule row (`─` repeated). Claude Code draws the
/// composer as a block between two of them.
fn is_rule_row(line: &str) -> bool {
    let trimmed = line.trim();
    let mut count = 0;
    for ch in trimmed.chars() {
        if ch != '─' {
            return false;
        }
        count += 1;
    }
    count >= 20
}

/// Drop ANSI escape sequences, keeping the printable text.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            skip_escape(&mut chars);
        } else {
            out.push(ch);
        }
    }
    out
}

/// Consume the remainder of an escape sequence, returning its SGR parameters
/// when it was one (`ESC [ ... m`).
fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    if chars.peek() != Some(&'[') {
        // Not a CSI. Drop one byte and move on; capture-pane emits almost
        // nothing else, and guessing further would risk eating real text.
        chars.next();
        return None;
    }
    chars.next();
    let mut params = String::new();
    for ch in chars.by_ref() {
        if ch.is_ascii_digit() || ch == ';' {
            params.push(ch);
        } else {
            return (ch == 'm').then_some(params);
        }
    }
    None
}

/// Does this row start with the composer's prompt glyph?
fn starts_with_prompt(line: &str) -> bool {
    let rest = line.trim_start_matches(['│', ' ', '\t']);
    rest.starts_with(['>', '❯', '›'])
}

/// The composer block: the rows between the closest rule pair that actually
/// brackets a prompt row.
///
/// Not simply "the last two rules": Claude Code draws other full-width rules
/// (a slash-command menu, a dialog) BELOW the composer, and taking the last
/// pair unconditionally then points the region at that block instead, where the
/// payload is guaranteed absent. Requiring the region's first row to carry the
/// prompt glyph pins it to the composer.
///
/// The composer WRAPS, and only its first row carries `❯` (followed by U+00A0,
/// not a plain space); continuation rows have no prefix at all, which is why
/// the region is scanned whole rather than one prompt-prefixed line.
///
/// Rows are returned with their ANSI attributes intact, because DIM is what
/// separates a ghost suggestion from real typed text.
fn composer_region(pane: &str) -> Option<String> {
    let lines: Vec<&str> = pane.lines().collect();
    let plain: Vec<String> = lines.iter().map(|line| strip_ansi(line)).collect();
    let rules: Vec<usize> = plain
        .iter()
        .enumerate()
        .filter(|(_, line)| is_rule_row(line))
        .map(|(idx, _)| idx)
        .collect();
    rules.windows(2).rev().find_map(|pair| {
        let (top, bottom) = (pair[0], pair[1]);
        (bottom > top + 1 && starts_with_prompt(&plain[top + 1]))
            .then(|| lines[top + 1..bottom].join("\n"))
    })
}

/// One visible character of the composer plus whether it was drawn DIM.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cell {
    ch: char,
    dim: bool,
}

/// The composer's content: every non-whitespace character after the prompt
/// glyph, each tagged with the DIM attribute it was drawn with.
///
/// DIM state is tracked per row and reset at each row boundary: `capture-pane
/// -e` re-emits the attributes in force at the start of every line, so a row
/// carries its own state and leaking one row's attribute into the next would
/// mis-tag wrapped content.
///
/// THE PROMPT GLYPH IS SKIPPED ONCE, on the region's FIRST row, because that is
/// the only row Claude Code draws it on; continuation rows carry an indent and
/// nothing else. Re-running the skip per row ate the first non-blank character
/// of any wrapped row THAT HAPPENED TO BE ONE OF THE GLYPHS (measured: an
/// ordinary character was kept, so this is narrower than "every wrapped row",
/// and it is not narrow enough to be harmless). A payload line beginning with
/// `>` is routine: a Slack or Discord reply the bridge relays quoted with `> `,
/// a markdown blockquote in a broadcast, a `>>>>>>>` conflict marker. That
/// character was then missing from the REGION while still present in the
/// NEEDLE, so the payload could not recognise itself, the ingest gate never
/// accepted, and `tmux_send` returned `Err` BEFORE Enter was ever pressed.
fn composer_cells(region: &str) -> Vec<Cell> {
    let mut cells = Vec::new();
    let mut prompt_pending = true;
    for line in region.lines() {
        let mut dim = false;
        let mut chars = line.chars().peekable();
        let mut at_row_start = true;
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                if let Some(params) = skip_escape(&mut chars) {
                    apply_sgr(&params, &mut dim);
                }
                continue;
            }
            if at_row_start {
                // Leading box drawing / indent is never content, on any row.
                if ch.is_whitespace() || ch == '\u{a0}' || ch == '│' {
                    continue;
                }
                at_row_start = false;
                if std::mem::take(&mut prompt_pending) && matches!(ch, '>' | '❯' | '›') {
                    continue;
                }
            }
            if !ch.is_whitespace() && ch != '\u{a0}' {
                cells.push(Cell { ch, dim });
            }
        }
    }
    cells
}

/// Apply one SGR parameter list to the DIM flag. `0` (reset) and `22` (normal
/// intensity) clear it, `2` sets it; everything else (colours in particular) is
/// irrelevant here. Note the real captures interleave them, e.g.
/// `ESC[2m ESC[39m Press up to edit queued messages`, so colour codes must not
/// be treated as a reset.
fn apply_sgr(params: &str, dim: &mut bool) {
    if params.is_empty() {
        *dim = false;
        return;
    }
    for param in params.split(';') {
        match param {
            "" | "0" | "22" => *dim = false,
            "2" => *dim = true,
            _ => {}
        }
    }
}

/// True when the composer holds no LIVE content.
///
/// Empty, or entirely dim. Dim covers both traps at once: Claude Code's ghost
/// of a previous prompt in an idle composer, and `Press up to edit queued
/// messages` on a busy session that has already ACCEPTED the send. Reading
/// either as pending makes a caller skip that session forever.
fn region_is_clear(region: &str) -> bool {
    let cells = composer_cells(region);
    cells.is_empty() || cells.iter().all(|cell| cell.dim)
}

/// The composer's content with all whitespace removed, so a needle matches
/// across the row wrapping that `capture-pane` bakes in.
fn region_squeezed(region: &str) -> String {
    composer_cells(region).into_iter().map(|cell| cell.ch).collect()
}

/// The composer's LIVE content only, i.e. with every DIM cell dropped.
///
/// The ingest gate counts needles over this rather than over
/// [`region_squeezed`]: Claude Code's ghost suggestion is a dim copy of the
/// previous prompt, so a payload equal to the previous prompt (an ATC heartbeat
/// re-sent after the last one was accepted) matches the ghost character for
/// character in the squeezed form, and the same region then reads as both
/// "empty, so the send was accepted" and "the payload is visibly in the
/// composer, press Enter now".
fn region_live_squeezed(region: &str) -> String {
    composer_cells(region)
        .into_iter()
        .filter(|cell| !cell.dim)
        .map(|cell| cell.ch)
        .collect()
}

/// How many times this payload's tail needle is LIVE in the composer.
fn count_live_needles(region: &str, payload: &str) -> usize {
    payload_needle(payload).map_or(0, |needle| {
        region_live_squeezed(region).matches(needle.as_str()).count()
    })
}

/// Tally the paste placeholders in a composer region.
fn paste_tally(region: &str) -> PasteTally {
    tally_pastes(&region_squeezed(region))
}

/// Tally placeholders in already-squeezed text, so both the region form and the
/// last-prompt-line fallback share one parser.
fn tally_pastes(squeezed: &str) -> PasteTally {
    let mut tally = PasteTally::default();
    for rest in squeezed.split(PLACEHOLDER_MARK).skip(1) {
        tally.count += 1;
        // `[Pasted text #12 +5 lines]`, squeezed to `[Pastedtext#12+5lines]`.
        let digits: String = rest
            .strip_prefix('#')
            .unwrap_or_default()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(index) = digits.parse::<u64>() {
            tally.max_index = tally.max_index.max(index);
        }
    }
    tally
}

fn squeeze(text: &str) -> String {
    text.chars().filter(|c| !c.is_whitespace() && *c != '\u{a0}').collect()
}

/// A distinctive, wrap-proof fragment of the payload: its LAST
/// [`NEEDLE_CHARS`] non-whitespace characters.
///
/// The tail, for two independent reasons. The composer viewport is
/// tail-anchored, so on a real 80x24 pane a long payload's head is simply not
/// on screen. And in the partial-submit symptom the head is the part that got
/// ACCEPTED while the tail is what stayed stranded, so a head needle is missing
/// exactly when the send failed.
///
/// There is NO minimum length. A one-character payload (`2`, the reply to an
/// attention push offering numbered options) is verified like any other: a
/// short needle is weak evidence of PENDING, but the primary signal is
/// emptiness, which is strictly stronger and does not depend on the needle at
/// all. `None` comes back only for a payload with no non-whitespace character,
/// which cannot be recognised on a screen by construction.
fn payload_needle(text: &str) -> Option<String> {
    let squeezed = squeeze(text);
    if squeezed.is_empty() {
        return None;
    }
    let skip = squeezed.chars().count().saturating_sub(NEEDLE_CHARS);
    Some(squeezed.chars().skip(skip).collect())
}

fn region_shows_payload_tail(region: &str, payload: &str) -> bool {
    payload_needle(payload).is_some_and(|needle| region_squeezed(region).contains(&needle))
}

/// What a composer inspection has to work with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Attribution<'a> {
    /// The STRICT post-send check: the exact bytes we wrote, plus what the
    /// composer already held when we wrote them. The baseline is needed here
    /// for the same reason the ingest gate needs it: a placeholder that a human
    /// staged is not evidence that OUR payload is still parked, and treating it
    /// as such makes the retry loop press Enter on their text.
    Payload(&'a str, Baseline),
    /// The CONSERVATIVE pre-check, which has no payload and can only recognise
    /// machine-nudge shapes. The tally is the placeholder count this pane
    /// carried before our last write, when one was recorded; without it a
    /// placeholder cannot be attributed to us at all.
    Precheck(Option<PasteTally>),
}

/// Classify the composer.
///
/// With [`Attribution::Payload`] this is strict and three-valued;
/// [`Verdict::Submitted`] is returned ONLY for a composer that was read and
/// found clear. With [`Attribution::Precheck`] it is the conservative pre-check
/// and reports pending only for machine-nudge shapes.
fn composer_state(pane: &str, how: Attribution<'_>) -> Verdict {
    let Some(region) = composer_region(pane) else {
        return match how {
            // A pane we could not parse is never proof of delivery. The caller
            // must be told "unknown", not "submitted".
            Attribution::Payload(..) => Verdict::Unverified,
            Attribution::Precheck(base) if last_prompt_line_pending(pane, base) => Verdict::Pending,
            Attribution::Precheck(_) => Verdict::Submitted,
        };
    };
    if region_is_clear(&region) {
        return Verdict::Submitted;
    }
    let (payload, baseline) = match how {
        // The conservative pre-check: machine-nudge shapes only, never "the
        // composer is not empty", which is the human-typing case, and never a
        // bare placeholder, which is the human-PASTING case.
        Attribution::Precheck(base) => {
            let squeezed = region_squeezed(&region);
            return if squeezed.contains(HEARTBEAT_MARK) || paste_is_ours(&squeezed, base) {
                Verdict::Pending
            } else {
                Verdict::Submitted
            };
        }
        Attribution::Payload(payload, baseline) => (payload, baseline),
    };
    if paste_tally(&region).grew_beyond(baseline.pastes)
        || region_shows_payload_tail(&region, payload)
    {
        Verdict::Pending
    } else {
        // The composer holds live content that is not ours: a human started
        // typing, or the pane redrew into something we do not model. Our
        // payload may well have gone through, but we cannot prove it, and
        // pressing Enter again would submit their text.
        Verdict::Unverified
    }
}

/// Did a placeholder appear AFTER our last write to this pane?
///
/// `None` (no record) is not "assume ours": it is the honest answer that this
/// placeholder cannot be attributed, and the pre-check must not answer it with
/// an Enter.
fn paste_is_ours(squeezed: &str, base: Option<PasteTally>) -> bool {
    base.is_some_and(|base| tally_pastes(squeezed).grew_beyond(base))
}

/// Inspect visible pane text for an unsubmitted composer.
///
/// [`Attribution::Payload`] is the STRICT post-send check,
/// [`Attribution::Precheck`] the CONSERVATIVE one; see [`composer_state`] and
/// [`pane_has_unsubmitted_input`] for why the two must not be the same
/// predicate.
fn composer_pending(pane: &str, how: Attribution<'_>) -> bool {
    composer_state(pane, how) == Verdict::Pending
}

/// Historical fallback for the payload-less pre-check on a pane with no
/// composer region: inspect the last prompt-prefixed line only.
fn last_prompt_line_pending(pane: &str, base: Option<PasteTally>) -> bool {
    for line in pane.lines().rev() {
        let t = strip_ansi(line);
        let t = t.trim_start_matches(['│', ' ', '\t']);
        for prefix in ['>', '❯', '›'] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let rest = squeeze(rest);
                return rest.contains(HEARTBEAT_MARK) || paste_is_ours(&rest, base);
            }
        }
    }
    false
}

pub async fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .status()
        .await
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
#[path = "tmux_tests.rs"]
mod tests;

/// `#[ignore]`d probes that drive these functions against a REAL claude pane.
/// Feature-gated alongside the other tmux-touching tests and skipped unless the
/// operator names a live session, so a normal test run never reaches them.
#[cfg(all(test, feature = "tmux-tests"))]
#[path = "live_probe_tests.rs"]
mod live_probe;
