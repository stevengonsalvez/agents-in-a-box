//! Fleet session reducer orchestration.
//!
//! Provider hooks and tmux discovery normalize into the Hangar Fleet tables.
//! SQLite owns canonical state and revision order. Live broadcasts only wake
//! subscribers after the matching revision commits.

use ainb_fleet_core::discover::discover_from_tmux;
use ainb_fleet_core::types::{
    AttentionState, Confidence, FleetSession, LifecycleState, ManagementState, Provider,
    SessionKey, TransportHealth,
};
use ainb_hangar_store::repo::fleet::{
    ApplyFleetEventResult, FleetEventRow, FleetRepo, FleetRepoError, FleetSessionPatch,
    FleetSessionRow, NewFleetEvent, ObservationAuthority,
};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::events::EventSink;
use crate::fleet_provider::codex::{CodexApprovalKind, CodexCapabilities, CodexInbound};

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
}

/// Apply one exact provider hook and wake revision subscribers after commit.
pub async fn apply_hook(
    pool: &SqlitePool,
    events: &EventSink,
    observation: HookObservation<'_>,
) -> Result<ApplyFleetEventResult, FleetRepoError> {
    let provider = parse_provider(observation.provider);
    let session_key = SessionKey::managed(provider, observation.provider_session_id);
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
    let (lifecycle_state, attention_state) = states_for_hook(observation.event_type);
    let request_fingerprint = match attention_state {
        Some(AttentionState::Ask | AttentionState::Approval) => {
            let fingerprint = match (provider, observation.event_type) {
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
        event_id: observation.event_id,
        session_key: session_key.to_string(),
        observed_at: observation.observed_at,
        authority: ObservationAuthority::Authoritative,
        event_type: observation.event_type.to_string(),
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
            ..FleetSessionPatch::default()
        },
    };
    let result = FleetRepo::apply_event(pool, &event).await?;
    if !result.duplicate {
        events.emit_fleet_revision(result.revision);
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
    let snapshot = FleetRepo::snapshot(pool).await?;
    let mut sessions = Vec::with_capacity(snapshot.sessions.len());
    for row in &snapshot.sessions {
        let current_request = if row.current_request_fingerprint.is_some() {
            current_request_wire(pool, &row.session_key).await?
        } else {
            None
        };
        sessions.push(session_wire(row, current_request));
    }
    Ok(ainb_hangar_proto::fleet::FleetSnapshot {
        head_revision: snapshot.head_revision,
        sessions,
    })
}

/// Read complete payload for current structured request or approval.
pub async fn current_request_wire(
    pool: &SqlitePool,
    session_key: &str,
) -> Result<Option<Value>, sqlx::Error> {
    sqlx::query_scalar::<_, String>(
        "SELECT payload FROM fleet_event \
         WHERE session_key = ? AND event_type IN (\
            'AskUserQuestion', 'PermissionRequest', \
            'item/tool/requestUserInput', \
            'item/commandExecution/requestApproval', \
            'item/fileChange/requestApproval', \
            'item/permissions/requestApproval'\
         ) AND applied = 1 ORDER BY revision DESC LIMIT 1",
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
    let discovered = match discover_from_tmux().await {
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
    let _manager_sequence = event_id;
    let event_id;
    let (thread_id, event_type, payload, lifecycle, attention, fingerprint) = match inbound {
        CodexInbound::RequestUserInput(request) => {
            let payload = serde_json::to_value(&request).ok()?;
            let fingerprint = fingerprint_value(&payload);
            event_id = format!("codex-request:{}:{fingerprint}", request.identity.thread_id);
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
            event_id = format!("codex-request:{thread_id}:{fingerprint}");
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
            event_id = format!(
                "codex-event:{thread_id}:{}:{}",
                method,
                fingerprint_value(&params)
            );
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
            event_id = format!("codex-request:{thread_id}:{fingerprint}");
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
            _ => wire::FleetProvider::Unknown,
        },
        provider_session_id: row.provider_session_id.clone(),
        tmux_target: row.tmux_target.clone(),
        process_start_fingerprint: row.process_start_fingerprint.clone(),
        cwd: row.cwd.clone(),
        display_name: row.display_name.clone(),
        lifecycle: parse_lifecycle(&row.lifecycle_state),
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
        version: row.version,
        updated_revision: row.updated_revision,
    }
}

fn event_wire(row: &FleetEventRow) -> ainb_hangar_proto::fleet::FleetEvent {
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
    restore_tmux_transport(pool, events, &sessions, observed_at).await?;
    let registered = FleetRepo::snapshot(pool).await?.sessions;
    let mut discovered: std::collections::HashSet<String> =
        sessions.iter().map(|session| session.session_key.to_string()).collect();
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
        if row.tmux_target.is_none()
            || discovered.contains(&row.session_key)
            || (row.lifecycle_state == "EXITED" && row.transport_health == "UNAVAILABLE")
        {
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
    row.provider == session.provider.as_str()
        && row.tmux_target == session.exact_tmux_target
        && row.process_start_fingerprint == session.process_start_fingerprint
        && row.cwd == session.cwd
        && row.management_state == management_token(session.management)
        && row.confidence == confidence_token(session.confidence)
        && row.lifecycle_state == state_token(session.lifecycle)
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
        loop {
            ticker.tick().await;
            let observed_at = clock.now_ms();
            if let Err(error) = reconcile_tmux_once(&pool, &events, observed_at).await {
                tracing::debug!(error = %error, "fleet tmux discovery unavailable");
                if let Err(downgrade) = mark_tmux_unavailable(&pool, &events, observed_at).await {
                    tracing::warn!(error = %downgrade, "fleet tmux downgrade failed");
                }
            }
        }
    })
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

async fn restore_tmux_transport(
    pool: &SqlitePool,
    events: &EventSink,
    discovered: &[FleetSession],
    observed_at: i64,
) -> Result<(), FleetRepoError> {
    let snapshot = FleetRepo::snapshot(pool).await?;
    for row in snapshot.sessions {
        let Some(target) = row.tmux_target.as_deref() else {
            continue;
        };
        let live = discovered.iter().any(|session| {
            session.exact_tmux_target.as_deref() == Some(target)
                && session.process_start_fingerprint == row.process_start_fingerprint
        });
        if !live || row.transport_health == "HEALTHY" {
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
    Ok(())
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
        _ => Provider::Unknown,
    }
}

fn states_for_hook(event_type: &str) -> (Option<LifecycleState>, Option<AttentionState>) {
    match event_type {
        "SessionStart" => (Some(LifecycleState::Starting), Some(AttentionState::None)),
        "UserPromptSubmit" | "PreToolUse" | "PostToolUse" => {
            (Some(LifecycleState::Running), Some(AttentionState::None))
        }
        "AskUserQuestion" => (Some(LifecycleState::Idle), Some(AttentionState::Ask)),
        "PermissionRequest" => (Some(LifecycleState::Idle), Some(AttentionState::Approval)),
        "Notification" => (None, Some(AttentionState::Waiting)),
        "Stop" | "SubagentStop" => (
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

fn managed_capabilities(provider: Provider) -> String {
    let broker = provider == Provider::Claude;
    serde_json::to_string(&ainb_hangar_proto::fleet::FleetCapabilities {
        structured_answer: broker,
        approvals: broker,
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
        approvals: capabilities.approvals,
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

fn claude_permission_identity(payload: &Value) -> Option<(String, String)> {
    let hook = payload.get("payload").unwrap_or(payload);
    let tool = payload
        .get("matcher")
        .or_else(|| hook.get("tool_name"))
        .or_else(|| hook.get("tool"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
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
    use ainb_hangar_store::Store;

    #[test]
    fn lifecycle_and_attention_are_independent() {
        assert_eq!(
            states_for_hook("AskUserQuestion"),
            (Some(LifecycleState::Idle), Some(AttentionState::Ask))
        );
        assert_eq!(
            states_for_hook("PermissionRequest"),
            (Some(LifecycleState::Idle), Some(AttentionState::Approval))
        );
        assert_eq!(
            states_for_hook("Stop"),
            (
                Some(LifecycleState::TurnComplete),
                Some(AttentionState::None)
            )
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
            "different-manager-sequence".to_string(),
            CodexInbound::RequestUserInput(request),
            &capabilities,
            101,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(replay.duplicate);

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
}
