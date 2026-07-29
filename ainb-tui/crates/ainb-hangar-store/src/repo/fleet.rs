//! Authoritative Fleet session read model, revision log, and action receipts.
//!
//! One transaction applies each normalized provider or discovery event. The
//! unique event id makes replay idempotent, the database assigns one global
//! revision, and accepted changes advance the target session's version. State
//! groups keep separate authority and timestamps so an inferred tmux sample
//! cannot replace an authoritative provider or hook value.

use sqlx::{Row, Sqlite, SqlitePool, Transaction};

/// Authority of one normalized observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationAuthority {
    /// Provider RPC or lifecycle hook with exact session identity.
    Authoritative,
    /// Tmux, process, or transcript inference.
    Inferred,
}

impl ObservationAuthority {
    /// Stable database token.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Authoritative => "authoritative",
            Self::Inferred => "inferred",
        }
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Authoritative => 2,
            Self::Inferred => 1,
        }
    }

    fn parse(value: &str) -> Self {
        if value == "authoritative" {
            Self::Authoritative
        } else {
            Self::Inferred
        }
    }
}

/// Optional changes carried by one normalized Fleet event.
///
/// `None` means leave that field unchanged. Optional identity fields are not
/// cleared by this patch shape. Explicit clearing can be added as a typed action
/// when a provider supplies a trustworthy detach event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FleetSessionPatch {
    /// Provider token, normally `claude`, `codex`, or `unknown`.
    pub provider: Option<String>,
    /// Provider-owned stable session id.
    pub provider_session_id: Option<String>,
    /// Exact tmux target used for attach and degraded control.
    pub tmux_target: Option<String>,
    /// Process start identity paired with a legacy tmux target.
    pub process_start_fingerprint: Option<String>,
    /// Current working directory, display and routing metadata only.
    pub cwd: Option<String>,
    /// Human-readable session label.
    pub display_name: Option<String>,
    /// `MANAGED` or `DEGRADED`.
    pub management_state: Option<String>,
    /// Serialized capability object.
    pub capabilities: Option<String>,
    /// `HIGH`, `MEDIUM`, or `LOW`.
    pub confidence: Option<String>,
    /// Independent lifecycle state.
    pub lifecycle_state: Option<String>,
    /// Active provider child-work count.
    pub active_work_count: Option<i64>,
    /// Independent attention state.
    pub attention_state: Option<String>,
    /// Exact active request fingerprint. `Some(None)` explicitly clears it.
    pub current_request_fingerprint: Option<Option<String>>,
    /// Independent transport health.
    pub transport_health: Option<String>,
}

impl FleetSessionPatch {
    fn has_metadata(&self) -> bool {
        self.provider.is_some()
            || self.provider_session_id.is_some()
            || self.tmux_target.is_some()
            || self.process_start_fingerprint.is_some()
            || self.cwd.is_some()
            || self.display_name.is_some()
            || self.management_state.is_some()
            || self.capabilities.is_some()
            || self.confidence.is_some()
    }
}

/// One normalized event to apply to the Fleet read model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFleetEvent {
    /// Replay-safe provider or legacy fingerprint.
    pub event_id: String,
    /// Stable Fleet identity, never cwd.
    pub session_key: String,
    /// Observation time in epoch milliseconds.
    pub observed_at: i64,
    /// Whether this event is authoritative or inferred.
    pub authority: ObservationAuthority,
    /// Normalized event discriminator.
    pub event_type: String,
    /// Serialized raw or normalized event body.
    pub payload: String,
    /// Read-model changes derived from the event.
    pub patch: FleetSessionPatch,
}

/// Canonical Fleet session row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSessionRow {
    /// Stable Fleet identity.
    pub session_key: String,
    /// Provider token.
    pub provider: String,
    /// Provider-owned session id.
    pub provider_session_id: Option<String>,
    /// Exact tmux target.
    pub tmux_target: Option<String>,
    /// Legacy process start fingerprint.
    pub process_start_fingerprint: Option<String>,
    /// Current working directory metadata.
    pub cwd: String,
    /// Human-readable label.
    pub display_name: Option<String>,
    /// Lifecycle state token.
    pub lifecycle_state: String,
    /// Number of active provider child-work items.
    pub active_work_count: i64,
    /// Workload group timestamp.
    pub workload_updated_at: i64,
    /// Workload group authority.
    pub workload_authority: String,
    /// Attention state token.
    pub attention_state: String,
    /// Fingerprint of current structured request or approval.
    pub current_request_fingerprint: Option<String>,
    /// Managed or degraded token.
    pub management_state: String,
    /// Transport health token.
    pub transport_health: String,
    /// Serialized capability object.
    pub capabilities: String,
    /// Overall provenance of the last accepted change.
    pub provenance: String,
    /// Confidence token.
    pub confidence: String,
    /// First discovery time.
    pub discovered_at: i64,
    /// Last accepted observation time.
    pub last_observed_at: i64,
    /// Metadata group timestamp.
    pub metadata_updated_at: i64,
    /// Metadata group authority.
    pub metadata_authority: String,
    /// Lifecycle group timestamp.
    pub lifecycle_updated_at: i64,
    /// Lifecycle group authority.
    pub lifecycle_authority: String,
    /// Attention group timestamp.
    pub attention_updated_at: i64,
    /// Attention group authority.
    pub attention_authority: String,
    /// Transport group timestamp.
    pub transport_updated_at: i64,
    /// Transport group authority.
    pub transport_authority: String,
    /// Optimistic concurrency version.
    pub version: i64,
    /// Revision that last changed this row.
    pub updated_revision: i64,
}

/// One durable Fleet change-log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetEventRow {
    /// Global monotonic revision.
    pub revision: i64,
    /// Replay-safe event identity.
    pub event_id: String,
    /// Target session.
    pub session_key: String,
    /// Observation time.
    pub observed_at: i64,
    /// Authority token.
    pub authority: String,
    /// Event discriminator.
    pub event_type: String,
    /// Serialized event body.
    pub payload: String,
    /// Session version after this event was considered.
    pub session_version: i64,
    /// Whether this event changed canonical session state.
    pub applied: bool,
}

