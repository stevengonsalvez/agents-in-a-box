//! The daemon's answer router — deliver an answer to one open attention row from
//! any surface, exactly once, into the right session (architecture §4.3, spec P2).
//!
//! Every surface (TUI control centre, web, bridge, ATC) answers through the same
//! `attention/answer` RPC, which lands here. The router enforces the two
//! guarantees the converged inbox promises:
//!
//! 1. **First-answer-wins (exactly-once):** the `open → answered` flip is a
//!    conditional UPDATE ([`AttentionRepo::mark_answered_if_open`]). Two surfaces
//!    answering the same row serialise at the database; exactly one flips it and
//!    delivers, the other is told "already answered by X" and delivers nothing.
//!
//! 2. **C1 misroute refusal:** an answer must reach the EXACT agent that asked.
//!    The hook's session id often differs from a discovered [`Session::id`], so an
//!    exact-id miss falls back to a cwd correlation — but two agents can share a
//!    cwd, so an ambiguous cwd would risk sending the answer to the WRONG agent.
//!    The router REFUSES on ambiguity (more than one discovered session in the
//!    cwd, or a merged session that aggregated 2+ sources) rather than guess —
//!    the guard lifted from the TUI fleet panel's `control::resolve_and_send`.
//!    Attention rows are also DURABLE and outlive the raising agent, so the
//!    router additionally binds the cwd fallback to the transcript captured at
//!    raise time: if the original session has exited and a DIFFERENT agent now
//!    occupies the cwd (its transcript is newer), the router refuses rather than
//!    answer an agent that never asked.
//!
//! Ordering matters: the C1 target resolution runs BEFORE the flip, so an
//! ambiguous or dead target leaves the row OPEN and answerable later (a session
//! may exit, or the ambiguity may resolve). Only once a unique target is known
//! does the router claim the row (win-or-lose) and deliver via the one verified
//! send path ([`send`], the multi-line-submit-verified tmux path).

use ainb_fleet_core::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
use ainb_fleet_core::read::jsonl_tail::latest_transcript_for_cwd;
use ainb_fleet_core::send::send;
use ainb_fleet_core::types::{SendOutcome, Session};
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};
use ainb_hangar_store::repo::attention::{AttentionRepo, AttentionRow};
use sqlx::SqlitePool;
use std::time::{Duration, Instant};

use crate::events::EventSink;

/// The resolved delivery target for an answer, or a refusal.
enum Target {
    /// A single, unambiguous live session to deliver into.
    Send(Session),
    /// The C1 guard refused: the target could not be resolved unambiguously.
    Ambiguous(String),
    /// No live session matched (the target may have exited).
    NoTarget(String),
}

/// Answer one open attention row: guard, claim (first-answer-wins), deliver.
///
/// Returns a tagged [`AnswerResult`] the caller serialises back to the surface;
/// a store fault propagates as a [`sqlx::Error`] the RPC layer maps to an
/// internal error. A successful delivery emits an `AttentionAnswered` event on
/// the fleet-wide attention stream so every surface moves the card to
/// `answered(by=…)`.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if reading the row or the conditional flip fails.
pub async fn answer(
    pool: &SqlitePool,
    events: &EventSink,
    params: &AnswerParams,
    now_ms: i64,
) -> Result<AnswerResult, sqlx::Error> {
    // Load the row to recover the raising session's id + cwd (and to short-circuit
    // an already-answered row before any discovery I/O).
    let Some(row) = AttentionRepo::get(pool, &params.attention_id).await? else {
        return Ok(AnswerResult::NoTarget {
            reason: "no such attention row (already resolved or never existed)".to_string(),
        });
    };
    if row.state != "open" {
        return Ok(AnswerResult::AlreadyAnswered {
            by: row.answered_by.unwrap_or_else(|| "unknown".to_string()),
        });
    }

    // C1: resolve the delivery target BEFORE claiming, so an ambiguous / dead
    // target leaves the row open and answerable later.
    match resolve_target(
        &row.session_id,
        &row.cwd,
        row.raise_transcript.as_deref(),
        params.is_answer,
    )
    .await
    {
        Target::Ambiguous(reason) => Ok(AnswerResult::Ambiguous { reason }),
        Target::NoTarget(reason) => Ok(AnswerResult::NoTarget { reason }),
        Target::Send(session) => {
            // Claim the answer. A second surface that also resolved a target loses
            // this flip (0 rows) and delivers nothing.
            let flipped = AttentionRepo::mark_answered_if_open(
                pool,
                &params.attention_id,
                &params.answered_by,
                &params.answer,
                now_ms,
            )
            .await?;
            if flipped == 0 {
                let by = AttentionRepo::get(pool, &params.attention_id)
                    .await?
                    .and_then(|r| r.answered_by)
                    .unwrap_or_else(|| "unknown".to_string());
                return Ok(AnswerResult::AlreadyAnswered { by });
            }

            // We won: deliver via the one verified send path. Only a CONFIRMED
            // delivery (`Tmux` / `Broker`) keeps the row answered + emits the
            // `AttentionAnswered` nudge. On a delivery fault (`Failed`, or an
            // `Err`) the claim is COMPENSATED — the row is reverted to `open` so
            // the still-blocked agent's request stays in the inbox and remains
            // answerable, rather than leaving the feed forever on a transient
            // tmux/broker miss.
            match deliver(&session, &row, &params.answer).await {
                Ok(SendOutcome::Tmux { tmux_session }) => {
                    emit_answered(events, params);
                    Ok(AnswerResult::Delivered {
                        via: format!("tmux ({tmux_session})"),
                    })
                }
                Ok(SendOutcome::Broker { peer_id }) => {
                    emit_answered(events, params);
                    Ok(AnswerResult::Delivered {
                        via: format!("broker ({peer_id})"),
                    })
                }
                Ok(SendOutcome::Failed { reason }) => {
                    reopen_on_failed_delivery(pool, events, &row, params, now_ms).await?;
                    Ok(AnswerResult::DeliveryFailed { reason })
                }
                Err(e) => {
                    reopen_on_failed_delivery(pool, events, &row, params, now_ms).await?;
                    Ok(AnswerResult::DeliveryFailed {
                        reason: e.to_string(),
                    })
                }
            }
        }
    }
}

/// How an answer reaches the agent, decided by [`route_answer`] from the row's
/// payload, the answer text, and what the target pane shows RIGHT NOW.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Route {
    /// Drive the agent's option picker by position (zero-based).
    Picker(usize),
    /// Type the answer into the session ([`send`]): a free-text ASK, a target
    /// with no picker on screen (a plain shell, a non-Claude render, a picker
    /// the agent already dismissed), or a broker-first transport.
    Text,
    /// Deliver nothing and reopen the row: the pane shows an option picker and
    /// this answer cannot be routed into it.
    Refuse(String),
}

