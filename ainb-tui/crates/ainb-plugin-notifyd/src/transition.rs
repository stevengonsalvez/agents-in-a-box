//! Transition daemon — materialize `current_state` from the `events` log.
//!
//! agent-deck style: a background loop folds the append-only `events` log
//! into the materialized `current_state` read model, one row per
//! `(session_id, cwd)`. Readers (Wave 4) query `current_state` instead of
//! scanning transcripts/panes on the hot path.
//!
//! # What this module folds (and what it deliberately does NOT)
//!
//! This loop materializes **hook-sourced** state only (`source = "hook"`).
//! `notifyd` intentionally does NOT depend on the heavy `ainb`/`ainb-core`
//! crate (verified: it is absent from `Cargo.toml`), so the tmux / transcript
//! classification path (`first_api_error`, `last_api_error_from_jsonl`, the
//! full `classify()`) — which covers transient in-progress API errors and
//! NON-Claude agents (Codex/Gemini fire no Claude hooks) — lives in the `ainb`
//! crate and is merged at **READ time in Wave 4**: `fleet needs` reads
//! `current_state` for hook-backed sessions and falls back to `classify()`
//! for sessions absent from / stale in `current_state`. Pulling that logic
//! into notifyd would force a new heavy dependency, so option (a) from the
//! plan is taken here. See the Phase 4 reader migration for the fold.
//!
//! # The event → kind state machine (per `(session_id, cwd)`)
//!
//! Events are folded in `seq` order (oldest → newest); the **latest meaningful
//! event sets the state**. The classifier's snapshot priority is
//! `ASK > ERR > WAIT > IDLE`; because we fold an ordered stream rather than
//! evaluate a snapshot, that priority is realised as: a signal event sets its
//! kind, and a *newer* user turn (`UserPromptSubmit` / `SessionStart`) clears
//! it back to `RUNNING`. Concretely the rule is:
//!
//! | event_type · matcher                  | kind   | effect                                   |
//! |---------------------------------------|--------|------------------------------------------|
//! | `PreToolUse` · `AskUserQuestion`      | `ASK`  | unanswered interview is the latest signal |
//! | `Notification` · `permission_prompt`  | `WAIT` | waiting on the user (permission)          |
//! | `Notification` · `idle_prompt`        | `WAIT` | waiting on the user (idle input prompt)   |
//! | `StopFailure`                         | `ERR`  | API error after retries exhausted         |
//! | `Stop`                                | `IDLE` once `age >= idle_threshold`, else `RUNNING` (turn-ended-but-fresh) |
//! | `UserPromptSubmit` / `SessionStart`   | `RUNNING` | a new user turn → clears ASK/WAIT/IDLE/ERR |
//! | `SessionEnd`                          | `DONE` | terminal                                  |
//!
//! So the materialized kind is "the latest non-lifecycle signal (ASK/WAIT/ERR),
//! UNLESS a newer user turn reset it to RUNNING, OR a `Stop` aged it into IDLE,
//! OR `SessionEnd` ended it as DONE." This matches `fleet needs`: ASK when an
//! unanswered AskUserQuestion is the latest tool event; ERR when a StopFailure
//! is the newest signal; WAIT when a Notification is the newest; else
//! IDLE-on-age from the last Stop. `Stop` carries no `stop_reason` in
//! Claude Code 2.1.19 (the firing IS turn-end), so IDLE is purely age-driven
//! here — the `stop_reason` null/end_turn nuance stays in the tmux fallback.
//!
//! # IDLE is TIME-driven, not event-driven (the re-sweep)
//!
//! A `Stop` folded while `age < idle_threshold` is written `RUNNING` and would
//! otherwise NEVER become `IDLE` under steady state: the fold is cursor-driven,
//! so with no new event after the Stop, [`events_since`] returns nothing and the
//! pass returns early — the age transition would never fire. To fix that, a
//! freshly-stopped session is written `RUNNING` but with its stop timestamp
//! stamped into `context` as `{"stopped_ts": <ms>}`, and EVERY materialize pass
//! runs a second, TIME-driven [`resweep_idle`] step (independent of the events
//! cursor) that re-scans these stop-pending `RUNNING` rows and promotes any
//! whose `now - stopped_ts >= idle_threshold` to `IDLE` with `idle_minutes`.
//!
//! Readers treat `RUNNING` (with or without a `stopped_ts` marker) as
//! not-a-need, so the marker is invisible to the reader contract — the kind set
//! stays exactly `ASK|ERR|WAIT|IDLE|RUNNING|DONE`. Once promoted to `IDLE` the
//! row's context is the classifier-shaped `{"idle_minutes": N}`; a later user
//! turn clears it back to plain `RUNNING` (no marker) via the normal fold.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::{EventRow, StateRow, Store, StoreError};