/// One durable Fleet revision safe for the public payload-free timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetTimelineRow {
    /// Global monotonic revision.
    pub revision: i64,
    /// Target session.
    pub session_key: String,
    /// Observation time.
    pub observed_at: i64,
    /// Authority token.
    pub authority: String,
    /// Known normalized event discriminator.
    pub event_type: String,
    /// Session version after this event was considered.
    pub session_version: i64,
    /// Whether this event changed canonical session state.
    pub applied: bool,
}

/// Result of applying or replaying one event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyFleetEventResult {
    /// Assigned global revision.
    pub revision: i64,
    /// Session version after consideration.
    pub session_version: i64,
    /// Whether canonical state changed.
    pub applied: bool,
    /// True when the same event id had already committed.
    pub duplicate: bool,
    /// Current canonical row.
    pub session: FleetSessionRow,
}

/// Consistent Fleet snapshot and its global revision head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSnapshot {
    /// Highest committed Fleet event revision.
    pub head_revision: i64,
    /// Canonical sessions ordered by stable key.
    pub sessions: Vec<FleetSessionRow>,
}

/// One session row and its current request payload from one subscription read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSessionProjectionRow {
    /// Canonical session state.
    pub session: FleetSessionRow,
    /// Complete current structured request or approval, if one remains active.
    pub current_request: Option<serde_json::Value>,
}

/// Atomic subscription baseline and bounded durable replay interval.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetSubscriptionProjection {
    /// Highest durable revision included by this projection.
    pub head_revision: i64,
    /// Canonical sessions and current request payloads at `head_revision`.
    pub sessions: Vec<FleetSessionProjectionRow>,
    /// Durable rows after the requested cursor, capped by the caller limit.
    pub replay: Vec<FleetEventRow>,
}

/// Action receipt insert or status update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewActionReceipt {
    /// Idempotent action request id.
    pub request_id: String,
    /// Target session key.
    pub session_key: String,
    /// Stable action kind token.
    pub action_kind: String,
    /// Stable fingerprint of exact action or structured request payload.
    pub action_fingerprint: String,
    /// Session version required when action was accepted.
    pub expected_version: i64,
    /// Shared broadcast idempotency key, when action belongs to a broadcast.
    pub idempotency_key: Option<String>,
    /// Delivery status token.
    pub status: String,
    /// Optional human-readable detail.
    pub detail: Option<String>,
    /// Session version observed when delivery completed.
    pub session_version: Option<i64>,
    /// First creation time.
    pub created_at: i64,
    /// Last status update time.
    pub updated_at: i64,
}

/// Durable action receipt row.
pub type ActionReceiptRow = NewActionReceipt;

/// Fleet repository failure.
#[derive(Debug, thiserror::Error)]
pub enum FleetRepoError {
    /// SQLite query or constraint failure.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
    /// One event id was reused for a different session.
    #[error("fleet event id {event_id:?} already belongs to session {existing_session:?}")]
    EventIdCollision {
        /// Colliding event id.
        event_id: String,
        /// Session that first committed it.
        existing_session: String,
    },
    /// One action request id was reused for a different target or action.
    #[error("fleet action request id {request_id:?} was reused with different identity")]
    ReceiptCollision {
        /// Colliding request id.
        request_id: String,
    },
    /// Action targets a session that does not exist.
    #[error("fleet session {session_key:?} was not found")]
    SessionNotFound {
        /// Missing session key.
        session_key: String,
    },
    /// Action was prepared against an older or future session version.
    #[error("fleet session {session_key:?} version is {actual}, expected {expected}")]
    StaleVersion {
        /// Target session key.
        session_key: String,
        /// Version required by caller.
        expected: i64,
        /// Current canonical version.
        actual: i64,
    },
    /// Structured action does not match current provider request.
    #[error("fleet session {session_key:?} request fingerprint does not match")]
    RequestFingerprintMismatch {
        /// Target session key.
        session_key: String,
    },
}

/// Stateless typed wrapper over the Fleet tables.
pub struct FleetRepo;

