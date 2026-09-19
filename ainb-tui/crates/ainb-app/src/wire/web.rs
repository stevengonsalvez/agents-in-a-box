// ABOUTME: The web dashboard's session rows, projected from the redacted
// Sessions section view. The browser never sees a session field that did not
// first pass through that view (issue #1056), and it sees every session,
// whatever filter the TUI persisted (#1180).
//
//   AppState.sessions ──every_session_json──▶ redacted body ──session_rows──▶ /api/snapshot.sessions[]
//
// The projection only picks and renames values already in the frame body, so
// what the frame withholds (`display_name`) cannot reappear on the web. The
// other row values pass through as the session stores them: the frame scrubs
// `recent_logs`, `boss_prompt` and `preview_content`, none of which is a row
// key, and `tmux_session_name` and `workspace_path` are not scrubbed.
// `wire::shape` traces these rows into the committed key-path fixture, so a new
// row field fails the same gate a new frame field does.

use crate::app::AppState;
use crate::wire::frame::Frame;
use serde::Serialize;
use serde_json::Value;

/// One row of the web dashboard's session list, in the shape `frontend/app.js`
/// draws: the keys `ainb list --format json` uses, minus the label, and the
/// worktree named by its directory rather than its absolute path (#1097).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WebSessionRow {
    pub session_id: String,
    pub tmux_session_name: Option<String>,
    pub workspace_name: String,
    /// The worktree's directory name. The browser never gets the absolute
    /// path: it only draws the name, and attaching resolves the session by id.
    pub worktree_name: String,
    pub created_at: String,
    /// The tmux session exists.
    pub is_running: bool,
    /// The agent is running inside it.
    pub claude_active: bool,
}

/// The web rows for every session of `state`, read through its Sessions view.
///
/// Every session, whatever session filter the TUI persisted: the filter is the
/// TUI renderer's, and a stopped session the TUI hides is still one the web
/// lists and attaches (#1180). `ainb list --frame`, the web dashboard and its
/// attach lookup all route through here.
#[must_use]
pub fn session_rows(state: &AppState) -> Vec<WebSessionRow> {
    rows_from_body(&crate::wire::every_session_json(state))
}

/// The web rows a Sessions frame describes. Any other section's frame has no
/// workspaces and gives no rows. A frame carries only the rows its host's
/// filter shows; [`session_rows`] reads every session.
#[must_use]
pub fn rows_from_frame(frame: &Frame) -> Vec<WebSessionRow> {
    rows_from_body(frame.body())
}

/// The web rows a Sessions view body describes.
fn rows_from_body(body: &Value) -> Vec<WebSessionRow> {
    let text = |value: &Value| value.as_str().map(str::to_string);
    body["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workspace| {
            let workspace_name = text(&workspace["name"]).unwrap_or_default();
            workspace["sessions"].as_array().into_iter().flatten().map(move |session| {
                // `Running` and `Idle` have a live tmux session; `Stopped` and
                // `Error(..)` do not.
                let status = session["status"].as_str();
                WebSessionRow {
                    session_id: text(&session["id"]).unwrap_or_default(),
                    tmux_session_name: text(&session["tmux_session_name"]),
                    workspace_name: workspace_name.clone(),
                    worktree_name: session["workspace_path"]
                        .as_str()
                        .and_then(|path| std::path::Path::new(path).file_name())
                        .map(|name| card_text(&name.to_string_lossy()))
                        .unwrap_or_default(),
                    created_at: text(&session["created_at"]).unwrap_or_default(),
                    is_running: matches!(status, Some("Running" | "Idle")),
                    claude_active: status == Some("Running"),
                }
            })
        })
        .collect()
}