/// Default idle threshold in minutes. Matches the classifier's
/// `DEFAULT_IDLE_MIN` (`ainb-core` `fleet::read::needs`); overridable via the
/// same `AINB_FLEET_IDLE_MIN` env var so hook-sourced IDLE and tmux-sourced
/// IDLE agree on when a stopped session becomes idle.
const DEFAULT_IDLE_MIN: i64 = 5;

/// Milliseconds per minute. IDLE age is `(now - stop_ts) / MS_PER_MIN` with a
/// `>= idle_threshold_min` gate — a FLOOR division that matches the classifier
/// exactly (`ainb-core` `fleet::read::needs` computes `age_ms / 60_000` and
/// gates with the same `>=`), so hook-IDLE and tmux-IDLE agree at the boundary
/// (e.g. both flip to IDLE at exactly 5 whole minutes, not 4 or 6).
const MS_PER_MIN: i64 = 60_000;

/// `context` key carrying the `Stop` timestamp on a stop-pending `RUNNING` row.
/// Its presence is the signal [`resweep_idle`] uses to find rows that should
/// flip to `IDLE` once they age past the threshold. Invisible to readers, which
/// treat any `RUNNING` row as not-a-need.
const STOPPED_TS_KEY: &str = "stopped_ts";

/// Materialized state kinds. String constants kept in lock-step with the
/// classifier's `NeedsContext` discriminants + the lifecycle kinds.
mod kind {
    pub const ASK: &str = "ASK";
    pub const ERR: &str = "ERR";
    pub const WAIT: &str = "WAIT";
    pub const IDLE: &str = "IDLE";
    pub const RUNNING: &str = "RUNNING";
    pub const DONE: &str = "DONE";
}

/// The provenance stamped on every row this loop writes.
const SOURCE_HOOK: &str = "hook";

/// ASK context — mirrors the classifier's `AskUserQuestionData`
/// (`ainb-core` `fleet::read::jsonl_tail`) field-for-field so a reader can
/// deserialize either source identically. `notifyd` redefines it (rather than
/// importing) to avoid an `ainb-core` dependency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AskUserQuestionData {
    question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    header: Option<String>,
    options: Vec<AskOption>,
    #[serde(default)]
    multi_select: bool,
}

/// One AskUserQuestion option — mirrors the classifier's `AskOption`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct AskOption {
    label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// WAIT context — the discriminator + any tool/message detail from the
/// `Notification` payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WaitContext {
    /// `permission_prompt` | `idle_prompt`.
    reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// ERR context — the `error_type` from the `StopFailure` payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ErrContext {
    error_type: String,
}

/// Fold one pass of the event log into `current_state`.
///
/// Reads every event with `seq > materialize_offset`, folds them per
/// `(session_id, cwd)`, upserts the resulting rows, and advances the durable
/// cursor to the highest folded `seq`. Idempotent: re-running picks up only
/// new events, so a row is never double-applied (a second call with no new
/// events is a no-op).
///
/// Returns the number of `current_state` rows upserted this pass (0 when there
/// was nothing new to fold).
pub fn materialize(store: &Store) -> Result<usize, StoreError> {
    let idle_threshold_min = idle_threshold_min();
    let now_ms = chrono::Utc::now().timestamp_millis();
    materialize_at(store, now_ms, idle_threshold_min)
}

/// `materialize` with the clock + threshold injected, so tests are deterministic
/// (no wall-clock, no env). Production calls [`materialize`].
pub fn materialize_at(
    store: &Store,
    now_ms: i64,
    idle_threshold_min: i64,
) -> Result<usize, StoreError> {
    // 1. EVENT-DRIVEN fold: apply every event past the durable cursor.
    let cursor = store.read_materialize_seq()?;
    let events = store.events_since(cursor)?;

    // Fold newest-wins per session. We carry the seed of the existing row so a
    // partial batch (e.g. a Stop folded last pass, a UserPromptSubmit this
    // pass) composes correctly across passes; but since each event is applied
    // in `seq` order and the latest wins, we only need to track the accumulator
    // PER (session,cwd) WITHIN this batch — the previous row was already the
    // fold of everything up to `cursor`.
    use std::collections::HashMap;
    let mut acc: HashMap<(String, String), Accum> = HashMap::new();
    let mut max_seq = cursor;

    for ev in &events {
        max_seq = max_seq.max(ev.seq);
        let key = (ev.session_id.clone(), ev.cwd.clone());
        let entry = acc.entry(key).or_default();
        entry.apply(ev);
    }

    let mut upserted = 0;
    for ((session_id, cwd), state) in acc {
        let Some(row) = state.into_row(session_id, cwd, now_ms, idle_threshold_min) else {
            continue;
        };
        store.upsert_current_state(&row)?;
        upserted += 1;
    }

    // Advance the cursor only after every fold landed. A crash mid-loop simply
    // re-folds the same events on restart (upsert is last-write-wins, so the
    // re-fold is idempotent) — we never skip un-folded events.
    if max_seq > cursor {
        store.write_materialize_seq(max_seq)?;
    }

    // 2. TIME-DRIVEN re-sweep: promote stop-pending RUNNING rows to IDLE on age,
    //    INDEPENDENT of the events cursor. This is what makes IDLE fire under
    //    steady state — a Stop folded while fresh leaves a `RUNNING` row marked
    //    with `stopped_ts`; with no further event the fold above is a no-op, so
    //    only this pass can flip the row to IDLE once it ages. Always runs (even
    //    when no new events arrived), so it must NOT be gated on `events`.
    upserted += resweep_idle(store, now_ms, idle_threshold_min)?;

    Ok(upserted)
}