impl FleetRepo {
    /// Apply one normalized event atomically.
    ///
    /// Duplicate event ids return the original revision without changing state.
    /// A collision across session keys is rejected. Every new event receives a
    /// revision, while the session version advances only when canonical state
    /// changes.
    pub async fn apply_event(
        pool: &SqlitePool,
        event: &NewFleetEvent,
    ) -> Result<ApplyFleetEventResult, FleetRepoError> {
        let mut tx = pool.begin().await?;

        if let Some(prior) = event_by_id(&mut tx, &event.event_id).await? {
            if prior.session_key != event.session_key {
                return Err(FleetRepoError::EventIdCollision {
                    event_id: event.event_id.clone(),
                    existing_session: prior.session_key,
                });
            }
            let session = session_by_key_tx(&mut tx, &event.session_key)
                .await?
                .expect("fleet event foreign key must resolve its session");
            tx.commit().await?;
            return Ok(ApplyFleetEventResult {
                revision: prior.revision,
                session_version: prior.session_version,
                applied: prior.applied,
                duplicate: true,
                session,
            });
        }

        let prior = session_by_key_tx(&mut tx, &event.session_key).await?;
        let is_new = prior.is_none();
        let (mut session, changed) = match prior {
            Some(mut row) => {
                let changed = apply_patch(&mut row, event);
                if changed {
                    row.version += 1;
                    row.last_observed_at = row.last_observed_at.max(event.observed_at);
                    row.provenance = event.authority.as_str().to_string();
                }
                (row, changed)
            }
            None => (new_session(event), true),
        };

        if is_new {
            insert_session(&mut tx, &session).await?;
        }

        let revision = sqlx::query(
            "INSERT INTO fleet_event \
             (event_id, session_key, observed_at, authority, event_type, payload, \
              request_fingerprint, session_version, applied) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&event.event_id)
        .bind(&event.session_key)
        .bind(event.observed_at)
        .bind(event.authority.as_str())
        .bind(&event.event_type)
        .bind(&event.payload)
        .bind(event.patch.current_request_fingerprint.as_ref().and_then(Clone::clone))
        .bind(session.version)
        .bind(i64::from(changed))
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();

        if changed {
            session.updated_revision = revision;
            update_session(&mut tx, &session).await?;
        }
        tx.commit().await?;

        Ok(ApplyFleetEventResult {
            revision,
            session_version: session.version,
            applied: changed,
            duplicate: false,
            session,
        })
    }

    /// Hide one inferred duplicate behind its authoritative managed session.
    /// Event history remains attached to the legacy key and one committed
    /// revision records the supersession for subscribers.
    pub async fn supersede_session(
        pool: &SqlitePool,
        legacy_key: &str,
        managed_key: &str,
        observed_at: i64,
    ) -> Result<Option<i64>, FleetRepoError> {
        let event_id = format!("fleet-supersede:{legacy_key}:{managed_key}");
        let mut tx = pool.begin().await?;
        if let Some(prior) = event_by_id(&mut tx, &event_id).await? {
            tx.commit().await?;
            return Ok(Some(prior.revision));
        }
        let version: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM fleet_session WHERE session_key = ? \
             AND management_state = 'DEGRADED' AND visible = 1",
        )
        .bind(legacy_key)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(version) = version else {
            tx.commit().await?;
            return Ok(None);
        };
        let managed_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM fleet_session WHERE session_key = ? AND visible = 1)",
        )
        .bind(managed_key)
        .fetch_one(&mut *tx)
        .await?;
        if !managed_exists {
            tx.commit().await?;
            return Ok(None);
        }
        let next_version = version + 1;
        sqlx::query(
            "UPDATE fleet_session SET visible = 0, superseded_by = ?, version = ?, \
             last_observed_at = MAX(last_observed_at, ?) WHERE session_key = ?",
        )
        .bind(managed_key)
        .bind(next_version)
        .bind(observed_at)
        .bind(legacy_key)
        .execute(&mut *tx)
        .await?;
        let payload = serde_json::json!({ "supersededBy": managed_key }).to_string();
        let revision = sqlx::query(
            "INSERT INTO fleet_event \
             (event_id, session_key, observed_at, authority, event_type, payload, \
              session_version, applied) VALUES (?, ?, ?, 'authoritative', \
              'session_superseded', ?, ?, 1)",
        )
        .bind(&event_id)
        .bind(legacy_key)
        .bind(observed_at)
        .bind(payload)
        .bind(next_version)
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        sqlx::query("UPDATE fleet_session SET updated_revision = ? WHERE session_key = ?")
            .bind(revision)
            .bind(legacy_key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(revision))
    }

    /// Fetch one canonical session by stable key.
    pub async fn get_session(
        pool: &SqlitePool,
        session_key: &str,
    ) -> Result<Option<FleetSessionRow>, sqlx::Error> {
        let row = sqlx::query(SESSION_SELECT_BY_KEY)
            .bind(session_key)
            .fetch_optional(pool)
            .await?;
        row.as_ref().map(session_from_row).transpose()
    }

    /// Validate optimistic concurrency and optional structured request identity.
    pub async fn validate_action_target(
        pool: &SqlitePool,
        session_key: &str,
        expected_version: i64,
        expected_request_fingerprint: Option<&str>,
    ) -> Result<FleetSessionRow, FleetRepoError> {
        let session = Self::get_session(pool, session_key).await?.ok_or_else(|| {
            FleetRepoError::SessionNotFound {
                session_key: session_key.to_string(),
            }
        })?;
        if session.version != expected_version {
            return Err(FleetRepoError::StaleVersion {
                session_key: session_key.to_string(),
                expected: expected_version,
                actual: session.version,
            });
        }
        if expected_request_fingerprint.is_some_and(|expected| {
            session.current_request_fingerprint.as_deref() != Some(expected)
        }) {
            return Err(FleetRepoError::RequestFingerprintMismatch {
                session_key: session_key.to_string(),
            });
        }
        Ok(session)
    }

    /// Read a consistent canonical snapshot plus its event-log head.
    pub async fn snapshot(pool: &SqlitePool) -> Result<FleetSnapshot, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let head_revision: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(revision), 0) FROM fleet_event")
                .fetch_one(&mut *tx)
                .await?;
        let rows = sqlx::query(SESSION_SELECT_ALL).fetch_all(&mut *tx).await?;
        let sessions = rows.iter().map(session_from_row).collect::<Result<_, _>>()?;
        tx.commit().await?;
        Ok(FleetSnapshot {
            head_revision,
            sessions,
        })
    }

    /// Read one subscription projection in a single transaction.
    ///
    /// The durable head, session rows, active request bodies, and replay rows
    /// all come from the same SQLite read transaction. Callers request one
    /// extra replay row when they need to distinguish a full capped replay from
    /// an over-limit cursor.
    pub async fn subscription_projection(
        pool: &SqlitePool,
        after_revision: i64,
        replay_limit: i64,
    ) -> Result<FleetSubscriptionProjection, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let head_revision: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(revision), 0) FROM fleet_event")
                .fetch_one(&mut *tx)
                .await?;
        let rows = sqlx::query(SESSION_SELECT_ALL).fetch_all(&mut *tx).await?;
        let mut sessions = Vec::with_capacity(rows.len());
        for row in &rows {
            let session = session_from_row(row)?;
            let current_request =
                if let Some(request_fingerprint) = session.current_request_fingerprint.as_deref() {
                    sqlx::query_scalar::<_, String>(
                        "SELECT payload FROM fleet_event \
                     WHERE session_key = ? AND event_type IN (\
                        'AskUserQuestion', 'PermissionRequest', \
                        'item/tool/requestUserInput', \
                        'item/commandExecution/requestApproval', \
                        'item/fileChange/requestApproval', \
                        'item/permissions/requestApproval'\
                     ) AND request_fingerprint = ? AND applied = 1 AND revision <= ? \
                     ORDER BY revision DESC LIMIT 1",
                    )
                    .bind(&session.session_key)
                    .bind(request_fingerprint)
                    .bind(head_revision)
                    .fetch_optional(&mut *tx)
                    .await?
                    .and_then(|payload| serde_json::from_str(&payload).ok())
                } else {
                    None
                };
            sessions.push(FleetSessionProjectionRow {
                session,
                current_request,
            });
        }
        let replay = if after_revision > 0 && after_revision < head_revision {
            let rows = sqlx::query(
                "SELECT revision, event_id, session_key, observed_at, authority, event_type, \
                        payload, session_version, applied \
                 FROM fleet_event WHERE revision > ? AND revision <= ? \
                 ORDER BY revision ASC LIMIT ?",
            )
            .bind(after_revision)
            .bind(head_revision)
            .bind(replay_limit.max(0))
            .fetch_all(&mut *tx)
            .await?;
            rows.iter().map(event_from_row).collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        tx.commit().await?;
        Ok(FleetSubscriptionProjection {
            head_revision,
            sessions,
            replay,
        })
    }

    /// Read durable events after a global revision, oldest first.
    pub async fn events_after(
        pool: &SqlitePool,
        after_revision: i64,
        limit: i64,
    ) -> Result<Vec<FleetEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT revision, event_id, session_key, observed_at, authority, event_type, \
                    payload, session_version, applied \
             FROM fleet_event WHERE revision > ? ORDER BY revision ASC LIMIT ?",
        )
        .bind(after_revision)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    /// Read a bounded, payload-free Fleet timeline after a global revision.
    ///
    /// The query joins visible Fleet sessions and filters the closed raw type
    /// allowlist before `LIMIT`, so excluded history can never consume a page
    /// or strand a caller's cursor.
    pub async fn timeline_after(
        pool: &SqlitePool,
        after_revision: i64,
        session_key: Option<&str>,
        limit: i64,
    ) -> Result<Vec<FleetTimelineRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT e.revision, e.session_key, e.observed_at, e.authority, e.event_type, \
                    e.session_version, e.applied \
             FROM fleet_event e \
             INNER JOIN fleet_session s ON s.session_key = e.session_key AND s.visible = 1 \
             WHERE e.revision > ? \
               AND (? IS NULL OR e.session_key = ?) \
               AND e.event_type IN ( \
                   'SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', \
                   'AskUserQuestion', 'PermissionRequest', 'Notification', 'Stop', \
                   'SubagentStop', 'StopFailure', 'SessionEnd', \
                   'codex_manager_unavailable', 'codex_manager_recovered', \
                   'codex_managed_tui_started', 'tmux_missing', 'tmux_unavailable', \
                   'tmux_available', 'tmux_discovered', 'session_superseded' \
               ) \
             ORDER BY e.revision ASC LIMIT ?",
        )
        .bind(after_revision)
        .bind(session_key)
        .bind(session_key)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(timeline_from_row).collect()
    }

    /// Insert or advance one action receipt.
    ///
    /// A repeated request id may update delivery status only when its target and
    /// action kind match the original receipt.
    pub async fn upsert_action_receipt(
        pool: &SqlitePool,
        receipt: &NewActionReceipt,
    ) -> Result<ActionReceiptRow, FleetRepoError> {
        let result = sqlx::query(
            "INSERT INTO fleet_action_receipt \
             (request_id, session_key, action_kind, action_fingerprint, expected_version, \
              idempotency_key, status, detail, session_version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(request_id) DO UPDATE SET \
                status = excluded.status, \
                detail = excluded.detail, \
                session_version = excluded.session_version, \
                updated_at = excluded.updated_at \
             WHERE fleet_action_receipt.session_key = excluded.session_key \
               AND fleet_action_receipt.action_kind = excluded.action_kind \
               AND fleet_action_receipt.action_fingerprint = excluded.action_fingerprint \
               AND fleet_action_receipt.expected_version = excluded.expected_version \
               AND fleet_action_receipt.idempotency_key IS excluded.idempotency_key",
        )
        .bind(&receipt.request_id)
        .bind(&receipt.session_key)
        .bind(&receipt.action_kind)
        .bind(&receipt.action_fingerprint)
        .bind(receipt.expected_version)
        .bind(&receipt.idempotency_key)
        .bind(&receipt.status)
        .bind(&receipt.detail)
        .bind(receipt.session_version)
        .bind(receipt.created_at)
        .bind(receipt.updated_at)
        .execute(pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(FleetRepoError::ReceiptCollision {
                request_id: receipt.request_id.clone(),
            });
        }
        Self::get_action_receipt(pool, &receipt.request_id).await?.ok_or_else(|| {
            FleetRepoError::ReceiptCollision {
                request_id: receipt.request_id.clone(),
            }
        })
    }

    /// Fetch one action receipt by request id.
    pub async fn get_action_receipt(
        pool: &SqlitePool,
        request_id: &str,
    ) -> Result<Option<ActionReceiptRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT request_id, session_key, action_kind, action_fingerprint, \
                    expected_version, idempotency_key, status, detail, session_version, \
                    created_at, updated_at \
             FROM fleet_action_receipt WHERE request_id = ?",
        )
        .bind(request_id)
        .fetch_optional(pool)
        .await?;
        row.as_ref().map(receipt_from_row).transpose()
    }

    /// List durable action receipts newest first.
    pub async fn list_action_receipts(
        pool: &SqlitePool,
        limit: i64,
    ) -> Result<Vec<ActionReceiptRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT request_id, session_key, action_kind, action_fingerprint, expected_version, \
                    idempotency_key, status, detail, session_version, created_at, updated_at \
             FROM fleet_action_receipt ORDER BY updated_at DESC, request_id DESC LIMIT ?",
        )
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(receipt_from_row).collect()
    }
}

