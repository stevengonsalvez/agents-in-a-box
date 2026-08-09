//! Fleet session reducer orchestration.
//!
//! Provider hooks and tmux discovery normalize into the Hangar Fleet tables.
//! SQLite owns canonical state and revision order. Live broadcasts only wake
//! subscribers after the matching revision commits.

use ainb_fleet_core::discover::{discover_all_tmux_panes, discover_from_tmux};
use ainb_fleet_core::read::ModelInfo;
use ainb_fleet_core::types::{
    AttentionState, Confidence, FleetSession, LifecycleState, ManagementState, Provider,
    SessionKey, TransportHealth,
};
use ainb_hangar_store::repo::fleet::{
    ApplyFleetEventResult, FleetEventRow, FleetRepo, FleetRepoError, FleetSessionPatch,
    FleetSessionRow, NewFleetEvent, ObservationAuthority,
};
use ainb_hangar_store::repo::fleet_provider_event::{
    FleetProviderEventError, FleetProviderEventRepo, NewFleetProviderEvent,
};
use ainb_hangar_store::repo::fleet_work::{FleetWorkRepo, FleetWorkUpdate};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::events::EventSink;
use crate::fleet_provider::codex::{
    CodexApprovalKind, CodexCapabilities, CodexInbound, CodexInboundEnvelope,
    parse_inbound_envelope,
};

/// Semantic hook observation before storage normalization.
#[derive(Debug, Clone)]
pub struct HookObservation<'a> {
    /// Replay-safe hook or legacy event identifier.
    pub event_id: String,
    /// Provider token supplied by hook installation.
    pub provider: &'a str,
    /// Provider-owned stable session identifier.
    pub provider_session_id: &'a str,
    /// Working directory metadata.
    pub cwd: &'a str,
    /// Semantic hook discriminator.
    pub event_type: &'a str,
    /// Complete raw hook payload.
    pub payload: &'a Value,
    /// Observation time in epoch milliseconds.
    pub observed_at: i64,
    /// Model + effort read from this session's transcript tail, when the caller
    /// took that bounded read for this line (see [`crate::attention_ingest`]).
    ///
    /// Whichever field the hook payload also carries, the hook wins: it describes
    /// this exact event, while the tail describes the newest record on disk.
    pub transcript_model: Option<ModelInfo>,
}

/// Reprojecting an old Claude interview failed its explicit safety gate.
#[derive(Debug, thiserror::Error)]
pub enum FleetReprojectError {
    /// Session was absent.
    #[error("Fleet session {0:?} was not found")]
    SessionNotFound(String),
    /// Recovery only accepts managed Claude sessions.
    #[error("Fleet session {0:?} is not a managed Claude session")]
    NotManagedClaude(String),
    /// Caller must pin the version inspected before mutation.
    #[error("Fleet session {session_key:?} version is {actual}, expected {expected}")]
    StaleVersion {
        /// Target session.
        session_key: String,
        /// Version supplied by caller.
        expected: i64,
        /// Current version.
        actual: i64,
    },
    /// Only a stale waiting projection may be recovered.
    #[error("Fleet session {0:?} is not waiting for recovery")]
    NotWaiting(String),
    /// Durable history has no unresolved structured interview.
    #[error("Fleet session {0:?} has no recoverable Claude interview")]
    NoRecoverableInterview(String),
    /// Store failure.
    #[error(transparent)]
    Store(#[from] FleetRepoError),
    /// Query failure.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

/// Rebuild one stale Claude interview from its durable hook history.
///
/// This only appends a canonical recovery event. It never calls the broker,
/// writes to tmux, or sends a provider response.
pub async fn reproject_claude_interview(
    pool: &SqlitePool,
    events: &EventSink,
    session_key: &str,
    expected_version: i64,
    observed_at: i64,
) -> Result<ApplyFleetEventResult, FleetReprojectError> {
    let session = FleetRepo::get_session(pool, session_key)
        .await?
        .ok_or_else(|| FleetReprojectError::SessionNotFound(session_key.to_string()))?;
    if session.provider != "claude" || session.management_state != "MANAGED" {
        return Err(FleetReprojectError::NotManagedClaude(
            session_key.to_string(),
        ));
    }
    if session.version != expected_version {
        return Err(FleetReprojectError::StaleVersion {
            session_key: session_key.to_string(),
            expected: expected_version,
            actual: session.version,
        });
    }
    if session.attention_state != "WAITING" {
        return Err(FleetReprojectError::NotWaiting(session_key.to_string()));
    }

    let mut active_request = None;
    for event in FleetRepo::events_for_session(pool, session_key).await? {
        if event.event_id.starts_with("fleet-reproject:") {
            continue;
        }
        let Ok(payload) = serde_json::from_str::<Value>(&event.payload) else {
            continue;
        };
        let event_type = canonical_hook_event_type(&event.event_type, &payload);
        let (_, attention) = states_for_hook(event_type, &payload);
        match attention {
            Some(AttentionState::Ask)
                if claude_request_identity(&payload).is_some()
                    && claude_questions(&payload).is_some() =>
            {
                active_request = Some((event.event_id, payload));
            }
            // A generic notification caused the original lost projection. It
            // carries no provider completion signal, so it cannot invalidate
            // an earlier unresolved interview.
            Some(AttentionState::Waiting) | None => {}
            Some(_) => active_request = None,
        }
    }
    let Some((source_event_id, payload)) = active_request else {
        return Err(FleetReprojectError::NoRecoverableInterview(
            session_key.to_string(),
        ));
    };
    let fingerprint = claude_request_identity(&payload)
        .map(|identity| ainb_plugin_notifyd::broker::request_fingerprint(&identity))
        .ok_or_else(|| FleetReprojectError::NoRecoverableInterview(session_key.to_string()))?;
    let result = FleetRepo::apply_event_if_version(
        pool,
        &NewFleetEvent {
            event_id: format!("fleet-reproject:{session_key}:{source_event_id}"),
            session_key: session_key.to_string(),
            observed_at,
            authority: ObservationAuthority::Authoritative,
            event_type: "AskUserQuestion".to_string(),
            payload: serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
            patch: FleetSessionPatch {
                lifecycle_state: Some(state_token(LifecycleState::Idle)),
                attention_state: Some(attention_token(AttentionState::Ask)),
                current_request_fingerprint: Some(Some(fingerprint)),
                ..FleetSessionPatch::default()
            },
        },
        expected_version,
    )
    .await
    .map_err(|error| match error {
        FleetRepoError::StaleVersion { actual, .. } => FleetReprojectError::StaleVersion {
            session_key: session_key.to_string(),
            expected: expected_version,
            actual,
        },
        FleetRepoError::SessionNotFound { .. } => {
            FleetReprojectError::SessionNotFound(session_key.to_string())
        }
        error => FleetReprojectError::Store(error),
    })?;
    if !result.duplicate {
        events.emit_fleet_revision(result.revision);
    }
    Ok(result)
}

/// Longest provider model or effort token the roster will store.
///
/// The longest real id observed is well under half this. The bound exists so a
/// lying or compromised provider cannot turn a per-session column into a
/// storage channel, not to fit any particular vendor's naming.
const MODEL_TOKEN_MAX: usize = 64;

/// One provider-reported model or effort token, clamped.
///
/// Rejects empty, over [`MODEL_TOKEN_MAX`] bytes, or any character outside
/// `[A-Za-z0-9._:@/+-]`. This is the privacy clamp and the lying-provider clamp
/// in one predicate: a rejected token is stored as "never observed" rather than
/// rendered, so nothing a provider says can reach the roster as free text. The
/// charset also rejects Claude's `<synthetic>` placeholder, which is not a
/// model and appeared in half of one recent transcript sample.
///
/// Accepted tokens are otherwise stored VERBATIM. No case folding, no
/// vendor-prefix stripping: presentation is the client's job, and a
/// normalisation here would be indistinguishable from an observation once it is
/// in the column.
///
/// The ONE exception is a trailing `[...]`, which is stripped before the token
/// is validated. Claude's hooks report `claude-opus-5[1m]` where its transcript
/// writes `claude-opus-5`; the bracket carries the context window, not the model
/// identity. Both producers must therefore collapse onto one token, or a single
/// session renders two different models depending on which one observed it last.
/// Widening the charset to admit brackets would do the opposite — it would let
/// the two spellings coexist as distinct values — and rejecting the bracketed
/// form outright would cost every Claude row its model until the turn ends,
/// since the hook is the only producer that fires mid-turn.
pub(crate) fn model_token(raw: &str) -> Option<String> {
    if raw.is_empty() || raw.len() > MODEL_TOKEN_MAX {
        return None;
    }
    let identity = raw
        .strip_suffix(']')
        .and_then(|head| head.rsplit_once('[').map(|(identity, _)| identity))
        .unwrap_or(raw);
    (!identity.is_empty()
        && identity.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'@' | b'/' | b'+' | b'-')
        }))
    .then(|| identity.to_string())
}

/// Whether a hook payload describes the session's OWN thread rather than one of
/// its subagents.
///
/// Claude reports every subagent's hook under the SAME `session_id` as the main
/// thread, carrying that subagent's model and effort. Ungated, a roster row shows
/// whichever `Task()` ran most recently — a value that changes several times a
/// minute, always looks plausible, and leaves nothing red in CI.
///
/// Only an ABSENT `agent_type` (what the main thread actually emits) or the
/// literal `MAIN` passes. An EMPTY string does not: measured live, 80 of 81
/// empty-string events in a 3000-event window were `SubagentStop` carrying an
/// `agent_transcript_path`.
pub(crate) fn is_main_thread(payload: &Value) -> bool {
    payload
        .pointer("/payload/agent_type")
        .and_then(Value::as_str)
        .is_none_or(|agent| agent == "MAIN")
}

/// The `(model, reasoning_effort)` this hook line establishes for its session.
///
/// Both providers are served by the same two producers, crossed over: Claude
/// hooks carry the effort and their transcript carries the model, Codex hooks
/// carry the model and their rollout carries the effort. Reading both sources
/// unconditionally of provider needs no provider branch — the source that does
/// not apply is simply absent.
///
/// [`model_token`] is applied to each candidate INDEPENDENTLY before the
/// fallback, so a hook value the clamp rejects (Claude's `claude-opus-5[1m]`)
/// yields to the transcript's spelling rather than suppressing it.
fn observed_model_pair(observation: &HookObservation<'_>) -> (Option<String>, Option<String>) {
    if !is_main_thread(observation.payload) {
        return (None, None);
    }
    let transcript = observation.transcript_model.as_ref();
    let from_hook = |pointer: &str| {
        observation
            .payload
            .pointer(pointer)
            .and_then(Value::as_str)
            .and_then(model_token)
    };
    let from_transcript =
        |field: fn(&ModelInfo) -> Option<&str>| transcript.and_then(field).and_then(model_token);
    (
        from_hook("/payload/model").or_else(|| from_transcript(|info| info.model.as_deref())),
        from_hook("/payload/effort/level")
            .or_else(|| from_transcript(|info| info.effort.as_deref())),
    )
}

/// Apply one exact provider hook and wake revision subscribers after commit.
pub async fn apply_hook(
    pool: &SqlitePool,
    events: &EventSink,
    observation: HookObservation<'_>,
) -> Result<ApplyFleetEventResult, FleetRepoError> {
    let provider = parse_provider(observation.provider);
    let session_key = SessionKey::managed(provider, observation.provider_session_id);
    let source_event_id = observation.event_id.clone();
    // Claude emits a PermissionRequest and then a generic notification around an
    // AskUserQuestion. They describe the same picker, not two operator actions.
    let event_type = canonical_hook_event_type(observation.event_type, observation.payload);
    let source_event_type = observation
        .payload
        .pointer("/payload/hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or(observation.event_type);
    let duplicate_claude_structured_permission = provider == Provider::Claude
        && source_event_type == "PermissionRequest"
        && claude_hook_tool_name(observation.payload) == Some("AskUserQuestion");
    let preserve_active_request = (observation.event_type == "Notification"
        || duplicate_claude_structured_permission)
        && FleetRepo::get_session(pool, session_key.as_str())
            .await?
            .is_some_and(|session| {
                matches!(session.attention_state.as_str(), "ASK" | "APPROVAL")
                    && session.current_request_fingerprint.is_some()
            });
    let tmux_target = observation
        .payload
        .get("tmux_target")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let process_start_fingerprint = observation
        .payload
        .get("process_start_fingerprint")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let exact_tmux_identity = tmux_target.is_some() && process_start_fingerprint.is_some();
    let (model, reasoning_effort) = observed_model_pair(&observation);
    let (lifecycle_state, attention_state) = if preserve_active_request {
        (None, None)
    } else {
        states_for_hook(event_type, observation.payload)
    };
    let request_fingerprint = match attention_state {
        Some(AttentionState::Ask | AttentionState::Approval) => {
            let fingerprint = match (provider, event_type) {
                (Provider::Claude, "AskUserQuestion") => {
                    claude_request_identity(observation.payload).map_or_else(
                        || fingerprint_value(observation.payload),
                        |identity| ainb_plugin_notifyd::broker::request_fingerprint(&identity),
                    )
                }
                (Provider::Claude, "PermissionRequest") => {
                    claude_permission_identity(observation.payload).map_or_else(
                        || fingerprint_value(observation.payload),
                        |(tool, context)| {
                            ainb_plugin_notifyd::broker::permission_fingerprint(&tool, &context)
                        },
                    )
                }
                _ => fingerprint_value(observation.payload),
            };
            Some(Some(fingerprint))
        }
        Some(AttentionState::None) => Some(None),
        _ => None,
    };
    let event = NewFleetEvent {
        event_id: source_event_id.clone(),
        session_key: session_key.to_string(),
        observed_at: observation.observed_at,
        authority: ObservationAuthority::Authoritative,
        event_type: event_type.to_string(),
        payload: serde_json::to_string(observation.payload).unwrap_or_else(|_| "{}".to_string()),
        patch: FleetSessionPatch {
            provider: Some(provider.as_str().to_string()),
            provider_session_id: Some(observation.provider_session_id.to_string()),
            tmux_target: tmux_target.clone(),
            process_start_fingerprint: process_start_fingerprint.clone(),
            cwd: Some(observation.cwd.to_string()),
            management_state: (provider == Provider::Claude).then(|| "MANAGED".to_string()),
            capabilities: (provider == Provider::Claude)
                .then(|| claude_managed_capabilities(exact_tmux_identity)),
            confidence: Some("HIGH".to_string()),
            lifecycle_state: lifecycle_state.map(state_token),
            attention_state: attention_state.map(attention_token),
            current_request_fingerprint: request_fingerprint,
            transport_health: (provider == Provider::Claude).then(|| "HEALTHY".to_string()),
            model,
            reasoning_effort,
            ..FleetSessionPatch::default()
        },
    };
    let result = FleetRepo::apply_event(pool, &event).await?;
    if !result.duplicate {
        events.emit_fleet_revision(result.revision);
    }
    if let Some(update) = hook_work_update(&observation, session_key.as_str()) {
        apply_workload_projection(pool, events, &update).await?;
    }
    if let (Some(target), Some(fingerprint)) =
        (tmux_target.as_deref(), process_start_fingerprint.as_deref())
    {
        retire_correlated_legacy(
            pool,
            events,
            session_key.as_str(),
            provider.as_str(),
            target,
            fingerprint,
            observation.observed_at,
        )
        .await?;
    }
    Ok(result)
}

async fn apply_workload_projection(
    pool: &SqlitePool,
    events: &EventSink,
    update: &FleetWorkUpdate,
) -> Result<ApplyFleetEventResult, FleetRepoError> {
    let active_work_count = FleetWorkRepo::apply(pool, update).await?;
    publish_workload_projection(pool, events, update, active_work_count).await
}

async fn publish_workload_projection(
    pool: &SqlitePool,
    events: &EventSink,
    update: &FleetWorkUpdate,
    active_work_count: i64,
) -> Result<ApplyFleetEventResult, FleetRepoError> {
    let workload = FleetRepo::apply_event(
        pool,
        &NewFleetEvent {
            event_id: format!("workload:{}", update.event_id),
            session_key: update.session_key.clone(),
            observed_at: update.observed_at,
            authority: ObservationAuthority::Authoritative,
            event_type: "workload_changed".to_string(),
            payload: serde_json::to_string(&serde_json::json!({
                "kind": update.kind,
                "workKey": update.work_key,
                "active": update.active,
                "activeWorkCount": active_work_count,
            }))
            .unwrap_or_else(|_| "{}".to_string()),
            patch: FleetSessionPatch {
                active_work_count: Some(active_work_count),
                ..FleetSessionPatch::default()
            },
        },
    )
    .await?;
    if !workload.duplicate {
        events.emit_fleet_revision(workload.revision);
    }
    Ok(workload)
}

fn hook_work_update(
    observation: &HookObservation<'_>,
    session_key: &str,
) -> Option<FleetWorkUpdate> {
    let hook = observation.payload.get("payload").unwrap_or(observation.payload);
    let (kind, active, key_names): (&str, bool, &[&str]) = match observation.event_type {
        "SubagentStart" => ("subagent", true, &["agent_id", "agentId"]),
        "SubagentStop" => ("subagent", false, &["agent_id", "agentId"]),
        "TaskCreated" => ("task", true, &["task_id", "taskId"]),
        "TaskCompleted" => ("task", false, &["task_id", "taskId"]),
        _ => return None,
    };
    let work_key = key_names
        .iter()
        .find_map(|name| hook.get(*name).and_then(Value::as_str))
        .filter(|value| !value.is_empty())?
        .to_string();
    Some(FleetWorkUpdate {
        provider: parse_provider(observation.provider).as_str().to_string(),
        session_key: session_key.to_string(),
        work_key,
        kind: kind.to_string(),
        active,
        event_id: observation.event_id.clone(),
        observed_at: observation.observed_at,
    })
}

async fn retire_correlated_legacy(
    pool: &SqlitePool,
    events: &EventSink,
    managed_key: &str,
    provider: &str,
    tmux_target: &str,
    process_start_fingerprint: &str,
    observed_at: i64,
) -> Result<(), FleetRepoError> {
    let legacy_keys = sqlx::query_scalar::<_, String>(
        "SELECT session_key FROM fleet_session WHERE session_key != ? AND provider = ? \
         AND management_state = 'DEGRADED' AND tmux_target = ? \
         AND process_start_fingerprint = ? AND visible = 1",
    )
    .bind(managed_key)
    .bind(provider)
    .bind(tmux_target)
    .bind(process_start_fingerprint)
    .fetch_all(pool)
    .await?;
    for legacy_key in legacy_keys {
        if let Some(revision) =
            FleetRepo::supersede_session(pool, &legacy_key, managed_key, observed_at).await?
        {
            events.emit_fleet_revision(revision);
        }
    }
    Ok(())
}

/// Read a wire-ready consistent snapshot from Hangar SQLite.
pub async fn snapshot_wire(
    pool: &SqlitePool,
) -> Result<ainb_hangar_proto::fleet::FleetSnapshot, sqlx::Error> {
    let projection = FleetRepo::subscription_projection(pool, 0, 0).await?;
    Ok(subscription_snapshot_wire(&projection))
}

/// Read a wire-ready Fleet subscription projection from one store transaction.
pub async fn subscription_wire(
    pool: &SqlitePool,
    after_revision: i64,
    replay_limit: i64,
) -> Result<ainb_hangar_store::repo::fleet::FleetSubscriptionProjection, FleetRepoError> {
    ainb_hangar_store::repo::fleet::FleetRepo::subscription_projection(
        pool,
        after_revision,
        replay_limit,
    )
    .await
    .map_err(FleetRepoError::from)
}

/// Convert one atomic store subscription projection into its Fleet wire shape.
pub fn subscription_snapshot_wire(
    projection: &ainb_hangar_store::repo::fleet::FleetSubscriptionProjection,
) -> ainb_hangar_proto::fleet::FleetSnapshot {
    ainb_hangar_proto::fleet::FleetSnapshot {
        head_revision: projection.head_revision,
        sessions: projection
            .sessions
            .iter()
            .map(|row| session_wire(&row.session, row.current_request.clone()))
            .collect(),
    }
}

/// Read complete payload for current structured request or approval.
pub async fn current_request_wire(
    pool: &SqlitePool,
    session_key: &str,
) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT event.payload FROM fleet_session AS session \
         JOIN fleet_event AS event ON event.session_key = session.session_key \
         WHERE session.session_key = ? \
           AND event.request_fingerprint = session.current_request_fingerprint \
           AND event.event_type IN (\
            'AskUserQuestion', 'PermissionRequest', \
            'item/tool/requestUserInput', \
            'item/commandExecution/requestApproval', \
            'item/fileChange/requestApproval', \
            'item/permissions/requestApproval'\
         ) AND event.applied = 1 ORDER BY event.revision DESC LIMIT 1",
    )
    .bind(session_key)
    .fetch_optional(pool)
    .await
    .map(|payload| payload.and_then(|payload| serde_json::from_str(&payload).ok()))
}