/// Re-scan stop-pending `RUNNING` rows and promote any that have aged past the
/// idle threshold to `IDLE`. Returns the number of rows flipped this pass.
///
/// A stop-pending row is a `RUNNING` row whose `context` carries
/// [`STOPPED_TS_KEY`] (stamped by [`Accum::into_row`] when a fresh `Stop` was
/// the latest event). This step is the TIME-driven half of materialize: it runs
/// every pass regardless of whether any new event was folded, so a steady-state
/// session with no further hook activity still transitions Stop → IDLE on age.
///
/// Idempotent: a row already promoted to `IDLE` no longer matches the
/// `RUNNING + stopped_ts` predicate, and a fresh user turn rewrites the row to
/// plain `RUNNING` (no marker) so it is no longer a re-sweep candidate.
fn resweep_idle(store: &Store, now_ms: i64, idle_threshold_min: i64) -> Result<usize, StoreError> {
    let mut flipped = 0;
    for row in store.list_stop_pending_running()? {
        let Some(stop_ts) = stopped_ts_from_context(row.context.as_deref()) else {
            continue;
        };
        let age_min = now_ms.saturating_sub(stop_ts) / MS_PER_MIN;
        if age_min >= idle_threshold_min {
            let promoted = StateRow {
                kind: kind::IDLE.to_string(),
                context: Some(serde_json::json!({ "idle_minutes": age_min }).to_string()),
                ..row
            };
            store.upsert_current_state(&promoted)?;
            flipped += 1;
        }
    }
    Ok(flipped)
}

/// Extract the `stopped_ts` marker from a stop-pending `RUNNING` row's context.
fn stopped_ts_from_context(context: Option<&str>) -> Option<i64> {
    let ctx = context?;
    serde_json::from_str::<Value>(ctx)
        .ok()?
        .get(STOPPED_TS_KEY)
        .and_then(Value::as_i64)
}

/// The idle threshold (minutes) — same env knob + default as the classifier.
fn idle_threshold_min() -> i64 {
    std::env::var("AINB_FLEET_IDLE_MIN")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_IDLE_MIN)
}

/// Per-session fold accumulator. Tracks the latest meaningful signal plus the
/// lifecycle so the final kind can be decided once (at the end of the batch),
/// honoring the "newer user turn resets to RUNNING" rule.
#[derive(Debug, Default)]
struct Accum {
    /// The latest signal/lifecycle event that decides the kind.
    decided: Option<Decision>,
    /// `parent` from the latest event that carried one (sticky — a later event
    /// without a parent field does not clear an established parentage).
    parent: Option<String>,
    /// `ts` of the newest event applied (drives IDLE age + `last_event_ts`).
    last_event_ts: i64,
}

/// The kind + its context decided by the latest meaningful event.
#[derive(Debug, Clone)]
enum Decision {
    Ask(String),  // serialized AskUserQuestionData
    Err(String),  // serialized ErrContext
    Wait(String), // serialized WaitContext
    /// A `Stop` fired at this ts — resolved to IDLE-on-age or RUNNING when the
    /// row is built (age is recomputed against `now` at materialize time).
    Stopped(i64),
    /// A new user turn (UserPromptSubmit / SessionStart) → RUNNING.
    Running,
    /// SessionEnd → terminal DONE.
    Done(Option<String>), // optional reason
}