const SESSION_SELECT_BY_KEY: &str = "SELECT session_key, provider, provider_session_id, \
    tmux_target, process_start_fingerprint, cwd, display_name, lifecycle_state, \
    attention_state, current_request_fingerprint, management_state, transport_health, capabilities, provenance, \
    confidence, discovered_at, last_observed_at, metadata_updated_at, \
    metadata_authority, lifecycle_updated_at, lifecycle_authority, \
    attention_updated_at, attention_authority, transport_updated_at, \
    transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision \
    FROM fleet_session WHERE session_key = ?";

const SESSION_SELECT_ALL: &str = "SELECT session_key, provider, provider_session_id, \
    tmux_target, process_start_fingerprint, cwd, display_name, lifecycle_state, \
    attention_state, current_request_fingerprint, management_state, transport_health, capabilities, provenance, \
    confidence, discovered_at, last_observed_at, metadata_updated_at, \
    metadata_authority, lifecycle_updated_at, lifecycle_authority, \
    attention_updated_at, attention_authority, transport_updated_at, \
    transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision \
    FROM fleet_session WHERE visible = 1 ORDER BY session_key ASC";

async fn session_by_key_tx(
    tx: &mut Transaction<'_, Sqlite>,
    session_key: &str,
) -> Result<Option<FleetSessionRow>, sqlx::Error> {
    let row = sqlx::query(SESSION_SELECT_BY_KEY)
        .bind(session_key)
        .fetch_optional(&mut **tx)
        .await?;
    row.as_ref().map(session_from_row).transpose()
}

