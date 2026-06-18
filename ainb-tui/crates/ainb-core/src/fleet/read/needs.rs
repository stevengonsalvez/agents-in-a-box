// ABOUTME: Classifier — turn a Session + its transcript into a NeedsRow
// indicating whether (and how) the session is blocked waiting on input.
//
// Priority: ASK > ERR > IDLE > WAIT. First matching kind wins; we don't
// chase multiple signals per session because the UI shows one card.

use serde::{Deserialize, Serialize};

use crate::fleet::enrich_cache;
use crate::fleet::read::errors::detect_error_signals;
use crate::fleet::read::jsonl_tail::{
    AskUserQuestionData, is_turn_end_stop_reason, last_api_error_from_jsonl,
    last_ask_user_question, last_assistant_info, latest_transcript_for_cwd,
};
use crate::fleet::types::{Session, Signal};

/// JSONL ERR-fallback window — newest N transcript rows scanned when the pane
/// capture finds no error.
const ERR_JSONL_WINDOW: usize = 40;

/// Default idle threshold (override via `AINB_FLEET_IDLE_MIN` or `--idle-min`).
const DEFAULT_IDLE_MIN: i64 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "context", rename_all = "UPPERCASE")]
pub enum NeedsContext {
    Ask(AskUserQuestionData),
    Err(ErrContext),
    Idle(IdleContext),
    Wait(WaitContext),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrContext {
    pub pattern: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdleContext {
    pub idle_minutes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitContext {
    /// "WAITING:" or "needs input:"
    pub marker: String,
    pub text: String,
}

/// One row emitted by `ainb fleet needs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NeedsRow {
    pub session: Session,
    #[serde(flatten)]
    pub context: NeedsContext,
    /// Hint to the calling LLM about the answer-routing channel.
    pub route_hint: RouteHint,
    /// blake3 key of the serialized context. The enrich producer writes its
    /// drafted suggestion under this exact key, so the reader and producer
    /// never disagree and an entry self-invalidates when the session advances.
    #[serde(default)]
    pub enrich_key: String,
    /// Fresh cached suggestion, attached by the reader when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enriched: Option<String>,
    /// True when this card has no fresh cache entry and enrichment is enabled —
    /// i.e. it should be drafted by the producer (inline or batched agent).
    #[serde(default)]
    pub need_enrich: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RouteHint {
    Broker,
    Tmux,
    None,
}

/// Per-classifier dependency. We read everything we need up-front so the
/// classifier itself is a pure function (no I/O), easy to unit-test.
pub struct ClassifyInput {
    pub session: Session,
    pub pane_text: Option<String>,
    pub idle_threshold_min: i64,
    pub now_ms: i64,
}

impl ClassifyInput {
    #[must_use]
    pub fn from_env(session: Session, pane_text: Option<String>, now_ms: i64) -> Self {
        let idle_threshold_min = std::env::var("AINB_FLEET_IDLE_MIN")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_IDLE_MIN);
        Self {
            session,
            pane_text,
            idle_threshold_min,
            now_ms,
        }
    }
}

/// Classify a single session. Reads its transcript (if any) and merges with
/// the supplied pane text + session summary. Returns None when nothing
/// indicates the session needs attention.
pub fn classify(input: ClassifyInput) -> Option<NeedsRow> {
    let transcript_path = latest_transcript_for_cwd(&input.session.cwd);
    let route_hint = derive_route_hint(&input.session);

    // 1. ASK — strongest signal, JSONL tool_use block.
    if let Some(path) = &transcript_path {
        if let Some(aq) = last_ask_user_question(path) {
            return Some(make_row(input.session, NeedsContext::Ask(aq), route_hint));
        }
    }

    // 2. ERR — API-error regex over the pane, with a JSONL fallback when the
    //    pane capture misses (error scrolled past the 80-line window, or the
    //    capture itself failed/returned empty).
    let err = input
        .pane_text
        .as_deref()
        .and_then(|pane| first_api_error(pane, input.now_ms))
        .or_else(|| {
            transcript_path
                .as_ref()
                .and_then(|p| last_api_error_from_jsonl(p, ERR_JSONL_WINDOW, input.now_ms))
        });
    if let Some((pattern, snippet)) = err {
        return Some(make_row(
            input.session,
            NeedsContext::Err(ErrContext { pattern, snippet }),
            route_hint,
        ));
    }

    // 3. WAIT — explicit opt-in marker.
    //    Broker summary starts with "WAITING:" (carried in Session.summary
    //    when explicitly set). Extract the text into an owned string first
    //    so the session-borrow ends before we move session into NeedsRow.
    let wait_text: Option<String> = input
        .session
        .summary
        .as_deref()
        .and_then(|s| s.strip_prefix("WAITING:"))
        .map(|rest| rest.trim().to_string());
    if let Some(text) = wait_text {
        return Some(make_row(
            input.session,
            NeedsContext::Wait(WaitContext {
                marker: "WAITING:".to_string(),
                text,
            }),
            route_hint,
        ));
    }

    // 4. IDLE — assistant turn ended, no user follow-up, last seen N min ago.
    //    TURN-END: Claude (>= 2.1.x) stamps a finished assistant turn's text
    //    row with `stop_reason: null`, not `"end_turn"` — so we accept null too
    //    (see `is_turn_end_stop_reason`). That's safe here because the row must
    //    ALSO carry visible text (`text_snippet`), have no user follow-up, and
    //    be at least `idle_threshold_min` (default 5) old; a 5-min-stale text
    //    row is realistically a finished turn, not a mid-stream write.
    if let Some(path) = &transcript_path {
        if let Some(info) = last_assistant_info(path) {
            if !info.has_user_follow_up
                && is_turn_end_stop_reason(info.stop_reason.as_deref())
                && info.text_snippet.is_some()
                && info.ts_ms > 0
            {
                let age_ms = input.now_ms.saturating_sub(info.ts_ms);
                let age_min = age_ms / 60_000;
                if age_min >= input.idle_threshold_min {
                    return Some(make_row(
                        input.session,
                        NeedsContext::Idle(IdleContext {
                            idle_minutes: age_min,
                            last_assistant_text: info.text_snippet,
                        }),
                        route_hint,
                    ));
                }
            }
        }
    }

    None
}

/// Build a `NeedsRow`, stamping the content `enrich_key` from the serialized
/// context. `enriched` / `need_enrich` are filled later by the orchestrator
/// (it owns the cache lookup and the enable flag).
fn make_row(session: Session, context: NeedsContext, route_hint: RouteHint) -> NeedsRow {
    let enrich_key = enrich_cache::ctx_key(&serde_json::to_string(&context).unwrap_or_default());
    NeedsRow {
        session,
        context,
        route_hint,
        enrich_key,
        enriched: None,
        need_enrich: false,
    }
}

/// First API-error signal in `text`, as `(pattern, raw_snippet)`.
fn first_api_error(text: &str, now_ms: i64) -> Option<(String, String)> {
    detect_error_signals(text, now_ms).into_iter().find_map(|s| match s {
        Signal::ApiError { pattern, raw, .. } => Some((pattern, raw)),
        _ => None,
    })
}

/// Advisory hint for the answer-routing channel. Mirrors the default
/// `tmux-first` send transport: prefer a live tmux pane, fall back to a broker
/// peer only when there is no tmux session. (Actual delivery is governed by
/// `AINB_FLEET_TRANSPORT` in the send path.)
fn derive_route_hint(session: &Session) -> RouteHint {
    if session.tmux_session.is_some() {
        RouteHint::Tmux
    } else if session.peer_id.is_some() {
        RouteHint::Broker
    } else {
        RouteHint::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::SessionSource;

    fn mk_session(cwd: &str) -> Session {
        Session {
            id: "test".to_string(),
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

    #[test]
    fn route_hint_prefers_tmux() {
        // A live tmux pane wins even when a broker peer is also registered.
        let mut s = mk_session("/x");
        s.peer_id = Some("p".to_string());
        assert!(matches!(derive_route_hint(&s), RouteHint::Tmux));
    }

    #[test]
    fn route_hint_broker_when_no_tmux() {
        let mut s = mk_session("/x");
        s.tmux_session = None;
        s.peer_id = Some("p".to_string());
        assert!(matches!(derive_route_hint(&s), RouteHint::Broker));
    }

    #[test]
    fn route_hint_none_when_no_targets() {
        let mut s = mk_session("/x");
        s.tmux_session = None;
        assert!(matches!(derive_route_hint(&s), RouteHint::None));
    }

    #[test]
    fn waiting_prefix_in_summary_yields_wait() {
        let mut s = mk_session("/nonexistent-path-xyz");
        s.summary = Some("WAITING: pick option 2".to_string());
        // Transcript path won't exist for this cwd, so ASK/IDLE branches skip;
        // pane text empty so ERR skips. WAIT branch triggers.
        let row = classify(ClassifyInput {
            session: s,
            pane_text: None,
            idle_threshold_min: 5,
            now_ms: 0,
        })
        .expect("should classify");
        match row.context {
            NeedsContext::Wait(w) => {
                assert_eq!(w.marker, "WAITING:");
                assert_eq!(w.text, "pick option 2");
            }
            _ => panic!("expected Wait variant"),
        }
    }

    #[test]
    fn no_signal_returns_none() {
        let s = mk_session("/nonexistent-path-xyz");
        let r = classify(ClassifyInput {
            session: s,
            pane_text: None,
            idle_threshold_min: 5,
            now_ms: 0,
        });
        assert!(r.is_none());
    }

    #[test]
    fn err_pattern_in_pane_yields_err() {
        let s = mk_session("/nonexistent-path-xyz");
        let pane = String::from("…\nAPI Error: rate_limited please retry\n…");
        let row = classify(ClassifyInput {
            session: s,
            pane_text: Some(pane),
            idle_threshold_min: 5,
            now_ms: 1_700_000_000_000,
        })
        .expect("should classify");
        match row.context {
            NeedsContext::Err(e) => {
                assert!(e.pattern == "rate_limited" || e.pattern == "fetch_failed");
                assert!(e.snippet.contains("rate_limited") || e.snippet.contains("API Error"));
            }
            _ => panic!("expected Err variant"),
        }
    }

    // --- IDLE turn-end detection (the 2.1.x null stop_reason fix) ---------

    /// Plant a transcript under `~/.claude/projects/<slug>/` for a UNIQUE cwd so
    /// `classify` resolves it via `latest_transcript_for_cwd`. Returns the
    /// fabricated cwd plus a guard that removes the project dir on drop.
    struct TranscriptFixture {
        cwd: String,
        dir: std::path::PathBuf,
    }
    impl Drop for TranscriptFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn plant_transcript(tag: &str, rows: &[String]) -> TranscriptFixture {
        use std::io::Write;
        // Unique, never-real cwd so we don't collide with a live session.
        let cwd = format!(
            "/ainb-test-idle/{tag}/{}/{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let mut dir = dirs::home_dir().expect("home dir");
        dir.push(".claude");
        dir.push("projects");
        // The first path segment of the slug is the unique project dir root we
        // own and can safely remove.
        let slug = crate::fleet::read::cwd_to_project_slug(&cwd);
        dir.push(&slug);
        std::fs::create_dir_all(&dir).expect("create project dir");
        let mut f = std::fs::File::create(dir.join("session.jsonl")).expect("create transcript");
        for r in rows {
            writeln!(f, "{r}").unwrap();
        }
        TranscriptFixture { cwd, dir }
    }

    fn iso(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms).unwrap().to_rfc3339()
    }

    #[test]
    fn idle_detected_for_null_stop_reason_with_text_when_old_and_no_followup() {
        let now_ms = 1_700_000_000_000;
        let old_ms = now_ms - 10 * 60_000; // 10 min old (> 5 min threshold)
        let fx = plant_transcript(
            "null-idle",
            &[
                format!(
                    r#"{{"type":"user","message":{{"content":"do the thing"}},"timestamp":"{}"}}"#,
                    iso(old_ms - 1000)
                ),
                format!(
                    r#"{{"type":"assistant","message":{{"stop_reason":null,"content":[{{"type":"text","text":"All done."}}]}},"timestamp":"{}"}}"#,
                    iso(old_ms)
                ),
            ],
        );
        let row = classify(ClassifyInput {
            session: mk_session(&fx.cwd),
            pane_text: None,
            idle_threshold_min: 5,
            now_ms,
        })
        .expect("should classify IDLE on a finished null-stop_reason turn");
        match row.context {
            NeedsContext::Idle(idle) => {
                assert!(idle.idle_minutes >= 5, "got {} min", idle.idle_minutes);
                assert_eq!(idle.last_assistant_text.as_deref(), Some("All done."));
            }
            other => panic!("expected Idle, got {other:?}"),
        }
    }

    #[test]
    fn idle_detected_for_explicit_end_turn() {
        let now_ms = 1_700_000_000_000;
        let old_ms = now_ms - 10 * 60_000;
        let fx = plant_transcript(
            "endturn-idle",
            &[format!(
                r#"{{"type":"assistant","message":{{"stop_reason":"end_turn","content":[{{"type":"text","text":"finished"}}]}},"timestamp":"{}"}}"#,
                iso(old_ms)
            )],
        );
        let row = classify(ClassifyInput {
            session: mk_session(&fx.cwd),
            pane_text: None,
            idle_threshold_min: 5,
            now_ms,
        })
        .expect("should classify IDLE on an explicit end_turn");
        assert!(matches!(row.context, NeedsContext::Idle(_)));
    }

    #[test]
    fn no_idle_when_user_follows_up() {
        let now_ms = 1_700_000_000_000;
        let old_ms = now_ms - 10 * 60_000;
        let fx = plant_transcript(
            "followup",
            &[
                format!(
                    r#"{{"type":"assistant","message":{{"stop_reason":null,"content":[{{"type":"text","text":"done"}}]}},"timestamp":"{}"}}"#,
                    iso(old_ms)
                ),
                format!(
                    r#"{{"type":"user","message":{{"content":"next"}},"timestamp":"{}"}}"#,
                    iso(old_ms + 1000)
                ),
            ],
        );
        let r = classify(ClassifyInput {
            session: mk_session(&fx.cwd),
            pane_text: None,
            idle_threshold_min: 5,
            now_ms,
        });
        assert!(r.is_none(), "user follow-up must suppress IDLE");
    }

    #[test]
    fn no_idle_when_turn_too_recent() {
        let now_ms = 1_700_000_000_000;
        let recent_ms = now_ms - 60_000; // 1 min old (< 5 min threshold)
        let fx = plant_transcript(
            "recent",
            &[format!(
                r#"{{"type":"assistant","message":{{"stop_reason":null,"content":[{{"type":"text","text":"done"}}]}},"timestamp":"{}"}}"#,
                iso(recent_ms)
            )],
        );
        let r = classify(ClassifyInput {
            session: mk_session(&fx.cwd),
            pane_text: None,
            idle_threshold_min: 5,
            now_ms,
        });
        assert!(r.is_none(), "a 1-min-old turn is below the idle threshold");
    }

    #[test]
    fn no_idle_for_tool_only_null_row() {
        // A `null`-stop_reason row carrying only a tool_use block is mid-flight,
        // never IDLE — the text gate (text_snippet.is_some()) rejects it.
        let now_ms = 1_700_000_000_000;
        let old_ms = now_ms - 10 * 60_000;
        let fx = plant_transcript(
            "toolonly",
            &[format!(
                r#"{{"type":"assistant","message":{{"stop_reason":null,"content":[{{"type":"tool_use","name":"Bash","input":{{"command":"ls"}}}}]}},"timestamp":"{}"}}"#,
                iso(old_ms)
            )],
        );
        let r = classify(ClassifyInput {
            session: mk_session(&fx.cwd),
            pane_text: None,
            idle_threshold_min: 5,
            now_ms,
        });
        assert!(r.is_none(), "tool-only null row must not be IDLE");
    }

    #[test]
    fn no_idle_for_non_terminal_stop_reason() {
        let now_ms = 1_700_000_000_000;
        let old_ms = now_ms - 10 * 60_000;
        let fx = plant_transcript(
            "maxtokens",
            &[format!(
                r#"{{"type":"assistant","message":{{"stop_reason":"max_tokens","content":[{{"type":"text","text":"truncated"}}]}},"timestamp":"{}"}}"#,
                iso(old_ms)
            )],
        );
        let r = classify(ClassifyInput {
            session: mk_session(&fx.cwd),
            pane_text: None,
            idle_threshold_min: 5,
            now_ms,
        });
        assert!(r.is_none(), "max_tokens is not a turn-end");
    }
}