/// One card of the web dashboard's `needs[]`, allow-listed from the card the
/// daemon inbox and status read produce (#1081).
///
/// Section 20's frame withholds a session's `cwd` and its raw request; this
/// card does the same for the browser. `cwd` is replaced by its last path
/// component, `workspaceName`. For an ainb worktree that is the worktree
/// directory (`repo--ainb-session-…`), not the session list's workspace name,
/// so a surface that has the session row prefers the row's name. The
/// payload keeps only the fields the dashboard draws, every string scrubbed.
/// Keys are camelCase, as `frontend/app.js` reads them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebNeedCard {
    /// The attention id to answer, or `None` for a card with no inbox row.
    pub attention_id: Option<String>,
    pub kind: String,
    pub wire_kind: String,
    pub session_id: String,
    pub workspace_name: String,
    pub workspace_id: Option<String>,
    pub degraded: bool,
    pub created_at: i64,
    /// Push channels resolved at raise time, as channel tokens (`web`, `os`).
    pub channels: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_observed_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_unbound: Option<bool>,
    pub payload: WebNeedPayload,
}

/// The rendered detail of a need: what the card says and the options an ASK
/// offers. Every string passes `redact::scrub`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebNeedPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The most characters any projected string keeps, after scrubbing. A card is
/// a summary; a longer value is cut with `…` rather than shipped whole.
pub const MAX_CARD_TEXT_CHARS: usize = 512;

/// What an unparsed payload shows. The raw request is not a summary the card
/// can bound safely, so none of it reaches the browser.
pub const UNPARSED_PAYLOAD_TEXT: &str = "(request details are in the session)";

/// Scrub `text`, then keep at most [`MAX_CARD_TEXT_CHARS`] characters.
fn card_text(text: &str) -> String {
    let scrubbed = crate::fleet::bridge::redact::scrub(text);
    if scrubbed.chars().count() <= MAX_CARD_TEXT_CHARS {
        return scrubbed;
    }
    let mut cut: String = scrubbed.chars().take(MAX_CARD_TEXT_CHARS - 1).collect();
    cut.push('…');
    cut
}

/// The web cards for a `needs` array of daemon cards. Anything that is not an
/// object is dropped; a key the allow-list does not name never reaches a card.
#[must_use]
pub fn need_cards(needs: &Value) -> Vec<WebNeedCard> {
    let text = |value: &Value| value.as_str().map(card_text);
    needs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|card| card.is_object())
        .map(|card| {
            let workspace_name = card["cwd"]
                .as_str()
                .and_then(|cwd| std::path::Path::new(cwd).file_name())
                .map(|name| card_text(&name.to_string_lossy()))
                .unwrap_or_default();
            WebNeedCard {
                attention_id: text(&card["attentionId"]),
                kind: text(&card["kind"]).unwrap_or_default(),
                wire_kind: text(&card["wireKind"]).unwrap_or_default(),
                session_id: text(&card["sessionId"]).unwrap_or_default(),
                workspace_name,
                workspace_id: text(&card["workspaceId"]),
                degraded: card["degraded"].as_bool().unwrap_or(false),
                created_at: card["createdAt"].as_i64().unwrap_or_default(),
                channels: card["channels"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(text)
                    .collect(),
                session_key: text(&card["sessionKey"]),
                state: text(&card["state"]),
                provenance: text(&card["provenance"]),
                tier: card["tier"].as_u64().and_then(|tier| u8::try_from(tier).ok()),
                evidence_observed_at: card["evidenceObservedAt"].as_i64(),
                host_id: text(&card["hostId"]),
                pane_unbound: card["paneUnbound"].as_bool(),
                payload: need_payload(&card["payload"]),
            }
        })
        .collect()
}

