// ABOUTME: Read the event-sourced `current_state` table and convert its rows
// into the classifier's `NeedsRow` shape, so `fleet needs` (and the heartbeat
// that shells it) can read hook-materialized state on the hot path instead of
// scanning panes/transcripts.
//
// This is the READ-time half of the Wave 4 migration. The notifyd transition
// daemon materializes one `current_state` row per `(session_id, cwd)` from the
// append-only `events` log (`source = "hook"`). Here we open that SQLite store
// strictly READ-ONLY via `Store::open_readonly` (SQLITE_OPEN_READ_ONLY: no
// migrate, no create, no WAL pragma writes) so this reader path — `fleet needs`
// and the heartbeat that shells it — can NEVER migrate or mutate the daemon's
// DB. We fold the rows back into the `NeedsRow`/`NeedsContext` types the rest
// of the fleet read-side already speaks. We do NOT re-implement SQLite access;
// we reuse the notifyd `Store`.
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
        // READ-ONLY: never migrate/create/WAL-write the daemon's DB from the
        // reader path (a `fleet needs`/heartbeat must not mutate notifyd state).
        match Store::open_readonly(&db) {
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
        self.resolve_with_healthy_window(
            session,
            now_ms,
            stale_window_ms,
            effective_stale_window_ms(),
        )
    }

    /// `resolve()` with the HEALTHY-suppressing-kind staleness window passed in
    /// explicitly (instead of read from the env). Pure + deterministic, so the
    /// staleness behaviour is unit-testable without mutating process env (the
    /// crate's tests can't call the now-`unsafe` `set_var`).
    #[must_use]
    pub fn resolve_with_healthy_window(
        &self,
        session: &Session,
        now_ms: i64,
        stale_window_ms: i64,
        healthy_window_ms: i64,
    ) -> Resolution {
        // An empty-cwd session cannot be safely correlated to a current_state
        // row by cwd: two distinct empty-cwd sessions collapse onto the same
        // `""` key in `by_cwd`, so any match here would be a mis-attribution.
        // Never trust an empty-cwd current_state match — fall back to the live
        // tmux/transcript scan (which keys on the real session, not the cwd).
        if session.cwd.is_empty() {
            return Resolution::Fallback;
        }
        let Some(row) = self.by_cwd.get(&session.cwd) else {
            return Resolution::Fallback;
        };
        // Non-Claude / tmux-folded rows are not authoritative on the hot path:
        // defer to the live classify() scan, which is the canonical source for
        // those agents and for transient errors.
        if row.source != SOURCE_HOOK {
            return Resolution::Fallback;
        }
        // Staleness guard. ASK/ERR/WAIT are *sticky* needs — an interview can
        // sit unanswered for an hour and still be the truth — so age is a poor
        // staleness signal for them and the window stays OFF (a stale ASK is
        // still a real need). The HEALTHY-suppressing kinds (RUNNING/DONE) are
        // the dangerous case: a dead daemon that stopped materializing leaves a
        // stale RUNNING/DONE row that would keep the session `Healthy` forever,
        // SUPPRESSING the tmux fallback that would otherwise surface a real,
        // newly-arrived need. So for RUNNING/DONE we apply `healthy_window_ms`
        // even when the caller passed 0 — a stale healthy row then falls back to
        // a live scan after the window.
        let is_healthy_kind = matches!(row.kind.as_str(), "RUNNING" | "DONE");
        let window = if is_healthy_kind {
            // For healthy-suppressing kinds: honour an explicit caller window if
            // larger, else the env/default floor. (Sticky kinds use the caller's
            // window verbatim — 0 = off.)
            stale_window_ms.max(healthy_window_ms)
        } else {
            stale_window_ms
        };
        if window > 0 && now_ms.saturating_sub(row.last_event_ts) > window {
            return Resolution::Fallback;
        }
        match needs_row_from_state(session.clone(), row) {
            Some(needs) => Resolution::Hook(Box::new(needs)),
            // RUNNING / DONE → healthy, not a need, but still hook-authoritative.
            None => Resolution::Healthy,
        }
    }
}

