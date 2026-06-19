// ABOUTME: Read the event-sourced `current_state` table and convert its rows
// into the classifier's `NeedsRow` shape, so `fleet needs` (and the heartbeat
// that shells it) can read hook-materialized state on the hot path instead of
// scanning panes/transcripts.
//
// This is the READ-time half of the Wave 4 migration. The notifyd transition
// daemon materializes one `current_state` row per `(session_id, cwd)` from the
// append-only `events` log (`source = "hook"`). Here we open that SQLite store
// READ-ONLY — exactly the way the TUI Inbox screen does (`components/inbox.rs`)
// — and fold the rows back into the `NeedsRow`/`NeedsContext` types the rest of
// the fleet read-side already speaks. We do NOT re-implement SQLite access; we
// reuse the notifyd `Store`.
//
// Mapping (kind → NeedsContext), mirroring the classifier's priorities and its
// "healthy session ⇒ no row" rule:
//
//   ASK     → NeedsContext::Ask   (context is the AskUserQuestionData verbatim)
//   ERR     → NeedsContext::Err   (error_type → pattern + snippet)
//   WAIT    → NeedsContext::Wait  (reason → marker, message/tool → text)
//   IDLE    → NeedsContext::Idle  (idle_minutes; no transcript text from hooks)
//   RUNNING → None                (actively working — not a "need")
//   DONE    → None                (terminal — surfaced via the inbox, not needs)
//
// RUNNING/DONE return `None` exactly as `classify()` returns `None` for a
// healthy session, so a working/finished session never appears in `fleet needs`.
//
// The materialized row is keyed by `(session_id, cwd)`; the fleet correlates a
// `Session` to its hook-side state by **cwd** (the same cwd-correlation layer
// the Inbox uses — see `Store::unread_by_cwd`). A `CurrentStateIndex` groups the
// rows by cwd so the merge in `cli/fleet/needs.rs` is an O(1) lookup per
// session.

use std::collections::HashMap;

use ainb_plugin_notifyd::{Paths, StateRow, Store};

use crate::fleet::read::jsonl_tail::AskUserQuestionData;
use crate::fleet::read::needs::{
    ErrContext, IdleContext, NeedsContext, NeedsRow, RouteHint, WaitContext, make_row,
};
use crate::fleet::types::Session;

/// Provenance string the notifyd materializer stamps on a hook-sourced row.
pub const SOURCE_HOOK: &str = "hook";
/// Provenance string for a tmux/transcript-folded row (non-Claude / transient).
pub const SOURCE_TMUX: &str = "tmux";

/// A folded view of the `current_state` table, indexed by `cwd` for the
/// per-session merge. Holds the *most recent* hook-sourced row per cwd (the
/// materializer keeps one row per `(session_id, cwd)`; if two sessions share a
/// cwd we keep the freshest by `last_event_ts`, matching how a reader would see
/// "the latest thing happening in this directory").
#[derive(Debug, Default)]
pub struct CurrentStateIndex {
    by_cwd: HashMap<String, StateRow>,
}

impl CurrentStateIndex {
    /// Open the notifyd store READ-ONLY and snapshot `current_state` into a
    /// cwd-indexed map. Returns an empty index (never an error) when the DB is
    /// absent or unreadable — the caller then transparently falls back to the
    /// tmux/transcript `classify()` path for every session, exactly as before
    /// the event store existed. This keeps `fleet needs` working with the
    /// daemon down / not yet installed.
    #[must_use]
    pub fn load() -> Self {
        let Some(db) = Paths::from_home().ok().map(|p| p.db) else {
            return Self::default();
        };
        // Mirror inbox.rs: only attempt to open when the file already exists, so
        // a never-installed notifyd doesn't create an empty db as a side effect
        // of a read.
        if !db.exists() {
            return Self::default();
        }
        match Store::open(&db) {
            Ok(store) => match store.list_current_state() {
                Ok(rows) => Self::from_rows(rows),
                Err(e) => {
                    tracing::warn!(error = ?e, "fleet needs: list_current_state failed; tmux fallback");
                    Self::default()
                }
            },
            Err(e) => {
                tracing::warn!(error = ?e, path = %db.display(), "fleet needs: current_state store open failed; tmux fallback");
                Self::default()
            }
        }
    }