/// Apply one ordered Codex app-server request or lifecycle event.
pub async fn apply_codex_inbound(
    pool: &SqlitePool,
    events: &EventSink,
    event_id: String,
    inbound: CodexInbound,
    capabilities: &CodexCapabilities,
    observed_at: i64,
) -> Result<Option<ApplyFleetEventResult>, FleetRepoError> {
    let normalized = normalize_codex_inbound(event_id, inbound, capabilities, observed_at);
    let Some(mut event) = normalized else {
        return Ok(None);
    };
    if FleetRepo::get_session(pool, &event.session_key)
        .await?
        .is_some_and(|row| row.tmux_target.is_some() && row.transport_health == "HEALTHY")
    {
        event.patch.capabilities = event.patch.capabilities.as_deref().map(|serialized| {
            with_tmux_capabilities(&with_managed_lifecycle_capabilities(serialized, true), true)
        });
    }
    let result = FleetRepo::apply_event(pool, &event).await?;
    if !result.duplicate {
        events.emit_fleet_revision(result.revision);
    }
    Ok(Some(result))
}

/// Error while persisting or reducing one provider source envelope.
#[derive(Debug, thiserror::Error)]
pub enum FleetProviderIngressError {
    /// Raw source ledger failed.
    #[error(transparent)]
    ProviderEvent(#[from] FleetProviderEventError),
    /// Fleet projection failed.
    #[error(transparent)]
    Fleet(#[from] FleetRepoError),
}

/// Persist one exact Codex app-server envelope before reducing it. A crash after
/// the source write is safe: retrying the same manager sequence reuses its row,
/// then completes the projection link.
pub async fn ingest_codex_inbound(
    pool: &SqlitePool,
    events: &EventSink,
    event_id: String,
    envelope: CodexInboundEnvelope,
    capabilities: &CodexCapabilities,
    observed_at: i64,
) -> Result<Option<ApplyFleetEventResult>, FleetProviderIngressError> {
    let method = envelope
        .raw
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let provider_session_id = envelope.raw.get("params").and_then(codex_thread_id);
    let session_key = provider_session_id
        .as_deref()
        .map(|thread_id| SessionKey::managed(Provider::Codex, thread_id).to_string());
    FleetProviderEventRepo::append(
        pool,
        &NewFleetProviderEvent {
            event_id: event_id.clone(),
            provider: "codex".to_string(),
            source: "codex_app_server".to_string(),
            session_key,
            provider_session_id,
            observed_at,
            received_at: observed_at,
            event_type: method,
            raw_payload: serde_json::to_string(&envelope.raw).unwrap_or_default(),
        },
    )
    .await?;
    reduce_codex_source_event(pool, events, &event_id, envelope, capabilities, observed_at).await
}

async fn reduce_codex_source_event(
    pool: &SqlitePool,
    events: &EventSink,
    event_id: &str,
    envelope: CodexInboundEnvelope,
    capabilities: &CodexCapabilities,
    observed_at: i64,
) -> Result<Option<ApplyFleetEventResult>, FleetProviderIngressError> {
    apply_codex_child_work(pool, events, &envelope.raw, event_id, observed_at).await?;
    let result = apply_codex_inbound(
        pool,
        events,
        event_id.to_string(),
        envelope.inbound,
        capabilities,
        observed_at,
    )
    .await?;
    if let Some(reduced) = &result {
        FleetProviderEventRepo::mark_projected(pool, event_id, reduced.revision).await?;
    }
    Ok(result)
}

/// Replay Codex source envelopes that committed before their Fleet projection.
/// Each source row keeps its original event ID and observation time, so replay
/// is idempotent and cannot manufacture a newer state.
pub async fn replay_unprojected_codex_events(
    pool: &SqlitePool,
    events: &EventSink,
    capabilities: &CodexCapabilities,
) -> Result<usize, FleetProviderIngressError> {
    let rows = FleetProviderEventRepo::unprojected(pool, "codex", "codex_app_server", 1_000)
        .await
        .map_err(FleetProviderEventError::from)?;
    let mut replayed = 0;
    for row in rows {
        let raw: Value = match serde_json::from_str(&row.raw_payload) {
            Ok(raw) => raw,
            Err(error) => {
                tracing::warn!(event_id = %row.event_id, error = %error, "Codex source replay skipped invalid raw envelope");
                continue;
            }
        };
        let envelope = match parse_inbound_envelope(&raw) {
            Ok(envelope) => envelope,
            Err(error) => {
                tracing::warn!(event_id = %row.event_id, error = %error, "Codex source replay skipped unsupported envelope");
                continue;
            }
        };
        reduce_codex_source_event(
            pool,
            events,
            &row.event_id,
            envelope,
            capabilities,
            row.observed_at,
        )
        .await?;
        replayed += 1;
    }
    Ok(replayed)
}

async fn apply_codex_child_work(
    pool: &SqlitePool,
    events: &EventSink,
    raw: &Value,
    event_id: &str,
    observed_at: i64,
) -> Result<(), FleetRepoError> {
    let method = raw.get("method").and_then(Value::as_str);
    let params = raw.get("params").unwrap_or(&Value::Null);
    match method {
        Some("thread/started") => {
            let Some(parent_thread_id) = codex_parent_thread_id(params) else {
                return Ok(());
            };
            let Some(child_thread_id) = codex_thread_id(params) else {
                return Ok(());
            };
            let session_key = SessionKey::managed(Provider::Codex, &parent_thread_id).to_string();
            if FleetRepo::get_session(pool, &session_key).await?.is_none() {
                return Ok(());
            }
            let update = FleetWorkUpdate {
                provider: "codex".to_string(),
                session_key,
                work_key: child_thread_id,
                kind: "child_thread".to_string(),
                active: true,
                event_id: event_id.to_string(),
                observed_at,
            };
            apply_workload_projection(pool, events, &update).await?;
        }
        Some("thread/closed" | "thread/archived") => {
            let Some(child_thread_id) = codex_thread_id(params) else {
                return Ok(());
            };
            for (session_key, active_work_count) in FleetWorkRepo::complete_by_work_key(
                pool,
                "codex",
                &child_thread_id,
                event_id,
                observed_at,
            )
            .await?
            {
                let update = FleetWorkUpdate {
                    provider: "codex".to_string(),
                    session_key,
                    work_key: child_thread_id.clone(),
                    kind: "child_thread".to_string(),
                    active: false,
                    event_id: event_id.to_string(),
                    observed_at,
                };
                publish_workload_projection(pool, events, &update, active_work_count).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn codex_parent_thread_id(params: &Value) -> Option<String> {
    params
        .get("parentThreadId")
        .or_else(|| params.get("parent_thread_id"))
        .or_else(|| {
            params.get("thread").and_then(|thread| {
                thread.get("parentThreadId").or_else(|| thread.get("parent_thread_id"))
            })
        })
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Downgrade managed Codex rows when app-server transport exits.
pub async fn mark_codex_manager_unavailable(
    pool: &SqlitePool,
    events: &EventSink,
    observed_at: i64,
) -> Result<usize, FleetRepoError> {
    let snapshot = FleetRepo::snapshot(pool).await?;
    let mut changed = 0;
    for row in snapshot
        .sessions
        .into_iter()
        .filter(|row| row.provider == "codex" && row.management_state == "MANAGED")
    {
        let tmux_available = row.tmux_target.is_some() && row.transport_health == "HEALTHY";
        let event = NewFleetEvent {
            event_id: format!(
                "codex-manager:unavailable:{}:{observed_at}",
                row.session_key
            ),
            session_key: row.session_key.clone(),
            observed_at,
            authority: ObservationAuthority::Authoritative,
            event_type: "codex_manager_unavailable".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                management_state: Some("DEGRADED".to_string()),
                capabilities: Some(if tmux_available {
                    degraded_capabilities()
                } else {
                    with_tmux_capabilities("{}", false)
                }),
                transport_health: Some(
                    if tmux_available {
                        "DEGRADED"
                    } else {
                        "UNAVAILABLE"
                    }
                    .to_string(),
                ),
                ..FleetSessionPatch::default()
            },
        };
        let result = FleetRepo::apply_event(pool, &event).await?;
        if !result.duplicate {
            events.emit_fleet_revision(result.revision);
        }
        changed += usize::from(result.applied);
    }
    Ok(changed)
}

/// Restore quiet managed Codex rows after manager respawn. Recovery requires
/// exact app-server thread read plus exact live tmux process identity, then one
/// authoritative revision restores transport and lifecycle capabilities.
pub async fn recover_codex_manager(
    pool: &SqlitePool,
    events: &EventSink,
    manager: &crate::fleet_provider::codex_manager::CodexManagerHandle,
    observed_at: i64,
) -> Result<usize, FleetRepoError> {
    // Liveness, not roster: a managed Codex pane must not read as gone merely
    // because its agent process is momentarily absent from the tree.
    let discovered = match discover_all_tmux_panes().await {
        Ok(discovered) => discovered,
        Err(error) => {
            tracing::debug!(error = %error, "Codex manager recovery tmux discovery unavailable");
            return Ok(0);
        }
    };
    let snapshot = FleetRepo::snapshot(pool).await?;
    let mut changed = 0;
    for row in snapshot.sessions.into_iter().filter(|row| {
        row.provider == "codex"
            && row.provider_session_id.is_some()
            && row.tmux_target.is_some()
            && row.process_start_fingerprint.is_some()
    }) {
        let live = discovered.iter().any(|candidate| {
            candidate.exact_tmux_target == row.tmux_target
                && candidate.process_start_fingerprint == row.process_start_fingerprint
        });
        if !live {
            continue;
        }
        let thread_id = row.provider_session_id.as_deref().unwrap_or_default();
        if manager.thread_read(thread_id).await.is_err() {
            continue;
        }
        let event = codex_manager_recovery_event(&row, manager.capabilities(), observed_at);
        let result = FleetRepo::apply_event(pool, &event).await?;
        if !result.duplicate {
            events.emit_fleet_revision(result.revision);
        }
        changed += usize::from(result.applied);
    }
    Ok(changed)
}

fn codex_manager_recovery_event(
    row: &FleetSessionRow,
    capabilities: &CodexCapabilities,
    observed_at: i64,
) -> NewFleetEvent {
    NewFleetEvent {
        event_id: format!("codex-manager:recovered:{}:{observed_at}", row.session_key),
        session_key: row.session_key.clone(),
        observed_at,
        authority: ObservationAuthority::Authoritative,
        event_type: "codex_manager_recovered".to_string(),
        payload: "{}".to_string(),
        patch: FleetSessionPatch {
            management_state: Some("MANAGED".to_string()),
            capabilities: Some(with_tmux_capabilities(
                &with_managed_lifecycle_capabilities(
                    &codex_managed_capabilities(capabilities),
                    true,
                ),
                true,
            )),
            confidence: Some("HIGH".to_string()),
            transport_health: Some("HEALTHY".to_string()),
            ..FleetSessionPatch::default()
        },
    }
}

/// Persist exact tmux identity for one Fleet-launched managed Codex TUI.
pub async fn register_managed_codex_tmux(
    pool: &SqlitePool,
    events: &EventSink,
    thread_id: &str,
    cwd: &str,
    tmux: &FleetSession,
    capabilities: &CodexCapabilities,
    observed_at: i64,
) -> Result<ApplyFleetEventResult, FleetRepoError> {
    let target = tmux.exact_tmux_target.clone().ok_or_else(|| FleetRepoError::SessionNotFound {
        session_key: format!("codex:{thread_id}:tmux-target"),
    })?;
    let fingerprint =
        tmux.process_start_fingerprint
            .clone()
            .ok_or_else(|| FleetRepoError::SessionNotFound {
                session_key: format!("codex:{thread_id}:process-fingerprint"),
            })?;
    let event = NewFleetEvent {
        event_id: format!("codex-tmux:{thread_id}:{fingerprint}"),
        session_key: SessionKey::managed(Provider::Codex, thread_id).to_string(),
        observed_at,
        authority: ObservationAuthority::Authoritative,
        event_type: "codex_managed_tui_started".to_string(),
        payload: serde_json::to_string(tmux).unwrap_or_else(|_| "{}".to_string()),
        patch: FleetSessionPatch {
            provider: Some("codex".to_string()),
            provider_session_id: Some(thread_id.to_string()),
            tmux_target: Some(target),
            process_start_fingerprint: Some(fingerprint),
            cwd: Some(cwd.to_string()),
            management_state: Some("MANAGED".to_string()),
            capabilities: Some(with_tmux_capabilities(
                &with_managed_lifecycle_capabilities(
                    &codex_managed_capabilities(capabilities),
                    true,
                ),
                true,
            )),
            confidence: Some("HIGH".to_string()),
            lifecycle_state: Some("STARTING".to_string()),
            attention_state: Some("NONE".to_string()),
            transport_health: Some("HEALTHY".to_string()),
            ..FleetSessionPatch::default()
        },
    };
    let result = FleetRepo::apply_event(pool, &event).await?;
    if !result.duplicate {
        events.emit_fleet_revision(result.revision);
    }
    Ok(result)
}

/// Persist terminal local lifecycle after exact managed Codex tmux shutdown.
pub async fn mark_managed_codex_exited(
    pool: &SqlitePool,
    events: &EventSink,
    session_key: &str,
    event_type: &str,
    capabilities: &CodexCapabilities,
    observed_at: i64,
) -> Result<ApplyFleetEventResult, FleetRepoError> {
    let event = NewFleetEvent {
        event_id: format!("codex-lifecycle:{event_type}:{session_key}:{observed_at}"),
        session_key: session_key.to_string(),
        observed_at,
        authority: ObservationAuthority::Authoritative,
        event_type: event_type.to_string(),
        payload: "{}".to_string(),
        patch: FleetSessionPatch {
            capabilities: Some(with_tmux_capabilities(
                &with_managed_lifecycle_capabilities(
                    &codex_managed_capabilities(capabilities),
                    false,
                ),
                false,
            )),
            lifecycle_state: Some("EXITED".to_string()),
            attention_state: Some("NONE".to_string()),
            current_request_fingerprint: Some(None),
            transport_health: Some("UNAVAILABLE".to_string()),
            ..FleetSessionPatch::default()
        },
    };
    let result = FleetRepo::apply_event(pool, &event).await?;
    if !result.duplicate {
        events.emit_fleet_revision(result.revision);
    }
    Ok(result)
}

fn normalize_codex_inbound(
    event_id: String,
    inbound: CodexInbound,
    capabilities: &CodexCapabilities,
    observed_at: i64,
) -> Option<NewFleetEvent> {
    let (thread_id, event_type, payload, lifecycle, attention, fingerprint) = match inbound {
        CodexInbound::RequestUserInput(request) => {
            let payload = serde_json::to_value(&request).ok()?;
            let fingerprint = fingerprint_value(&payload);
            (
                request.identity.thread_id.clone(),
                "item/tool/requestUserInput".to_string(),
                payload,
                Some(LifecycleState::Idle),
                Some(AttentionState::Ask),
                Some(Some(fingerprint)),
            )
        }
        CodexInbound::Approval(request) => {
            let event_type = match request.kind {
                CodexApprovalKind::CommandExecution => "item/commandExecution/requestApproval",
                CodexApprovalKind::FileChange => "item/fileChange/requestApproval",
                CodexApprovalKind::Permissions => "item/permissions/requestApproval",
            };
            let payload = serde_json::json!({
                "identity": {
                    "requestId": request.identity.request_id.as_value(),
                    "threadId": request.identity.thread_id,
                    "turnId": request.identity.turn_id,
                    "itemId": request.identity.item_id,
                },
                "kind": match request.kind {
                    CodexApprovalKind::CommandExecution => "commandExecution",
                    CodexApprovalKind::FileChange => "fileChange",
                    CodexApprovalKind::Permissions => "permissions",
                },
                "params": request.params,
            });
            let thread_id = payload["identity"]["threadId"].as_str()?.to_string();
            let fingerprint = fingerprint_value(&payload);
            (
                thread_id,
                event_type.to_string(),
                payload,
                Some(LifecycleState::Idle),
                Some(AttentionState::Approval),
                Some(Some(fingerprint)),
            )
        }
        CodexInbound::Notification { method, params } => {
            let thread_id = codex_thread_id(&params)?;
            let (lifecycle, attention, fingerprint) = codex_notification_state(&method);
            (thread_id, method, params, lifecycle, attention, fingerprint)
        }
        CodexInbound::OtherRequest {
            request_id,
            method,
            params,
        } => {
            let thread_id = codex_thread_id(&params)?;
            let payload = serde_json::json!({
                "requestId": request_id.as_value(),
                "method": method,
                "params": params,
            });
            let fingerprint = fingerprint_value(&payload);
            (
                thread_id,
                method,
                payload,
                Some(LifecycleState::Running),
                Some(AttentionState::Waiting),
                Some(Some(fingerprint)),
            )
        }
    };
    let session_key = SessionKey::managed(Provider::Codex, &thread_id).to_string();
    Some(NewFleetEvent {
        event_id,
        session_key,
        observed_at,
        authority: ObservationAuthority::Authoritative,
        event_type,
        payload: serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string()),
        patch: FleetSessionPatch {
            provider: Some("codex".to_string()),
            provider_session_id: Some(thread_id),
            management_state: Some("MANAGED".to_string()),
            capabilities: Some(codex_managed_capabilities(capabilities)),
            confidence: Some("HIGH".to_string()),
            lifecycle_state: lifecycle.map(state_token),
            attention_state: attention.map(attention_token),
            current_request_fingerprint: fingerprint,
            transport_health: Some("HEALTHY".to_string()),
            ..FleetSessionPatch::default()
        },
    })
}

fn codex_thread_id(params: &Value) -> Option<String> {
    params
        .get("threadId")
        .or_else(|| params.get("thread_id"))
        .and_then(Value::as_str)
        .or_else(|| {
            params.get("thread").and_then(|thread| thread.get("id")).and_then(Value::as_str)
        })
        .map(str::to_string)
}

fn codex_notification_state(
    method: &str,
) -> (
    Option<LifecycleState>,
    Option<AttentionState>,
    Option<Option<String>>,
) {
    match method {
        "thread/started" => (
            Some(LifecycleState::Idle),
            Some(AttentionState::None),
            Some(None),
        ),
        "turn/started" => (
            Some(LifecycleState::Running),
            Some(AttentionState::None),
            Some(None),
        ),
        "turn/completed" => (
            Some(LifecycleState::TurnComplete),
            Some(AttentionState::None),
            Some(None),
        ),
        "thread/closed" | "thread/archived" => (
            Some(LifecycleState::Exited),
            Some(AttentionState::None),
            Some(None),
        ),
        method if method.contains("error") || method.contains("failed") => {
            (None, Some(AttentionState::Error), None)
        }
        _ => (None, None, None),
    }
}

/// Read durable wire events after a global revision.
pub async fn events_after_wire(
    pool: &SqlitePool,
    after_revision: i64,
    limit: i64,
) -> Result<Vec<ainb_hangar_proto::fleet::FleetEvent>, sqlx::Error> {
    FleetRepo::events_after(pool, after_revision, limit)
        .await
        .map(|events| events.iter().map(event_wire).collect())
}

fn session_wire(
    row: &FleetSessionRow,
    current_request: Option<Value>,
) -> ainb_hangar_proto::fleet::FleetSession {
    use ainb_hangar_proto::fleet as wire;
    wire::FleetSession {
        session_key: row.session_key.clone(),
        provider: match row.provider.as_str() {
            "claude" => wire::FleetProvider::Claude,
            "codex" => wire::FleetProvider::Codex,
            "copilot" => wire::FleetProvider::Copilot,
            "acp" => wire::FleetProvider::Acp,
            _ => wire::FleetProvider::Unknown,
        },
        provider_session_id: row.provider_session_id.clone(),
        tmux_target: row.tmux_target.clone(),
        process_start_fingerprint: row.process_start_fingerprint.clone(),
        cwd: row.cwd.clone(),
        display_name: row.display_name.clone(),
        lifecycle: parse_lifecycle(&row.lifecycle_state),
        active_work_count: row.active_work_count,
        attention: parse_attention(&row.attention_state),
        current_request_fingerprint: row.current_request_fingerprint.clone(),
        current_request,
        management: if row.management_state == "MANAGED" {
            wire::ManagementState::Managed
        } else {
            wire::ManagementState::Degraded
        },
        transport_health: match row.transport_health.as_str() {
            "HEALTHY" => wire::TransportHealth::Healthy,
            "DEGRADED" => wire::TransportHealth::Degraded,
            "UNAVAILABLE" => wire::TransportHealth::Unavailable,
            _ => wire::TransportHealth::Unknown,
        },
        capabilities: serde_json::from_str(&row.capabilities).unwrap_or_default(),
        provenance: if row.provenance == "authoritative" {
            wire::FleetProvenance::Authoritative
        } else {
            wire::FleetProvenance::Inferred
        },
        confidence: match row.confidence.as_str() {
            "HIGH" => wire::FleetConfidence::High,
            "MEDIUM" => wire::FleetConfidence::Medium,
            _ => wire::FleetConfidence::Low,
        },
        discovered_at: row.discovered_at,
        last_observed_at: row.last_observed_at,
        lifecycle_updated_at: row.lifecycle_updated_at,
        attention_updated_at: row.attention_updated_at,
        model: row.model.clone(),
        reasoning_effort: row.reasoning_effort.clone(),
        model_updated_at: row.model_updated_at,
        version: row.version,
        updated_revision: row.updated_revision,
    }
}

/// Convert one durable Fleet event row into its public wire representation.
pub fn event_wire(row: &FleetEventRow) -> ainb_hangar_proto::fleet::FleetEvent {
    ainb_hangar_proto::fleet::FleetEvent {
        revision: row.revision,
        event_id: row.event_id.clone(),
        session_key: row.session_key.clone(),
        observed_at: row.observed_at,
        provenance: if row.authority == "authoritative" {
            ainb_hangar_proto::fleet::FleetProvenance::Authoritative
        } else {
            ainb_hangar_proto::fleet::FleetProvenance::Inferred
        },
        event_type: row.event_type.clone(),
        payload: serde_json::from_str(&row.payload).unwrap_or(Value::Null),
        session_version: row.session_version,
        applied: row.applied,
    }
}

fn parse_lifecycle(value: &str) -> ainb_hangar_proto::fleet::LifecycleState {
    use ainb_hangar_proto::fleet::LifecycleState as State;
    match value {
        "STARTING" => State::Starting,
        "RUNNING" => State::Running,
        "TURN_COMPLETE" => State::TurnComplete,
        "IDLE" => State::Idle,
        "EXITED" => State::Exited,
        _ => State::Unknown,
    }
}

fn parse_attention(value: &str) -> ainb_hangar_proto::fleet::AttentionState {
    use ainb_hangar_proto::fleet::AttentionState as State;
    match value {
        "ASK" => State::Ask,
        "APPROVAL" => State::Approval,
        "WAITING" => State::Waiting,
        "ERROR" => State::Error,
        _ => State::None,
    }
}

/// Reconcile every exact tmux pane into the authoritative registry.
///
/// Discovery failures leave durable state untouched. A missing tmux server is
/// reported by fleet-core as an empty roster, not an error.
pub async fn reconcile_tmux_once(
    pool: &SqlitePool,
    events: &EventSink,
    observed_at: i64,
) -> anyhow::Result<usize> {
    let sessions = discover_from_tmux().await?;
    let live_bindings = restore_tmux_transport(pool, events, &sessions, observed_at).await?;
    let registered = FleetRepo::snapshot(pool).await?.sessions;
    let mut discovered: std::collections::HashSet<String> =
        sessions.iter().map(|session| session.session_key.to_string()).collect();
    // Union in the rows the restore pass found live by (target, fingerprint).
    // Without this the two halves disagree about what "missing" means and an
    // orphaned-key row churns forever — see `restore_tmux_transport`.
    discovered.extend(live_bindings);
    let mut applied = 0;
    for session in sessions {
        if let Some(managed) = registered.iter().find(|row| {
            row.management_state == "MANAGED"
                && row.tmux_target == session.exact_tmux_target
                && row.process_start_fingerprint == session.process_start_fingerprint
        }) {
            discovered.insert(managed.session_key.clone());
            continue;
        }
        let prior = FleetRepo::get_session(pool, session.session_key.as_str()).await?;
        if prior.as_ref().is_some_and(|row| tmux_row_matches(row, &session)) {
            continue;
        }
        let mut event = tmux_event(&session, observed_at);
        if prior.is_some() {
            event.event_id.push_str(&format!(":{observed_at}"));
        }
        match FleetRepo::apply_event(pool, &event).await {
            Ok(result) => {
                if !result.duplicate {
                    events.emit_fleet_revision(result.revision);
                }
                if result.applied {
                    applied += 1;
                }
            }
            Err(error) => tracing::warn!(error = %error, "fleet tmux reconcile failed"),
        }
    }
    let snapshot = FleetRepo::snapshot(pool).await?;
    for row in snapshot.sessions {
        // TRANSITION, NOT STATE. The guard keys off `transport_health` ALONE —
        // the thing `tmux_missing_event` actually always writes.
        //
        // It used to also require `lifecycle_state == "EXITED"`, which the event
        // only sets when `management_state != "MANAGED" && lifecycle_authority ==
        // "inferred"`. Every hook-backed session is MANAGED and authoritative, so
        // for those the conjunct could never become true, the row never reached
        // the skip, and each one re-emitted `tmux_missing` on EVERY 3s tick —
        // forever, with a timestamped `event_id` that dedup cannot collapse. That
        // wrote 832,711 rows (76% of `fleet_event`, 197k/day) on one host before
        // it was found. Same defect the discovery loop above already fixed via
        // `lifecycle_settled`; this pass never got the equivalent guard.
        //
        // Keying on transport alone is self-healing: when tmux reappears the
        // discovery loop puts the session in `discovered` and flips
        // `transport_health` back to HEALTHY, so the next disappearance is a real
        // transition and emits again.
        if !needs_tmux_missing_event(&row, &discovered) {
            continue;
        }
        let event = tmux_missing_event(&row, observed_at);
        match FleetRepo::apply_event(pool, &event).await {
            Ok(result) => {
                if !result.duplicate {
                    events.emit_fleet_revision(result.revision);
                }
                if result.applied {
                    applied += 1;
                }
            }
            Err(error) => tracing::warn!(error = %error, "fleet tmux exit reconcile failed"),
        }
    }
    Ok(applied)
}

/// Does this registered row need a fresh `tmux_missing` event this pass?
///
/// The loop's termination condition, split out so it is testable without a real
/// tmux server. It must be the exact complement of what
/// [`tmux_missing_event`]'s patch writes, or the emit never terminates — see the
/// comment at the call site for the 832,711-row incident that proved it.
fn needs_tmux_missing_event(
    row: &FleetSessionRow,
    discovered: &std::collections::HashSet<String>,
) -> bool {
    row.tmux_target.is_some()
        && !discovered.contains(&row.session_key)
        && row.transport_health != "UNAVAILABLE"
}

fn tmux_missing_event(row: &FleetSessionRow, observed_at: i64) -> NewFleetEvent {
    NewFleetEvent {
        event_id: format!("tmux:missing:{}:{observed_at}", row.session_key),
        session_key: row.session_key.clone(),
        observed_at,
        authority: ObservationAuthority::Authoritative,
        event_type: "tmux_missing".to_string(),
        payload: "{}".to_string(),
        patch: FleetSessionPatch {
            capabilities: Some(capabilities_for_tmux_state(row, false)),
            lifecycle_state: (row.management_state != "MANAGED"
                && row.lifecycle_authority == "inferred")
                .then(|| "EXITED".to_string()),
            transport_health: Some("UNAVAILABLE".to_string()),
            ..FleetSessionPatch::default()
        },
    }
}

fn tmux_row_matches(row: &FleetSessionRow, session: &FleetSession) -> bool {
    // A lifecycle set by a provider hook outranks this inferred tmux sample, so
    // the repo will never apply ours over it. Comparing them anyway would report
    // a permanent mismatch and append one no-op `fleet_event` per discovery
    // tick, forever, for every hook-backed session.
    let lifecycle_settled = row.lifecycle_authority == "authoritative"
        || row.lifecycle_state == state_token(session.lifecycle);
    row.provider == session.provider.as_str()
        && row.tmux_target == session.exact_tmux_target
        && row.process_start_fingerprint == session.process_start_fingerprint
        && row.cwd == session.cwd
        && row.management_state == management_token(session.management)
        && row.confidence == confidence_token(session.confidence)
        && lifecycle_settled
        && row.attention_state == attention_token(session.attention)
        && row.transport_health == transport_token(session.transport_health)
}

/// Keep unmanaged tmux sessions visible even when hooks are absent.
#[must_use]
pub fn spawn_tmux_reconciler(pool: SqlitePool, events: EventSink) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    if std::env::var("AINB_FLEET_DISABLE_TMUX_DISCOVERY").as_deref() == Ok("1") {
        return tokio::spawn(async {});
    }
    tokio::spawn(async move {
        let clock = SystemClock;
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(3));
        // One iteration does a full `FleetRepo::snapshot` plus per-session work,
        // which on a large roster has been measured over 1s. Under tokio's default
        // `Burst` a period that short turns every overrun into back-to-back
        // catch-up ticks with no sleep at all — a hot loop that then makes its own
        // queries slower. `Delay` re-bases the schedule on completion instead.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let observed_at = clock.now_ms();
            if let Err(error) = reconcile_tmux_once(&pool, &events, observed_at).await {
                tracing::debug!(error = %error, "fleet tmux discovery unavailable");
                if let Err(downgrade) = mark_tmux_unavailable(&pool, &events, observed_at).await {
                    tracing::warn!(error = %downgrade, "fleet tmux downgrade failed");
                }
            }
            // Runs whether or not discovery succeeded: the rows it retires are
            // precisely the ones discovery can no longer see.
            if let Err(error) = reap_stale_sessions(&pool, &events, observed_at).await {
                tracing::warn!(error = %error, "fleet stale reap failed");
            }
        }
    })
}

/// Demote dead sessions out of the visible roster on a slow clock.
///
/// Separate task, NOT folded into [`spawn_tmux_reconciler`]'s 3s loop: that
/// loop already does a full snapshot plus per-session work per tick and was
/// measured over 1s on a large roster. Archiving is pure cleanup with no
/// deadline, so it gets its own timer and cannot contribute to that hot path.
#[must_use]
pub fn spawn_session_archiver(pool: SqlitePool, events: EventSink) -> tokio::task::JoinHandle<()> {
    use ainb_hangar_core::clock::{HangarClock as _, SystemClock};
    tokio::spawn(async move {
        let clock = SystemClock;
        // `interval` would fire its FIRST tick immediately, putting a batch of
        // write transactions into the middle of daemon boot, which is already
        // the busiest moment for the single SQLite writer. Nothing here is
        // urgent — the rows have been dead for a day — so the first pass waits
        // out the boot convergence.
        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + SESSION_ARCHIVE_INTERVAL,
            SESSION_ARCHIVE_INTERVAL,
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            match archive_dead_sessions(&pool, &events, clock.now_ms()).await {
                Ok(0) => {}
                Ok(archived) => tracing::info!(archived, "fleet archived dead sessions"),
                Err(error) => tracing::warn!(error = %error, "fleet session archive failed"),
            }
        }
    })
}

/// How often [`archive_dead_sessions`] runs. Ten minutes drains a 1,440-row
/// backlog at [`SESSION_ARCHIVE_BATCH`] per pass in half an hour, and costs
/// one indexed candidate read per pass in the steady state.
const SESSION_ARCHIVE_INTERVAL: std::time::Duration = std::time::Duration::from_mins(10);

/// Rows demoted per pass. Each is its own transaction, so this bounds how long
/// one pass can hold the single `SQLite` writer away from the reconciler.
const SESSION_ARCHIVE_BATCH: i64 = 500;

/// How long a retired session stays in the visible roster.
///
/// Deliberately much longer than [`SESSION_STALE_TTL_MS`] (15 min), which is
/// the clock for marking a session `EXITED`. Those two are different questions:
/// "is this session dead" is answered in minutes so the roster tells the truth,
/// while "will anyone look at this dead session again today" is answered in
/// hours so that closing and reopening a session in one working day never needs
/// an unarchive. There is a revival path (`update_session`), but not needing it
/// is cheaper than needing it.
const SESSION_ARCHIVE_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// Demote long-dead sessions out of the visible roster.
///
/// Measured motivation: 1,440 of 1,472 `visible = 1` rows on a real profile
/// were `EXITED` sessions that every `SESSION_SELECT_ALL` scanned on the 3s
/// tick. `reap_stale_sessions` already stamps them `EXITED`; the missing half
/// was the visibility demotion, so this is a demotion, not a new lifecycle
/// state (`lifecycle_state` carries a `SQLite` CHECK that cannot be altered).
///
/// Nothing is deleted: the row keeps its identity and event history, stays
/// reachable by key, and re-appears in `FleetRepo::list_archived`.
///
/// # Errors
/// Propagates a store failure from reading candidates.
pub async fn archive_dead_sessions(
    pool: &SqlitePool,
    events: &EventSink,
    observed_at: i64,
) -> Result<usize, FleetRepoError> {
    let cutoff = observed_at.saturating_sub(SESSION_ARCHIVE_TTL_MS);
    let candidates = FleetRepo::list_archivable(pool, cutoff, SESSION_ARCHIVE_BATCH).await?;
    let mut archived = 0;
    let mut head = None;
    for session_key in candidates {
        match FleetRepo::archive_session(pool, &session_key, observed_at).await {
            // `None` means a hook revived the row between the candidate read
            // and the transaction. Not an error: the row belongs on screen.
            Ok(None) => {}
            Ok(Some(revision)) => {
                head = Some(revision);
                archived += 1;
            }
            Err(error) => tracing::warn!(error = %error, session_key, "fleet archive failed"),
        }
    }
    // ONE wakeup for the whole pass, carrying the highest revision it committed
    // — never one per row.
    //
    // `spawn_fleet_forwarder` DISCARDS the broadcast value (`Ok(_revision) =>
    // {}`) and uses it purely as a nudge to drain the durable log from its own
    // cursor to head, so N sends carry exactly the information of one. They are
    // not equally harmless: the fleet broadcast holds `CHANNEL_CAPACITY` = 256
    // (`events.rs`), the forwarder only calls `recv()` after finishing a drain,
    // and for the fleet stream alone `RecvError::Lagged` is TERMINAL — it emits
    // `fleet/resync_required` and returns. So a pass emitting more than 256
    // times could overrun a mid-drain subscriber and tear down every connected
    // TUI's live stream, which on the measured 1,440-row backlog is precisely
    // what a per-row emit would have done.
    //
    // Deliberately NOT fixed by shrinking `SESSION_ARCHIVE_BATCH` under 256:
    // that batch size is tuned for how long one pass may hold the SQLite
    // writer, and pinning it to an unrelated constant in another module would
    // be a false coupling that the next person to tune either one would break.
    if let Some(revision) = head {
        events.emit_fleet_revision(revision);
    }
    Ok(archived)
}

/// How long a session may go unobserved before the reaper retires it.
///
/// Generous on purpose. A false retire is self-healing — the next hook is
/// authoritative with a newer `observed_at` and replaces everything — while a
/// too-eager one would blink a live-but-quiet session out of the roster.
const SESSION_STALE_TTL_MS: i64 = 15 * 60 * 1000;

/// Retire sessions nothing can observe any more.
///
/// Authoritative state has no decay: an inferred sample can never outrank it
/// (`should_replace`). That is correct while observations keep arriving, but it
/// strands rows whose observer is gone for good — a hook-sourced session with no
/// tmux binding whose process died, or one parked in an attention state that
/// only a later hook could clear. The tmux sweeps cannot help: they only visit
/// rows carrying a `tmux_target`.
///
/// So the fix is a producer, not a weaker conflict rule. "No observation for
/// [`SESSION_STALE_TTL_MS`] AND unreachable" is a claim about the ledger that
/// the daemon is entitled to make, so it is stamped `Authoritative` with
/// `observed_at = now` and lands through the ordinary ordering with no change to
/// the authority model.
///
/// Retires both open beads in one pass: the TMPDIR sessions stuck at
/// `TURN_COMPLETE`, and `Notification`-driven rows stuck at attention `WAITING`
/// in the operator's default tab.
///
/// # Errors
/// Propagates a store failure from reading the snapshot or applying an event.
pub async fn reap_stale_sessions(
    pool: &SqlitePool,
    events: &EventSink,
    observed_at: i64,
) -> Result<usize, FleetRepoError> {
    let snapshot = FleetRepo::snapshot(pool).await?;
    let mut retired = 0;
    for row in snapshot.sessions {
        // Already retired: skip, so a steady state emits nothing. Without this
        // the reaper would re-assert the same terminal state every tick, which
        // is the churn class fixed in PR #554.
        if row.lifecycle_state == "EXITED" && row.attention_state == "NONE" {
            continue;
        }
        // Only unreachable rows. A row with a live tmux binding is still
        // observable, and its `last_observed_at` legitimately freezes while
        // nothing changes, because `tmux_row_matches` suppresses no-op events.
        // Reaping on age alone would kill healthy idle sessions.
        if row.tmux_target.is_some() && row.transport_health != "UNAVAILABLE" {
            continue;
        }
        // Only the CODEX manager owns a lifecycle we must not touch. Skipping
        // every `MANAGED` row would make this reaper a no-op for exactly the
        // sessions it exists to retire: a Claude hook stamps `MANAGED` to mean
        // "structured control is available" (see `apply_hook`), so every
        // hook-sourced Claude row carries it — including all 9 stranded
        // TURN_COMPLETE rows measured on a real profile. Mirrors the ownership
        // filter `recover_codex_manager` already uses.
        if row.provider == "codex" && row.management_state == "MANAGED" {
            continue;
        }
        if observed_at.saturating_sub(row.last_observed_at) < SESSION_STALE_TTL_MS {
            continue;
        }

        let event = NewFleetEvent {
            event_id: format!("stale:retired:{}:{observed_at}", row.session_key),
            session_key: row.session_key.clone(),
            observed_at,
            authority: ObservationAuthority::Authoritative,
            event_type: "session_stale".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                capabilities: Some(capabilities_for_tmux_state(&row, false)),
                lifecycle_state: Some("EXITED".to_string()),
                attention_state: Some("NONE".to_string()),
                // `Some(None)` CLEARS the stored prompt: leaving it would keep a
                // dead session advertising a request nobody can answer.
                current_request_fingerprint: Some(None),
                transport_health: Some("UNAVAILABLE".to_string()),
                ..FleetSessionPatch::default()
            },
        };
        match FleetRepo::apply_event(pool, &event).await {
            Ok(result) => {
                if !result.duplicate {
                    events.emit_fleet_revision(result.revision);
                }
                if result.applied {
                    retired += 1;
                }
            }
            Err(error) => tracing::warn!(error = %error, "fleet stale retire failed"),
        }
    }
    Ok(retired)
}

