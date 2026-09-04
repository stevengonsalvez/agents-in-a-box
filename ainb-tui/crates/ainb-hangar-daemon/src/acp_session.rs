//! One ACP session on the chat bus, from either door.
//!
//! `fleet/acp_session_create` and a task caller need the same two writes: mint
//! the `fleet_session` + `fleet_acp_session` PAIR for a scope, and put a prompt
//! on the bus with its PENDING delivery leg. Each is ONE transaction here and
//! both doors call it, so the RPC and a task cannot drift into two slightly
//! different rows for the same thing. Neither spawns anything: the pool starts
//! the adapter lazily on the first prompt, so a session that never receives a
//! message costs nothing but a row.

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_store::repo::fleet::{FleetSessionPatch, NewFleetEvent, ObservationAuthority};
use ainb_hangar_store::repo::fleet_acp_session::{
    FleetAcpSessionError, FleetAcpSessionRepo, FleetAcpSessionRow, NewFleetAcpSession,
};
use ainb_hangar_store::repo::fleet_message::{
    FleetMessageError, FleetMessageRepo, NewFleetMessage,
};
use sqlx::SqlitePool;

use crate::events::EventSink;

/// Why [`ensure`] refused.
///
/// Everything but [`EnsureError::Store`] is the caller's fault and renders as
/// `invalid_params` on the wire; `Store` is the daemon's and renders as
/// `internal`.
#[derive(Debug, thiserror::Error)]
pub enum EnsureError {
    /// A session rooted nowhere cannot be prompted.
    #[error("cwd must not be empty")]
    EmptyCwd,
    /// An explicit scope that is blank is a typo, not a request for a private
    /// scope (leave it out for that).
    #[error("scope_key must not be empty")]
    EmptyScopeKey,
    /// The provider token is in neither the pool's registry nor the built-ins.
    #[error("unknown ACP provider {provider:?}; it is not in the adapter registry")]
    UnknownProvider {
        /// The token that was asked for.
        provider: String,
    },
    /// The scope already has a live session for a DIFFERENT provider or cwd.
    #[error(
        "scope_key {scope_key:?} is already held by a session whose {field} is {held:?}, \
         not {asked:?}; stop it before creating a different one"
    )]
    ScopeHeld {
        /// The contested scope.
        scope_key: String,
        /// Which column disagrees: `provider` or `cwd`.
        field: &'static str,
        /// What the incumbent session has.
        held: String,
        /// What the caller asked for.
        asked: String,
    },
    /// `SQLite` failed.
    #[error("acp session create: {0}")]
    Store(#[from] FleetAcpSessionError),
}