fn need_payload(payload: &Value) -> WebNeedPayload {
    let text = |key: &str| payload[key].as_str().map(card_text);
    match payload {
        // A payload that did not parse is the raw request: a placeholder, never
        // the text itself.
        Value::String(_) => WebNeedPayload {
            text: Some(UNPARSED_PAYLOAD_TEXT.to_string()),
            ..WebNeedPayload::default()
        },
        Value::Object(_) => WebNeedPayload {
            question: text("question"),
            options: payload["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|option| option.as_str().map(card_text))
                .collect(),
            text: text("text"),
            marker: text("marker"),
            snippet: text("snippet"),
            pattern: text("pattern"),
            message: text("message"),
        },
        _ => WebNeedPayload::default(),
    }
}

/// Token and cost counts of one cost row, as the dashboard sums them.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WebCostBucket {
    pub input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub call_count: u64,
    pub cost_usd: Option<f64>,
}

/// Fleet-wide cost totals.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WebCostTotals {
    pub cost_usd: f64,
    pub session_count: u64,
    pub model_count: u64,
    pub bucket: WebCostBucket,
}

/// Spend on one model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebModelCost {
    pub model: String,
    pub cost_usd: f64,
    pub bucket: WebCostBucket,
}

/// Spend in one workspace group.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebGroupCost {
    pub group: String,
    pub cost_usd: f64,
    pub session_count: u64,
    pub bucket: WebCostBucket,
}

/// The web dashboard's cost panel, allow-listed from `ainb fleet cost`'s
/// report (#1113).
///
/// The dashboard draws `totals`, `models` and `groups` only. The report's
/// `sessions[]` (with each session's absolute `cwd` and its path-derived
/// `project`), `daily[]` and `budget_breaches[]` never reach the browser, and
/// the two labels kept (`model`, `group`) are scrubbed and clamped.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WebCost {
    pub totals: WebCostTotals,
    pub models: Vec<WebModelCost>,
    pub groups: Vec<WebGroupCost>,
}