/// Pick the [`Route`] for `answer`. `pane` is the target's current screen when
/// tmux delivery is preferred and the capture succeeded, else `None`.
///
/// An `AskUserQuestion` ASK leaves a real option PICKER open in the agent's
/// pane, and that picker does not read typed text: typing the chosen label and
/// pressing Enter accepted whatever option was HIGHLIGHTED, so the store
/// recorded the operator's pick while the agent acted on the default. So while
/// the picker is on screen, an option answer is routed by position and anything
/// else is refused rather than typed. Without a picker on screen the answer is
/// text, exactly as before: that is the plain-shell delivery target the
/// converged harness pins (CC01) and every non-Claude render.
pub(crate) fn route_answer(pane: Option<&str>, picker: Option<&Picker>, answer: &str) -> Route {
    let Some(pane) = pane else {
        return Route::Text;
    };
    let Some(picker) = picker else {
        // A free-text ASK: typed as text, unless SOME picker is on screen
        // (Enter into an option picker, or a permission dialog, accepts the
        // highlighted row: an unintended pick or tool approval).
        if highlighted_option(pane).is_some() {
            return Route::Refuse(
                "a picker is on the agent's screen; free text is not typed into it".to_string(),
            );
        }
        return Route::Text;
    };
    let labels = picker.labels.as_slice();
    if !picker_visible(pane, picker) {
        // Picker chrome that is NOT this row's picker is a later question (or
        // another tool's prompt): typing into it would answer the wrong thing,
        // so refuse and leave the row open for the surface that sees the pane.
        if highlighted_option(pane).is_some() {
            return Route::Refuse(
                "a different picker is on the agent's screen; this row's options are gone"
                    .to_string(),
            );
        }
        return Route::Text;
    }
    match picker_position(labels, answer) {
        Some(position) if position < 9 => Route::Picker(position),
        Some(position) => Route::Refuse(format!(
            "option {} cannot be routed by key (pickers beyond 9 options are not supported)",
            position + 1
        )),
        None => Route::Refuse(format!(
            "the agent's picker expects one of its {} options; free text is not typed into it",
            labels.len()
        )),
    }
}

/// Deliver `answer` into `session` along the [`Route`] the pane dictates.
async fn deliver(
    session: &Session,
    row: &AttentionRow,
    answer: &str,
) -> anyhow::Result<SendOutcome> {
    use ainb_fleet_core::read::capture_pane;
    use ainb_fleet_core::send::tmux_delivery_preferred;

    let picker = picker_from_payload(&row.payload);
    let target = session.tmux_session.as_deref().filter(|_| tmux_delivery_preferred());
    // The pane is read whenever keys would go into one, picker or not: a
    // free-text answer must not be typed into a picker either. A pane the
    // daemon cannot read while the row says a picker is up is not typed into
    // blind: the row stays open for a surface that can see the screen.
    let pane = match target {
        Some(t) => match capture_pane(t, 0).await {
            Ok(pane) => Some(pane),
            Err(e) if picker.is_some() => {
                return Ok(SendOutcome::Failed {
                    reason: format!("could not read the agent's pane to route the pick: {e:#}"),
                });
            }
            Err(_) => None,
        },
        None => None,
    };
    match route_answer(pane.as_deref(), picker.as_ref(), answer) {
        Route::Text => send(session, answer).await,
        Route::Refuse(reason) => Ok(SendOutcome::Failed { reason }),
        Route::Picker(position) => {
            // `route_answer` only yields Picker with a pane, hence a target,
            // and a picker.
            let (Some(target), Some(picker)) = (target, picker) else {
                return Ok(SendOutcome::Failed {
                    reason: "picker route without a tmux target".to_string(),
                });
            };
            Ok(deliver_picker(target, position, &picker).await)
        }
    }
}

/// The single-question option picker an ASK payload describes: the question
/// text (matched on screen so a later picker with a look-alike first option is
/// not taken for this one) and its option labels in display order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Picker {
    question: String,
    labels: Vec<String>,
}

/// The picker an ASK payload offers, or `None` for a payload that is not a
/// single-question option picker (free text, multi-select, not an ASK). A
/// multi-select ASK therefore has no routed path: with its picker on screen
/// the free-text guard refuses, and the operator answers it at the pane
/// (typing into it used to report Delivered while the picker kept whatever
/// was highlighted).
fn picker_from_payload(payload: &str) -> Option<Picker> {
    let v: serde_json::Value = serde_json::from_str(payload).ok()?;
    if v.get("kind").and_then(|k| k.as_str()) != Some("ASK") {
        return None;
    }
    let ctx = v.get("context")?;
    if ctx.get("multi_select").and_then(|m| m.as_bool()) == Some(true) {
        return None;
    }
    let labels: Vec<String> = ctx
        .get("options")?
        .as_array()?
        .iter()
        .filter_map(|o| o.get("label").and_then(|l| l.as_str()).map(str::to_string))
        .collect();
    let question = ctx
        .get("question")
        .and_then(|q| q.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    (!labels.is_empty()).then_some(Picker { question, labels })
}

/// The zero-based position `answer` names among `labels`: the label verbatim,
/// the label case-insensitively, or a 1-based digit (the bridge's "reply N"
/// contract). Anything else, prefixes included, is free text: coercing a
/// prefix into a pick turned a deliberate free-text answer into a selection.
fn picker_position(labels: &[String], answer: &str) -> Option<usize> {
    let wanted = answer.trim();
    if wanted.is_empty() {
        return None;
    }
    if let Some(i) = labels.iter().position(|l| l.trim() == wanted) {
        return Some(i);
    }
    if let Some(i) = labels.iter().position(|l| l.trim().eq_ignore_ascii_case(wanted)) {
        return Some(i);
    }
    match wanted.parse::<usize>() {
        Ok(n) if (1..=labels.len()).contains(&n) => Some(n - 1),
        _ => None,
    }
}

/// Route a picker answer into `target` by pressing the option's DIGIT, then
/// read the pane until it settles. Claude Code 2.1.258 COMMITS on the number
/// key (probed live 2026-09-02: `2` alone closed a three-option
/// `AskUserQuestion` and echoed `→ Green` within 1.5s, before any Enter); older
/// builds only moved the highlight and needed Enter. [`picker_step`] tells the
/// two apart from the capture, so Enter is sent only when the picker is still
/// open with the highlight on our option, and never into the prompt that
/// replaces a closed picker. Once the picker is gone the pane is watched for
/// [`PICKER_SETTLE`] more so the answered echo, which renders a beat after the
/// close, is read: an echo naming OUR option confirms at once, one naming
/// another option fails the delivery, no echo within the window is a clean
/// delivery. Any `Failed` reopens the row through the caller's compensation
/// path.
///
/// One `AskUserQuestion` call may carry several questions; the attention row
/// models the first (the ingest keeps `questions[0]`), so this answers question
/// 1 and a second question's picker appearing afterwards is the agent moving on,
/// not a failure of this delivery.
async fn deliver_picker(target: &str, position: usize, picker: &Picker) -> SendOutcome {
    use ainb_fleet_core::read::capture_pane;
    use ainb_fleet_core::send::tmux_send_picker_key;

    let digit = (position + 1).to_string();
    if let Err(e) = tmux_send_picker_key(target, &digit).await {
        return SendOutcome::Failed {
            reason: format!("picker key {digit} failed: {e:#}"),
        };
    }
    let delivered = SendOutcome::Tmux {
        tmux_session: target.to_string(),
    };
    let mut committed = false;
    let mut gone_since: Option<Instant> = None;
    for _ in 0..60 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let Ok(pane) = capture_pane(target, 0).await else {
            continue;
        };
        let step = picker_step(&pane, picker, position);
        match settle(step, gone_since, Instant::now(), committed) {
            Settle::Delivered => return delivered,
            Settle::Recorded(other) => {
                return SendOutcome::Failed {
                    reason: format!(
                        "agent recorded option {} ({}) instead of option {digit}",
                        other + 1,
                        picker.labels[other]
                    ),
                };
            }
            Settle::Commit => {
                gone_since = None;
                if let Err(e) = tmux_send_picker_key(target, "Enter").await {
                    return SendOutcome::Failed {
                        reason: format!("picker Enter key failed: {e:#}"),
                    };
                }
                committed = true;
            }
            Settle::Wait(since) => gone_since = since,
        }
    }
    // The budget ran out. A picker that closed late (still inside its settle
    // window) was answered; only a picker that never closed is a failure.
    if gone_since.is_some() {
        return delivered;
    }
    SendOutcome::Failed {
        reason: format!(
            "picker still open after key {digit}: the highlight never settled on option {digit} and the picker never closed"
        ),
    }
}