/// Find the live ACP session for `scope_key`, or mint one: the `fleet_session`
/// + `fleet_acp_session` pair under one key, in one transaction, no spawn.
///
/// `scope_key` defaults to `session:<session_key>` (a private scope). Idempotent
/// per LIVE scope, but only while the caller asked for the same session:
/// answering a `codex-acp` create with a live `claude-agent-acp` key would
/// silently hand back a session that prompts a DIFFERENT agent than the caller
/// believes it is driving, and answering a create for `~/work/api` with a
/// session rooted at `~/work/web` would run every later prompt against a
/// different repository. Both misroute in silence for the whole life of the
/// scope, so both are [`EnsureError::ScopeHeld`].
///
/// The provider is validated against the ADAPTER REGISTRY, not the schema: the
/// store only length-checks `provider` so the next adapter needs no migration,
/// which makes this the one place an unknown token is refused. With no pool
/// installed (the daemon is still booting, or a test) the built-in names stand
/// in for it.
pub async fn ensure(
    pool: &SqlitePool,
    events: &EventSink,
    provider: &str,
    cwd: &str,
    scope_key: Option<&str>,
) -> Result<FleetAcpSessionRow, EnsureError> {
    // Validated HERE, not at the RPC door: the task caller comes through the
    // same function and must not be able to mint a rootless or blank-scoped
    // session either.
    if cwd.trim().is_empty() {
        return Err(EnsureError::EmptyCwd);
    }
    if scope_key.is_some_and(|scope| scope.trim().is_empty()) {
        return Err(EnsureError::EmptyScopeKey);
    }
    let acp = crate::acp_pool::active_handle().await;
    let known = acp.as_ref().map_or_else(
        || ainb_acp::config::AdapterConfig::is_known_adapter(provider),
        |pool| pool.knows(provider),
    );
    if !known {
        return Err(EnsureError::UnknownProvider {
            provider: provider.to_string(),
        });
    }
    let permission_mode = acp.as_ref().map_or_else(
        || "default".to_string(),
        |pool| pool.permission_mode(provider),
    );

    let session_key = FleetAcpSessionRepo::mint_session_key(&SystemIdGen);
    let scope_key = scope_key.map_or_else(|| format!("session:{session_key}"), str::to_string);
    let now = SystemClock.now_ms();
    let event = NewFleetEvent {
        event_id: format!("acp-session-create:{session_key}"),
        session_key: session_key.clone(),
        observed_at: now,
        authority: ObservationAuthority::Authoritative,
        event_type: "acp_session_created".to_string(),
        payload: serde_json::json!({
            "provider": provider,
            "cwd": cwd,
            "scopeKey": scope_key,
            "permissionMode": permission_mode,
        })
        .to_string(),
        patch: FleetSessionPatch {
            // `acp` on the WIRE, with the concrete adapter in
            // `fleet_acp_session.provider`. The snapshot maps this token to
            // `FleetProvider::Acp`; anything else would render as Unknown.
            provider: Some(crate::acp_pool::ACP_PROVIDER_TOKEN.to_string()),
            cwd: Some(cwd.to_string()),
            display_name: crate::fleet::display_name_for_cwd(cwd),
            management_state: Some("MANAGED".to_string()),
            capabilities: Some(acp_capabilities()),
            confidence: Some("HIGH".to_string()),
            lifecycle_state: Some("IDLE".to_string()),
            attention_state: Some("NONE".to_string()),
            transport_health: Some("HEALTHY".to_string()),
            ..FleetSessionPatch::default()
        },
    };
    let (row, revision) = FleetAcpSessionRepo::insert_with_fleet_session(
        pool,
        &NewFleetAcpSession {
            session_key: session_key.clone(),
            scope_key,
            provider: provider.to_string(),
            cwd: cwd.to_string(),
            permission_mode,
            state: "IDLE".to_string(),
            created_at: now,
            last_active_at: now,
        },
        &event,
    )
    .await?;
    // The scope was ALREADY held by a live session, so this create replayed
    // onto it instead of minting the key above. Graft 4 rejects the same class
    // of replay on `fleet_message`; this is its ACP twin.
    if row.session_key != session_key {
        let mismatch = if row.provider == provider {
            (row.cwd != cwd).then(|| ("cwd", row.cwd.clone(), cwd.to_string()))
        } else {
            Some(("provider", row.provider.clone(), provider.to_string()))
        };
        if let Some((field, held, asked)) = mismatch {
            return Err(EnsureError::ScopeHeld {
                scope_key: row.scope_key,
                field,
                held,
                asked,
            });
        }
    }
    if let Some(revision) = revision {
        events.emit_fleet_revision(revision);
    }
    Ok(row)
}

/// Put `text` on the bus addressed to `session_key`.
///
/// One `user` message row in the session's own scope plus its PENDING delivery
/// leg, in one transaction. Returns the message id the caller hands to
/// `AcpPool::submit_prompt` (or, for the run that owns a `task:` scope,
/// `AcpPool::submit_task_prompt`).
///
/// The leg is what the pool resolves at turn end, so a prompt that skipped
/// this would have no receipt for anyone to read. `sender` names the door
/// (`operator`, `task:<id>`); the timeline shows it and nothing routes on it.
pub async fn enqueue(
    pool: &SqlitePool,
    session_key: &str,
    sender: &str,
    text: &str,
) -> Result<String, FleetMessageError> {
    let scope_key = FleetAcpSessionRepo::get(pool, session_key)
        .await?
        .map_or_else(|| format!("session:{session_key}"), |row| row.scope_key);
    let row = FleetMessageRepo::insert_message_with_deliveries(
        pool,
        &NewFleetMessage {
            id: SystemIdGen.new_ulid(),
            request_id: None,
            request_fingerprint: None,
            scope_key,
            origin_message_id: None,
            sender: sender.to_string(),
            kind: "user".to_string(),
            body: text.to_string(),
            created_at: SystemClock.now_ms(),
        },
        std::slice::from_ref(&session_key.to_string()),
    )
    .await?;
    Ok(row.id)
}

/// EXACTLY the actions Phase 5 wires, and nothing else.
///
/// `action_capability` gates on this JSON before any handler runs, so an unset
/// flag is a Rejected receipt rather than an action that reaches the pool and
/// fails somewhere less legible.
fn acp_capabilities() -> String {
    serde_json::to_string(&ainb_hangar_proto::fleet::FleetCapabilities {
        structured_answer: true,
        structured_dismiss: false,
        approvals: true,
        approval_session: false,
        send_prompt: true,
        continue_turn: false,
        retry: false,
        interrupt: true,
        start: false,
        stop: true,
        restart: false,
        kill: true,
        archive: false,
        // An ACP session has no pane. Leaving these true would make the tmux
        // surfaces offer attach/paste for a session that can never honour it.
        tmux_attach: false,
        tmux_text: false,
        verified_picker: false,
    })
    .unwrap_or_else(|_| "{}".to_string())
}