/// Mark cached tmux routes unavailable without discarding durable sessions.
pub async fn mark_tmux_unavailable(
    pool: &SqlitePool,
    events: &EventSink,
    observed_at: i64,
) -> Result<usize, FleetRepoError> {
    let snapshot = FleetRepo::snapshot(pool).await?;
    let mut changed = 0;
    for row in snapshot.sessions.into_iter().filter(|row| row.tmux_target.is_some()) {
        let event = NewFleetEvent {
            event_id: format!("tmux:unavailable:{}:{observed_at}", row.session_key),
            session_key: row.session_key.clone(),
            observed_at,
            authority: ObservationAuthority::Authoritative,
            event_type: "tmux_unavailable".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                capabilities: Some(capabilities_for_tmux_state(&row, false)),
                transport_health: Some("UNAVAILABLE".to_string()),
                ..FleetSessionPatch::default()
            },
        };
        let result = FleetRepo::apply_event(pool, &event).await?;
        if !result.duplicate {
            events.emit_fleet_revision(result.revision);
        }
        changed += usize::from(result.applied);
    }
    Ok(changed)
}

/// Returns every `session_key` whose tmux binding is LIVE by (target,
/// fingerprint), whether or not this pass emitted an event for it.
///
/// The caller must union that set into its own `discovered` set. Both halves of
/// the reconcile then agree on what "missing" means, which is the thing that was
/// broken: this loop matches by (target, fingerprint) so a provider-qualified
/// MANAGED row stays reachable, while the missing-sweep matches by `session_key`.
/// When a pane's `session_key` changes but the pane does not, the orphaned
/// predecessor row satisfies this loop and NOT the sweep's, so it flip-flopped —
/// restored to HEALTHY here, marked UNAVAILABLE there, one applied event each,
/// every tick, forever.
///
/// `0075_prune_tmux_flipflop.sql` one-time-pruned 41,200 rows of this and PR
/// #554 added the `lifecycle_state == "EXITED"` skip below, which only covers an
/// orphan that reached EXITED. An orphan stuck at RUNNING never does —
/// authoritative lifecycle has no decay — so it kept churning. Measured again on
/// 2026-08-08 at 40 events/min after the sweep-side guard alone was fixed:
/// `tmux_available` and `tmux_missing` for one session, same timestamp, every 3s.
async fn restore_tmux_transport(
    pool: &SqlitePool,
    events: &EventSink,
    discovered: &[FleetSession],
    observed_at: i64,
) -> Result<std::collections::HashSet<String>, FleetRepoError> {
    let snapshot = FleetRepo::snapshot(pool).await?;
    let mut live_bindings = std::collections::HashSet::new();

    // ONE row per pane may claim the binding: the most recently observed.
    //
    // A pane is a single thing, so at most one session row can really own it.
    // When a pane's key changes but the pane does not, BOTH the old row and the
    // new one match by (target, fingerprint), and reporting both live pinned the
    // orphan HEALTHY forever — `reap_stale_sessions` skips any row that is not
    // UNAVAILABLE, so it never reached EXITED and `archive_dead_sessions` never
    // took it either. That left a ghost in the roster advertising an attachable
    // pane that belongs to a different session. Claiming liveness for the winner
    // only lets every loser fall through to the missing-sweep and retire, which
    // is the behaviour that existed before the flip-flop fix and must survive it.
    let mut owner_of_pane: std::collections::HashMap<(String, Option<String>), (i64, String)> =
        std::collections::HashMap::new();
    for row in &snapshot.sessions {
        let Some(target) = row.tmux_target.clone() else {
            continue;
        };
        let matches_live_pane = discovered.iter().any(|session| {
            session.exact_tmux_target.as_deref() == Some(target.as_str())
                && session.process_start_fingerprint == row.process_start_fingerprint
        });
        if !matches_live_pane {
            continue;
        }
        let pane = (target, row.process_start_fingerprint.clone());
        match owner_of_pane.get(&pane) {
            Some((seen, _)) if *seen >= row.last_observed_at => {}
            _ => {
                owner_of_pane.insert(pane, (row.last_observed_at, row.session_key.clone()));
            }
        }
    }
    let owners: std::collections::HashSet<String> =
        owner_of_pane.into_values().map(|(_, key)| key).collect();

    for row in snapshot.sessions {
        let Some(target) = row.tmux_target.as_deref() else {
            continue;
        };
        let live = owners.contains(&row.session_key)
            && discovered.iter().any(|session| {
                session.exact_tmux_target.as_deref() == Some(target)
                    && session.process_start_fingerprint == row.process_start_fingerprint
            });
        // Report liveness BEFORE the emit guards below. A row can be live and
        // still not need an event (already HEALTHY, or EXITED); it is just as
        // not-missing in those cases, and reporting only the rows that emitted
        // would leave exactly the steady state uncovered.
        if live {
            live_bindings.insert(row.session_key.clone());
        }
        // An EXITED row is never restored, even when a pane still matches its
        // target and fingerprint. Matching here is by (target, fingerprint) —
        // deliberately, so a MANAGED row whose key is provider-qualified is still
        // reachable — while the missing-sweep matches by session_key. When those
        // two disagree the row flip-flops: this loop restores it to HEALTHY, the
        // sweep's skip (which needs EXITED *and* UNAVAILABLE) then misses, and it
        // is marked UNAVAILABLE again, one applied event each per tick forever.
        //
        // They disagree whenever a pane's session_key changes but the pane does
        // not — which is exactly what happens when provider detection improves,
        // since the provider is part of `SessionKey::legacy`. The old row is
        // orphaned, stays EXITED, and still matches by target+fingerprint.
        if !live || row.transport_health == "HEALTHY" || row.lifecycle_state == "EXITED" {
            continue;
        }
        let event = NewFleetEvent {
            event_id: format!("tmux:available:{}:{observed_at}", row.session_key),
            session_key: row.session_key.clone(),
            observed_at,
            authority: ObservationAuthority::Authoritative,
            event_type: "tmux_available".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                capabilities: Some(capabilities_for_tmux_state(&row, true)),
                transport_health: Some("HEALTHY".to_string()),
                ..FleetSessionPatch::default()
            },
        };
        let result = FleetRepo::apply_event(pool, &event).await?;
        if !result.duplicate {
            events.emit_fleet_revision(result.revision);
        }
    }
    Ok(live_bindings)
}