/// What the delivery loop does with one classified capture.
#[derive(Debug, PartialEq, Eq)]
enum Settle {
    /// Report the answer delivered.
    Delivered,
    /// Report the answer failed: the agent recorded another option.
    Recorded(usize),
    /// Press Enter (a build that only moved the highlight on the digit).
    Commit,
    /// Keep polling, carrying the moment the picker was first seen gone (or
    /// `None` while it is still on screen).
    Wait(Option<Instant>),
}

/// The settle rule, pure so it can be table-tested: a confirming echo or a
/// contrary one decides at once; a picker gone with no echo yet is only a
/// delivery once it has stayed gone for [`PICKER_SETTLE`] (the echo renders a
/// beat after the close, and a repaint can drop the cursor line for a frame);
/// a picker still open with our option highlighted gets exactly one Enter.
fn settle(step: PickerStep, gone_since: Option<Instant>, now: Instant, committed: bool) -> Settle {
    match step {
        PickerStep::Confirmed => Settle::Delivered,
        PickerStep::Recorded(other) => Settle::Recorded(other),
        PickerStep::Gone => {
            let since = gone_since.unwrap_or(now);
            if now.saturating_duration_since(since) >= PICKER_SETTLE {
                Settle::Delivered
            } else {
                Settle::Wait(Some(since))
            }
        }
        PickerStep::Commit if !committed => Settle::Commit,
        PickerStep::Commit | PickerStep::Pending => Settle::Wait(None),
    }
}

/// How long a closed picker's pane is watched for the answered echo before a
/// delivery with no echo counts as clean.
const PICKER_SETTLE: Duration = Duration::from_millis(2000);

/// What one pane capture says about a picker answer in flight.
#[derive(Debug, PartialEq, Eq)]
enum PickerStep {
    /// The picker is gone and the answered echo names OUR option.
    Confirmed,
    /// The picker is gone and the answered echo names a DIFFERENT option.
    Recorded(usize),
    /// The picker is gone and no echo has rendered yet.
    Gone,
    /// The picker is still open with the highlight on our option: a build
    /// that moves on the digit and commits on Enter.
    Commit,
    /// The picker is still open and the highlight has not reached our option.
    Pending,
}

/// Classify `pane` after the option digit was sent for `position`. Once the
/// route has been decided, a picker showing our options counts as still open
/// even when its question line has scrolled off a short pane: "gone" needs the
/// option block itself to be gone, never a missing question.
fn picker_step(pane: &str, picker: &Picker, position: usize) -> PickerStep {
    if !options_visible(pane, picker) {
        return match echoed_option(pane, &picker.labels) {
            Some(echoed) if echoed == position => PickerStep::Confirmed,
            Some(other) => PickerStep::Recorded(other),
            None => PickerStep::Gone,
        };
    }
    if highlighted_option(pane) == Some(position) {
        PickerStep::Commit
    } else {
        PickerStep::Pending
    }
}

/// Is THIS picker on screen? Requires the picker's option block
/// ([`options_visible`]) and, when the payload carried a question, that
/// question's probe INSIDE the block: on a line above the cursor line and
/// below the last answered echo (`→ `), so our question echoed from an earlier
/// answer higher up the pane never vouches for a later picker that happens to
/// open with the same first option.
fn picker_visible(pane: &str, picker: &Picker) -> bool {
    if !options_visible(pane, picker) {
        return false;
    }
    if picker.question.is_empty() {
        return true;
    }
    let lines: Vec<&str> = pane.lines().collect();
    let Some(cursor) = lines.iter().position(|l| cursor_option(l).is_some()) else {
        return false;
    };
    let block_start = lines[..cursor].iter().rposition(|l| l.contains("→ ")).map_or(0, |i| i + 1);
    let probe = picker_probe(&picker.question);
    lines[block_start..cursor].iter().any(|l| l.contains(probe))
}

/// The picker's option block: the live cursor line (`❯ N.` for an N within
/// the option count) plus the first option's numbered probe, so a transcript
/// that merely echoes numbered lines after the picker closed, or an agent
/// printing look-alike lines, does not pass.
fn options_visible(pane: &str, picker: &Picker) -> bool {
    let Some(first) = picker.labels.first() else {
        return false;
    };
    highlighted_option(pane).is_some_and(|i| i < picker.labels.len())
        && pane.contains(&format!("1. {}", picker_probe(first)))
}

/// The zero-based option the picker cursor `❯ N.` currently sits on, if a
/// picker cursor line is on screen.
fn highlighted_option(pane: &str) -> Option<usize> {
    pane.lines().find_map(cursor_option)
}

/// The zero-based option a single `❯ N.` cursor line names, if `line` is one.
fn cursor_option(line: &str) -> Option<usize> {
    let rest = line.trim_start().strip_prefix('❯')?.trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with('.') {
        return None;
    }
    digits.parse::<usize>().ok()?.checked_sub(1)
}