async fn event_by_id(
    tx: &mut Transaction<'_, Sqlite>,
    event_id: &str,
) -> Result<Option<FleetEventRow>, sqlx::Error> {
    let row = sqlx::query(
        "SELECT revision, event_id, session_key, observed_at, authority, event_type, \
                payload, session_version, applied \
         FROM fleet_event WHERE event_id = ?",
    )
    .bind(event_id)
    .fetch_optional(&mut **tx)
    .await?;
    row.as_ref().map(event_from_row).transpose()
}

fn new_session(event: &NewFleetEvent) -> FleetSessionRow {
    let authority = event.authority.as_str().to_string();
    let metadata_at = event.observed_at;
    let lifecycle_at = event.patch.lifecycle_state.as_ref().map_or(0, |_| event.observed_at);
    let attention_at = if event.patch.attention_state.is_some()
        || event.patch.current_request_fingerprint.is_some()
    {
        event.observed_at
    } else {
        0
    };
    let transport_at = event.patch.transport_health.as_ref().map_or(0, |_| event.observed_at);
    let workload_at = event.patch.active_work_count.map_or(0, |_| event.observed_at);
    FleetSessionRow {
        session_key: event.session_key.clone(),
        provider: event.patch.provider.clone().unwrap_or_else(|| "unknown".to_string()),
        provider_session_id: event.patch.provider_session_id.clone(),
        tmux_target: event.patch.tmux_target.clone(),
        process_start_fingerprint: event.patch.process_start_fingerprint.clone(),
        cwd: event.patch.cwd.clone().unwrap_or_default(),
        display_name: event.patch.display_name.clone(),
        lifecycle_state: event
            .patch
            .lifecycle_state
            .clone()
            .unwrap_or_else(|| "UNKNOWN".to_string()),
        active_work_count: event.patch.active_work_count.unwrap_or(0),
        workload_updated_at: workload_at,
        workload_authority: if workload_at == 0 {
            "inferred".to_string()
        } else {
            authority.clone()
        },
        attention_state: event.patch.attention_state.clone().unwrap_or_else(|| "NONE".to_string()),
        current_request_fingerprint: event.patch.current_request_fingerprint.clone().flatten(),
        management_state: event
            .patch
            .management_state
            .clone()
            .unwrap_or_else(|| "DEGRADED".to_string()),
        transport_health: event
            .patch
            .transport_health
            .clone()
            .unwrap_or_else(|| "UNKNOWN".to_string()),
        capabilities: event.patch.capabilities.clone().unwrap_or_else(|| "{}".to_string()),
        provenance: authority.clone(),
        confidence: event.patch.confidence.clone().unwrap_or_else(|| "LOW".to_string()),
        discovered_at: event.observed_at,
        last_observed_at: event.observed_at,
        metadata_updated_at: metadata_at,
        metadata_authority: authority.clone(),
        lifecycle_updated_at: lifecycle_at,
        lifecycle_authority: if lifecycle_at == 0 {
            "inferred".to_string()
        } else {
            authority.clone()
        },
        attention_updated_at: attention_at,
        attention_authority: if attention_at == 0 {
            "inferred".to_string()
        } else {
            authority.clone()
        },
        transport_updated_at: transport_at,
        transport_authority: if transport_at == 0 {
            "inferred".to_string()
        } else {
            authority
        },
        version: 1,
        updated_revision: 0,
    }
}

