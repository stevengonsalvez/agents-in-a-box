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

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

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
/// Wait after each Enter before checking whether the composer cleared.
const SUBMIT_CHECK_DELAY: Duration = Duration::from_millis(700);
/// Total Enter attempts (the first plus retries).
const ENTER_ATTEMPTS: u32 = 4;
/// Backoff between Enter attempts.
const RETRY_BACKOFF: Duration = Duration::from_secs(1);

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

    // ONE literal write, as `main` does: chunking under the paste classifier
    // would suppress the `[Pasted text #N]` placeholder that the ATC coalesce
    // guard reads. A non-zero exit here means tmux never handed the keys to the
    // pane, so no bytes were written and the broker may still try.
    if let Err(error) = send_keys_literal(tmux_session, text).await {
        return Err(SendError::nothing_written(format!(
            "send to {tmux_session} wrote nothing: {error:#}"
        ))
        .into());
    }

    let mut last = Verdict::Unverified;
    let mut ever_observed = false;
    for attempt in 1..=ENTER_ATTEMPTS {
        let gate = wait_for_ingest(tmux_session, text).await;
        if gate == Gate::PayloadVisible {
            ever_observed = true;
        }
        let action = gate_action(gate);
        if action == GateAction::Refuse {
            return Err(SendError::written(if ever_observed {
                // It was there, and now it is not, but the composer is not
                // clear either: something else is on screen and we cannot say
                // what happened to our turn.
                format!(
                    "send to {tmux_session} was written and seen in the composer, but its state \
could no longer be verified before Enter attempt {attempt}"
                )
            } else {
                format!(
                    "send to {tmux_session} was written but never appeared in the composer within \
{INGEST_TIMEOUT:?}; Enter was NOT pressed, so the payload may still be draining into the pane"
                )
            })
            .into());
        }
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
        if action == GateAction::PressAndAccept {
            // Nothing on this pane can distinguish a swallowed Enter from an
            // accepted one, and a second Enter would submit a second turn, so
            // the retry budget is not spent here: one write, one Enter, exactly
            // as `main` sends. That is also why this reports `Ok` rather than a
            // failure. See [`gate_action`] for the full rationale and for the
            // residual hole (a real Claude pane that is momentarily unparseable
            // is accepted here without proof, which is parity with `main`).
            return Ok(());
        }
        sleep(SUBMIT_CHECK_DELAY).await;
        last = composer_verdict(tmux_session, text).await;
        if last == Verdict::Submitted {
            return Ok(());
        }
        if attempt < ENTER_ATTEMPTS {
            sleep(RETRY_BACKOFF).await;
        }
    }

    Err(SendError::written(if last == Verdict::Pending {
        format!(
            "send to {tmux_session} was typed into the composer but NOT submitted after \
{ENTER_ATTEMPTS} Enter attempts (the payload is still parked in the composer)"
        )
    } else {
        format!(
            "send to {tmux_session} could not be verified after {ENTER_ATTEMPTS} Enter attempts \
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

async fn composer_verdict(tmux_session: &str, payload: &str) -> Verdict {
    (capture_pane_ansi(tmux_session, 0).await).map_or(Verdict::Unverified, |pane| {
        composer_state(&pane, Some(payload))
    })
}

/// Poll until the payload is VISIBLY in the composer.
///
/// A "two identical captures" settle cannot do this job: on a busy target that
/// has not started draining, the composer is unchanged (and usually empty),
/// which scores as stable, so the first Enter goes into a still-draining pane,
/// which is the exact CR-fusion condition. The gate therefore keys on the
/// PAYLOAD.
///
/// Two acceptance shapes, because a payload can be invisible as text:
///
///   * its tail needle is on screen, which proves the LAST bytes have landed;
///   * or a paste placeholder is on screen AND the region has stopped changing.
///     Measured at 80x24: a 900-byte write rendered as a bare `[Pasted text #3]`
///     with no payload text visible at all, so the needle can never appear. A
///     placeholder alone is not enough (more reads may still be in flight),
///     hence the stability requirement on top of it.
async fn wait_for_ingest(tmux_session: &str, payload: &str) -> Gate {
    let deadline = Instant::now() + INGEST_TIMEOUT;
    let mut previous: Option<Option<String>> = None;
    let mut saw_composer = false;
    loop {
        if let Ok(pane) = capture_pane_ansi(tmux_session, 0).await {
            let region = composer_region(&pane);
            let unchanged = previous.as_ref() == Some(&region);
            match &region {
                Some(region) => {
                    saw_composer = true;
                    if region_shows_payload_tail(region, payload)
                        || (region_shows_placeholder(region) && unchanged)
                    {
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
/// LIMIT, for the same reason the strict check needs the payload: at 80x24 a
/// parked heartbeat that renders as raw text keeps its `[HEARTBEAT` head OFF
/// SCREEN, so this returns `false` for it. Widening the match to "composer is
/// not empty" is not available here: that is precisely the human-typing case.
pub async fn pane_has_unsubmitted_input(tmux_session: &str) -> bool {
    capture_pane_ansi(tmux_session, 0)
        .await
        .is_ok_and(|pane| composer_pending(&pane, None))
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
fn composer_cells(region: &str) -> Vec<Cell> {
    let mut cells = Vec::new();
    for line in region.lines() {
        let mut dim = false;
        let mut chars = line.chars().peekable();
        let mut at_start = true;
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                if let Some(params) = skip_escape(&mut chars) {
                    apply_sgr(&params, &mut dim);
                }
                continue;
            }
            if at_start {
                // Leading box drawing / indent, then at most one prompt glyph.
                if ch.is_whitespace() || ch == '\u{a0}' || ch == '│' {
                    continue;
                }
                at_start = false;
                if matches!(ch, '>' | '❯' | '›') {
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

/// Is a paste placeholder on screen? Positive evidence of an unsubmitted send
/// on its own: a SUBMITTED paste leaves zero `[Pasted text #` anywhere on the
/// pane, because Claude Code expands it to raw text in the transcript.
fn region_shows_placeholder(region: &str) -> bool {
    region_squeezed(region).contains(PLACEHOLDER_MARK)
}

fn region_shows_payload_tail(region: &str, payload: &str) -> bool {
    payload_needle(payload).is_some_and(|needle| region_squeezed(region).contains(&needle))
}

/// Classify the composer.
///
/// With `payload` (the post-send verification) this is strict and three-valued;
/// [`Verdict::Submitted`] is returned ONLY for a composer that was read and
/// found clear. Without a payload it is the conservative pre-check and reports
/// pending only for machine-nudge shapes.
fn composer_state(pane: &str, payload: Option<&str>) -> Verdict {
    let Some(region) = composer_region(pane) else {
        return match payload {
            // A pane we could not parse is never proof of delivery. The caller
            // must be told "unknown", not "submitted".
            Some(_) => Verdict::Unverified,
            None if last_prompt_line_pending(pane) => Verdict::Pending,
            None => Verdict::Submitted,
        };
    };
    if region_is_clear(&region) {
        return Verdict::Submitted;
    }
    let Some(payload) = payload else {
        // The conservative pre-check: machine-nudge shapes only, never "the
        // composer is not empty", which is the human-typing case.
        let squeezed = region_squeezed(&region);
        return if squeezed.contains(PLACEHOLDER_MARK) || squeezed.contains(HEARTBEAT_MARK) {
            Verdict::Pending
        } else {
            Verdict::Submitted
        };
    };
    if region_shows_placeholder(&region) || region_shows_payload_tail(&region, payload) {
        Verdict::Pending
    } else {
        // The composer holds live content that is not ours: a human started
        // typing, or the pane redrew into something we do not model. Our
        // payload may well have gone through, but we cannot prove it, and
        // pressing Enter again would submit their text.
        Verdict::Unverified
    }
}

/// Inspect visible pane text for an unsubmitted composer.
///
/// `Some(payload)` is the STRICT post-send check, `None` the CONSERVATIVE
/// pre-check; see [`composer_state`] and [`pane_has_unsubmitted_input`] for why
/// the two must not be the same predicate.
fn composer_pending(pane: &str, payload: Option<&str>) -> bool {
    composer_state(pane, payload) == Verdict::Pending
}

/// Historical fallback for the payload-less pre-check on a pane with no
/// composer region: inspect the last prompt-prefixed line only.
fn last_prompt_line_pending(pane: &str) -> bool {
    for line in pane.lines().rev() {
        let t = strip_ansi(line);
        let t = t.trim_start_matches(['│', ' ', '\t']);
        for prefix in ['>', '❯', '›'] {
            if let Some(rest) = t.strip_prefix(prefix) {
                let rest = squeeze(rest);
                return rest.contains(PLACEHOLDER_MARK) || rest.contains(HEARTBEAT_MARK);
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