impl Accum {
    /// Apply one event, updating the decided kind newest-wins.
    fn apply(&mut self, ev: &EventRow) {
        self.last_event_ts = self.last_event_ts.max(ev.ts);
        if let Some(p) = parent_from_payload(&ev.payload) {
            self.parent = Some(p);
        }

        let decision = match ev.event_type.as_str() {
            "PreToolUse" if ev.matcher.as_deref() == Some("AskUserQuestion") => {
                Some(Decision::Ask(ask_context(&ev.payload)))
            }
            // A PreToolUse for any OTHER tool is not a needs-attention signal;
            // ignore it (don't reset an established ASK/WAIT/ERR).
            "PreToolUse" => None,
            "Notification" => Some(Decision::Wait(wait_context(
                ev.matcher.as_deref(),
                &ev.payload,
            ))),
            "StopFailure" => Some(Decision::Err(err_context(
                ev.matcher.as_deref(),
                &ev.payload,
            ))),
            "Stop" => Some(Decision::Stopped(ev.ts)),
            "UserPromptSubmit" | "SessionStart" => Some(Decision::Running),
            "SessionEnd" => Some(Decision::Done(reason_from_payload(&ev.payload))),
            // Unknown / telemetry event — does not change the decided kind.
            _ => None,
        };

        if let Some(d) = decision {
            self.decided = Some(d);
        }
    }

    /// Resolve the accumulated fold into a `current_state` row. Returns `None`
    /// when no meaningful event was seen for this session in the batch (so we
    /// don't clobber an existing row with a no-op).
    fn into_row(
        self,
        session_id: String,
        cwd: String,
        now_ms: i64,
        idle_threshold_min: i64,
    ) -> Option<StateRow> {
        let decided = self.decided?;
        let (kind, context) = match decided {
            Decision::Ask(ctx) => (kind::ASK, Some(ctx)),
            Decision::Err(ctx) => (kind::ERR, Some(ctx)),
            Decision::Wait(ctx) => (kind::WAIT, Some(ctx)),
            Decision::Done(reason) => {
                let ctx = reason.map(|r| serde_json::json!({ "reason": r }).to_string());
                (kind::DONE, ctx)
            }
            Decision::Running => (kind::RUNNING, None),
            Decision::Stopped(stop_ts) => {
                // Claude Code 2.1.19 `Stop` carries no stop_reason: the firing
                // IS turn-end. A freshly-stopped session is RUNNING/quiet until
                // it ages past the threshold, at which point it is IDLE — the
                // exact age semantics of the classifier's step-4 IDLE.
                //
                // CRITICAL: a fresh stop is written `RUNNING` but STAMPED with
                // `stopped_ts` so the time-driven `resweep_idle` can later flip
                // it to IDLE on age WITHOUT a new event. Without this marker the
                // age transition would never fire under steady state (no new
                // event → empty `events_since` → early no-op).
                let age_min = now_ms.saturating_sub(stop_ts) / MS_PER_MIN;
                if age_min >= idle_threshold_min {
                    let ctx = serde_json::json!({ "idle_minutes": age_min }).to_string();
                    (kind::IDLE, Some(ctx))
                } else {
                    // RUNNING, but carry the stop ts so the re-sweep can promote
                    // it once it ages. Readers treat RUNNING as not-a-need and
                    // ignore its context, so this marker is invisible to them.
                    let ctx = serde_json::json!({ STOPPED_TS_KEY: stop_ts }).to_string();
                    (kind::RUNNING, Some(ctx))
                }
            }
        };

        Some(StateRow {
            session_id,
            cwd,
            kind: kind.to_string(),
            context,
            parent: self.parent,
            last_event_ts: self.last_event_ts,
            source: SOURCE_HOOK.to_string(),
        })
    }
}

/// Extract the ASK context (serialized `AskUserQuestionData`) from a
/// `PreToolUse(AskUserQuestion)` payload. The raw hook stdin carries
/// `tool_input` = the AskUserQuestion tool input
/// (`{ questions: [{ question, header, options: [{label, description}],
/// multiSelect }] }`), the same shape the classifier reads from the transcript
/// tool_use block (`find_ask_user_question`). We take the FIRST question to
/// match the classifier. A best-effort empty shape is produced when the
/// payload is malformed so ASK still surfaces (with placeholder text) rather
/// than being dropped.
fn ask_context(payload: &str) -> String {
    let data = parse_ask(payload).unwrap_or_else(|| AskUserQuestionData {
        question: "(no question text)".to_string(),
        header: None,
        options: Vec::new(),
        multi_select: false,
    });
    serde_json::to_string(&data).unwrap_or_default()
}