fn apply_patch(row: &mut FleetSessionRow, event: &NewFleetEvent) -> bool {
    let mut changed = false;
    let authority = event.authority;
    let authority_token = authority.as_str().to_string();

    if event.patch.has_metadata()
        && should_replace(
            authority,
            event.observed_at,
            &row.metadata_authority,
            row.metadata_updated_at,
        )
    {
        assign_if_some(&mut row.provider, &event.patch.provider);
        assign_option_if_some(
            &mut row.provider_session_id,
            &event.patch.provider_session_id,
        );
        assign_option_if_some(&mut row.tmux_target, &event.patch.tmux_target);
        assign_option_if_some(
            &mut row.process_start_fingerprint,
            &event.patch.process_start_fingerprint,
        );
        assign_if_some(&mut row.cwd, &event.patch.cwd);
        assign_option_if_some(&mut row.display_name, &event.patch.display_name);
        assign_if_some(&mut row.management_state, &event.patch.management_state);
        assign_if_some(&mut row.capabilities, &event.patch.capabilities);
        assign_if_some(&mut row.confidence, &event.patch.confidence);
        row.metadata_updated_at = event.observed_at;
        row.metadata_authority.clone_from(&authority_token);
        changed = true;
    }

    if event.patch.lifecycle_state.is_some() {
        if should_replace(
            authority,
            event.observed_at,
            &row.lifecycle_authority,
            row.lifecycle_updated_at,
        ) {
            assign_if_some(&mut row.lifecycle_state, &event.patch.lifecycle_state);
            row.lifecycle_updated_at = event.observed_at;
            row.lifecycle_authority.clone_from(&authority_token);
            changed = true;
        }
    }

    if let Some(count) = event.patch.active_work_count {
        if should_replace(
            authority,
            event.observed_at,
            &row.workload_authority,
            row.workload_updated_at,
        ) {
            row.active_work_count = count;
            row.workload_updated_at = event.observed_at;
            row.workload_authority.clone_from(&authority_token);
            changed = true;
        }
    }

    if event.patch.attention_state.is_some() || event.patch.current_request_fingerprint.is_some() {
        if should_replace(
            authority,
            event.observed_at,
            &row.attention_authority,
            row.attention_updated_at,
        ) {
            assign_if_some(&mut row.attention_state, &event.patch.attention_state);
            if let Some(fingerprint) = &event.patch.current_request_fingerprint {
                row.current_request_fingerprint.clone_from(fingerprint);
            }
            row.attention_updated_at = event.observed_at;
            row.attention_authority.clone_from(&authority_token);
            changed = true;
        }
    }

    if let Some(health) = &event.patch.transport_health {
        if should_replace(
            authority,
            event.observed_at,
            &row.transport_authority,
            row.transport_updated_at,
        ) {
            row.transport_health.clone_from(health);
            row.transport_updated_at = event.observed_at;
            row.transport_authority = authority_token;
            changed = true;
        }
    }

    changed
}

fn should_replace(
    incoming: ObservationAuthority,
    observed_at: i64,
    stored_authority: &str,
    stored_at: i64,
) -> bool {
    let stored = ObservationAuthority::parse(stored_authority);
    incoming.rank() > stored.rank()
        || (incoming.rank() == stored.rank() && observed_at >= stored_at)
}

fn assign_if_some(target: &mut String, value: &Option<String>) {
    if let Some(value) = value {
        target.clone_from(value);
    }
}

fn assign_option_if_some(target: &mut Option<String>, value: &Option<String>) {
    if let Some(value) = value {
        *target = Some(value.clone());
    }
}

async fn insert_session(
    tx: &mut Transaction<'_, Sqlite>,
    row: &FleetSessionRow,
) -> Result<(), sqlx::Error> {
    let query = sqlx::query(
        "INSERT INTO fleet_session (session_key, provider, provider_session_id, \
            tmux_target, process_start_fingerprint, cwd, display_name, lifecycle_state, \
            attention_state, current_request_fingerprint, management_state, transport_health, capabilities, provenance, \
            confidence, discovered_at, last_observed_at, metadata_updated_at, \
            metadata_authority, lifecycle_updated_at, lifecycle_authority, \
            attention_updated_at, attention_authority, transport_updated_at, \
            transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    bind_session(query, row).execute(&mut **tx).await?;
    Ok(())
}

async fn update_session(
    tx: &mut Transaction<'_, Sqlite>,
    row: &FleetSessionRow,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE fleet_session SET \
            provider = ?, provider_session_id = ?, tmux_target = ?, \
            process_start_fingerprint = ?, cwd = ?, display_name = ?, \
            lifecycle_state = ?, attention_state = ?, current_request_fingerprint = ?, management_state = ?, \
            transport_health = ?, capabilities = ?, provenance = ?, confidence = ?, \
            discovered_at = ?, last_observed_at = ?, metadata_updated_at = ?, \
            metadata_authority = ?, lifecycle_updated_at = ?, lifecycle_authority = ?, \
            attention_updated_at = ?, attention_authority = ?, transport_updated_at = ?, \
            transport_authority = ?, active_work_count = ?, workload_updated_at = ?, workload_authority = ?, version = ?, updated_revision = ? \
         WHERE session_key = ?",
    )
    .bind(&row.provider)
    .bind(&row.provider_session_id)
    .bind(&row.tmux_target)
    .bind(&row.process_start_fingerprint)
    .bind(&row.cwd)
    .bind(&row.display_name)
    .bind(&row.lifecycle_state)
    .bind(&row.attention_state)
    .bind(&row.current_request_fingerprint)
    .bind(&row.management_state)
    .bind(&row.transport_health)
    .bind(&row.capabilities)
    .bind(&row.provenance)
    .bind(&row.confidence)
    .bind(row.discovered_at)
    .bind(row.last_observed_at)
    .bind(row.metadata_updated_at)
    .bind(&row.metadata_authority)
    .bind(row.lifecycle_updated_at)
    .bind(&row.lifecycle_authority)
    .bind(row.attention_updated_at)
    .bind(&row.attention_authority)
    .bind(row.transport_updated_at)
    .bind(&row.transport_authority)
    .bind(row.active_work_count)
    .bind(row.workload_updated_at)
    .bind(&row.workload_authority)
    .bind(row.version)
    .bind(row.updated_revision)
    .bind(&row.session_key)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn bind_session<'q>(
    query: sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    row: &'q FleetSessionRow,
) -> sqlx::query::Query<'q, Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    query
        .bind(&row.session_key)
        .bind(&row.provider)
        .bind(&row.provider_session_id)
        .bind(&row.tmux_target)
        .bind(&row.process_start_fingerprint)
        .bind(&row.cwd)
        .bind(&row.display_name)
        .bind(&row.lifecycle_state)
        .bind(&row.attention_state)
        .bind(&row.current_request_fingerprint)
        .bind(&row.management_state)
        .bind(&row.transport_health)
        .bind(&row.capabilities)
        .bind(&row.provenance)
        .bind(&row.confidence)
        .bind(row.discovered_at)
        .bind(row.last_observed_at)
        .bind(row.metadata_updated_at)
        .bind(&row.metadata_authority)
        .bind(row.lifecycle_updated_at)
        .bind(&row.lifecycle_authority)
        .bind(row.attention_updated_at)
        .bind(&row.attention_authority)
        .bind(row.transport_updated_at)
        .bind(&row.transport_authority)
        .bind(row.active_work_count)
        .bind(row.workload_updated_at)
        .bind(&row.workload_authority)
        .bind(row.version)
        .bind(row.updated_revision)
}