    /// Build an index from already-loaded rows. Public for tests + so a caller
    /// holding a `Store` can avoid re-opening. Keeps the freshest row per cwd.
    #[must_use]
    pub fn from_rows(rows: Vec<StateRow>) -> Self {
        let mut by_cwd: HashMap<String, StateRow> = HashMap::new();
        for row in rows {
            match by_cwd.get(&row.cwd) {
                Some(existing) if existing.last_event_ts >= row.last_event_ts => {}
                _ => {
                    by_cwd.insert(row.cwd.clone(), row);
                }
            }
        }
        Self { by_cwd }
    }

    /// The materialized row for a session's cwd, if any.
    #[must_use]
    pub fn get(&self, cwd: &str) -> Option<&StateRow> {
        self.by_cwd.get(cwd)
    }

    /// Resolve a session against the event-sourced state. Returns:
    ///
    /// - `Resolution::Hook(row)` — an authoritative hook-sourced needs row is
    ///   present (ASK/ERR/WAIT/IDLE). The caller uses it directly and does NOT
    ///   scan the pane/transcript.
    /// - `Resolution::Healthy` — a hook-sourced row says the session is
    ///   RUNNING/DONE (working/finished), so it is NOT a need; the caller emits
    ///   nothing for it (mirroring `classify()` returning `None`). This still
    ///   suppresses the tmux fallback: the hooks know better than a pane scan.
    /// - `Resolution::Fallback` — no usable hook row (absent, `source = tmux`,
    ///   or stale): the caller runs the tmux/transcript `classify()` path. This
    ///   is the path that covers non-Claude agents (Codex/Gemini fire no Claude
    ///   hooks) and transient in-progress API errors.
    #[must_use]
    pub fn resolve(&self, session: &Session, now_ms: i64, stale_window_ms: i64) -> Resolution {
        let Some(row) = self.by_cwd.get(&session.cwd) else {
            return Resolution::Fallback;
        };
        // Non-Claude / tmux-folded rows are not authoritative on the hot path:
        // defer to the live classify() scan, which is the canonical source for
        // those agents and for transient errors.
        if row.source != SOURCE_HOOK {
            return Resolution::Fallback;
        }
        // Defensive staleness guard: when enabled (window > 0) a hook row whose
        // newest event is older than the window is treated as unreliable (the
        // daemon may have stopped materializing) and we fall back. Disabled by
        // default (window = 0) because ASK/ERR/WAIT are *sticky* states — an
        // interview can sit unanswered for an hour and still be the truth, so
        // age alone is a poor staleness signal; only flip this on when a caller
        // has an independent reason to distrust an old row.
        if stale_window_ms > 0 && now_ms.saturating_sub(row.last_event_ts) > stale_window_ms {
            return Resolution::Fallback;
        }
        match needs_row_from_state(session.clone(), row) {
            Some(needs) => Resolution::Hook(Box::new(needs)),
            // RUNNING / DONE → healthy, not a need, but still hook-authoritative.
            None => Resolution::Healthy,
        }
    }
}

/// Outcome of resolving one session against `current_state`.
#[derive(Debug)]
pub enum Resolution {
    /// An authoritative hook-sourced needs row; use it, skip the pane scan.
    Hook(Box<NeedsRow>),
    /// Hook says the session is healthy (RUNNING/DONE) — emit no needs row,
    /// and do not fall back to a pane scan.
    Healthy,
    /// No usable hook row — run the tmux/transcript `classify()` path.
    Fallback,
}

/// Convert a single hook-sourced [`StateRow`] into a [`NeedsRow`], or `None`
/// for the healthy lifecycle kinds (RUNNING/DONE) that are not "needs".
///
/// The `source` of the produced row is carried from the StateRow so a consumer
/// (and the JSON output) can tell hook-sourced from tmux-sourced needs.
#[must_use]
pub fn needs_row_from_state(session: Session, row: &StateRow) -> Option<NeedsRow> {
    let route_hint = RouteHint::from_session(&session);
    let context = context_from_state(&row.kind, row.context.as_deref())?;
    let mut needs = make_row(session, context, route_hint);
    needs.source = Some(row.source.clone());
    Some(needs)
}