/// The option the pane's "→ <label>" answered echo names, if any. The label
/// verbatim first; then, for an echo the pane wrapped mid-label, the ONE label
/// the surviving text is a prefix of (two candidates is no verdict); last, for
/// an echo with trailing decoration, the longest label probe that prefixes it.
/// Never the first label that merely shares a prefix, so `deploy later` is not
/// read back as `deploy`.
fn echoed_option(pane: &str, labels: &[String]) -> Option<usize> {
    let echo = pane.lines().rev().find(|l| l.contains("→ "))?;
    let after = echo.rsplit("→ ").next()?.trim();
    if after.is_empty() {
        return None;
    }
    if let Some(i) = labels.iter().position(|l| l.trim() == after) {
        return Some(i);
    }
    let mut by_prefix = labels.iter().enumerate().filter(|(_, l)| l.trim().starts_with(after));
    if let (Some((i, _)), None) = (by_prefix.next(), by_prefix.next()) {
        return Some(i);
    }
    labels
        .iter()
        .enumerate()
        .filter(|(_, l)| after.starts_with(picker_probe(l)))
        .max_by_key(|(_, l)| picker_probe(l).len())
        .map(|(i, _)| i)
}

/// The leading slice of an option label a wrapped pane render still shows
/// contiguously: 12 characters, cut on a char boundary.
fn picker_probe(label: &str) -> &str {
    let mut end = label.len().min(12);
    while !label.is_char_boundary(end) {
        end -= 1;
    }
    &label[..end]
}

/// Emit the `AttentionAnswered` nudge on the fleet-wide attention stream.
fn emit_answered(events: &EventSink, params: &AnswerParams) {
    events.emit_attention(HangarEvent::AttentionAnswered {
        attention_id: params.attention_id.clone(),
        by: params.answered_by.clone(),
    });
}

/// Undo a claim whose last-mile delivery failed: revert the row to `open` and
/// re-raise it on the attention stream so every surface shows it as answerable
/// again. Scoped to this caller's own claim ([`AttentionRepo::reopen`]), so a
/// concurrent winner is never clobbered.
async fn reopen_on_failed_delivery(
    pool: &SqlitePool,
    events: &EventSink,
    row: &AttentionRow,
    params: &AnswerParams,
    now_ms: i64,
) -> Result<(), sqlx::Error> {
    let reverted =
        AttentionRepo::reopen(pool, &params.attention_id, &params.answered_by, now_ms).await?;
    if reverted > 0 {
        // Re-nudge so live surfaces re-show the card without waiting for the
        // next snapshot pull. Best-effort (dropped when there are no subscribers).
        events.emit_attention(HangarEvent::AttentionRaised {
            attention_id: row.id.clone(),
            session_id: row.session_id.clone(),
            workspace_id: row.workspace_id.clone(),
            kind: row.kind.as_str().to_string(),
            degraded: row.degraded,
            created_at: row.created_at,
            // Re-nudge carries the row's ORIGINAL raise-time channels — a reopen
            // must not re-resolve (that would let a mid-flight rule edit change
            // where an in-flight attention routes), so the fan-out decision stays
            // fixed at first raise.
            channels: row.channels,
        });
    }
    Ok(())
}

/// Resolve a `(session_id, cwd)` to a single live [`Session`] to deliver into, or
/// a refusal — the C1 ambiguity guard lifted from `ainb-core`'s
/// `fleet::control::resolve_and_send`.
///
/// Discovery runs both sources in real parallel (`discover_from_ainb` is async;
/// `discover_from_peers` is a blocking SQLite read run on a blocking thread).
/// Resolution priority + guard:
///   1. EXACT session-id match → always safe (unambiguously the named agent).
///   2. No exact match → correlate by cwd, but ONLY when that cwd is unambiguous
///      AND the raising session still OWNS it. Ambiguous = more than one
///      discovered session shares the cwd, OR the merged session aggregated 2+
///      sources. Stale = the raising session's captured transcript is no longer
///      the newest in the cwd (a different agent took over the durable row's cwd
///      after the original exited). On ambiguity or staleness → refuse.
/// The C1 pick over discovered sessions, pure so the router and its tests share
/// one implementation.
#[derive(Debug)]
pub(crate) enum Pick {
    /// The hook's session id matched a discovered session outright.
    Exact(Session),
    /// One session's root is the raise cwd (`nested == false`) or its nearest
    /// ancestor (`nested == true`).
    ByCwd { session: Session, nested: bool },
    /// More than one session claims the raise cwd at the same depth.
    Ambiguous(usize),
    /// No discovered session matched.
    None,
}

/// Resolve which discovered session an attention row belongs to.
///
/// Exact id first. Otherwise the hook's cwd, which drifts BELOW the session
/// root as soon as the agent `cd`s into a subproject (`<worktree>/api`) while
/// discovery lists the session at its root: the most specific session whose
/// root contains the raise cwd wins, and two sessions at that same depth are
/// ambiguous (a merged session that aggregated 2+ sources counts as two).
pub(crate) fn pick_target(
    ainb: &[Session],
    peers: &[Session],
    session_id: &str,
    cwd: &str,
) -> Pick {
    let root_len = |s: &Session| {
        if session_owns_cwd(&s.cwd, cwd) {
            s.cwd.trim_end_matches('/').len()
        } else {
            0
        }
    };
    let deepest = ainb.iter().chain(peers.iter()).map(root_len).max().unwrap_or(0);
    let raw_cwd_count = if cwd.is_empty() || deepest == 0 {
        0
    } else {
        ainb.iter().chain(peers.iter()).filter(|s| root_len(s) == deepest).count()
    };

    let merged = merge_sessions(vec![ainb.to_vec(), peers.to_vec()]);
    if let Some(session) = merged.iter().find(|s| s.id == session_id) {
        return Pick::Exact(session.clone());
    }
    let Some(by_cwd) = merged
        .iter()
        .filter(|s| !cwd.is_empty() && session_owns_cwd(&s.cwd, cwd))
        .max_by_key(|s| s.cwd.trim_end_matches('/').len())
    else {
        return Pick::None;
    };
    if raw_cwd_count > 1 || by_cwd.sources.len() > 1 {
        return Pick::Ambiguous(raw_cwd_count.max(by_cwd.sources.len()));
    }
    let nested = by_cwd.cwd.trim_end_matches('/') != cwd.trim_end_matches('/');
    Pick::ByCwd {
        session: by_cwd.clone(),
        nested,
    }
}