fn with_tmux_capabilities(serialized: &str, available: bool) -> String {
    let mut capabilities: ainb_hangar_proto::fleet::FleetCapabilities =
        serde_json::from_str(serialized).unwrap_or_default();
    capabilities.tmux_attach = available;
    capabilities.tmux_text = available;
    capabilities.verified_picker = available;
    serde_json::to_string(&capabilities).unwrap_or_else(|_| "{}".to_string())
}

fn capabilities_for_tmux_state(row: &FleetSessionRow, available: bool) -> String {
    let serialized = if row.provider == "codex" && row.management_state == "MANAGED" {
        with_managed_lifecycle_capabilities(&row.capabilities, available)
    } else {
        row.capabilities.clone()
    };
    with_tmux_capabilities(&serialized, available)
}

fn tmux_event(session: &FleetSession, observed_at: i64) -> NewFleetEvent {
    let payload = serde_json::to_string(session).unwrap_or_else(|_| "{}".to_string());
    NewFleetEvent {
        event_id: format!(
            "tmux:discovered:{}:{}",
            session.session_key,
            fingerprint_bytes(payload.as_bytes())
        ),
        session_key: session.session_key.to_string(),
        observed_at,
        authority: ObservationAuthority::Inferred,
        event_type: "tmux_discovered".to_string(),
        payload,
        patch: FleetSessionPatch {
            provider: Some(session.provider.as_str().to_string()),
            tmux_target: session.exact_tmux_target.clone(),
            process_start_fingerprint: session.process_start_fingerprint.clone(),
            cwd: Some(session.cwd.clone()),
            management_state: Some(management_token(session.management).to_string()),
            capabilities: Some(degraded_capabilities()),
            confidence: Some(confidence_token(session.confidence).to_string()),
            lifecycle_state: Some(state_token(session.lifecycle)),
            attention_state: Some(attention_token(session.attention)),
            transport_health: Some(transport_token(session.transport_health).to_string()),
            ..FleetSessionPatch::default()
        },
    }
}

