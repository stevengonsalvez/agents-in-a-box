//! Daemon control-plane integration for the web surface (spec P8 / D18).
//!
//! Re-exports the shared client and wire error from `ainb-hangar-client`, and
//! owns the web-specific projection from daemon inbox rows onto the JSON `needs`
//! cards the dashboard renders, plus the [`Answerer`] route dependency seam.

use std::future::Future;
use std::pin::Pin;

pub use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_proto::connections::{SurfaceInfo, SurfaceKind};
use ainb_hangar_proto::events::AttentionRow;
use ainb_hangar_proto::snapshots::{AnswerParams, AnswerResult};
use serde_json::{Value, json};

// ── needs mapping ────────────────────────────────────────────────────────────

/// Map an [`AttentionRow`] wire `kind` token to the short display kind the
/// dashboard's needs cards already colour on (ASK / ERR / WAIT). Answerable
/// request families (question / approval / codex-request / escalation) render as
/// `ASK` so they get the answer buttons; errors and waits render as their own
/// non-answerable badges.
#[must_use]
pub fn display_kind(wire_kind: &str) -> &'static str {
    match wire_kind {
        "error" => "ERR",
        "waiting" => "WAIT",
        // ask_user_question | approval | codex_request_user | escalation | anything else
        _ => "ASK",
    }
}

/// Convert the daemon's open attention rows into the JSON `needs` array the
/// dashboard renders. Each card carries `attentionId` (the answer RPC target),
/// its display `kind`, the raising session + cwd, and the normalised request
/// `payload` so the frontend can render the question/options and wire answer
/// buttons. Ordering is preserved (the daemon returns oldest-first, which is the
/// urgency order the card shuffle relies on).
///
/// The `payload` field is best-effort JSON: the daemon stores it as a serialised
/// string, so a parseable payload is normalised (see [`normalize_payload`]) into
/// the flat shape the dashboard renders and an unparseable one falls back to the
/// raw string, so the card always has something to show.
#[must_use]
pub fn attention_to_needs(rows: &[AttentionRow]) -> Value {
    attention_to_needs_with_status(rows, &[])
}

/// [`attention_to_needs`], with every card stamped from the daemon's one status
/// read (D14), and a card added for any agent the status read says is blocked
/// that the inbox has no row for.
///
/// The stamp is what makes the dashboard's answer to "what state is this agent
/// in" the SAME answer the CLI and the TUI panel give: all three take it from
/// `ainb_hangar_proto::agent_status`, none of them re-derives it.
///
/// The added card matters for issue #916: an agent whose pane could not be
/// bound may be blocked with nothing able to type into it, and a dashboard that
/// only lists inbox rows would show one agent fewer than the panel does.
pub fn attention_to_needs_with_status(
    rows: &[AttentionRow],
    status: &[ainb_hangar_proto::agent_status::AgentStatusRow],
) -> Value {
    use ainb_hangar_proto::agent_status::AgentState;

    let by_session: std::collections::HashMap<
        &str,
        &ainb_hangar_proto::agent_status::AgentStatusRow,
    > = status
        .iter()
        .filter_map(|row| {
            // The status row is keyed by Fleet identity; the inbox is keyed
            // by the provider's own session id, which is the key's tail.
            row.session_key.split_once(':').map(|(_, id)| (id, row))
        })
        .collect();
    let mut cards: Vec<Value> = rows
        .iter()
        .map(|row| {
            let payload = normalize_payload(&row.payload);
            let mut card = json!({
                "attentionId": row.id,
                "kind": display_kind(&row.kind),
                "wireKind": row.kind,
                "sessionId": row.session_id,
                "cwd": row.cwd,
                "workspaceId": row.workspace_id,
                "degraded": row.degraded,
                "createdAt": row.created_at,
                "payload": payload,
                // The push channels resolved at raise time (tcp T5). The web-push
                // delivery loop filters on this: it buzzes a device only when the
                // rules routed this attention to the `web` channel.
                "channels": row.channels,
            });
            if let Some(status) = by_session.get(row.session_id.as_str()) {
                stamp_status(&mut card, status);
            }
            card
        })
        .collect();
    let carried: std::collections::HashSet<&str> =
        rows.iter().map(|row| row.session_id.as_str()).collect();
    for row in status.iter().filter(|row| row.state == AgentState::Waiting) {
        let provider_session_id = row.session_key.split_once(':').map_or("", |(_, id)| id);
        if carried.contains(provider_session_id) {
            continue;
        }
        let mut card = json!({
            "attentionId": Value::Null,
            "kind": "Waiting",
            "wireKind": "waiting",
            "sessionId": provider_session_id,
            "cwd": row.cwd,
            "workspaceId": Value::Null,
            "degraded": false,
            "createdAt": row.evidence_observed_at,
            "payload": json!({
                "marker": "needs input:",
                "text": if row.pane_unbound {
                    "blocked, and no tmux pane is bound to this session (see `ainb doctor`)"
                } else {
                    "blocked on a human; the question is in the attention inbox"
                },
            }),
            "channels": 0,
        });
        stamp_status(&mut card, row);
        cards.push(card);
    }
    Value::Array(cards)
}