async fn resolve_target(
    session_id: &str,
    cwd: &str,
    raise_transcript: Option<&str>,
    is_answer: bool,
) -> Target {
    let ainb_fut = discover_from_ainb();
    let peers_fut = tokio::task::spawn_blocking(discover_from_peers);
    let (ainb, peers_join) = tokio::join!(ainb_fut, peers_fut);
    let ainb: Vec<Session> = ainb.unwrap_or_default();
    let peers: Vec<Session> = peers_join.ok().and_then(Result::ok).unwrap_or_default();
    let label = if is_answer {
        "cannot safely answer"
    } else {
        "refusing to send"
    };

    let (by_cwd, nested) = match pick_target(&ainb, &peers, session_id, cwd) {
        Pick::Exact(session) => return Target::Send(session),
        Pick::None => {
            return Target::NoTarget(
                "no live session matched (target may have exited)".to_string(),
            );
        }
        Pick::Ambiguous(n) => {
            return Target::Ambiguous(format!(
                "ambiguous target — {label} ({n} sessions in this cwd)"
            ));
        }
        Pick::ByCwd { session, nested } => (session, nested),
    };

    // Attention rows are DURABLE and outlive the raising agent. If the original
    // session has exited and a DIFFERENT agent now occupies the cwd (its
    // transcript is newer), refuse rather than answer an agent that never asked.
    // A NESTED match (session root above the raise cwd) is only ever accepted
    // when the raise transcript confirms the owner: without that check a session
    // rooted at a broad ancestor ($HOME, a repos root) would become the delivery
    // target for every row raised anywhere beneath it.
    match raise_transcript.filter(|t| !t.is_empty()) {
        Some(raise_tx) => {
            // Transcripts are keyed by the session's ROOT cwd, not the
            // subdirectory the agent happened to be in when it asked.
            if !transcript_still_owns_cwd(&by_cwd.cwd, raise_tx) {
                return Target::Ambiguous(format!(
                    "stale target — {label} (the raising session no longer owns this cwd)"
                ));
            }
        }
        None if nested => {
            return Target::Ambiguous(format!(
                "nested target — {label} (the raise cwd is below the session root and no raise transcript confirms the owner)"
            ));
        }
        None => {}
    }

    Target::Send(by_cwd)
}

/// Is `raise_cwd` the session root `root` itself, or a directory below it?
/// Exact path-component containment, never a bare string prefix, so
/// `/w/app` does not own `/w/app2`.
fn session_owns_cwd(root: &str, raise_cwd: &str) -> bool {
    if root.is_empty() || raise_cwd.is_empty() {
        return false;
    }
    let root = root.trim_end_matches('/');
    let raise_cwd = raise_cwd.trim_end_matches('/');
    raise_cwd == root || raise_cwd.strip_prefix(root).is_some_and(|rest| rest.starts_with('/'))
}