/// Parse the first question out of a PreToolUse payload's `tool_input`.
fn parse_ask(payload: &str) -> Option<AskUserQuestionData> {
    let v: Value = serde_json::from_str(payload).ok()?;
    // `tool_input` is the universal PreToolUse field; tolerate `input` as an
    // alias (some producers / transcript blocks name it that way).
    let input = v.get("tool_input").or_else(|| v.get("input"))?;
    let questions = input.get("questions").and_then(Value::as_array)?;
    let first = questions.first()?;
    Some(AskUserQuestionData {
        question: first
            .get("question")
            .and_then(Value::as_str)
            .unwrap_or("(no question text)")
            .to_string(),
        header: first.get("header").and_then(Value::as_str).map(str::to_string),
        options: first
            .get("options")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|o| {
                        Some(AskOption {
                            label: o.get("label").and_then(Value::as_str)?.to_string(),
                            description: o
                                .get("description")
                                .and_then(Value::as_str)
                                .map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        multi_select: first.get("multiSelect").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// Build the WAIT context from the `Notification` matcher + payload.
fn wait_context(matcher: Option<&str>, payload: &str) -> String {
    let reason = matcher.unwrap_or("permission_prompt").to_string();
    let v: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let tool = v.get("tool_name").and_then(Value::as_str).map(str::to_string);
    let message = v.get("message").and_then(Value::as_str).map(str::to_string);
    serde_json::to_string(&WaitContext {
        reason,
        tool,
        message,
    })
    .unwrap_or_default()
}

/// Build the ERR context from the `StopFailure` matcher (the `error_type`) +
/// payload fallback.
fn err_context(matcher: Option<&str>, payload: &str) -> String {
    let error_type = matcher
        .map(str::to_string)
        .or_else(|| {
            serde_json::from_str::<Value>(payload)
                .ok()
                .and_then(|v| v.get("error_type").and_then(Value::as_str).map(str::to_string))
        })
        .unwrap_or_else(|| "unknown".to_string());
    serde_json::to_string(&ErrContext { error_type }).unwrap_or_default()
}

/// The session's `parent` — carried at the top level of the canonical event
/// line and folded into the row payload by the hook appender. `null`/absent
/// yields `None`.
fn parent_from_payload(payload: &str) -> Option<String> {
    let v: Value = serde_json::from_str(payload).ok()?;
    // The hook stamps `parent` at the line top-level; the ingest tailer stores
    // ONLY the inner `payload` object as `EventRow.payload`, so the parent may
    // also live inside the raw hook stdin payload. Check both.
    v.get("parent")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty())
}

/// The `reason` from a `SessionEnd` payload, when present.
fn reason_from_payload(payload: &str) -> Option<String> {
    serde_json::from_str::<Value>(payload)
        .ok()
        .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_string))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wall-clock ms anchor for deterministic age math.
    const NOW: i64 = 1_700_000_000_000;
    const IDLE_MIN: i64 = 5;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("a.db")).unwrap();
        (dir, store)
    }

    /// Append an event with a custom payload string.
    fn push(
        store: &Store,
        ts: i64,
        session: &str,
        cwd: &str,
        event_type: &str,
        matcher: Option<&str>,
        payload: &str,
    ) -> i64 {
        store
            .append_event(&EventRow {
                seq: 0,
                ts,
                session_id: session.into(),
                cwd: cwd.into(),
                transcript_path: format!("/t/{session}.jsonl"),
                agent: "claude".into(),
                event_type: event_type.into(),
                matcher: matcher.map(str::to_string),
                payload: payload.into(),
            })
            .unwrap()
    }

    fn state(store: &Store, session: &str, cwd: &str) -> StateRow {
        store
            .get_current_state(session, cwd)
            .unwrap()
            .unwrap_or_else(|| panic!("no current_state for {session}/{cwd}"))
    }

    #[test]
    fn ask_from_pretooluse_carries_question_and_options() {
        let (_d, store) = store();
        let payload = r#"{
            "transcript_path":"/t/s.jsonl",
            "tool_name":"AskUserQuestion",
            "tool_input":{"questions":[{
                "question":"Which deploy target?",
                "header":"Deploy",
                "options":[
                    {"label":"staging","description":"the staging cluster"},
                    {"label":"prod"}
                ],
                "multiSelect":true
            }]}
        }"#;
        push(
            &store,
            NOW,
            "s",
            "/p",
            "PreToolUse",
            Some("AskUserQuestion"),
            payload,
        );
        let n = materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(n, 1);

        let row = state(&store, "s", "/p");
        assert_eq!(row.kind, "ASK");
        assert_eq!(row.source, "hook");
        assert_eq!(row.last_event_ts, NOW);

        // Context deserializes back into the classifier-shaped struct.
        let ctx: AskUserQuestionData =
            serde_json::from_str(row.context.as_deref().unwrap()).unwrap();
        assert_eq!(ctx.question, "Which deploy target?");
        assert_eq!(ctx.header.as_deref(), Some("Deploy"));
        assert!(ctx.multi_select);
        assert_eq!(ctx.options.len(), 2);
        assert_eq!(ctx.options[0].label, "staging");
        assert_eq!(
            ctx.options[0].description.as_deref(),
            Some("the staging cluster")
        );
        assert_eq!(ctx.options[1].label, "prod");
        assert_eq!(ctx.options[1].description, None);
    }

    #[test]
    fn wait_from_notification_permission_and_idle() {
        let (_d, store) = store();
        push(
            &store,
            NOW,
            "perm",
            "/p",
            "Notification",
            Some("permission_prompt"),
            r#"{"notification_type":"permission_prompt","tool_name":"Bash","message":"allow Bash?"}"#,
        );
        push(
            &store,
            NOW,
            "idle",
            "/p",
            "Notification",
            Some("idle_prompt"),
            r#"{"notification_type":"idle_prompt","message":"waiting for input"}"#,
        );
        materialize_at(&store, NOW, IDLE_MIN).unwrap();

        let perm = state(&store, "perm", "/p");
        assert_eq!(perm.kind, "WAIT");
        let pctx: WaitContext = serde_json::from_str(perm.context.as_deref().unwrap()).unwrap();
        assert_eq!(pctx.reason, "permission_prompt");
        assert_eq!(pctx.tool.as_deref(), Some("Bash"));
        assert_eq!(pctx.message.as_deref(), Some("allow Bash?"));

        let idle = state(&store, "idle", "/p");
        assert_eq!(idle.kind, "WAIT");
        let ictx: WaitContext = serde_json::from_str(idle.context.as_deref().unwrap()).unwrap();
        assert_eq!(ictx.reason, "idle_prompt");
        assert_eq!(ictx.tool, None);
    }

    #[test]
    fn err_from_stopfailure_carries_error_type() {
        let (_d, store) = store();
        push(
            &store,
            NOW,
            "s",
            "/p",
            "StopFailure",
            Some("rate_limit"),
            r#"{"error_type":"rate_limit"}"#,
        );
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let row = state(&store, "s", "/p");
        assert_eq!(row.kind, "ERR");
        let ctx: ErrContext = serde_json::from_str(row.context.as_deref().unwrap()).unwrap();
        assert_eq!(ctx.error_type, "rate_limit");
    }

    #[test]
    fn err_falls_back_to_payload_error_type_when_matcher_absent() {
        let (_d, store) = store();
        push(
            &store,
            NOW,
            "s",
            "/p",
            "StopFailure",
            None,
            r#"{"error_type":"overloaded"}"#,
        );
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let ctx: ErrContext =
            serde_json::from_str(state(&store, "s", "/p").context.as_deref().unwrap()).unwrap();
        assert_eq!(ctx.error_type, "overloaded");
    }

    #[test]
    fn running_from_user_prompt_submit() {
        let (_d, store) = store();
        push(&store, NOW, "s", "/p", "UserPromptSubmit", None, "{}");
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let row = state(&store, "s", "/p");
        assert_eq!(row.kind, "RUNNING");
        assert_eq!(row.context, None);
    }

    #[test]
    fn idle_from_stop_older_than_threshold() {
        let (_d, store) = store();
        // Stop fired 10 min ago, threshold 5 min → IDLE.
        let stop_ts = NOW - 10 * 60_000;
        push(
            &store,
            stop_ts,
            "s",
            "/p",
            "Stop",
            None,
            r#"{"transcript_path":"/t/s.jsonl"}"#,
        );
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let row = state(&store, "s", "/p");
        assert_eq!(row.kind, "IDLE");
        let ctx: Value = serde_json::from_str(row.context.as_deref().unwrap()).unwrap();
        assert_eq!(ctx["idle_minutes"], 10);
    }

    #[test]
    fn fresh_stop_is_running_not_yet_idle() {
        let (_d, store) = store();
        // Stop fired 1 min ago, below the 5-min threshold → RUNNING/quiet.
        let stop_ts = NOW - 60_000;
        push(&store, stop_ts, "s", "/p", "Stop", None, "{}");
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let row = state(&store, "s", "/p");
        assert_eq!(
            row.kind, "RUNNING",
            "a freshly-stopped session is not yet IDLE"
        );
        // The RUNNING row must carry the stop marker so the re-sweep can later
        // promote it WITHOUT a new event arriving.
        let ctx: Value = serde_json::from_str(row.context.as_deref().unwrap()).unwrap();
        assert_eq!(ctx["stopped_ts"], stop_ts, "fresh stop stamps stopped_ts");
    }

    #[test]
    fn resweep_promotes_fresh_stop_to_idle_on_age_without_a_new_event() {
        // THE headline bug: under steady state a fresh Stop must become IDLE on
        // AGE alone — no new event, no back-dated timestamp. We fold a Stop that
        // is fresh at fold time (RUNNING), then advance ONLY a synthetic clock
        // past the threshold and re-run materialize. The TIME-driven re-sweep —
        // not the events cursor — must flip the row to IDLE.
        let (_d, store) = store();
        let stop_ts = NOW; // fired "now": age 0 at first fold.
        push(&store, stop_ts, "s", "/p", "Stop", None, "{}");

        // Pass 1 at NOW: age 0 < threshold → RUNNING (stop-pending).
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(
            state(&store, "s", "/p").kind,
            "RUNNING",
            "fresh stop is RUNNING at fold time"
        );

        // No new event arrives. Advance the clock 6 min past the stop.
        let later = NOW + 6 * 60_000;
        // events_since(cursor) is now EMPTY — only the re-sweep can act.
        let flipped = materialize_at(&store, later, IDLE_MIN).unwrap();
        assert!(flipped >= 1, "re-sweep flips the aged stop-pending row");

        let row = state(&store, "s", "/p");
        assert_eq!(
            row.kind, "IDLE",
            "steady-state age transition fires via the re-sweep (the headline fix)"
        );
        let ctx: Value = serde_json::from_str(row.context.as_deref().unwrap()).unwrap();
        assert_eq!(ctx["idle_minutes"], 6);
    }

    #[test]
    fn resweep_is_idempotent_and_user_turn_clears_pending() {
        // Once promoted to IDLE the row is no longer a re-sweep candidate (so a
        // second sweep is a no-op), and a later user turn rewrites it to plain
        // RUNNING with no marker — also no longer a candidate.
        let (_d, store) = store();
        push(&store, NOW, "s", "/p", "Stop", None, "{}");
        materialize_at(&store, NOW, IDLE_MIN).unwrap();

        let later = NOW + 6 * 60_000;
        materialize_at(&store, later, IDLE_MIN).unwrap();
        assert_eq!(state(&store, "s", "/p").kind, "IDLE");
        // Re-running the sweep does not re-flip / re-count an already-IDLE row.
        let again = materialize_at(&store, later + 60_000, IDLE_MIN).unwrap();
        assert_eq!(again, 0, "already-IDLE row is not a re-sweep candidate");

        // A user turn after IDLE → plain RUNNING (no stopped_ts marker).
        push(
            &store,
            later + 120_000,
            "s",
            "/p",
            "UserPromptSubmit",
            None,
            "{}",
        );
        materialize_at(&store, later + 120_000, IDLE_MIN).unwrap();
        let row = state(&store, "s", "/p");
        assert_eq!(row.kind, "RUNNING");
        assert_eq!(row.context, None, "a real user turn carries no stop marker");
        // And it stays RUNNING even as the clock advances (no pending stop).
        let n = materialize_at(&store, later + 999_999, IDLE_MIN).unwrap();
        assert_eq!(n, 0);
        assert_eq!(state(&store, "s", "/p").kind, "RUNNING");
    }

    #[test]
    fn user_prompt_after_stop_resets_to_running() {
        let (_d, store) = store();
        // An old Stop (would be IDLE) followed by a new user turn → RUNNING.
        let stop_ts = NOW - 10 * 60_000;
        push(&store, stop_ts, "s", "/p", "Stop", None, "{}");
        push(&store, NOW, "s", "/p", "UserPromptSubmit", None, "{}");
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let row = state(&store, "s", "/p");
        assert_eq!(row.kind, "RUNNING", "a new user turn clears IDLE");
        assert_eq!(row.last_event_ts, NOW);
    }

    #[test]
    fn user_prompt_after_ask_resets_to_running() {
        let (_d, store) = store();
        // ASK then a new user turn (the user answered / moved on) → RUNNING.
        push(
            &store,
            NOW - 1000,
            "s",
            "/p",
            "PreToolUse",
            Some("AskUserQuestion"),
            r#"{"tool_input":{"questions":[{"question":"q?","options":[]}]}}"#,
        );
        push(&store, NOW, "s", "/p", "UserPromptSubmit", None, "{}");
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(state(&store, "s", "/p").kind, "RUNNING");
    }

    #[test]
    fn ask_after_user_prompt_wins() {
        let (_d, store) = store();
        // A user turn then an ASK → ASK is the latest meaningful event.
        push(
            &store,
            NOW - 1000,
            "s",
            "/p",
            "UserPromptSubmit",
            None,
            "{}",
        );
        push(
            &store,
            NOW,
            "s",
            "/p",
            "PreToolUse",
            Some("AskUserQuestion"),
            r#"{"tool_input":{"questions":[{"question":"pick?","options":[{"label":"a"}]}]}}"#,
        );
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(state(&store, "s", "/p").kind, "ASK");
    }

    #[test]
    fn done_from_session_end_is_terminal() {
        let (_d, store) = store();
        push(
            &store,
            NOW - 1000,
            "s",
            "/p",
            "UserPromptSubmit",
            None,
            "{}",
        );
        push(
            &store,
            NOW,
            "s",
            "/p",
            "SessionEnd",
            None,
            r#"{"reason":"clear"}"#,
        );
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let row = state(&store, "s", "/p");
        assert_eq!(row.kind, "DONE");
        let ctx: Value = serde_json::from_str(row.context.as_deref().unwrap()).unwrap();
        assert_eq!(ctx["reason"], "clear");
    }

    #[test]
    fn parent_is_carried_into_the_row() {
        let (_d, store) = store();
        push(
            &store,
            NOW,
            "child",
            "/p",
            "Stop",
            None,
            r#"{"parent":"parent-sid","transcript_path":"/t/c.jsonl"}"#,
        );
        // Stop is fresh → RUNNING, but parent must still be carried.
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        let row = state(&store, "child", "/p");
        assert_eq!(row.parent.as_deref(), Some("parent-sid"));
    }

    #[test]
    fn pretooluse_for_other_tool_does_not_set_ask() {
        let (_d, store) = store();
        // A Bash PreToolUse is not a needs-attention signal: it must not
        // produce an ASK row, and with no other meaningful event there is no
        // row at all (we don't clobber with a no-op).
        push(
            &store,
            NOW,
            "s",
            "/p",
            "PreToolUse",
            Some("Bash"),
            r#"{"tool_name":"Bash"}"#,
        );
        let n = materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(n, 0, "non-ASK PreToolUse produces no state row");
        assert!(store.get_current_state("s", "/p").unwrap().is_none());
    }

    #[test]
    fn cursor_advances_and_is_idempotent() {
        let (_d, store) = store();
        push(&store, NOW, "s", "/p", "UserPromptSubmit", None, "{}");
        let first = materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(first, 1);
        let cursor_after = store.read_materialize_seq().unwrap();
        assert!(cursor_after > 0, "cursor advanced past the folded event");

        // Re-running with no new events is a no-op (no double-apply).
        let second = materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(second, 0, "re-running folds nothing new");
        assert_eq!(store.read_materialize_seq().unwrap(), cursor_after);

        // A NEW event after the cursor is folded on the next pass only.
        push(
            &store,
            NOW + 1000,
            "s",
            "/p",
            "StopFailure",
            Some("server_error"),
            r#"{"error_type":"server_error"}"#,
        );
        let third = materialize_at(&store, NOW + 1000, IDLE_MIN).unwrap();
        assert_eq!(third, 1);
        assert_eq!(state(&store, "s", "/p").kind, "ERR");
        assert!(store.read_materialize_seq().unwrap() > cursor_after);
    }

    #[test]
    fn cross_pass_fold_composes_via_persisted_row() {
        let (_d, store) = store();
        // Pass 1: an ASK lands.
        push(
            &store,
            NOW,
            "s",
            "/p",
            "PreToolUse",
            Some("AskUserQuestion"),
            r#"{"tool_input":{"questions":[{"question":"q?","options":[]}]}}"#,
        );
        materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(state(&store, "s", "/p").kind, "ASK");

        // Pass 2: a later user turn (only this NEW event is in the batch) must
        // still reset the persisted ASK row to RUNNING.
        push(
            &store,
            NOW + 1000,
            "s",
            "/p",
            "UserPromptSubmit",
            None,
            "{}",
        );
        materialize_at(&store, NOW + 1000, IDLE_MIN).unwrap();
        assert_eq!(state(&store, "s", "/p").kind, "RUNNING");
    }

    #[test]
    fn separate_sessions_fold_independently() {
        let (_d, store) = store();
        push(
            &store,
            NOW,
            "a",
            "/p",
            "PreToolUse",
            Some("AskUserQuestion"),
            r#"{"tool_input":{"questions":[{"question":"q?","options":[]}]}}"#,
        );
        push(
            &store,
            NOW,
            "b",
            "/p",
            "StopFailure",
            Some("overloaded"),
            r#"{"error_type":"overloaded"}"#,
        );
        let n = materialize_at(&store, NOW, IDLE_MIN).unwrap();
        assert_eq!(n, 2);
        assert_eq!(state(&store, "a", "/p").kind, "ASK");
        assert_eq!(state(&store, "b", "/p").kind, "ERR");
    }
}