fn session_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<FleetSessionRow, sqlx::Error> {
    Ok(FleetSessionRow {
        session_key: row.try_get("session_key")?,
        provider: row.try_get("provider")?,
        provider_session_id: row.try_get("provider_session_id")?,
        tmux_target: row.try_get("tmux_target")?,
        process_start_fingerprint: row.try_get("process_start_fingerprint")?,
        cwd: row.try_get("cwd")?,
        display_name: row.try_get("display_name")?,
        lifecycle_state: row.try_get("lifecycle_state")?,
        attention_state: row.try_get("attention_state")?,
        current_request_fingerprint: row.try_get("current_request_fingerprint")?,
        management_state: row.try_get("management_state")?,
        transport_health: row.try_get("transport_health")?,
        capabilities: row.try_get("capabilities")?,
        provenance: row.try_get("provenance")?,
        confidence: row.try_get("confidence")?,
        discovered_at: row.try_get("discovered_at")?,
        last_observed_at: row.try_get("last_observed_at")?,
        metadata_updated_at: row.try_get("metadata_updated_at")?,
        metadata_authority: row.try_get("metadata_authority")?,
        lifecycle_updated_at: row.try_get("lifecycle_updated_at")?,
        lifecycle_authority: row.try_get("lifecycle_authority")?,
        attention_updated_at: row.try_get("attention_updated_at")?,
        attention_authority: row.try_get("attention_authority")?,
        transport_updated_at: row.try_get("transport_updated_at")?,
        transport_authority: row.try_get("transport_authority")?,
        active_work_count: row.try_get("active_work_count")?,
        workload_updated_at: row.try_get("workload_updated_at")?,
        workload_authority: row.try_get("workload_authority")?,
        version: row.try_get("version")?,
        updated_revision: row.try_get("updated_revision")?,
    })
}

fn event_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<FleetEventRow, sqlx::Error> {
    Ok(FleetEventRow {
        revision: row.try_get("revision")?,
        event_id: row.try_get("event_id")?,
        session_key: row.try_get("session_key")?,
        observed_at: row.try_get("observed_at")?,
        authority: row.try_get("authority")?,
        event_type: row.try_get("event_type")?,
        payload: row.try_get("payload")?,
        session_version: row.try_get("session_version")?,
        applied: row.try_get::<i64, _>("applied")? != 0,
    })
}

fn timeline_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<FleetTimelineRow, sqlx::Error> {
    Ok(FleetTimelineRow {
        revision: row.try_get("revision")?,
        session_key: row.try_get("session_key")?,
        observed_at: row.try_get("observed_at")?,
        authority: row.try_get("authority")?,
        event_type: row.try_get("event_type")?,
        session_version: row.try_get("session_version")?,
        applied: row.try_get::<i64, _>("applied")? != 0,
    })
}