/// Default staleness window (ms) applied ONLY to the HEALTHY-suppressing kinds
/// (RUNNING/DONE) so a stale "healthy" row from a stopped daemon eventually
/// falls back to a live tmux scan instead of masking a real need forever. A few
/// minutes is long enough to outlast normal materializer latency but short
/// enough that a dead daemon surfaces quickly. Overridable via
/// `AINB_FLEET_STATE_STALE_MS` (a non-negative integer; 0 disables it).
const DEFAULT_HEALTHY_STALE_WINDOW_MS: i64 = 5 * 60_000;

/// The effective healthy-kind staleness window, honouring the
/// `AINB_FLEET_STATE_STALE_MS` override (clamped to ≥ 0; unset/invalid → the
/// default).
fn effective_stale_window_ms() -> i64 {
    std::env::var("AINB_FLEET_STATE_STALE_MS")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .map(|v| v.max(0))
        .unwrap_or(DEFAULT_HEALTHY_STALE_WINDOW_MS)
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
    fn resolve_fallback_for_empty_cwd_session() {
        // Two distinct empty-cwd sessions would collapse onto the "" key, so an
        // empty-cwd match is never trustworthy — always fall back.
        let idx = CurrentStateIndex::from_rows(vec![state(
            "",
            "ASK",
            Some(r#"{"question":"q?","options":[]}"#),
            SOURCE_HOOK,
            100,
        )]);
        assert!(matches!(
            idx.resolve(&mk_session(""), 200, 0),
            Resolution::Fallback
        ));
    }

    #[test]
    fn stale_running_row_falls_back_even_with_caller_window_off() {
        // A dead daemon left a stale RUNNING row. With the caller window OFF (0),
        // the previous behaviour kept the session Healthy forever (masking any
        // real need). Now the healthy-kind window kicks in: a RUNNING row older
        // than the healthy window falls back to a live scan. (Uses the explicit
        // healthy-window form so the test never touches process env.)
        let idx = CurrentStateIndex::from_rows(vec![state("/p", "RUNNING", None, SOURCE_HOOK, 0)]);
        // now = 10min > 5min healthy window → stale healthy row → fallback.
        assert!(matches!(
            idx.resolve_with_healthy_window(&mk_session("/p"), 10 * 60_000, 0, 5 * 60_000),
            Resolution::Fallback
        ));
        // A FRESH RUNNING row (within the window) is still authoritatively Healthy.
        let fresh = CurrentStateIndex::from_rows(vec![state(
            "/p",
            "RUNNING",
            None,
            SOURCE_HOOK,
            10 * 60_000,
        )]);
        assert!(matches!(
            fresh.resolve_with_healthy_window(
                &mk_session("/p"),
                10 * 60_000 + 1_000,
                0,
                5 * 60_000
            ),
            Resolution::Healthy
        ));
    }

    #[test]
    fn stale_done_row_falls_back_so_a_new_need_surfaces() {
        // DONE is also a HEALTHY-suppressing kind: a stale DONE must fall back so
        // a freshly-arrived need (post-completion) is not masked.
        let idx = CurrentStateIndex::from_rows(vec![state(
            "/p",
            "DONE",
            Some(r#"{"reason":"clear"}"#),
            SOURCE_HOOK,
            0,
        )]);
        assert!(matches!(
            idx.resolve_with_healthy_window(&mk_session("/p"), 10 * 60_000, 0, 5 * 60_000),
            Resolution::Fallback
        ));
    }

    #[test]
    fn stale_ask_row_stays_authoritative_with_caller_window_off() {
        // ASK is sticky: a stale (old) ASK is still a real, unanswered need, so
        // the healthy-kind window must NOT apply to it — window stays 0 (off)
        // even though we pass a non-zero healthy window for the healthy kinds.
        let idx = CurrentStateIndex::from_rows(vec![state(
            "/p",
            "ASK",
            Some(r#"{"question":"q?","options":[]}"#),
            SOURCE_HOOK,
            0,
        )]);
        // Way past the healthy window, but ASK is sticky → still Hook.
        assert!(matches!(
            idx.resolve_with_healthy_window(&mk_session("/p"), 60 * 60_000, 0, 5 * 60_000),
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