/// Stamp one card with the D14 identity tuple.
fn stamp_status(card: &mut Value, status: &ainb_hangar_proto::agent_status::AgentStatusRow) {
    let (session_key, state, provenance, tier, evidence_observed_at) = status.identity_tuple();
    let Some(object) = card.as_object_mut() else {
        return;
    };
    object.insert("sessionKey".into(), json!(session_key));
    object.insert("state".into(), json!(state));
    object.insert("provenance".into(), json!(provenance));
    object.insert("tier".into(), json!(tier));
    object.insert("evidenceObservedAt".into(), json!(evidence_observed_at));
    object.insert("hostId".into(), json!(status.host_id));
    object.insert("paneUnbound".into(), json!(status.pane_unbound));
}

/// Normalise one stored attention payload into the flat card shape the dashboard
/// renders (`{ question | marker | snippet | …, options: [String] }`).
///
/// The daemon stores the request *context* verbatim, and a real ingest wraps the
/// request fields under a `context` object with `AskUserQuestion` options as
/// `{ "label": "…" }` objects (the canonical shape the TUI control centre and the
/// acceptance fixtures use). The dashboard's card renderer, by contrast, reads
/// the detail fields (`question`, `marker`, …) and the option *strings* at the
/// TOP level. Without this bridge a real ASK card renders its title but no
/// question and, fatally, no answer buttons, because `payload.options` is
/// absent. So the mapping boundary flattens a `context` wrapper up one level and
/// converts each option to its label string, while leaving an already-flat
/// payload (or a raw non-object string) untouched.
fn normalize_payload(raw: &str) -> Value {
    let parsed = match serde_json::from_str::<Value>(raw) {
        Ok(v) => v,
        // Not JSON: surface the raw string so the card still shows something.
        Err(_) => return json!(raw),
    };

    // Unwrap a `context` wrapper when present; otherwise normalise in place.
    let base = match parsed.get("context") {
        Some(Value::Object(_)) => parsed.get("context").cloned().unwrap_or(parsed),
        _ => parsed,
    };

    // Only object payloads carry render fields; anything else passes through.
    let Value::Object(mut map) = base else {
        return base;
    };

    // Convert `options` to an array of label strings: accept `{ "label": "x" }`
    // objects (canonical) OR bare strings (already-flat test/legacy payloads).
    if let Some(Value::Array(opts)) = map.get("options") {
        let labels: Vec<Value> = opts
            .iter()
            .filter_map(|opt| match opt {
                Value::Object(o) => o.get("label").and_then(Value::as_str).map(|s| json!(s)),
                Value::String(s) => Some(json!(s)),
                _ => None,
            })
            .collect();
        map.insert("options".to_string(), Value::Array(labels));
    }

    Value::Object(map)
}

// ── answer seam ──────────────────────────────────────────────────────────────

/// The `/api/answer` route's dependency: route an answer through the daemon.
/// Abstracted so route tests can inject a deterministic fake instead of dialling
/// a real socket, mirroring the [`crate::data::DataSource`] seam.
pub trait Answerer: Send + Sync + 'static {
    /// Deliver `params` to the daemon's `attention/answer` RPC.
    fn answer(
        &self,
        params: AnswerParams,
    ) -> Pin<Box<dyn Future<Output = Result<AnswerResult, DaemonError>> + Send + '_>>;
}

/// The [`SurfaceInfo`] describing this process as the web dashboard.
#[must_use]
pub fn web_surface() -> SurfaceInfo {
    SurfaceInfo {
        kind: SurfaceKind::Web,
        pid: std::process::id(),
    }
}

/// A [`DaemonClient`] configured with [`SurfaceKind::Web`] at this process PID.
///
/// Resolves a fresh client from the environment and stamps its surface metadata
/// so "the web is Web at this pid" lives in one place.
pub fn web_client() -> Result<DaemonClient, DaemonError> {
    let mut client = DaemonClient::from_env()?;
    client.set_surface(web_surface());
    Ok(client)
}