fn parse_provider(value: &str) -> Provider {
    match value.to_ascii_lowercase().as_str() {
        "claude" => Provider::Claude,
        "codex" => Provider::Codex,
        "copilot" => Provider::Copilot,
        _ => Provider::Unknown,
    }
}

fn states_for_hook(
    event_type: &str,
    payload: &Value,
) -> (Option<LifecycleState>, Option<AttentionState>) {
    match event_type {
        "SessionStart" => (Some(LifecycleState::Starting), Some(AttentionState::None)),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" | "PostToolUseFailure"
        | "PostToolBatch" | "SubagentStart" | "TaskCreated" => {
            (Some(LifecycleState::Running), Some(AttentionState::None))
        }
        "AskUserQuestion" => (Some(LifecycleState::Idle), Some(AttentionState::Ask)),
        "PermissionRequest" => (Some(LifecycleState::Idle), Some(AttentionState::Approval)),
        "Notification" => (None, Some(AttentionState::Waiting)),
        "Stop" if has_active_background_work(payload) => {
            (Some(LifecycleState::Running), Some(AttentionState::None))
        }
        "Stop" => (
            Some(LifecycleState::TurnComplete),
            Some(AttentionState::None),
        ),
        "StopFailure" => (
            Some(LifecycleState::TurnComplete),
            Some(AttentionState::Error),
        ),
        "SessionEnd" => (Some(LifecycleState::Exited), Some(AttentionState::None)),
        _ => (None, None),
    }
}

fn canonical_hook_event_type<'a>(event_type: &'a str, payload: &Value) -> &'a str {
    if event_type == "PermissionRequest"
        && claude_hook_tool_name(payload) == Some("AskUserQuestion")
    {
        "AskUserQuestion"
    } else {
        event_type
    }
}

fn claude_hook_tool_name(payload: &Value) -> Option<&str> {
    let hook = payload.get("payload").unwrap_or(payload);
    payload
        .get("matcher")
        .or_else(|| hook.get("tool_name"))
        .or_else(|| hook.get("tool"))
        .and_then(Value::as_str)
}

fn has_active_background_work(payload: &Value) -> bool {
    let hook = payload.get("payload").unwrap_or(payload);
    [
        "background_tasks",
        "backgroundTasks",
        "session_crons",
        "sessionCrons",
    ]
    .iter()
    .filter_map(|field| hook.get(*field))
    .any(|value| {
        value.as_array().is_some_and(|items| !items.is_empty())
            || value.as_object().is_some_and(|items| !items.is_empty())
    })
}

fn managed_capabilities(provider: Provider) -> String {
    let broker = provider == Provider::Claude;
    serde_json::to_string(&ainb_hangar_proto::fleet::FleetCapabilities {
        structured_answer: broker,
        structured_dismiss: broker,
        approvals: broker,
        approval_session: false,
        send_prompt: false,
        continue_turn: false,
        retry: false,
        interrupt: false,
        start: false,
        stop: false,
        restart: false,
        kill: false,
        archive: false,
        tmux_attach: false,
        tmux_text: false,
        verified_picker: false,
    })
    .unwrap_or_else(|_| "{}".to_string())
}

fn claude_managed_capabilities(exact_tmux_identity: bool) -> String {
    let capabilities = managed_capabilities(Provider::Claude);
    if exact_tmux_identity {
        with_tmux_capabilities(
            &with_managed_lifecycle_capabilities(&capabilities, true),
            true,
        )
    } else {
        capabilities
    }
}

fn codex_managed_capabilities(capabilities: &CodexCapabilities) -> String {
    serde_json::to_string(&ainb_hangar_proto::fleet::FleetCapabilities {
        structured_answer: capabilities.request_user_input,
        structured_dismiss: false,
        approvals: capabilities.approvals,
        approval_session: capabilities.approvals,
        send_prompt: true,
        continue_turn: true,
        retry: true,
        interrupt: true,
        start: true,
        stop: false,
        restart: false,
        kill: false,
        archive: capabilities.thread_archive,
        tmux_attach: false,
        tmux_text: false,
        verified_picker: false,
    })
    .unwrap_or_else(|_| "{}".to_string())
}

fn with_managed_lifecycle_capabilities(serialized: &str, available: bool) -> String {
    let mut capabilities: ainb_hangar_proto::fleet::FleetCapabilities =
        serde_json::from_str(serialized).unwrap_or_default();
    capabilities.stop = available;
    capabilities.restart = available;
    capabilities.kill = available;
    capabilities.archive &= available;
    serde_json::to_string(&capabilities).unwrap_or_else(|_| "{}".to_string())
}

fn degraded_capabilities() -> String {
    serde_json::to_string(&ainb_hangar_proto::fleet::FleetCapabilities {
        tmux_attach: true,
        tmux_text: true,
        verified_picker: true,
        ..ainb_hangar_proto::fleet::FleetCapabilities::default()
    })
    .unwrap_or_else(|_| "{}".to_string())
}

fn fingerprint_value(value: &Value) -> String {
    let body = serde_json::to_vec(value).unwrap_or_default();
    fingerprint_bytes(&body)
}

fn claude_request_identity(payload: &Value) -> Option<Value> {
    let hook = payload.get("payload").unwrap_or(payload);
    let tool_input = hook.get("tool_input").or_else(|| hook.get("input"))?.clone();
    Some(serde_json::json!({
        "tool_use_id": hook.get("tool_use_id").cloned().unwrap_or(Value::Null),
        "tool_input": tool_input,
    }))
}

fn claude_questions(payload: &Value) -> Option<&Vec<Value>> {
    let hook = payload.get("payload").unwrap_or(payload);
    hook.get("tool_input")
        .or_else(|| hook.get("input"))?
        .get("questions")?
        .as_array()
        .filter(|questions| !questions.is_empty())
}

fn claude_permission_identity(payload: &Value) -> Option<(String, String)> {
    let hook = payload.get("payload").unwrap_or(payload);
    let tool = claude_hook_tool_name(payload).unwrap_or_default().to_string();
    let context = hook
        .get("tool_input")
        .or_else(|| hook.get("input"))
        .map(Value::to_string)
        .unwrap_or_default();
    (!tool.is_empty()).then_some((tool, context))
}

fn fingerprint_bytes(body: &[u8]) -> String {
    let hash = body.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    format!("fnv1a64:{hash:016x}")
}

fn state_token(value: LifecycleState) -> String {
    match value {
        LifecycleState::Starting => "STARTING",
        LifecycleState::Running => "RUNNING",
        LifecycleState::TurnComplete => "TURN_COMPLETE",
        LifecycleState::Idle => "IDLE",
        LifecycleState::Exited => "EXITED",
        LifecycleState::Unknown => "UNKNOWN",
    }
    .to_string()
}

fn attention_token(value: AttentionState) -> String {
    match value {
        AttentionState::None => "NONE",
        AttentionState::Ask => "ASK",
        AttentionState::Approval => "APPROVAL",
        AttentionState::Waiting => "WAITING",
        AttentionState::Error => "ERROR",
    }
    .to_string()
}

const fn management_token(value: ManagementState) -> &'static str {
    match value {
        ManagementState::Managed => "MANAGED",
        ManagementState::Degraded => "DEGRADED",
    }
}

const fn confidence_token(value: Confidence) -> &'static str {
    match value {
        Confidence::Authoritative => "HIGH",
        Confidence::Observed => "MEDIUM",
        Confidence::Inferred => "LOW",
    }
}