/// The web cost panel for a `fleet cost` report, or `None` when there is no
/// report object (the verb is absent from this build, or it failed).
#[must_use]
pub fn cost_panel(report: &Value) -> Option<WebCost> {
    let report = report.as_object()?;
    let number = |value: &Value| value.as_f64().unwrap_or_default();
    let count = |value: &Value| value.as_u64().unwrap_or_default();
    let bucket = |value: &Value| WebCostBucket {
        input_tokens: count(&value["input_tokens"]),
        cache_creation_tokens: count(&value["cache_creation_tokens"]),
        cache_read_tokens: count(&value["cache_read_tokens"]),
        output_tokens: count(&value["output_tokens"]),
        reasoning_tokens: count(&value["reasoning_tokens"]),
        call_count: count(&value["call_count"]),
        cost_usd: value["cost_usd"].as_f64(),
    };
    let rows = |key: &str| {
        report.get(key).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default()
    };
    let totals = report.get("totals").unwrap_or(&Value::Null);
    Some(WebCost {
        totals: WebCostTotals {
            cost_usd: number(&totals["cost_usd"]),
            session_count: count(&totals["session_count"]),
            model_count: count(&totals["model_count"]),
            bucket: bucket(&totals["bucket"]),
        },
        models: rows("models")
            .iter()
            .map(|row| WebModelCost {
                model: row["model"].as_str().map(card_text).unwrap_or_default(),
                cost_usd: number(&row["cost_usd"]),
                bucket: bucket(&row["bucket"]),
            })
            .collect(),
        groups: rows("groups")
            .iter()
            .map(|row| WebGroupCost {
                group: row["group"].as_str().map(card_text).unwrap_or_default(),
                cost_usd: number(&row["cost_usd"]),
                session_count: count(&row["session_count"]),
                bucket: bucket(&row["bucket"]),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SessionStatus;
    use crate::wire::shape::{PlainSeed, sample_state};

    const CANARY: &str = "ghp_ProofCanary0123456789abcdefghijklmnopq";

    #[test]
    fn the_cost_panel_keeps_totals_models_and_groups_and_drops_session_paths() {
        let bucket = serde_json::json!({"input_tokens": 10, "output_tokens": 5, "call_count": 1, "cost_usd": 0.5});
        let report = serde_json::json!({
            "totals": {"cost_usd": 0.5, "session_count": 1, "model_count": 1, "bucket": bucket},
            "sessions": [{
                "session_id": "s1",
                "provider": "claude",
                "project": "-home-op-private-client-repo",
                "cwd": "/home/op/private-client/repo",
                "group": "repo",
                "cost_usd": 0.5,
                "bucket": bucket,
            }],
            "models": [{"model": "claude-sonnet", "cost_usd": 0.5, "bucket": bucket}],
            "daily": [{"date": "2026-09-15", "cost_usd": 0.5, "bucket": bucket}],
            "groups": [{"group": format!("repo {CANARY}"), "cost_usd": 0.5, "session_count": 1, "bucket": bucket}],
            "budget_breaches": [],
        });

        let panel = cost_panel(&report).expect("a report object");
        let json = serde_json::to_string(&panel).expect("serialises");

        for gone in [
            "/home/op",
            "private-client",
            "sessions",
            "daily",
            "budget_breaches",
            CANARY,
        ] {
            assert!(
                !json.contains(gone),
                "{gone} reached the cost panel: {json}"
            );
        }
        assert_eq!(panel.totals.session_count, 1);
        assert_eq!(panel.totals.bucket.input_tokens, 10);
        assert_eq!(panel.models[0].model, "claude-sonnet");
        assert!(panel.groups[0].group.starts_with("repo "));
        assert_eq!(cost_panel(&serde_json::Value::Null), None);
    }

    #[test]
    fn a_need_card_keeps_the_rendered_fields_and_drops_cwd_and_the_raw_request() {
        let needs = serde_json::json!([{
            "attentionId": "01J0ATTENTION",
            "kind": "ASK",
            "wireKind": "ask_user_question",
            "sessionId": "s1",
            "cwd": "/home/op/private-client/repo",
            "workspaceId": null,
            "degraded": false,
            "createdAt": 1_700_000_000_000_i64,
            "channels": ["web"],
            "sessionKey": "claude:s1",
            "state": "waiting",
            "tier": 0,
            "payload": {
                "question": format!("deploy with {CANARY}?"),
                "options": ["yes", format!("use {CANARY}")],
                "tool_input": {"secret": CANARY, "questions": []},
                "transcript_path": "/home/op/.claude/projects/x.jsonl",
            },
        }]);

        let cards = need_cards(&needs);
        let json = serde_json::to_string(&cards).expect("cards serialise");

        assert_eq!(cards.len(), 1);
        assert!(!json.contains(CANARY), "{json}");
        assert!(!json.contains("/home/op"), "{json}");
        for gone in ["cwd", "tool_input", "transcript_path"] {
            assert!(!json.contains(gone), "{gone} reached the card: {json}");
        }
        let card = &cards[0];
        assert_eq!(card.workspace_name, "repo");
        assert_eq!(card.attention_id.as_deref(), Some("01J0ATTENTION"));
        assert_eq!(card.channels, ["web"]);
        assert_eq!(card.payload.options.len(), 2);
        assert!(card.payload.question.as_deref().is_some_and(|q| q.starts_with("deploy with")));
    }

    #[test]
    fn an_unparsed_payload_is_a_placeholder_not_the_raw_request() {
        let needs = serde_json::json!([{"kind": "ERR", "payload": "raw request text"}]);
        let cards = need_cards(&needs);
        assert_eq!(
            cards[0].payload.text.as_deref(),
            Some(UNPARSED_PAYLOAD_TEXT)
        );
    }

    #[test]
    fn every_projected_string_is_clamped_after_scrubbing() {
        let long = "q".repeat(MAX_CARD_TEXT_CHARS * 4);
        let needs = serde_json::json!([{
            "kind": long,
            "sessionId": long,
            "cwd": format!("/w/{long}"),
            "payload": {"question": long, "options": [long], "message": long},
        }]);
        let card = &need_cards(&needs)[0];
        let lengths = [
            card.kind.chars().count(),
            card.session_id.chars().count(),
            card.workspace_name.chars().count(),
            card.payload.question.as_deref().unwrap_or_default().chars().count(),
            card.payload.options[0].chars().count(),
            card.payload.message.as_deref().unwrap_or_default().chars().count(),
        ];
        assert!(
            lengths.iter().all(|len| *len == MAX_CARD_TEXT_CHARS),
            "{lengths:?}"
        );
        assert!(card.payload.question.as_deref().is_some_and(|q| q.ends_with('…')));
    }

    #[test]
    fn a_credential_shaped_label_never_reaches_the_web_rows() {
        let mut state = sample_state(&mut PlainSeed);
        let session = &mut state.sessions.get_mut().workspaces[0].sessions[0];
        session.display_name = Some(format!("deploy {CANARY}"));
        session.tmux_session_name = Some("tmux_repo-1".to_string());

        let rows = session_rows(&state);
        let json = serde_json::to_string(&rows).expect("rows serialise");

        assert_eq!(rows.len(), 1);
        assert!(!json.contains(CANARY), "{json}");
        assert!(!json.contains("display_name"), "{json}");
        assert_eq!(rows[0].workspace_name, "sample-repo");
        assert_eq!(rows[0].tmux_session_name.as_deref(), Some("tmux_repo-1"));
        assert_eq!(rows[0].worktree_name, "sample-repo");
        assert!(!json.contains("/work/"), "no absolute path: {json}");
    }

    #[test]
    fn a_credential_shaped_worktree_directory_is_scrubbed_in_the_row() {
        let mut state = sample_state(&mut PlainSeed);
        state.sessions.get_mut().workspaces[0].sessions[0].workspace_path =
            format!("/work/{CANARY}");
        let rows = session_rows(&state);
        assert!(
            !rows[0].worktree_name.contains(CANARY),
            "{:?}",
            rows[0].worktree_name
        );
    }

    /// The TUI's Shift+F filter is a fact about the TUI's renderer, not about
    /// the sessions: the web rows, `ainb list --frame` and the attach lookup
    /// read every session whatever filter the TUI persisted (#1180).
    #[test]
    fn the_web_rows_carry_every_session_whatever_the_tui_filter() {
        use crate::app::state::SessionFilter;
        use crate::models::SessionMode;

        let mut state = sample_state(&mut PlainSeed);
        let session = &mut state.sessions.get_mut().workspaces[0].sessions[0];
        session.mode = SessionMode::Interactive;
        session.status = SessionStatus::Stopped;
        let stopped = session.id.to_string();

        for filter in [
            SessionFilter::All,
            SessionFilter::ActiveOnly,
            SessionFilter::StoppedOnly,
        ] {
            state.sessions.get_mut().session_filter = filter;
            let rows = session_rows(&state);
            assert!(
                rows.iter().any(|row| row.session_id == stopped),
                "{filter:?} dropped the stopped session: {rows:?}"
            );
        }
        state.sessions.get_mut().workspaces[0].sessions[0].status = SessionStatus::Running;
        state.sessions.get_mut().session_filter = SessionFilter::StoppedOnly;
        assert_eq!(
            session_rows(&state).len(),
            1,
            "a running row under StoppedOnly"
        );
    }

    #[test]
    fn the_session_status_decides_running_and_agent_active() {
        let mut state = sample_state(&mut PlainSeed);
        let expect = [
            (SessionStatus::Running, true, true),
            (SessionStatus::Idle, true, false),
            (SessionStatus::Stopped, false, false),
            (SessionStatus::Error("pane gone".to_string()), false, false),
        ];
        for (status, running, active) in expect {
            state.sessions.get_mut().workspaces[0].sessions[0].status = status.clone();
            let row = &session_rows(&state)[0];
            assert_eq!(
                (row.is_running, row.claude_active),
                (running, active),
                "{status:?}"
            );
        }
    }
}