fn receipt_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ActionReceiptRow, sqlx::Error> {
    Ok(ActionReceiptRow {
        request_id: row.try_get("request_id")?,
        session_key: row.try_get("session_key")?,
        action_kind: row.try_get("action_kind")?,
        action_fingerprint: row.try_get("action_fingerprint")?,
        expected_version: row.try_get("expected_version")?,
        idempotency_key: row.try_get("idempotency_key")?,
        status: row.try_get("status")?,
        detail: row.try_get("detail")?,
        session_version: row.try_get("session_version")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn event(
        event_id: &str,
        session_key: &str,
        at: i64,
        authority: ObservationAuthority,
        patch: FleetSessionPatch,
    ) -> NewFleetEvent {
        NewFleetEvent {
            event_id: event_id.to_string(),
            session_key: session_key.to_string(),
            observed_at: at,
            authority,
            event_type: "observation".to_string(),
            payload: "{}".to_string(),
            patch,
        }
    }

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        (dir, store)
    }

    #[tokio::test]
    async fn duplicate_event_is_idempotent() {
        let (_dir, store) = store().await;
        let input = event(
            "e-1",
            "claude:s-1",
            100,
            ObservationAuthority::Authoritative,
            FleetSessionPatch {
                provider: Some("claude".to_string()),
                lifecycle_state: Some("RUNNING".to_string()),
                ..FleetSessionPatch::default()
            },
        );
        let first = FleetRepo::apply_event(store.pool(), &input).await.unwrap();
        let replay = FleetRepo::apply_event(store.pool(), &input).await.unwrap();

        assert!(!first.duplicate);
        assert!(replay.duplicate);
        assert_eq!(replay.revision, first.revision);
        assert_eq!(replay.session_version, first.session_version);
        assert_eq!(
            FleetRepo::events_after(store.pool(), 0, 100).await.unwrap().len(),
            1
        );
    }

    #[tokio::test]
    async fn lifecycle_and_attention_advance_independently() {
        let (_dir, store) = store().await;
        FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-life",
                "claude:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("RUNNING".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        let result = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-attn",
                "claude:s-1",
                200,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    attention_state: Some("ASK".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.session.lifecycle_state, "RUNNING");
        assert_eq!(result.session.lifecycle_updated_at, 100);
        assert_eq!(result.session.attention_state, "ASK");
        assert_eq!(result.session.attention_updated_at, 200);
        assert_eq!(result.session.version, 2);
    }

    #[tokio::test]
    async fn inferred_event_never_overwrites_authoritative_group() {
        let (_dir, store) = store().await;
        FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-auth",
                "codex:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("RUNNING".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        let inferred = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-inferred",
                "codex:s-1",
                1_000,
                ObservationAuthority::Inferred,
                FleetSessionPatch {
                    lifecycle_state: Some("EXITED".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert!(!inferred.applied);
        assert_eq!(inferred.session.lifecycle_state, "RUNNING");
        assert_eq!(inferred.session.version, 1);
        assert_eq!(
            FleetRepo::events_after(store.pool(), 0, 100).await.unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn same_cwd_sessions_stay_distinct() {
        let (_dir, store) = store().await;
        for (event_id, key, provider) in [
            ("e-claude", "claude:same", "claude"),
            ("e-codex", "codex:same", "codex"),
        ] {
            FleetRepo::apply_event(
                store.pool(),
                &event(
                    event_id,
                    key,
                    100,
                    ObservationAuthority::Authoritative,
                    FleetSessionPatch {
                        provider: Some(provider.to_string()),
                        cwd: Some("/same/repo".to_string()),
                        ..FleetSessionPatch::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        let snapshot = FleetRepo::snapshot(store.pool()).await.unwrap();
        assert_eq!(snapshot.sessions.len(), 2);
        assert_eq!(snapshot.sessions[0].session_key, "claude:same");
        assert_eq!(snapshot.sessions[1].session_key, "codex:same");
        assert_eq!(snapshot.head_revision, 2);
    }

    #[tokio::test]
    async fn action_receipt_updates_status_but_rejects_identity_collision() {
        let (_dir, store) = store().await;
        let mut receipt = NewActionReceipt {
            request_id: "req-1".to_string(),
            session_key: "claude:s-1".to_string(),
            action_kind: "send_prompt".to_string(),
            action_fingerprint: "sha256:prompt".to_string(),
            expected_version: 1,
            idempotency_key: None,
            status: "PENDING".to_string(),
            detail: None,
            session_version: Some(1),
            created_at: 100,
            updated_at: 100,
        };
        FleetRepo::upsert_action_receipt(store.pool(), &receipt).await.unwrap();
        receipt.status = "DELIVERED".to_string();
        receipt.updated_at = 200;
        let delivered = FleetRepo::upsert_action_receipt(store.pool(), &receipt).await.unwrap();
        assert_eq!(delivered.status, "DELIVERED");
        assert_eq!(delivered.created_at, 100);

        receipt.session_key = "codex:other".to_string();
        assert!(matches!(
            FleetRepo::upsert_action_receipt(store.pool(), &receipt).await,
            Err(FleetRepoError::ReceiptCollision { .. })
        ));
    }

    #[tokio::test]
    async fn action_target_requires_current_version_and_request_fingerprint() {
        let (_dir, store) = store().await;
        let created = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-request",
                "claude:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    attention_state: Some("ASK".to_string()),
                    current_request_fingerprint: Some(Some("sha256:request".to_string())),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        FleetRepo::validate_action_target(
            store.pool(),
            "claude:s-1",
            created.session_version,
            Some("sha256:request"),
        )
        .await
        .unwrap();
        assert!(matches!(
            FleetRepo::validate_action_target(
                store.pool(),
                "claude:s-1",
                created.session_version + 1,
                Some("sha256:request"),
            )
            .await,
            Err(FleetRepoError::StaleVersion { .. })
        ));
        assert!(matches!(
            FleetRepo::validate_action_target(
                store.pool(),
                "claude:s-1",
                created.session_version,
                Some("sha256:other"),
            )
            .await,
            Err(FleetRepoError::RequestFingerprintMismatch { .. })
        ));

        let cleared = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-clear-request",
                "claude:s-1",
                200,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    attention_state: Some("NONE".to_string()),
                    current_request_fingerprint: Some(None),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(cleared.session.attention_state, "NONE");
        assert_eq!(cleared.session.current_request_fingerprint, None);
    }

    #[tokio::test]
    async fn subscription_projection_selects_payload_matching_current_request_fingerprint() {
        let (_dir, store) = store().await;
        let mut current = event(
            "e-current-request",
            "claude:s-1",
            200,
            ObservationAuthority::Authoritative,
            FleetSessionPatch {
                attention_state: Some("ASK".to_string()),
                current_request_fingerprint: Some(Some("fnv1a64:current".to_string())),
                ..FleetSessionPatch::default()
            },
        );
        current.event_type = "AskUserQuestion".to_string();
        current.payload = serde_json::json!({ "request": "current" }).to_string();
        FleetRepo::apply_event(store.pool(), &current).await.unwrap();

        let mut stale = event(
            "e-stale-request",
            "claude:s-1",
            100,
            ObservationAuthority::Authoritative,
            FleetSessionPatch {
                attention_state: Some("ASK".to_string()),
                current_request_fingerprint: Some(Some("fnv1a64:stale".to_string())),
                transport_health: Some("HEALTHY".to_string()),
                ..FleetSessionPatch::default()
            },
        );
        stale.event_type = "AskUserQuestion".to_string();
        stale.payload = serde_json::json!({ "request": "stale" }).to_string();
        let stale_result = FleetRepo::apply_event(store.pool(), &stale).await.unwrap();
        assert!(stale_result.applied);
        assert_eq!(
            stale_result.session.current_request_fingerprint.as_deref(),
            Some("fnv1a64:current")
        );

        let projection = FleetRepo::subscription_projection(store.pool(), 0, 100).await.unwrap();
        assert_eq!(projection.sessions.len(), 1);
        assert_eq!(
            projection.sessions[0].current_request.as_ref().unwrap()["request"],
            "current"
        );
    }
}