/// Map a `(kind, context-json)` pair from `current_state` to a `NeedsContext`.
/// Returns `None` for RUNNING/DONE (healthy) and for any unknown kind.
fn context_from_state(kind: &str, context_json: Option<&str>) -> Option<NeedsContext> {
    match kind {
        "ASK" => {
            // Context is the AskUserQuestionData the materializer serialized
            // from the PreToolUse payload — the exact shape the classifier
            // emits from the transcript, so it round-trips into the same type.
            let aq: AskUserQuestionData = context_json
                .and_then(|c| serde_json::from_str(c).ok())
                .unwrap_or_else(|| AskUserQuestionData {
                    question: "(no question text)".to_string(),
                    header: None,
                    options: Vec::new(),
                    multi_select: false,
                });
            Some(NeedsContext::Ask(aq))
        }
        "ERR" => {
            // The materializer's ERR context is `{ "error_type": "<type>" }`.
            // The classifier's ErrContext is `{ pattern, snippet }`; map
            // error_type onto both so the existing renderers (text + web) show
            // the error without any shape change.
            let error_type = context_json
                .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
                .and_then(|v| v.get("error_type").and_then(|t| t.as_str()).map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            Some(NeedsContext::Err(ErrContext {
                pattern: error_type.clone(),
                snippet: error_type,
            }))
        }
        "WAIT" => {
            // The materializer's WAIT context is
            // `{ reason, tool?, message? }`. Map reason → marker and the most
            // descriptive available field → text.
            let v = context_json.and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok());
            let reason = v
                .as_ref()
                .and_then(|v| v.get("reason").and_then(|r| r.as_str()))
                .unwrap_or("notification")
                .to_string();
            let text = v
                .as_ref()
                .and_then(|v| {
                    v.get("message")
                        .and_then(|m| m.as_str())
                        .or_else(|| v.get("tool").and_then(|t| t.as_str()))
                })
                .unwrap_or("")
                .to_string();
            Some(NeedsContext::Wait(WaitContext {
                marker: reason,
                text,
            }))
        }
        "IDLE" => {
            // The materializer's IDLE context is `{ "idle_minutes": N }`. The
            // hook has no transcript text, so `last_assistant_text` is None
            // (the classifier fills it on the tmux path).
            let idle_minutes = context_json
                .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
                .and_then(|v| v.get("idle_minutes").and_then(serde_json::Value::as_i64))
                .unwrap_or(0);
            Some(NeedsContext::Idle(IdleContext {
                idle_minutes,
                last_assistant_text: None,
            }))
        }
        // RUNNING / DONE are healthy lifecycle states — not a "need". Unknown
        // kinds are treated the same (safe: nothing surfaces).
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::SessionSource;

    fn mk_session(cwd: &str) -> Session {
        Session {
            id: "sid".to_string(),
            cwd: cwd.to_string(),
            pid: None,
            git_root: None,
            tmux_session: Some("tmux_test".to_string()),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![SessionSource::Ainb],
            summary: None,
            last_seen_ms: None,
        }
    }

    fn state(cwd: &str, kind: &str, context: Option<&str>, source: &str, ts: i64) -> StateRow {
        StateRow {
            session_id: "hook-sid".to_string(),
            cwd: cwd.to_string(),
            kind: kind.to_string(),
            context: context.map(str::to_string),
            parent: None,
            last_event_ts: ts,
            source: source.to_string(),
        }
    }

    #[test]
    fn ask_state_maps_to_ask_needs_with_question_and_options() {
        let ctx = r#"{"question":"Pick one?","header":"Deploy","options":[{"label":"prod","description":"the prod cluster"},{"label":"staging"}],"multi_select":true}"#;
        let row = state("/p", "ASK", Some(ctx), SOURCE_HOOK, 100);
        let needs = needs_row_from_state(mk_session("/p"), &row).expect("ASK row");
        assert_eq!(needs.source.as_deref(), Some("hook"));
        match needs.context {
            NeedsContext::Ask(aq) => {
                assert_eq!(aq.question, "Pick one?");
                assert_eq!(aq.header.as_deref(), Some("Deploy"));
                assert!(aq.multi_select);
                assert_eq!(aq.options.len(), 2);
                assert_eq!(aq.options[0].label, "prod");
                assert_eq!(
                    aq.options[0].description.as_deref(),
                    Some("the prod cluster")
                );
                assert_eq!(aq.options[1].label, "staging");
            }
            other => panic!("expected Ask, got {other:?}"),
        }
        // The content key is stamped, so enrichment caching still keys correctly.
        assert!(!needs.enrich_key.is_empty());
    }

    #[test]
    fn err_state_maps_error_type_onto_pattern_and_snippet() {
        let row = state(
            "/p",
            "ERR",
            Some(r#"{"error_type":"rate_limit"}"#),
            SOURCE_HOOK,
            100,
        );
        let needs = needs_row_from_state(mk_session("/p"), &row).expect("ERR row");
        match needs.context {
            NeedsContext::Err(e) => {
                assert_eq!(e.pattern, "rate_limit");
                assert_eq!(e.snippet, "rate_limit");
            }
            other => panic!("expected Err, got {other:?}"),
        }
    }

    #[test]
    fn wait_state_maps_reason_and_message() {
        let row = state(
            "/p",
            "WAIT",
            Some(r#"{"reason":"permission_prompt","tool":"Bash","message":"allow Bash?"}"#),
            SOURCE_HOOK,
            100,
        );
        let needs = needs_row_from_state(mk_session("/p"), &row).expect("WAIT row");
        match needs.context {
            NeedsContext::Wait(w) => {
                assert_eq!(w.marker, "permission_prompt");
                assert_eq!(w.text, "allow Bash?");
            }
            other => panic!("expected Wait, got {other:?}"),
        }
    }

    #[test]
    fn idle_state_maps_idle_minutes_without_transcript_text() {
        let row = state(
            "/p",
            "IDLE",
            Some(r#"{"idle_minutes":12}"#),
            SOURCE_HOOK,
            100,
        );
        let needs = needs_row_from_state(mk_session("/p"), &row).expect("IDLE row");
        match needs.context {
            NeedsContext::Idle(i) => {
                assert_eq!(i.idle_minutes, 12);
                assert_eq!(i.last_assistant_text, None);
            }
            other => panic!("expected Idle, got {other:?}"),
        }
    }

    #[test]
    fn running_and_done_are_not_needs() {
        let running = state("/p", "RUNNING", None, SOURCE_HOOK, 100);
        assert!(needs_row_from_state(mk_session("/p"), &running).is_none());
        let done = state(
            "/p",
            "DONE",
            Some(r#"{"reason":"clear"}"#),
            SOURCE_HOOK,
            100,
        );
        assert!(needs_row_from_state(mk_session("/p"), &done).is_none());
    }

    #[test]
    fn resolve_hook_for_ask_row() {
        let idx = CurrentStateIndex::from_rows(vec![state(
            "/p",
            "ASK",
            Some(r#"{"question":"q?","options":[]}"#),
            SOURCE_HOOK,
            100,
        )]);
        match idx.resolve(&mk_session("/p"), 200, 0) {
            Resolution::Hook(row) => assert!(matches!(row.context, NeedsContext::Ask(_))),
            other => panic!("expected Hook, got {other:?}"),
        }
    }

    #[test]
    fn resolve_healthy_for_running_row_suppresses_fallback() {
        let idx =
            CurrentStateIndex::from_rows(vec![state("/p", "RUNNING", None, SOURCE_HOOK, 100)]);
        assert!(matches!(
            idx.resolve(&mk_session("/p"), 200, 0),
            Resolution::Healthy
        ));
    }

    #[test]
    fn resolve_fallback_when_session_absent_from_current_state() {
        // A tmux-only / non-Claude session has no current_state row at its cwd.
        let idx =
            CurrentStateIndex::from_rows(vec![state("/other", "ASK", None, SOURCE_HOOK, 100)]);
        assert!(matches!(
            idx.resolve(&mk_session("/p"), 200, 0),
            Resolution::Fallback
        ));
    }

    #[test]
    fn resolve_fallback_when_source_is_tmux() {
        let idx = CurrentStateIndex::from_rows(vec![state("/p", "ERR", None, SOURCE_TMUX, 100)]);
        assert!(matches!(
            idx.resolve(&mk_session("/p"), 200, 0),
            Resolution::Fallback
        ));
    }

    #[test]
    fn resolve_fallback_when_row_is_stale_and_window_enabled() {
        let idx = CurrentStateIndex::from_rows(vec![state("/p", "IDLE", None, SOURCE_HOOK, 0)]);
        // now=10min, window=5min → stale → fallback.
        assert!(matches!(
            idx.resolve(&mk_session("/p"), 10 * 60_000, 5 * 60_000),
            Resolution::Fallback
        ));
        // window disabled (0) → still authoritative even though old.
        assert!(matches!(
            idx.resolve(&mk_session("/p"), 10 * 60_000, 0),
            Resolution::Hook(_)
        ));
    }

    #[test]
    fn from_rows_keeps_freshest_row_per_cwd() {
        let idx = CurrentStateIndex::from_rows(vec![
            state("/p", "IDLE", None, SOURCE_HOOK, 100),
            state(
                "/p",
                "ASK",
                Some(r#"{"question":"q?","options":[]}"#),
                SOURCE_HOOK,
                200,
            ),
        ]);
        let row = idx.get("/p").expect("row for /p");
        assert_eq!(row.kind, "ASK", "freshest (ts=200) wins");
    }
}