/// Production [`Answerer`]: resolves a fresh [`DaemonClient`] from the
/// environment per call (so it picks up a daemon that started after the web
/// server did) and forwards the answer.
#[derive(Debug, Clone, Default)]
pub struct DaemonAnswerer;

impl Answerer for DaemonAnswerer {
    fn answer(
        &self,
        params: AnswerParams,
    ) -> Pin<Box<dyn Future<Output = Result<AnswerResult, DaemonError>> + Send + '_>> {
        Box::pin(async move {
            let client = web_client()?;
            client.answer(params).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: &str, payload: &str) -> AttentionRow {
        AttentionRow {
            id: id.to_string(),
            session_id: format!("sess-{id}"),
            cwd: format!("/work/{id}"),
            workspace_id: None,
            kind: kind.to_string(),
            payload: payload.to_string(),
            degraded: false,
            created_at: 1000,
            channels: ainb_hangar_proto::ChannelSet::NONE,
            version: 1,
        }
    }

    #[test]
    fn display_kind_maps_answerable_families_to_ask() {
        assert_eq!(display_kind("ask_user_question"), "ASK");
        assert_eq!(display_kind("approval"), "ASK");
        assert_eq!(display_kind("codex_request_user"), "ASK");
        assert_eq!(display_kind("escalation"), "ASK");
        assert_eq!(display_kind("error"), "ERR");
        assert_eq!(display_kind("waiting"), "WAIT");
    }

    #[test]
    fn attention_to_needs_carries_answer_id_and_display_kind() {
        let rows = vec![
            row(
                "a1",
                "ask_user_question",
                r#"{"question":"pick one","options":["x","y"]}"#,
            ),
            row("e1", "error", "boom"),
        ];
        let needs = attention_to_needs(&rows);
        let arr = needs.as_array().expect("needs is an array");
        assert_eq!(arr.len(), 2);

        // Ordering preserved (oldest-first urgency order from the daemon).
        assert_eq!(arr[0]["attentionId"], "a1");
        assert_eq!(arr[0]["kind"], "ASK");
        assert_eq!(arr[0]["wireKind"], "ask_user_question");
        // A parseable payload is embedded as structured JSON the card can render.
        assert_eq!(arr[0]["payload"]["question"], "pick one");
        assert_eq!(arr[0]["payload"]["options"][1], "y");

        assert_eq!(arr[1]["attentionId"], "e1");
        assert_eq!(arr[1]["kind"], "ERR");
        // An unparseable payload falls back to the raw string, never dropped.
        assert_eq!(arr[1]["payload"], "boom");
    }

    #[test]
    fn attention_to_needs_flattens_canonical_context_ask_payload() {
        // The canonical stored shape (real ingest + acceptance fixtures): the
        // request fields nested under `context`, with options as `{label}`
        // objects. The card renderer reads the detail + option STRINGS at the top
        // level, so the mapping must flatten the wrapper and unwrap the labels:
        // otherwise the ASK renders no question and no answer buttons.
        let payload = r#"{"kind":"ASK","context":{"question":"Ship to which env?","options":[{"label":"staging"},{"label":"prod"},{"label":"canary"}]}}"#;
        let rows = vec![row("att-ask-1", "ask_user_question", payload)];
        let needs = attention_to_needs(&rows);
        let card = &needs.as_array().expect("needs is an array")[0];

        assert_eq!(card["kind"], "ASK");
        // Detail is lifted out of the `context` wrapper to the top level.
        assert_eq!(card["payload"]["question"], "Ship to which env?");
        // Options are flattened to their label strings, in order; this is what
        // the frontend turns into the "1. staging" / "2. prod" answer buttons.
        assert_eq!(card["payload"]["options"][0], "staging");
        assert_eq!(card["payload"]["options"][1], "prod");
        assert_eq!(card["payload"]["options"][2], "canary");
    }

    #[test]
    fn attention_to_needs_flattens_canonical_wait_marker() {
        // A WAIT row nests its marker the same way; flattening keeps the
        // dashboard's `needDetail` (which reads `payload.marker`) working.
        let payload = r#"{"kind":"WAIT","context":{"marker":"WAITING: still idle"}}"#;
        let rows = vec![row("att-wait-1", "waiting", payload)];
        let needs = attention_to_needs(&rows);
        let card = &needs.as_array().expect("needs is an array")[0];
        assert_eq!(card["kind"], "WAIT");
        assert_eq!(card["payload"]["marker"], "WAITING: still idle");
    }

    #[test]
    fn attention_to_needs_empty_is_empty_array() {
        assert_eq!(attention_to_needs(&[]), json!([]));
    }
}