/// Does the session that raised the request still OWN `cwd`? True when the newest
/// transcript in the cwd's project dir is still the one captured at raise time.
///
/// Compared by file NAME — the session-unique transcript id — so the hook's
/// absolute path and the discovered path never disagree over formatting. A
/// missing current transcript (dir gone) is treated as NOT owning (refuse).
fn transcript_still_owns_cwd(cwd: &str, raise_transcript: &str) -> bool {
    let raise_name = std::path::Path::new(raise_transcript).file_name();
    raise_name.is_some()
        && latest_transcript_for_cwd(cwd).is_some_and(|current| current.file_name() == raise_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_fleet_core::types::SessionSource;
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::attention::{AttentionKind, NewAttention};

    /// The payload shape the hook ingest stores for a real `AskUserQuestion`
    /// (captured live from Claude Code 2.1.257).
    const ASK_PAYLOAD: &str = r#"{"kind":"ASK","context":{"question":"Where should Boxtrack's sqlite file live by default?","header":"DB path","options":[{"label":"data/boxtrack.db (Recommended)","description":"Repo-root data/ dir"},{"label":"api/app.db","description":"Beside the api"}],"multi_select":false}}"#;

    /// The hook's cwd drifts below the session root the moment the agent `cd`s
    /// into a subproject; the session still owns it. Containment is by path
    /// component, so a sibling with a shared name prefix never matches.
    #[test]
    fn session_owns_cwd_is_component_wise_containment() {
        assert!(session_owns_cwd("/w/app", "/w/app"));
        assert!(session_owns_cwd("/w/app", "/w/app/api"));
        assert!(session_owns_cwd("/w/app/", "/w/app/api/src"));
        assert!(!session_owns_cwd("/w/app", "/w/app2"));
        assert!(!session_owns_cwd("/w/app", "/w"));
        assert!(!session_owns_cwd("", "/w/app"));
        assert!(!session_owns_cwd("/w/app", ""));
    }

    fn labels() -> Vec<String> {
        picker().labels
    }

    fn picker() -> Picker {
        picker_from_payload(ASK_PAYLOAD).unwrap()
    }

    /// A picker with `labels` and no question text (an older hook payload).
    fn bare(labels: &[String]) -> Picker {
        Picker {
            question: String::new(),
            labels: labels.to_vec(),
        }
    }

    /// Option answers resolve to their PICKER POSITION by label (case-insensitive)
    /// or 1-based digit; anything else, prefixes and blanks included, is free
    /// text (no position).
    #[test]
    fn picker_position_resolves_label_and_digit_only() {
        let l = labels();
        assert_eq!(picker_position(&l, "api/app.db"), Some(1));
        assert_eq!(
            picker_position(&l, "data/boxtrack.db (Recommended)"),
            Some(0)
        );
        assert_eq!(picker_position(&l, "API/APP.DB"), Some(1));
        assert_eq!(picker_position(&l, "2"), Some(1));
        assert_eq!(picker_position(&l, "1"), Some(0));
        assert_eq!(
            picker_position(&l, "3"),
            None,
            "out of range digit is free text"
        );
        assert_eq!(
            picker_position(&l, "data/"),
            None,
            "a prefix is free text, never a pick"
        );
        assert_eq!(picker_position(&l, ""), None);
        assert_eq!(picker_position(&l, "   "), None);
        assert_eq!(picker_position(&l, "use postgres"), None);
        let ambiguous = vec!["deploy now".to_string(), "deploy later".to_string()];
        assert_eq!(picker_position(&ambiguous, "deploy"), None);
    }

    /// A multi-select or non-ASK payload never routes by position (the caller
    /// keeps the text send), and a free-text ASK has no labels at all.
    #[test]
    fn picker_labels_rejects_non_picker_payloads() {
        let multi = ASK_PAYLOAD.replace("\"multi_select\":false", "\"multi_select\":true");
        assert_eq!(picker_from_payload(&multi), None);
        assert_eq!(picker_from_payload(r#"{"kind":"ERR","context":{}}"#), None);
        assert_eq!(
            picker_from_payload(r#"{"kind":"ASK","context":{"question":"why?"}}"#),
            None
        );
        assert_eq!(
            labels(),
            vec![
                "data/boxtrack.db (Recommended)".to_string(),
                "api/app.db".to_string()
            ]
        );
    }

    /// Probes are the first 12 chars, cut on a char boundary, so a wrapped or
    /// truncated pane render of a long label still matches.
    #[test]
    fn picker_probe_is_a_bounded_prefix() {
        assert_eq!(picker_probe("api/app.db"), "api/app.db");
        assert_eq!(
            picker_probe("data/boxtrack.db (Recommended)"),
            "data/boxtrac"
        );
        assert_eq!(picker_probe("ééééééééééééééééééééééé long"), "éééééé");
    }

    /// A real Claude Code 2.1 picker render (cursor on option 1, `(Recommended)`
    /// wrapped onto its own line).
    const PICKER_PANE: &str = "\
 Where should Boxtrack's sqlite file live by default?
 ❯ 1. data/boxtrack.db             ┌──────┐
     (Recommended)                 │ repo/│
   2. api/app.db                   │      │
 Enter to select · ↑/↓ to navigate · Esc to cancel";
    /// The same pane after the picker closed and the agent echoed its answer.
    const ANSWERED_PANE: &str = "\
● User answered Claude's questions:
  ⎿  · Where should Boxtrack's sqlite file live by default? → api/app.db
❯ ";

    /// Visibility needs the live cursor line plus the first option: a transcript
    /// that only echoes numbered lines, an echoed answer, or a plain shell is not
    /// a picker.
    #[test]
    fn picker_visible_needs_the_cursor_and_the_first_option() {
        let p = picker();
        assert!(picker_visible(PICKER_PANE, &p));
        assert_eq!(highlighted_option(PICKER_PANE), Some(0));
        let moved = PICKER_PANE.replace(" ❯ 1.", "   1.").replace("   2. api", " ❯ 2. api");
        assert!(picker_visible(&moved, &p));
        assert_eq!(highlighted_option(&moved), Some(1));
        assert!(!picker_visible(ANSWERED_PANE, &p));
        assert!(
            !picker_visible("$ ls\n1. data/boxtrack.db\n2. api/app.db\n$ ", &p),
            "no cursor line"
        );
        assert!(
            !picker_visible("❯ 1. something else entirely\n  2. nope", &p),
            "wrong options"
        );
        assert!(
            !picker_visible("❯ 7. data/boxtrack.db", &p),
            "cursor beyond the option count"
        );
        // A later question that happens to open with the same first option is
        // not this picker: the question text is matched too.
        let other_question = PICKER_PANE.replace(
            "Where should Boxtrack's sqlite file live by default?",
            "Which file should the migration target?",
        );
        assert!(
            !picker_visible(&other_question, &p),
            "same options, other question"
        );
        assert!(
            picker_visible(&other_question, &bare(&p.labels)),
            "a payload with no question text falls back to the option match"
        );
        // The question must sit INSIDE the picker block: our question echoed
        // from the earlier answer, higher up the pane, does not vouch for a
        // later picker below it.
        let follow_up = format!("{ANSWERED_PANE}\n{other_question}");
        assert!(
            !picker_visible(&follow_up, &p),
            "our echoed question above a different picker is not our picker"
        );
        assert!(
            options_visible(&follow_up, &p),
            "...though its option block still reads as open"
        );
    }

    /// Once the route is decided, our picker with its question scrolled off a
    /// short pane is still OPEN (Pending / Commit), never "gone": a delivery
    /// is never reported on a picker nobody answered.
    #[test]
    fn picker_step_treats_a_scrolled_question_as_still_open() {
        let p = picker();
        let scrolled = PICKER_PANE.replacen(
            "Where should Boxtrack's sqlite file live by default?\n",
            "",
            1,
        );
        assert!(
            !picker_visible(&scrolled, &p),
            "the route would not pick it..."
        );
        assert_eq!(picker_step(&scrolled, &p, 1), PickerStep::Pending);
        let moved = scrolled.replace(" ❯ 1.", "   1.").replace("   2. api", " ❯ 2. api");
        assert_eq!(picker_step(&moved, &p, 1), PickerStep::Commit);
    }

    /// The settle rule, one capture at a time: echoes decide at once, a gone
    /// picker waits out the window (carrying its first-gone instant), a
    /// repaint that shows the picker again drops the window, Enter goes once.
    #[test]
    fn settle_table() {
        use std::time::{Duration, Instant};
        let t0 = Instant::now();
        let later = t0 + PICKER_SETTLE + Duration::from_millis(1);
        assert_eq!(
            settle(PickerStep::Confirmed, None, t0, false),
            Settle::Delivered
        );
        assert_eq!(
            settle(PickerStep::Recorded(0), Some(t0), later, true),
            Settle::Recorded(0)
        );
        assert_eq!(
            settle(PickerStep::Gone, None, t0, false),
            Settle::Wait(Some(t0))
        );
        assert_eq!(
            settle(
                PickerStep::Gone,
                Some(t0),
                t0 + Duration::from_millis(500),
                false
            ),
            Settle::Wait(Some(t0)),
            "inside the window: keep the first-gone instant"
        );
        assert_eq!(
            settle(PickerStep::Gone, Some(t0), later, false),
            Settle::Delivered
        );
        assert_eq!(
            settle(PickerStep::Pending, Some(t0), later, false),
            Settle::Wait(None)
        );
        assert_eq!(
            settle(PickerStep::Commit, Some(t0), later, false),
            Settle::Commit
        );
        assert_eq!(
            settle(PickerStep::Commit, None, later, true),
            Settle::Wait(None),
            "Enter is pressed once"
        );
    }

    /// The answered echo names the option the agent recorded, by probe.
    #[test]
    fn echoed_option_reads_the_answer_echo() {
        let l = labels();
        assert_eq!(echoed_option(ANSWERED_PANE, &l), Some(1));
        let first = ANSWERED_PANE.replace("→ api/app.db", "→ data/boxtrack.db (Recommended)");
        assert_eq!(echoed_option(&first, &l), Some(0));
        assert_eq!(echoed_option(PICKER_PANE, &l), None);
    }

    /// Labels that share a prefix resolve to the one the echo actually names:
    /// verbatim wins, and a wrapped echo takes the longest matching probe, so a
    /// correct delivery of `deploy later` is not failed as "recorded deploy".
    #[test]
    fn echoed_option_prefers_the_exact_then_the_longest_label() {
        let l = vec!["deploy".to_string(), "deploy later".to_string()];
        assert_eq!(
            echoed_option("  ⎿  · When? → deploy later\n❯ ", &l),
            Some(1)
        );
        assert_eq!(echoed_option("  ⎿  · When? → deploy\n❯ ", &l), Some(0));
        // Wrapped mid-label: the surviving text is a prefix of one label only.
        assert_eq!(echoed_option("  ⎿  · When? → deploy late\nr", &l), Some(1));
        // Wrapped so early that both labels fit: no verdict, not "the first".
        assert_eq!(echoed_option("  ⎿  · When? → dep\nloy later", &l), None);
        // Trailing decoration after a full label: the longest probe wins.
        assert_eq!(echoed_option("  ⎿  · When? → deploy later ✓", &l), Some(1));
        assert_eq!(echoed_option("  ⎿  · When? → ", &l), None);
    }

    /// The per-capture verdict behind `deliver_picker`, both picker behaviours:
    /// a build that commits on the digit (picker gone, echo names ours), one
    /// that only moves the highlight (still open, highlight on ours: commit),
    /// a highlight that has not moved yet, and an echo naming another option.
    #[test]
    fn picker_step_tells_commit_on_digit_from_highlight_only() {
        let p = picker();
        assert_eq!(picker_step(ANSWERED_PANE, &p, 1), PickerStep::Confirmed);
        assert_eq!(picker_step(ANSWERED_PANE, &p, 0), PickerStep::Recorded(1));
        let closed_no_echo = "● Thinking…\n❯ ";
        assert_eq!(picker_step(closed_no_echo, &p, 1), PickerStep::Gone);
        assert_eq!(picker_step(PICKER_PANE, &p, 1), PickerStep::Pending);
        let moved = PICKER_PANE.replace(" ❯ 1.", "   1.").replace("   2. api", " ❯ 2. api");
        assert_eq!(picker_step(&moved, &p, 1), PickerStep::Commit);
    }

    /// The routing table: no picker on screen (plain shell, closed picker, no
    /// tmux target, broker transport) types the answer as before; a visible
    /// picker routes an option by position and REFUSES free text or an option
    /// past the digit keys, never typing into it.
    #[test]
    fn route_answer_table() {
        let p = picker();
        assert_eq!(
            route_answer(None, Some(&p), "api/app.db"),
            Route::Text,
            "no pane"
        );
        assert_eq!(
            route_answer(Some("$ "), None, "yes"),
            Route::Text,
            "free-text ASK, plain shell"
        );
        assert!(
            matches!(
                route_answer(Some(PICKER_PANE), None, "yes"),
                Route::Refuse(_)
            ),
            "free-text ASK with a picker on screen: Enter would accept the highlighted row"
        );
        assert_eq!(
            route_answer(Some("$ "), Some(&p), "prod"),
            Route::Text,
            "plain shell target"
        );
        assert_eq!(
            route_answer(Some(ANSWERED_PANE), Some(&p), "api/app.db"),
            Route::Text,
            "picker gone"
        );
        assert!(
            matches!(
                route_answer(
                    Some("Which region?\n❯ 1. eu-west\n  2. us-east"),
                    Some(&p),
                    "api/app.db"
                ),
                Route::Refuse(_)
            ),
            "a later question's picker is refused, never typed into"
        );
        assert_eq!(
            route_answer(Some(PICKER_PANE), Some(&p), "api/app.db"),
            Route::Picker(1)
        );
        assert_eq!(
            route_answer(Some(PICKER_PANE), Some(&p), "2"),
            Route::Picker(1)
        );
        assert!(matches!(
            route_answer(Some(PICKER_PANE), Some(&p), "actually use postgres"),
            Route::Refuse(_)
        ));
        let many: Vec<String> = (1..=12).map(|i| format!("option {i}")).collect();
        let many = bare(&many);
        let pane = format!(
            "❯ 1. option 1\n{}",
            (2..=12).map(|i| format!("  {i}. option {i}")).collect::<Vec<_>>().join("\n")
        );
        assert!(matches!(
            route_answer(Some(&pane), Some(&many), "option 12"),
            Route::Refuse(_)
        ));
        assert_eq!(
            route_answer(Some(&pane), Some(&many), "option 9"),
            Route::Picker(8)
        );
    }

    fn session(id: &str, cwd: &str, src: SessionSource) -> Session {
        Session {
            id: id.to_string(),
            cwd: cwd.to_string(),
            pid: None,
            git_root: None,
            tmux_session: Some("tmux".to_string()),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![src],
            summary: None,
            last_seen_ms: None,
        }
    }

    #[test]
    fn exact_session_id_match_delivers() {
        let raw = vec![
            session("hook-sid", "/work/x", SessionSource::Ainb),
            session("other", "/work/x", SessionSource::Peers),
        ];
        assert!(matches!(
            pick_target(&raw, &[], "hook-sid", "/work/x"),
            Pick::Exact(s) if s.id == "hook-sid"
        ));
    }

    #[test]
    fn unambiguous_cwd_match_delivers() {
        let raw = vec![session("discovered-id", "/work/x", SessionSource::Ainb)];
        assert!(matches!(
            pick_target(&raw, &[], "hook-session-differs", "/work/x"),
            Pick::ByCwd { session, nested: false } if session.id == "discovered-id"
        ));
    }

    #[test]
    fn ambiguous_cwd_two_raw_sessions_refuses() {
        let raw = vec![
            session("a", "/work/x", SessionSource::Ainb),
            session("b", "/work/x", SessionSource::Ainb),
        ];
        assert!(matches!(
            pick_target(&raw, &[], "hook-sid", "/work/x"),
            Pick::Ambiguous(2)
        ));
    }

    #[test]
    fn ambiguous_cwd_multi_source_merge_refuses() {
        let ainb = vec![session("a", "/work/x", SessionSource::Ainb)];
        let peers = vec![session("a", "/work/x", SessionSource::Peers)];
        assert!(matches!(
            pick_target(&ainb, &peers, "hook-sid", "/work/x"),
            Pick::Ambiguous(2)
        ));
    }

    /// The hook's cwd drifted below the session root (the agent `cd`'d into
    /// `api/`): the session still resolves, flagged nested so the router can
    /// demand the raise transcript before trusting it.
    #[test]
    fn nested_cwd_resolves_to_the_session_root_as_nested() {
        let raw = vec![session("wt", "/w/app", SessionSource::Ainb)];
        assert!(matches!(
            pick_target(&raw, &[], "hook-sid", "/w/app/api"),
            Pick::ByCwd { session, nested: true } if session.id == "wt"
        ));
        assert!(
            matches!(pick_target(&raw, &[], "hook-sid", "/w/app2"), Pick::None),
            "sibling prefix"
        );
    }

    /// A session with its own tmux identity, so two of them are two sessions to
    /// `merge_sessions` (which folds sessions sharing a tmux target into one).
    fn distinct_session(id: &str, cwd: &str) -> Session {
        let mut s = session(id, cwd, SessionSource::Ainb);
        s.tmux_session = Some(format!("tmux-{id}"));
        s
    }

    /// Two ancestors: the most specific root wins outright (no ambiguity), and a
    /// trailing slash on a discovered cwd changes neither depth nor the count.
    #[test]
    fn deepest_ancestor_wins_and_trailing_slash_is_ignored() {
        let raw = vec![
            distinct_session("broad", "/w"),
            distinct_session("narrow", "/w/app/"),
        ];
        assert!(matches!(
            pick_target(&raw, &[], "hook-sid", "/w/app/api"),
            Pick::ByCwd { session, nested: true } if session.id == "narrow"
        ));
        let twins = vec![
            distinct_session("a", "/w/app"),
            distinct_session("b", "/w/app/"),
        ];
        assert!(
            matches!(
                pick_target(&twins, &[], "hook-sid", "/w/app/sub"),
                Pick::Ambiguous(2)
            ),
            "same directory with and without the trailing slash is one depth"
        );
    }

    /// Plant a transcript `<name>` under a UNIQUE cwd's `~/.claude/projects`
    /// slug dir. Returns the cwd, the planted file path, and a cleanup guard.
    struct TxFixture {
        cwd: String,
        dir: std::path::PathBuf,
    }
    impl Drop for TxFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn plant_transcript(name: &str) -> (TxFixture, std::path::PathBuf) {
        use std::io::Write;
        let cwd = format!(
            "/ainb-test-answer-c1/{}/{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let mut dir = dirs::home_dir().expect("home dir");
        dir.push(".claude");
        dir.push("projects");
        dir.push(ainb_fleet_core::read::jsonl_tail::cwd_to_project_slug(&cwd));
        std::fs::create_dir_all(&dir).expect("create project dir");
        let file = dir.join(name);
        writeln!(std::fs::File::create(&file).unwrap(), "{{}}").unwrap();
        (TxFixture { cwd, dir }, file)
    }

    #[test]
    fn transcript_owns_cwd_when_it_is_the_newest() {
        let (fx, file) = plant_transcript("session-a.jsonl");
        // The captured transcript IS the newest in the cwd → the raiser owns it.
        assert!(transcript_still_owns_cwd(&fx.cwd, file.to_str().unwrap()));
        // A stale/never-there transcript name → not owning.
        assert!(!transcript_still_owns_cwd(
            &fx.cwd,
            "/anywhere/session-gone.jsonl"
        ));
    }

    #[test]
    fn transcript_does_not_own_cwd_after_a_newer_session_takes_over() {
        let (fx, original) = plant_transcript("session-original.jsonl");
        // A DIFFERENT agent starts in the same cwd, writing a newer transcript.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let newcomer = fx.dir.join("session-newcomer.jsonl");
        std::fs::write(&newcomer, "{}\n").unwrap();

        // The original raiser's transcript is no longer the newest → refuse.
        assert!(
            !transcript_still_owns_cwd(&fx.cwd, original.to_str().unwrap()),
            "a newer session in the cwd means the original no longer owns it"
        );
        // The newcomer (had it been the raiser) would own it.
        assert!(transcript_still_owns_cwd(
            &fx.cwd,
            newcomer.to_str().unwrap()
        ));
    }

    #[test]
    fn no_session_in_cwd_is_no_match_not_refuse() {
        let raw = vec![session("a", "/other", SessionSource::Ainb)];
        assert!(matches!(
            pick_target(&raw, &[], "hook-sid", "/work/x"),
            Pick::None
        ));
    }

    #[test]
    fn empty_cwd_without_exact_match_is_no_match() {
        let raw = vec![session("a", "", SessionSource::Ainb)];
        assert!(matches!(pick_target(&raw, &[], "hook-sid", ""), Pick::None));
    }

    // --- answer() store-integration paths that need no live discovery ---------

    async fn seed_ws_and_open_row(pool: &SqlitePool, id: &str, session_id: &str, cwd: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-a")
            .bind("ws-a")
            .bind("ws-a")
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
        AttentionRepo::insert(
            pool,
            &NewAttention {
                id: id.to_string(),
                session_id: session_id.to_string(),
                cwd: cwd.to_string(),
                workspace_id: Some("ws-a".to_string()),
                kind: AttentionKind::AskUserQuestion,
                payload: "{}".to_string(),
                degraded: false,
                created_at: 1_000,
                raise_transcript: None,
                channels: ainb_hangar_core::channel::ChannelSet::NONE,
            },
        )
        .await
        .unwrap();
    }

    fn broker_sink() -> (crate::events::EventBroker, EventSink) {
        let broker = crate::events::EventBroker::new();
        let sink = broker.sink();
        (broker, sink)
    }

    #[tokio::test]
    async fn answering_a_missing_row_is_no_target() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "nope".into(),
            answer: "x".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };
        let res = answer(store.pool(), &sink, &params, 5000).await.unwrap();
        assert!(matches!(res, AnswerResult::NoTarget { .. }));
    }

    #[tokio::test]
    async fn answering_an_already_answered_row_reports_the_winner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_ws_and_open_row(store.pool(), "a1", "sid", "/work/x").await;
        // A prior answer already resolved it.
        AttentionRepo::mark_answered_if_open(store.pool(), "a1", "web", "first", 4000)
            .await
            .unwrap();

        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "a1".into(),
            answer: "second".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };
        let res = answer(store.pool(), &sink, &params, 5000).await.unwrap();
        match res {
            AnswerResult::AlreadyAnswered { by } => assert_eq!(by, "web"),
            other => panic!("expected AlreadyAnswered, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_failed_delivery_reopens_the_claimed_row() {
        // Simulate the winning-flip-then-failed-send sequence: claim the row,
        // then run the compensation the send-failure arms. The row must return to
        // the open feed so the still-blocked agent's request stays answerable.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_ws_and_open_row(store.pool(), "a1", "sid", "/work/x").await;
        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "a1".into(),
            answer: "option 2".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };

        // Win the flip (as answer() does before the last-mile send).
        assert_eq!(
            AttentionRepo::mark_answered_if_open(store.pool(), "a1", "tui", "option 2", 5000)
                .await
                .unwrap(),
            1
        );
        let row = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();

        // The send failed → compensate.
        reopen_on_failed_delivery(store.pool(), &sink, &row, &params, 5000)
            .await
            .unwrap();

        let after = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();
        assert_eq!(
            after.state, "open",
            "a failed delivery must not strand the row"
        );
        assert!(after.answered_by.is_none());
        // A later, live re-answer is therefore possible (row is open again).
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1,
            "the reopened request is back on the fleet feed"
        );
    }

    #[tokio::test]
    async fn open_row_with_no_live_target_stays_open() {
        // A unique, guaranteed-nonexistent session id + cwd: no discovered session
        // (real or otherwise) can match, so resolve_target is deterministically
        // NoTarget regardless of what the host's live roster is. The row must NOT
        // be claimed — an undeliverable answer leaves it answerable later.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        );
        let cwd = format!("/no/such/cwd/{nonce}");
        seed_ws_and_open_row(
            store.pool(),
            "a1",
            &format!("no-such-session-{nonce}"),
            &cwd,
        )
        .await;

        let (_b, sink) = broker_sink();
        let params = AnswerParams {
            attention_id: "a1".into(),
            answer: "x".into(),
            answered_by: "tui".into(),
            is_answer: true,
        };
        let res = answer(store.pool(), &sink, &params, 5000).await.unwrap();
        assert!(
            matches!(res, AnswerResult::NoTarget { .. }),
            "no live session matches the unique id/cwd → NoTarget"
        );
        // The row is still open — we never claimed an undeliverable answer.
        let row = AttentionRepo::get(store.pool(), "a1").await.unwrap().unwrap();
        assert_eq!(
            row.state, "open",
            "an unresolved answer leaves the row open"
        );
    }
}