const fn transport_token(value: TransportHealth) -> &'static str {
    match value {
        TransportHealth::Healthy => "HEALTHY",
        TransportHealth::Degraded => "DEGRADED",
        TransportHealth::Unavailable => "UNAVAILABLE",
        TransportHealth::Unknown => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::EventBroker;

    /// Real provider ids pass through byte-for-byte; everything a provider
    /// could turn into a storage or injection channel does not.
    #[test]
    fn model_token_admits_real_ids_and_clamps_the_rest() {
        for accepted in [
            "claude-opus-5",
            "claude-sonnet-4-5",
            "gpt-5.6-terra",
            "high",
            "xhigh",
            "o3_mini",
            "anthropic/claude-opus-5",
            "model:v1.2+build",
            "a".repeat(MODEL_TOKEN_MAX).as_str(),
        ] {
            assert_eq!(
                model_token(accepted).as_deref(),
                Some(accepted),
                "{accepted} is a real provider token and must survive VERBATIM"
            );
        }

        for rejected in [
            "",
            "<synthetic>",
            "claude opus 5",
            "opus\n5",
            "opus\u{0}5",
            "\u{1b}[31mopus",
            "модель",
            "claude-opus-5; DROP TABLE fleet_session",
            "a".repeat(MODEL_TOKEN_MAX + 1).as_str(),
        ] {
            assert_eq!(
                model_token(rejected),
                None,
                "{rejected:?} must clamp to never-observed, not reach the roster"
            );
        }
    }

    /// The session under test for every model/effort capture case below.
    const MODEL_SESSION_KEY: &str = "claude:sess-model";

    /// One hook observation carrying `payload`, all on [`MODEL_SESSION_KEY`]'s
    /// session so successive events exercise the model state group.
    fn model_hook<'a>(
        provider: &'a str,
        event_id: &str,
        payload: &'a Value,
        observed_at: i64,
    ) -> HookObservation<'a> {
        HookObservation {
            event_id: event_id.to_string(),
            provider,
            provider_session_id: "sess-model",
            cwd: "/repo",
            event_type: "PreToolUse",
            payload,
            observed_at,
            transcript_model: None,
        }
    }

    /// The `(model, reasoning_effort)` pair currently on `key`'s row.
    async fn model_pair(pool: &SqlitePool, key: &str) -> (Option<String>, Option<String>) {
        let session = FleetRepo::get_session(pool, key)
            .await
            .expect("session query")
            .expect("session exists");
        (session.model, session.reasoning_effort)
    }

    /// THE load-bearing gate.
    ///
    /// Claude reports every subagent's hook under the SAME `session_id` as the
    /// main thread, carrying the SUBAGENT's own effort. Ungated, the roster shows
    /// whatever `Task()` ran last — changing several times a minute, always a
    /// plausible value, with nothing red in CI and nothing odd in the logs.
    ///
    /// The main thread omits `agent_type` entirely; the literal `"MAIN"` the
    /// original plan expected was never observed live, so this fixture is built
    /// on absence.
    #[tokio::test]
    async fn subagent_hook_effort_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();

        let main = serde_json::json!({ "payload": { "effort": { "level": "high" } } });
        apply_hook(
            store.pool(),
            &sink,
            model_hook("claude", "e-main", &main, 1),
        )
        .await
        .expect("main-thread hook applies");
        assert_eq!(
            model_pair(store.pool(), MODEL_SESSION_KEY).await.1.as_deref(),
            Some("high"),
            "the main thread's own effort must reach the row"
        );

        // A subagent dispatched from that same session, running at a different
        // effort. Its hook is indistinguishable from the main thread's except for
        // `agent_type`.
        let subagent = serde_json::json!({
            "payload": { "agent_type": "superstar-engineer", "effort": { "level": "xhigh" } }
        });
        apply_hook(
            store.pool(),
            &sink,
            model_hook("claude", "e-subagent", &subagent, 2),
        )
        .await
        .expect("subagent hook applies");
        assert_eq!(
            model_pair(store.pool(), MODEL_SESSION_KEY).await.1.as_deref(),
            Some("high"),
            "a subagent's effort must never overwrite the session's own"
        );
    }

    /// `agent_type: ""` is NOT the main thread, and treating it as one is the
    /// obvious "helpful" mistake. Measured live: 80 of the 81 empty-string events
    /// in a 3000-event window were `SubagentStop` carrying an
    /// `agent_transcript_path`.
    #[tokio::test]
    async fn empty_agent_type_is_treated_as_subagent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();

        let main = serde_json::json!({ "payload": { "effort": { "level": "high" } } });
        apply_hook(
            store.pool(),
            &sink,
            model_hook("claude", "e-main", &main, 1),
        )
        .await
        .expect("main-thread hook applies");
        assert_eq!(
            model_pair(store.pool(), MODEL_SESSION_KEY).await.1.as_deref(),
            Some("high"),
            "the main thread's own effort must reach the row"
        );

        let empty_agent = serde_json::json!({
            "payload": {
                "agent_type": "",
                "agent_transcript_path": "/tmp/subagent.jsonl",
                "effort": { "level": "xhigh" }
            }
        });
        apply_hook(
            store.pool(),
            &sink,
            model_hook("claude", "e-empty", &empty_agent, 2),
        )
        .await
        .expect("empty-agent hook applies");
        assert_eq!(
            model_pair(store.pool(), MODEL_SESSION_KEY).await.1.as_deref(),
            Some("high"),
            "an empty agent_type is a subagent, not the main thread"
        );
    }

    /// Claude hooks carry the effort and no model. Measured live: 3390 hits on
    /// `/payload/effort/level` in a 6000-event window, against one stray model.
    #[tokio::test]
    async fn claude_hook_effort_lands_on_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();

        let payload = serde_json::json!({ "payload": { "effort": { "level": "high" } } });
        apply_hook(
            store.pool(),
            &sink,
            model_hook("claude", "e-effort", &payload, 1),
        )
        .await
        .expect("hook applies");

        assert_eq!(
            model_pair(store.pool(), MODEL_SESSION_KEY).await,
            (None, Some("high".to_string())),
            "the Claude hook establishes the effort and says nothing about the model"
        );
    }

    /// Codex hooks carry the model and no effort — the mirror image of Claude,
    /// which is why one provider-blind read serves both. Measured live: 42 of 42
    /// Codex hook payloads carried `/payload/model`.
    #[tokio::test]
    async fn codex_hook_model_lands_on_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();

        let payload = serde_json::json!({ "payload": { "model": "gpt-5.6-terra" } });
        apply_hook(
            store.pool(),
            &sink,
            model_hook("codex", "e-model", &payload, 1),
        )
        .await
        .expect("hook applies");

        assert_eq!(
            model_pair(store.pool(), "codex:sess-model").await,
            (Some("gpt-5.6-terra".to_string()), None),
            "the Codex hook establishes the model and says nothing about the effort"
        );
    }

    /// The hook describes THIS event; the transcript tail describes the newest
    /// record on disk, which may be a turn older. So the hook wins per field —
    /// but only when the clamp accepts it, or Claude's `claude-opus-5[1m]` would
    /// suppress the transcript's `claude-opus-5` and leave the model blank.
    #[tokio::test]
    async fn hook_value_beats_transcript_value() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();

        let payload = serde_json::json!({
            "payload": { "model": "gpt-5.6-terra", "effort": { "level": "high" } }
        });
        let mut observation = model_hook("codex", "e-both", &payload, 1);
        observation.transcript_model = Some(ModelInfo {
            model: Some("gpt-5.5-stale".to_string()),
            effort: Some("low".to_string()),
        });
        apply_hook(store.pool(), &sink, observation).await.expect("hook applies");

        assert_eq!(
            model_pair(store.pool(), "codex:sess-model").await,
            (Some("gpt-5.6-terra".to_string()), Some("high".to_string())),
            "the fresher hook value must win both fields"
        );
    }

    /// Pins the `claude-opus-5[1m]` decision end to end, not just in the clamp:
    /// the two producers spell one model differently, the clamp strips the
    /// context-window marker off the hook's spelling, and the row carries the
    /// single identity both of them agree on.
    #[tokio::test]
    async fn model_token_handles_context_window_suffix() {
        assert_eq!(
            model_token("claude-opus-5[1m]").as_deref(),
            Some("claude-opus-5"),
            "the context-window suffix is not part of the model identity"
        );

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();

        let payload = serde_json::json!({ "payload": { "model": "claude-opus-5[1m]" } });
        let mut observation = model_hook("claude", "e-suffix", &payload, 1);
        observation.transcript_model = Some(ModelInfo {
            model: Some("claude-opus-5".to_string()),
            effort: None,
        });
        apply_hook(store.pool(), &sink, observation).await.expect("hook applies");

        assert_eq!(
            model_pair(store.pool(), MODEL_SESSION_KEY).await.0.as_deref(),
            Some("claude-opus-5"),
            "both producers must land the same identity, whichever one wins"
        );
    }

    /// The hook spelling and the transcript spelling of ONE model must produce
    /// ONE token.
    ///
    /// Measured on the live host: Claude hooks report `claude-opus-5[1m]` while
    /// the transcript writes `claude-opus-5`. Two producers feed the same state
    /// group, so if the bracket survived, a row's model would flip between two
    /// spellings of the same thing depending on which producer observed it last
    /// — a churn that looks exactly like a real `/model` switch and would mint a
    /// revision every time.
    #[test]
    fn model_token_strips_context_window_suffix() {
        assert_eq!(
            model_token("claude-opus-5[1m]").as_deref(),
            Some("claude-opus-5"),
            "the context-window marker is not part of model identity"
        );
        assert_eq!(
            model_token("claude-opus-5[1m]"),
            model_token("claude-opus-5"),
            "both producers of one model must converge on one token"
        );
        assert_eq!(
            model_token("claude-sonnet-5[200k]").as_deref(),
            Some("claude-sonnet-5")
        );

        // Stripping must not become a way to smuggle a token past the charset:
        // anything left over is still validated, and a bracket that survives
        // the strip fails closed.
        assert_eq!(model_token("[1m]"), None, "the marker alone is not a model");
        assert_eq!(model_token("claude opus 5[1m]"), None);
        assert_eq!(model_token("a[b][c]"), None);
        assert_eq!(model_token("<synthetic>[1m]"), None);
        assert_eq!(
            model_token(&format!("{}[1m]", "a".repeat(MODEL_TOKEN_MAX))),
            None,
            "the byte bound is measured on what the provider SENT"
        );
    }

    /// `session_wire` is the ONE Rust literal of the wire session type, and it
    /// is reached from `subscription_snapshot_wire`, which serves both
    /// `fleet/snapshot` and the subscribe bootstrap. A column the store now
    /// carries but this literal omits is invisible to every client, with
    /// nothing red anywhere else.
    #[tokio::test]
    async fn session_wire_carries_model_pair() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();

        for (event_id, key, patch) in [
            (
                "e-observed",
                "claude:with-model",
                FleetSessionPatch {
                    provider: Some("claude".to_string()),
                    model: Some("claude-opus-5".to_string()),
                    reasoning_effort: Some("high".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
            (
                "e-unobserved",
                "claude:without-model",
                FleetSessionPatch {
                    provider: Some("claude".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        ] {
            FleetRepo::apply_event(
                pool,
                &NewFleetEvent {
                    event_id: event_id.to_string(),
                    session_key: key.to_string(),
                    observed_at: 1_700,
                    authority: ObservationAuthority::Authoritative,
                    event_type: "observation".to_string(),
                    payload: "{}".to_string(),
                    patch,
                },
            )
            .await
            .expect("event applies");
        }

        let projection = subscription_wire(pool, 0, 100).await.expect("projection");
        let snapshot = subscription_snapshot_wire(&projection);
        let session = |key: &str| {
            snapshot
                .sessions
                .iter()
                .find(|s| s.session_key == key)
                .expect("session reaches the wire")
                .clone()
        };

        let observed = session("claude:with-model");
        assert_eq!(observed.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(observed.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(observed.model_updated_at, 1_700);

        // Absence stays absence all the way to the wire: no placeholder, no
        // synthesised default, and a zero clock the client can read as
        // "never observed".
        let unobserved = session("claude:without-model");
        assert_eq!(unobserved.model, None);
        assert_eq!(unobserved.reasoning_effort, None);
        assert_eq!(unobserved.model_updated_at, 0);
    }

    /// The reaper retires what nothing can observe, and then goes quiet.
    ///
    /// Covers both stranded shapes in one pass: a `Notification` row parked at
    /// attention WAITING (which only a later hook could clear, so an abandoned
    /// session sat in the operator's DEFAULT tab forever) and, by the same
    /// route, a hook-sourced row with no tmux binding stuck at TURN_COMPLETE.
    ///
    /// The second reap MUST apply nothing. Re-asserting a terminal state every
    /// tick is exactly the churn class fixed in PR #554.
    #[tokio::test]
    async fn stale_unreachable_sessions_are_retired_once_and_then_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let pool = store.pool();

        // A hook-sourced session with no tmux binding, left WAITING at t=0.
        apply_hook(
            pool,
            &sink,
            HookObservation {
                event_id: "hook-abandoned".to_string(),
                provider: "claude",
                provider_session_id: "sess-abandoned",
                event_type: "Notification",
                cwd: "/tmp/ephemeral",
                payload: &serde_json::json!({}),
                observed_at: 0,
                transcript_model: None,
            },
        )
        .await
        .expect("hook applies");

        let key = FleetRepo::snapshot(pool)
            .await
            .expect("snapshot")
            .sessions
            .first()
            .expect("one session")
            .session_key
            .clone();
        let before = FleetRepo::get_session(pool, &key).await.unwrap().unwrap();
        assert_eq!(
            before.attention_state, "WAITING",
            "seeded in the stuck shape"
        );

        // Too soon: nothing is retired inside the TTL.
        assert_eq!(
            reap_stale_sessions(pool, &sink, SESSION_STALE_TTL_MS - 1).await.expect("reap"),
            0,
            "a session must not be retired before the TTL elapses"
        );

        let retired =
            reap_stale_sessions(pool, &sink, SESSION_STALE_TTL_MS + 1).await.expect("reap");
        assert_eq!(retired, 1, "the stranded session must be retired");

        let after = FleetRepo::get_session(pool, &key).await.unwrap().unwrap();
        assert_eq!(after.lifecycle_state, "EXITED");
        assert_eq!(
            after.attention_state, "NONE",
            "attention must clear or the row stays pinned in Needs-input"
        );
        assert_eq!(after.transport_health, "UNAVAILABLE");

        // Idempotent: a retired row produces no further events, forever.
        let version = after.version;
        assert_eq!(
            reap_stale_sessions(pool, &sink, SESSION_STALE_TTL_MS + 2).await.expect("reap"),
            0,
            "a retired row must never be re-asserted"
        );
        assert_eq!(
            FleetRepo::get_session(pool, &key).await.unwrap().unwrap().version,
            version,
            "a second reap must not touch the row"
        );
    }

    /// A reachable pane is never reaped, however quiet it is.
    ///
    /// `last_observed_at` legitimately freezes on a healthy idle session because
    /// `tmux_row_matches` suppresses no-op events, so age alone must not retire.
    #[tokio::test]
    async fn a_reachable_session_is_never_reaped_on_age_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let pool = store.pool();

        let target = "tmux_quiet:1.1";
        let fingerprint = "pane=%7;pid=77;session_started=1700000000";
        FleetRepo::apply_event(
            pool,
            &NewFleetEvent {
                event_id: "seed:reachable".to_string(),
                session_key: "legacy:claude:quiet".to_string(),
                observed_at: 0,
                authority: ObservationAuthority::Inferred,
                event_type: "tmux_discovered".to_string(),
                payload: "{}".to_string(),
                patch: FleetSessionPatch {
                    tmux_target: Some(target.to_string()),
                    process_start_fingerprint: Some(fingerprint.to_string()),
                    lifecycle_state: Some("IDLE".to_string()),
                    transport_health: Some("HEALTHY".to_string()),
                    ..FleetSessionPatch::default()
                },
            },
        )
        .await
        .expect("seed reachable row");

        assert_eq!(
            reap_stale_sessions(pool, &sink, SESSION_STALE_TTL_MS * 10).await.expect("reap"),
            0,
            "a HEALTHY pane must survive any amount of silence"
        );
        assert_eq!(
            FleetRepo::get_session(pool, "legacy:claude:quiet")
                .await
                .unwrap()
                .unwrap()
                .lifecycle_state,
            "IDLE"
        );
    }

    /// An orphaned row must not flip-flop with the missing-sweep, forever.
    ///
    /// `restore_tmux_transport` matches by (target, fingerprint) so a MANAGED
    /// row with a provider-qualified key stays reachable; the missing-sweep
    /// matches by `session_key`. When a pane's key changes but the pane does
    /// not — which is what happens when provider detection improves, since the
    /// provider is part of `SessionKey::legacy` — the old row is orphaned yet
    /// still matches by target+fingerprint. Restore then set it HEALTHY, the
    /// sweep's skip (which needs EXITED *and* UNAVAILABLE) missed, and it was
    /// marked UNAVAILABLE again: two applied events per row per tick, forever.
    /// Observed live at ~57,600 rows/day for two panes before this guard.
    #[tokio::test]
    async fn an_exited_orphan_row_stops_churning_against_a_live_pane() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let pool = store.pool();

        // A pane that IS live, and an EXITED orphan row pointing at the very
        // same target+fingerprint (the shape a provider re-detection leaves).
        let target = "tmux_demo:1.1";
        let fingerprint = "pane=%1;pid=101;session_started=1700000000";
        let discovered = vec![FleetSession {
            session_key: SessionKey::legacy(Provider::Claude, target, fingerprint),
            provider: Provider::Claude,
            provider_session_id: None,
            cwd: "/repo".to_string(),
            exact_tmux_target: Some(target.to_string()),
            pane_pid: Some(101),
            process_start_fingerprint: Some(fingerprint.to_string()),
            lifecycle: LifecycleState::Running,
            attention: AttentionState::None,
            management: ManagementState::Degraded,
            capabilities: ainb_fleet_core::types::Capabilities::degraded_tmux(),
            provenance: std::collections::BTreeSet::from([
                ainb_fleet_core::types::Provenance::Tmux,
            ]),
            confidence: ainb_fleet_core::types::Confidence::Inferred,
            transport_health: ainb_fleet_core::types::TransportHealth::Healthy,
            first_seen_ms: Some(1_700_000_000_000),
            last_seen_ms: None,
            version: 0,
        }];

        let orphan_key = SessionKey::legacy(Provider::Unknown, target, fingerprint).to_string();
        FleetRepo::apply_event(
            pool,
            &NewFleetEvent {
                event_id: "seed:orphan".to_string(),
                session_key: orphan_key.clone(),
                observed_at: 1,
                authority: ObservationAuthority::Authoritative,
                event_type: "seed".to_string(),
                payload: "{}".to_string(),
                patch: FleetSessionPatch {
                    tmux_target: Some(target.to_string()),
                    process_start_fingerprint: Some(fingerprint.to_string()),
                    lifecycle_state: Some("EXITED".to_string()),
                    transport_health: Some("UNAVAILABLE".to_string()),
                    ..FleetSessionPatch::default()
                },
            },
        )
        .await
        .expect("seed orphan");

        let before = FleetRepo::get_session(pool, &orphan_key)
            .await
            .expect("get")
            .expect("orphan present")
            .version;

        // Two passes of the restore half, exactly as the 3s loop runs it.
        for tick in 0..2 {
            restore_tmux_transport(pool, &sink, &discovered, 1_000 + tick)
                .await
                .expect("restore");
        }

        let after = FleetRepo::get_session(pool, &orphan_key)
            .await
            .expect("get")
            .expect("orphan present");
        assert_eq!(
            after.version, before,
            "an EXITED orphan must not be restored, or it flip-flops with the sweep forever"
        );
        assert_eq!(
            after.transport_health, "UNAVAILABLE",
            "the orphan must stay UNAVAILABLE so the missing-sweep keeps skipping it"
        );
    }

    /// The orphan that the EXITED skip above does NOT cover.
    ///
    /// Authoritative lifecycle has no decay, so an orphan whose last hook said
    /// RUNNING never reaches EXITED and the skip never fires. Restore marked it
    /// HEALTHY, the sweep saw a key it had not discovered and marked it
    /// UNAVAILABLE, forever. Measured live on 2026-08-08 at 40 events/min with
    /// `tmux_available` and `tmux_missing` carrying the SAME timestamp.
    ///
    /// The fix is agreement, not another skip: restore reports every live
    /// binding by (target, fingerprint) and the sweep unions that into its
    /// `discovered` set, so both halves decide "missing" the same way.
    #[tokio::test]
    async fn a_running_orphan_is_reported_live_so_the_sweep_skips_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let pool = store.pool();

        let target = "tmux_demo:2.2";
        let fingerprint = "pane=%2;pid=202;session_started=1700000001";
        let discovered = vec![FleetSession {
            session_key: SessionKey::legacy(Provider::Claude, target, fingerprint),
            provider: Provider::Claude,
            provider_session_id: None,
            cwd: "/repo".to_string(),
            exact_tmux_target: Some(target.to_string()),
            pane_pid: Some(202),
            process_start_fingerprint: Some(fingerprint.to_string()),
            lifecycle: LifecycleState::Running,
            attention: AttentionState::None,
            management: ManagementState::Degraded,
            capabilities: ainb_fleet_core::types::Capabilities::degraded_tmux(),
            provenance: std::collections::BTreeSet::from([
                ainb_fleet_core::types::Provenance::Tmux,
            ]),
            confidence: ainb_fleet_core::types::Confidence::Inferred,
            transport_health: ainb_fleet_core::types::TransportHealth::Healthy,
            first_seen_ms: Some(1_700_000_001_000),
            last_seen_ms: None,
            version: 0,
        }];

        // Same pane, different key, and RUNNING rather than EXITED — the shape
        // the previous guard could never retire.
        let orphan_key = SessionKey::legacy(Provider::Unknown, target, fingerprint).to_string();
        FleetRepo::apply_event(
            pool,
            &NewFleetEvent {
                event_id: "seed:running-orphan".to_string(),
                session_key: orphan_key.clone(),
                observed_at: 1,
                authority: ObservationAuthority::Authoritative,
                event_type: "seed".to_string(),
                payload: "{}".to_string(),
                patch: FleetSessionPatch {
                    tmux_target: Some(target.to_string()),
                    process_start_fingerprint: Some(fingerprint.to_string()),
                    lifecycle_state: Some("RUNNING".to_string()),
                    transport_health: Some("UNAVAILABLE".to_string()),
                    ..FleetSessionPatch::default()
                },
            },
        )
        .await
        .expect("seed running orphan");

        let live = restore_tmux_transport(pool, &sink, &discovered, 1_000).await.expect("restore");

        assert!(
            live.contains(&orphan_key),
            "a live (target, fingerprint) binding must be reported so the sweep skips it"
        );

        // The sweep's own predicate, fed the unioned set, must now stay quiet.
        let row = FleetRepo::get_session(pool, &orphan_key)
            .await
            .expect("get")
            .expect("orphan present");
        assert!(
            !needs_tmux_missing_event(&row, &live),
            "reporting the binding is what stops the 40/min flip-flop"
        );
    }

    /// Only ONE row per pane may claim the binding, or the loser never retires.
    ///
    /// Both the orphan and its successor match the same pane by (target,
    /// fingerprint). Reporting both live pinned the orphan HEALTHY forever:
    /// `reap_stale_sessions` skips anything not UNAVAILABLE, so it never reached
    /// EXITED and archiving never took it, leaving a ghost row advertising a pane
    /// that belongs to someone else.
    #[tokio::test]
    async fn only_the_newest_row_for_a_pane_claims_the_live_binding() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let pool = store.pool();

        let target = "tmux_demo:3.3";
        let fingerprint = "pane=%3;pid=303;session_started=1700000002";
        let discovered = vec![FleetSession {
            session_key: SessionKey::legacy(Provider::Claude, target, fingerprint),
            provider: Provider::Claude,
            provider_session_id: None,
            cwd: "/repo".to_string(),
            exact_tmux_target: Some(target.to_string()),
            pane_pid: Some(303),
            process_start_fingerprint: Some(fingerprint.to_string()),
            lifecycle: LifecycleState::Running,
            attention: AttentionState::None,
            management: ManagementState::Degraded,
            capabilities: ainb_fleet_core::types::Capabilities::degraded_tmux(),
            provenance: std::collections::BTreeSet::from([
                ainb_fleet_core::types::Provenance::Tmux,
            ]),
            confidence: ainb_fleet_core::types::Confidence::Inferred,
            transport_health: ainb_fleet_core::types::TransportHealth::Healthy,
            first_seen_ms: Some(1_700_000_002_000),
            last_seen_ms: None,
            version: 0,
        }];

        // Two rows, same pane. The successor was observed later. The orphan is
        // seeded HEALTHY because that is the state the bug left it pinned in.
        for (key, seen, health) in [
            ("codex:pane-orphan", 100, "HEALTHY"),
            ("codex:pane-owner", 900, "UNAVAILABLE"),
        ] {
            FleetRepo::apply_event(
                pool,
                &NewFleetEvent {
                    event_id: format!("seed:{key}"),
                    session_key: key.to_string(),
                    observed_at: seen,
                    authority: ObservationAuthority::Authoritative,
                    event_type: "seed".to_string(),
                    payload: "{}".to_string(),
                    patch: FleetSessionPatch {
                        tmux_target: Some(target.to_string()),
                        process_start_fingerprint: Some(fingerprint.to_string()),
                        lifecycle_state: Some("RUNNING".to_string()),
                        transport_health: Some(health.to_string()),
                        ..FleetSessionPatch::default()
                    },
                },
            )
            .await
            .expect("seed");
        }

        let live = restore_tmux_transport(pool, &sink, &discovered, 1_000).await.expect("restore");

        assert!(
            live.contains("codex:pane-owner"),
            "the newest row owns the pane"
        );
        assert!(
            !live.contains("codex:pane-orphan"),
            "the older row must NOT claim the pane, or it can never be retired"
        );

        let orphan = FleetRepo::get_session(pool, "codex:pane-orphan")
            .await
            .expect("get")
            .expect("orphan present");
        assert!(
            needs_tmux_missing_event(&orphan, &live),
            "the loser must fall through to the missing-sweep and retire"
        );
    }

    #[test]
    fn stop_with_background_work_remains_running() {
        let (lifecycle, attention) = states_for_hook(
            "Stop",
            &serde_json::json!({ "payload": { "background_tasks": [{ "id": "task-1" }] } }),
        );
        assert_eq!(lifecycle, Some(LifecycleState::Running));
        assert_eq!(attention, Some(AttentionState::None));
    }

    #[test]
    fn subagent_stop_never_completes_parent_turn() {
        let (lifecycle, attention) = states_for_hook("SubagentStop", &serde_json::json!({}));
        assert_eq!(lifecycle, None);
        assert_eq!(attention, None);
    }
    use ainb_hangar_store::Store;

    #[test]
    fn lifecycle_and_attention_are_independent() {
        assert_eq!(
            states_for_hook("AskUserQuestion", &serde_json::json!({})),
            (Some(LifecycleState::Idle), Some(AttentionState::Ask))
        );
        assert_eq!(
            states_for_hook("PermissionRequest", &serde_json::json!({})),
            (Some(LifecycleState::Idle), Some(AttentionState::Approval))
        );
        assert_eq!(
            states_for_hook("Stop", &serde_json::json!({})),
            (
                Some(LifecycleState::TurnComplete),
                Some(AttentionState::None)
            )
        );
        assert_eq!(
            states_for_hook("SessionEnd", &serde_json::json!({})),
            (Some(LifecycleState::Exited), Some(AttentionState::None))
        );
    }

    #[tokio::test]
    async fn claude_interview_stays_answerable_through_permission_notification() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let question = serde_json::json!({
            "questions": [{
                "question": "When?",
                "options": [{"label": "August"}]
            }]
        });
        let ask = serde_json::json!({ "payload": { "tool_input": question.clone() } });

        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "ask".into(),
                provider: "claude",
                provider_session_id: "session-1",
                cwd: "/repo",
                event_type: "AskUserQuestion",
                payload: &ask,
                observed_at: 1,
                transcript_model: None,
            },
        )
        .await
        .unwrap();
        let expected = FleetRepo::get_session(store.pool(), "claude:session-1")
            .await
            .unwrap()
            .unwrap()
            .current_request_fingerprint;

        let permission = serde_json::json!({
            "matcher": "AskUserQuestion",
            "payload": {
                "hook_event_name": "PermissionRequest",
                "tool_name": "AskUserQuestion"
            }
        });
        let notification = serde_json::json!({
            "payload": { "notification_type": "permission_prompt" }
        });
        for (event_id, event_type, payload, observed_at) in [
            ("permission", "AskUserQuestion", &permission, 2),
            ("notification", "Notification", &notification, 3),
        ] {
            apply_hook(
                store.pool(),
                &sink,
                HookObservation {
                    event_id: event_id.into(),
                    provider: "claude",
                    provider_session_id: "session-1",
                    cwd: "/repo",
                    event_type,
                    payload,
                    observed_at,
                    transcript_model: None,
                },
            )
            .await
            .unwrap();
        }

        let session =
            FleetRepo::get_session(store.pool(), "claude:session-1").await.unwrap().unwrap();
        assert_eq!(session.attention_state, "ASK");
        assert_eq!(session.current_request_fingerprint, expected);
        let current = current_request_wire(store.pool(), "claude:session-1")
            .await
            .unwrap()
            .expect("the active Ask payload survives duplicate PermissionRequest");
        assert_eq!(
            current["payload"]["tool_input"]["questions"][0]["question"],
            "When?"
        );
    }

    #[tokio::test]
    async fn reproject_claude_interview_recovers_old_waiting_projection() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let broker = EventBroker::new();
        let sink = broker.sink();
        let question = serde_json::json!({
            "questions": [{
                "question": "When?",
                "options": [{"label": "August"}]
            }]
        });
        let ask = serde_json::json!({ "payload": { "tool_input": question.clone() } });
        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "ask".into(),
                provider: "claude",
                provider_session_id: "session-1",
                cwd: "/repo",
                event_type: "AskUserQuestion",
                payload: &ask,
                observed_at: 1,
                transcript_model: None,
            },
        )
        .await
        .unwrap();

        let permission = serde_json::json!({
            "matcher": "AskUserQuestion",
            "payload": { "tool_input": question.clone() }
        });
        for (event_id, event_type, payload, patch, observed_at) in [
            (
                "old-permission",
                "PermissionRequest",
                permission,
                FleetSessionPatch {
                    attention_state: Some("APPROVAL".to_string()),
                    current_request_fingerprint: Some(Some("fnv1a64:old".to_string())),
                    ..FleetSessionPatch::default()
                },
                2,
            ),
            (
                "old-notification",
                "Notification",
                serde_json::json!({ "payload": { "notification_type": "permission_prompt" } }),
                FleetSessionPatch {
                    attention_state: Some("WAITING".to_string()),
                    ..FleetSessionPatch::default()
                },
                3,
            ),
        ] {
            FleetRepo::apply_event(
                store.pool(),
                &NewFleetEvent {
                    event_id: event_id.to_string(),
                    session_key: "claude:session-1".to_string(),
                    observed_at,
                    authority: ObservationAuthority::Authoritative,
                    event_type: event_type.to_string(),
                    payload: serde_json::to_string(&payload).unwrap(),
                    patch,
                },
            )
            .await
            .unwrap();
        }
        let stale =
            FleetRepo::get_session(store.pool(), "claude:session-1").await.unwrap().unwrap();
        assert_eq!(stale.attention_state, "WAITING");
        let mut revisions = broker.subscribe_fleet();

        let result =
            reproject_claude_interview(store.pool(), &sink, "claude:session-1", stale.version, 4)
                .await
                .unwrap();
        assert!(!result.duplicate);
        assert_eq!(revisions.recv().await.unwrap(), result.revision);
        let projection = FleetRepo::subscription_projection(store.pool(), 0, 100).await.unwrap();
        let restored = &projection.sessions[0];
        assert_eq!(restored.session.attention_state, "ASK");
        assert_eq!(
            restored.current_request.as_ref().unwrap()["payload"]["tool_input"]["questions"][0]["question"],
            "When?"
        );
    }

    #[test]
    fn codex_provider_blocked_request_is_idle_with_approval_attention() {
        use crate::fleet_provider::codex::{
            CodexApprovalRequest, CodexCapabilities, CodexInbound, CodexItemRequestIdentity,
            RpcRequestId,
        };
        let capabilities = CodexCapabilities {
            cli_version: "test".to_string(),
            daemon_version: None,
            app_server: true,
            stdio_proxy: true,
            request_user_input: true,
            approvals: true,
            thread_archive: true,
        };
        let event = normalize_codex_inbound(
            "sequence".to_string(),
            CodexInbound::Approval(CodexApprovalRequest {
                identity: CodexItemRequestIdentity {
                    request_id: RpcRequestId::new(serde_json::json!(7)).unwrap(),
                    thread_id: "thread-1".to_string(),
                    turn_id: "turn-1".to_string(),
                    item_id: "item-1".to_string(),
                },
                kind: CodexApprovalKind::CommandExecution,
                params: serde_json::json!({}),
            }),
            &capabilities,
            100,
        )
        .unwrap();
        assert_eq!(event.patch.lifecycle_state.as_deref(), Some("IDLE"));
        assert_eq!(event.patch.attention_state.as_deref(), Some("APPROVAL"));
    }

    #[test]
    fn managed_identity_does_not_include_cwd() {
        let a = SessionKey::managed(Provider::Claude, "session-1");
        let b = SessionKey::managed(Provider::Claude, "session-1");
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn same_cwd_providers_stay_distinct_and_states_reduce_independently() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let payload = serde_json::json!({"questions": [{"id": "q1"}]});

        for (provider, session, event_id) in [
            ("claude", "session-c", "hook-c"),
            ("codex", "thread-x", "hook-x"),
        ] {
            apply_hook(
                store.pool(),
                &sink,
                HookObservation {
                    event_id: event_id.to_string(),
                    provider,
                    provider_session_id: session,
                    cwd: "/same/repo",
                    event_type: "AskUserQuestion",
                    payload: &payload,
                    observed_at: 100,
                    transcript_model: None,
                },
            )
            .await
            .unwrap();
        }

        let snapshot = FleetRepo::snapshot(store.pool()).await.unwrap();
        assert_eq!(snapshot.sessions.len(), 2);
        assert_ne!(
            snapshot.sessions[0].session_key,
            snapshot.sessions[1].session_key
        );
        assert!(snapshot.sessions.iter().all(|row| row.attention_state == "ASK"));

        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "hook-c-stop".to_string(),
                provider: "claude",
                provider_session_id: "session-c",
                cwd: "/same/repo",
                event_type: "Stop",
                payload: &serde_json::json!({}),
                observed_at: 200,
                transcript_model: None,
            },
        )
        .await
        .unwrap();

        let claude =
            FleetRepo::get_session(store.pool(), "claude:session-c").await.unwrap().unwrap();
        let codex = FleetRepo::get_session(store.pool(), "codex:thread-x").await.unwrap().unwrap();
        assert_eq!(claude.lifecycle_state, "TURN_COMPLETE");
        assert_eq!(claude.attention_state, "NONE");
        assert_eq!(codex.lifecycle_state, "IDLE");
        assert_eq!(codex.attention_state, "ASK");
        assert_eq!(codex.management_state, "DEGRADED");
    }

    #[tokio::test]
    async fn child_work_keeps_parent_running_and_replay_safe() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let start = serde_json::json!({ "payload": { "agent_id": "agent-1" } });
        let stop = serde_json::json!({ "payload": { "agent_id": "agent-1" } });

        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "subagent-start-1".to_string(),
                provider: "claude",
                provider_session_id: "session-1",
                cwd: "/repo",
                event_type: "SubagentStart",
                payload: &start,
                observed_at: 100,
                transcript_model: None,
            },
        )
        .await
        .unwrap();
        let running =
            FleetRepo::get_session(store.pool(), "claude:session-1").await.unwrap().unwrap();
        assert_eq!(running.lifecycle_state, "RUNNING");
        assert_eq!(running.active_work_count, 1);

        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "subagent-stop-1".to_string(),
                provider: "claude",
                provider_session_id: "session-1",
                cwd: "/repo",
                event_type: "SubagentStop",
                payload: &stop,
                observed_at: 200,
                transcript_model: None,
            },
        )
        .await
        .unwrap();
        let stopped =
            FleetRepo::get_session(store.pool(), "claude:session-1").await.unwrap().unwrap();
        assert_eq!(stopped.lifecycle_state, "RUNNING");
        assert_eq!(stopped.active_work_count, 0);

        let replay = apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "subagent-stop-1".to_string(),
                provider: "claude",
                provider_session_id: "session-1",
                cwd: "/repo",
                event_type: "SubagentStop",
                payload: &stop,
                observed_at: 200,
                transcript_model: None,
            },
        )
        .await
        .unwrap();
        assert!(replay.duplicate);
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM fleet_work_item WHERE session_key = ?")
                .bind("claude:session-1")
                .fetch_one(store.pool())
                .await
                .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn tmux_absence_preserves_authoritative_degraded_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let payload = serde_json::json!({
            "tmux_target": "codex-a:0.0",
            "process_start_fingerprint": "fp-a"
        });
        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "codex-running".to_string(),
                provider: "codex",
                provider_session_id: "thread-a",
                cwd: "/repo",
                event_type: "UserPromptSubmit",
                payload: &payload,
                observed_at: 100,
                transcript_model: None,
            },
        )
        .await
        .unwrap();

        let row = FleetRepo::get_session(store.pool(), "codex:thread-a").await.unwrap().unwrap();
        assert_eq!(row.management_state, "DEGRADED");
        assert_eq!(row.lifecycle_state, "RUNNING");
        assert_eq!(row.lifecycle_authority, "authoritative");

        FleetRepo::apply_event(store.pool(), &tmux_missing_event(&row, 200))
            .await
            .unwrap();
        let missing =
            FleetRepo::get_session(store.pool(), "codex:thread-a").await.unwrap().unwrap();
        assert_eq!(missing.lifecycle_state, "RUNNING");
        assert_eq!(missing.lifecycle_authority, "authoritative");
        assert_eq!(missing.transport_health, "UNAVAILABLE");

        // The row above is EXACTLY the shape that looped: hook-backed, so
        // `tmux_missing_event` never patches `lifecycle_state` to EXITED, and the
        // old guard required EXITED before it would stop. One more pass must now
        // be a no-op — otherwise this session re-emits every 3s forever.
        assert!(
            !needs_tmux_missing_event(&missing, &std::collections::HashSet::new()),
            "an authoritative session already marked UNAVAILABLE must not re-emit"
        );
    }

    /// The emit loop must terminate for a hook-backed session: one transition in,
    /// one event out, then silence until tmux actually comes back.
    #[tokio::test]
    async fn tmux_missing_emits_once_per_transition_not_once_per_tick() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let payload = serde_json::json!({
            "tmux_target": "codex-loop:0.0",
            "process_start_fingerprint": "fp-loop"
        });
        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "loop-start".to_string(),
                provider: "codex",
                provider_session_id: "thread-loop",
                cwd: "/repo",
                event_type: "UserPromptSubmit",
                payload: &payload,
                observed_at: 100,
                transcript_model: None,
            },
        )
        .await
        .unwrap();

        let row = FleetRepo::get_session(store.pool(), "codex:thread-loop")
            .await
            .unwrap()
            .unwrap();
        let nobody = std::collections::HashSet::new();

        // Tmux has gone: the first pass is a real transition and must emit.
        assert!(
            needs_tmux_missing_event(&row, &nobody),
            "a healthy session whose pane vanished must emit once"
        );
        FleetRepo::apply_event(store.pool(), &tmux_missing_event(&row, 200))
            .await
            .unwrap();

        // Every subsequent pass sees the state it just wrote and must stay quiet.
        for tick in 0..5 {
            let row = FleetRepo::get_session(store.pool(), "codex:thread-loop")
                .await
                .unwrap()
                .unwrap();
            assert!(
                !needs_tmux_missing_event(&row, &nobody),
                "tick {tick}: re-emitting is the 832k-row bug"
            );
        }

        // Discovery seeing the pane again is what re-arms it.
        let seen: std::collections::HashSet<String> =
            std::iter::once("codex:thread-loop".to_string()).collect();
        let row = FleetRepo::get_session(store.pool(), "codex:thread-loop")
            .await
            .unwrap()
            .unwrap();
        assert!(
            !needs_tmux_missing_event(&row, &seen),
            "a discovered session is never missing"
        );
    }

    #[tokio::test]
    async fn exact_hook_tmux_identity_retires_only_correlated_legacy_row() {
        use ainb_fleet_core::types::{Capabilities, Provenance};
        use std::collections::BTreeSet;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        for (target, fingerprint) in [("claude-a:0.0", "fp-a"), ("claude-b:0.0", "fp-b")] {
            let session = FleetSession {
                session_key: SessionKey::legacy(Provider::Claude, target, fingerprint),
                provider: Provider::Claude,
                provider_session_id: None,
                cwd: "/same/repo".to_string(),
                exact_tmux_target: Some(target.to_string()),
                pane_pid: Some(42),
                process_start_fingerprint: Some(fingerprint.to_string()),
                lifecycle: LifecycleState::Unknown,
                attention: AttentionState::None,
                management: ManagementState::Degraded,
                capabilities: Capabilities::degraded_tmux(),
                provenance: BTreeSet::from([Provenance::Tmux]),
                confidence: Confidence::Inferred,
                transport_health: TransportHealth::Healthy,
                first_seen_ms: Some(100),
                last_seen_ms: None,
                version: 0,
            };
            FleetRepo::apply_event(store.pool(), &tmux_event(&session, 100)).await.unwrap();
        }

        let payload = serde_json::json!({
            "tmux_target": "claude-a:0.0",
            "process_start_fingerprint": "fp-a",
            "payload": {}
        });
        apply_hook(
            store.pool(),
            &sink,
            HookObservation {
                event_id: "hook-exact-a".to_string(),
                provider: "claude",
                provider_session_id: "session-a",
                cwd: "/same/repo",
                event_type: "SessionStart",
                payload: &payload,
                observed_at: 200,
                transcript_model: None,
            },
        )
        .await
        .unwrap();

        let snapshot = FleetRepo::snapshot(store.pool()).await.unwrap();
        assert_eq!(snapshot.sessions.len(), 2);
        let managed = snapshot
            .sessions
            .iter()
            .find(|row| row.session_key == "claude:session-a")
            .unwrap();
        assert_eq!(managed.tmux_target.as_deref(), Some("claude-a:0.0"));
        let capabilities: ainb_hangar_proto::fleet::FleetCapabilities =
            serde_json::from_str(&managed.capabilities).unwrap();
        assert!(capabilities.tmux_attach);
        assert!(capabilities.tmux_text);
        assert!(capabilities.verified_picker);
        assert!(capabilities.stop);
        assert!(capabilities.restart);
        assert!(capabilities.kill);
        assert!(!capabilities.archive);
        assert!(snapshot.sessions.iter().any(|row| {
            row.management_state == "DEGRADED" && row.tmux_target.as_deref() == Some("claude-b:0.0")
        }));
        assert!(!snapshot.sessions.iter().any(|row| {
            row.management_state == "DEGRADED" && row.tmux_target.as_deref() == Some("claude-a:0.0")
        }));
        let legacy_key = SessionKey::legacy(Provider::Claude, "claude-a:0.0", "fp-a").to_string();
        let superseded_by: String = sqlx::query_scalar(
            "SELECT superseded_by FROM fleet_session WHERE session_key = ? AND visible = 0",
        )
        .bind(&legacy_key)
        .fetch_one(store.pool())
        .await
        .unwrap();
        assert_eq!(superseded_by, "claude:session-a");
        let history: Vec<String> = sqlx::query_scalar(
            "SELECT event_type FROM fleet_event WHERE session_key = ? ORDER BY revision",
        )
        .bind(&legacy_key)
        .fetch_all(store.pool())
        .await
        .unwrap();
        assert_eq!(history, vec!["tmux_discovered", "session_superseded"]);
    }

    #[tokio::test]
    async fn codex_manager_preserves_exact_request_and_independent_state() {
        use crate::fleet_provider::codex::{
            CodexCapabilities, CodexInbound, CodexItemRequestIdentity, CodexQuestionRequest,
            RpcRequestId,
        };
        use crate::fleet_provider::{QuestionOption, StructuredQuestion};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let capabilities = CodexCapabilities {
            cli_version: "codex-test".to_string(),
            daemon_version: None,
            app_server: true,
            stdio_proxy: true,
            request_user_input: true,
            approvals: true,
            thread_archive: true,
        };
        let request = CodexQuestionRequest {
            identity: CodexItemRequestIdentity {
                request_id: RpcRequestId::new(serde_json::json!(41)).unwrap(),
                thread_id: "thread-1".to_string(),
                turn_id: "turn-2".to_string(),
                item_id: "item-3".to_string(),
            },
            questions: vec![StructuredQuestion {
                id: "q1".to_string(),
                header: "Pick".to_string(),
                question: "Which?".to_string(),
                options: vec![QuestionOption {
                    label: "A".to_string(),
                    description: "first".to_string(),
                }],
                multi_select: false,
                is_other: true,
                is_secret: false,
            }],
            auto_resolution_ms: Some(60_000),
        };
        apply_codex_inbound(
            store.pool(),
            &sink,
            "codex:req:1".to_string(),
            CodexInbound::RequestUserInput(request.clone()),
            &capabilities,
            100,
        )
        .await
        .unwrap();

        let snapshot = snapshot_wire(store.pool()).await.unwrap();
        let session = &snapshot.sessions[0];
        assert_eq!(session.session_key, "codex:thread-1");
        assert_eq!(
            session.management,
            ainb_hangar_proto::fleet::ManagementState::Managed
        );
        assert_eq!(
            session.attention,
            ainb_hangar_proto::fleet::AttentionState::Ask
        );
        assert_eq!(
            session.current_request.as_ref().unwrap()["questions"][0]["id"],
            "q1",
            "snapshot must carry the request body from its atomic projection"
        );
        assert_eq!(
            session.lifecycle,
            ainb_hangar_proto::fleet::LifecycleState::Idle
        );
        assert_eq!(
            session.current_request.as_ref().unwrap()["identity"]["requestId"],
            41
        );
        assert_eq!(
            session.current_request.as_ref().unwrap()["questions"][0]["options"][0]["label"],
            "A"
        );
        assert!(session.capabilities.structured_answer);

        let replay = apply_codex_inbound(
            store.pool(),
            &sink,
            "codex:req:1".to_string(),
            CodexInbound::RequestUserInput(request),
            &capabilities,
            101,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(replay.duplicate);

        let events = FleetRepo::events_after(store.pool(), 0, 10).await.unwrap();
        assert_eq!(events[0].event_id, "codex:req:1");

        apply_codex_inbound(
            store.pool(),
            &sink,
            "codex:event:2".to_string(),
            CodexInbound::Notification {
                method: "turn/started".to_string(),
                params: serde_json::json!({
                    "threadId": "thread-1",
                    "turn": { "id": "turn-4" }
                }),
            },
            &capabilities,
            200,
        )
        .await
        .unwrap();
        let row = FleetRepo::get_session(store.pool(), "codex:thread-1").await.unwrap().unwrap();
        assert_eq!(row.lifecycle_state, "RUNNING");
        assert_eq!(row.attention_state, "NONE");
        assert!(row.current_request_fingerprint.is_none());
    }

    #[tokio::test]
    async fn codex_source_ledger_preserves_unknown_raw_envelope_fields() {
        use crate::fleet_provider::codex::{CodexCapabilities, CodexInbound, CodexInboundEnvelope};
        use ainb_hangar_store::repo::fleet_provider_event::FleetProviderEventRepo;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let capabilities = CodexCapabilities {
            cli_version: "codex-test".to_string(),
            daemon_version: None,
            app_server: true,
            stdio_proxy: true,
            request_user_input: true,
            approvals: true,
            thread_archive: true,
        };
        let raw = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "turn/started",
            "params": { "threadId": "thread-raw", "turn": { "id": "turn-1" }, "futureField": "kept" },
            "topLevelFutureField": { "kept": true },
        });
        ingest_codex_inbound(
            store.pool(),
            &sink,
            "codex-manager:boot:1".to_string(),
            CodexInboundEnvelope {
                inbound: CodexInbound::Notification {
                    method: "turn/started".to_string(),
                    params: raw["params"].clone(),
                },
                raw: raw.clone(),
            },
            &capabilities,
            100,
        )
        .await
        .unwrap();

        let source = FleetProviderEventRepo::get(store.pool(), "codex-manager:boot:1")
            .await
            .unwrap()
            .expect("source envelope persisted");
        assert_eq!(
            serde_json::from_str::<Value>(&source.raw_payload).unwrap(),
            raw,
            "raw app-server envelope must keep unknown fields"
        );
        assert!(source.projection_revision.is_some());
    }

    #[tokio::test]
    async fn codex_child_thread_lifecycle_updates_only_explicit_parent_workload() {
        use crate::fleet_provider::codex::{CodexCapabilities, CodexInbound, CodexInboundEnvelope};

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let capabilities = CodexCapabilities {
            cli_version: "codex-test".to_string(),
            daemon_version: None,
            app_server: true,
            stdio_proxy: true,
            request_user_input: true,
            approvals: true,
            thread_archive: true,
        };
        let parent = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "thread/started",
            "params": { "thread": { "id": "parent" } },
        });
        ingest_codex_inbound(
            store.pool(),
            &sink,
            "manager:parent".to_string(),
            CodexInboundEnvelope {
                inbound: CodexInbound::Notification {
                    method: "thread/started".to_string(),
                    params: parent["params"].clone(),
                },
                raw: parent,
            },
            &capabilities,
            100,
        )
        .await
        .unwrap();
        let child = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "thread/started",
            "params": { "thread": { "id": "child", "parentThreadId": "parent" } },
        });
        ingest_codex_inbound(
            store.pool(),
            &sink,
            "manager:child-start".to_string(),
            CodexInboundEnvelope {
                inbound: CodexInbound::Notification {
                    method: "thread/started".to_string(),
                    params: child["params"].clone(),
                },
                raw: child,
            },
            &capabilities,
            200,
        )
        .await
        .unwrap();
        let parent_row =
            FleetRepo::get_session(store.pool(), "codex:parent").await.unwrap().unwrap();
        assert_eq!(parent_row.active_work_count, 1);

        let closed = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "thread/closed",
            "params": { "threadId": "child" },
        });
        // Simulate a crash after durable child completion but before its parent
        // workload revision. Replaying the same source event must repair it.
        assert_eq!(
            FleetWorkRepo::complete_by_work_key(
                store.pool(),
                "codex",
                "child",
                "manager:child-close",
                300,
            )
            .await
            .unwrap(),
            vec![("codex:parent".to_string(), 0)],
        );
        ingest_codex_inbound(
            store.pool(),
            &sink,
            "manager:child-close".to_string(),
            CodexInboundEnvelope {
                inbound: CodexInbound::Notification {
                    method: "thread/closed".to_string(),
                    params: closed["params"].clone(),
                },
                raw: closed,
            },
            &capabilities,
            300,
        )
        .await
        .unwrap();
        let parent_row =
            FleetRepo::get_session(store.pool(), "codex:parent").await.unwrap().unwrap();
        assert_eq!(parent_row.active_work_count, 0);
    }

    #[tokio::test]
    async fn managed_codex_tmux_outage_disables_only_tmux_fallback() {
        use crate::fleet_provider::codex::CodexCapabilities;
        use ainb_fleet_core::types::{Capabilities, Provenance};
        use std::collections::BTreeSet;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let tmux = FleetSession {
            session_key: SessionKey::legacy(Provider::Codex, "fleet-codex-x:0.0", "fp-1"),
            provider: Provider::Codex,
            provider_session_id: None,
            cwd: "/repo".to_string(),
            exact_tmux_target: Some("fleet-codex-x:0.0".to_string()),
            pane_pid: Some(42),
            process_start_fingerprint: Some("fp-1".to_string()),
            lifecycle: LifecycleState::Unknown,
            attention: AttentionState::None,
            management: ManagementState::Degraded,
            capabilities: Capabilities::degraded_tmux(),
            provenance: BTreeSet::from([Provenance::Tmux]),
            confidence: Confidence::Inferred,
            transport_health: TransportHealth::Healthy,
            first_seen_ms: Some(100),
            last_seen_ms: None,
            version: 0,
        };
        let capabilities = CodexCapabilities {
            cli_version: "codex-test".to_string(),
            daemon_version: None,
            app_server: true,
            stdio_proxy: true,
            request_user_input: true,
            approvals: true,
            thread_archive: true,
        };
        register_managed_codex_tmux(
            store.pool(),
            &sink,
            "thread-tmux",
            "/repo",
            &tmux,
            &capabilities,
            100,
        )
        .await
        .unwrap();
        let active = FleetRepo::get_session(store.pool(), "codex:thread-tmux")
            .await
            .unwrap()
            .unwrap();
        let active_capabilities: ainb_hangar_proto::fleet::FleetCapabilities =
            serde_json::from_str(&active.capabilities).unwrap();
        assert!(active_capabilities.stop);
        assert!(active_capabilities.restart);
        assert!(active_capabilities.kill);
        assert!(active_capabilities.archive);
        mark_tmux_unavailable(store.pool(), &sink, 200).await.unwrap();
        let row = FleetRepo::get_session(store.pool(), "codex:thread-tmux")
            .await
            .unwrap()
            .unwrap();
        let disabled: ainb_hangar_proto::fleet::FleetCapabilities =
            serde_json::from_str(&row.capabilities).unwrap();
        assert_eq!(row.management_state, "MANAGED");
        assert_eq!(row.tmux_target.as_deref(), Some("fleet-codex-x:0.0"));
        assert_eq!(row.transport_health, "UNAVAILABLE");
        assert!(disabled.structured_answer);
        assert!(!disabled.stop);
        assert!(!disabled.restart);
        assert!(!disabled.kill);
        assert!(!disabled.archive);
        assert!(!disabled.tmux_attach);
        assert!(!disabled.tmux_text);

        let recovery = codex_manager_recovery_event(&row, &capabilities, 300);
        FleetRepo::apply_event(store.pool(), &recovery).await.unwrap();
        let row = FleetRepo::get_session(store.pool(), "codex:thread-tmux")
            .await
            .unwrap()
            .unwrap();
        let restored: ainb_hangar_proto::fleet::FleetCapabilities =
            serde_json::from_str(&row.capabilities).unwrap();
        assert_eq!(row.transport_health, "HEALTHY");
        assert_eq!(row.management_state, "MANAGED");
        assert!(restored.tmux_attach);
        assert!(restored.tmux_text);
        assert!(restored.stop);
        assert!(restored.restart);
        assert!(restored.kill);
        assert!(restored.archive);
    }

    /// The reap-then-archive pipeline, end to end on its two real clocks: a
    /// stranded session is retired at 15 min, stays visible all day, and only
    /// then leaves the roster every snapshot scans.
    #[tokio::test]
    async fn a_reaped_session_leaves_the_roster_only_after_the_archive_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let sink = EventBroker::new().sink();
        let pool = store.pool();

        apply_hook(
            pool,
            &sink,
            HookObservation {
                event_id: "hook-abandoned".to_string(),
                provider: "claude",
                provider_session_id: "sess-abandoned",
                event_type: "Notification",
                cwd: "/tmp/ephemeral",
                payload: &serde_json::json!({}),
                observed_at: 0,
                transcript_model: None,
            },
        )
        .await
        .expect("hook applies");

        // Alive: nothing to archive, whatever the clock says.
        assert_eq!(
            archive_dead_sessions(pool, &sink, SESSION_ARCHIVE_TTL_MS * 2)
                .await
                .expect("archive"),
            0,
            "a session that was never retired must not be archived"
        );

        reap_stale_sessions(pool, &sink, SESSION_STALE_TTL_MS + 1).await.expect("reap");

        // Retired, but well inside the archive TTL: still on screen, which is
        // the point of the two clocks being different.
        assert_eq!(
            archive_dead_sessions(pool, &sink, SESSION_STALE_TTL_MS + 2)
                .await
                .expect("archive"),
            0,
            "a session retired minutes ago must stay visible"
        );
        assert_eq!(FleetRepo::snapshot(pool).await.unwrap().sessions.len(), 1);

        let archived = archive_dead_sessions(pool, &sink, SESSION_ARCHIVE_TTL_MS * 2)
            .await
            .expect("archive");

        assert_eq!(archived, 1, "past the archive TTL the dead row is demoted");
        assert!(
            FleetRepo::snapshot(pool).await.unwrap().sessions.is_empty(),
            "the 3s reconciler must stop scanning it"
        );
        assert_eq!(
            FleetRepo::list_archived(pool, 50).await.unwrap().len(),
            1,
            "and it stays browsable"
        );
        assert_eq!(
            archive_dead_sessions(pool, &sink, SESSION_ARCHIVE_TTL_MS * 3)
                .await
                .expect("archive"),
            0,
            "a second pass must be free"
        );
    }

    /// An archive pass sends ONE broadcast wakeup, however many rows it moved.
    ///
    /// The fleet broadcast holds 256 and `spawn_fleet_forwarder` treats
    /// `Lagged` as terminal — it emits `fleet/resync_required` and dies. A pass
    /// that emitted per row would, on the measured 1,440-row backlog, tear down
    /// every connected subscriber's live stream. The forwarder discards the
    /// value and re-drains the durable log from its own cursor, so one send
    /// carries exactly what N would.
    ///
    /// Seeds MORE dead sessions than the channel holds, so a per-row emit
    /// cannot pass this by luck.
    #[tokio::test]
    async fn an_archive_pass_emits_one_wakeup_no_matter_how_many_rows_it_moves() {
        const DEAD_SESSIONS: usize = 300;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let broker = EventBroker::new();
        let sink = broker.sink();
        let pool = store.pool();

        for n in 0..DEAD_SESSIONS {
            FleetRepo::apply_event(
                pool,
                &NewFleetEvent {
                    event_id: format!("seed-{n}"),
                    session_key: format!("claude:dead-{n:04}"),
                    observed_at: 1,
                    authority: ObservationAuthority::Authoritative,
                    event_type: "observation".to_string(),
                    payload: "{}".to_string(),
                    patch: FleetSessionPatch {
                        lifecycle_state: Some("EXITED".to_string()),
                        ..FleetSessionPatch::default()
                    },
                },
            )
            .await
            .expect("seed");
        }

        // Subscribe AFTER seeding, so the only sends counted are the pass's.
        let mut rx = broker.subscribe_fleet();
        let archived = archive_dead_sessions(pool, &sink, SESSION_ARCHIVE_TTL_MS * 2)
            .await
            .expect("archive");

        assert_eq!(archived, DEAD_SESSIONS, "the whole batch is archived");

        let mut wakeups = 0;
        while rx.try_recv().is_ok() {
            wakeups += 1;
        }
        assert_eq!(
            wakeups, 1,
            "a bulk janitor pass must nudge subscribers once, not once per row \
             — {DEAD_SESSIONS} sends would lag a 256-slot channel and kill the stream"
        );
    }
}
