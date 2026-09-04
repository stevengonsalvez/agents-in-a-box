//! Authoritative Fleet session read model, revision log, and action receipts.
//!
//! One transaction applies each normalized provider or discovery event. The
//! unique event id makes replay idempotent, the database assigns one global
//! revision, and accepted changes advance the target session's version. State
//! groups keep separate authority and timestamps so an inferred tmux sample
//! cannot replace an authoritative provider or hook value.

use std::future::Future;
use std::time::Duration;

use sqlx::{Row, Sqlite, SqlitePool, Transaction};

/// The BEGIN statement every write transaction in this module opens with.
///
/// `SQLite`'s default `BEGIN` is DEFERRED: it takes no lock until the first
/// statement. Every apply here READS before it WRITES (it must see the prior
/// event and the current session row to decide what to write), so a deferred
/// begin takes a read SNAPSHOT first and the later INSERT has to upgrade it. If
/// any other connection commits in that window the upgrade fails with
/// `SQLITE_BUSY` (5) or `SQLITE_BUSY_SNAPSHOT` (517), and `busy_timeout` does
/// **not** cover either: the busy handler is deliberately never invoked while
/// upgrading a read transaction, because waiting there could deadlock. The only
/// valid response is rollback-and-retry, the same mechanism
/// [`BoardRepo::auto_move_on_state`](crate::repo::board::BoardRepo::auto_move_on_state)
/// documents at length and [`crate::service::pull`] states generically.
///
/// This path is where it bit hardest. The daemon's tmux reconciler, hook ingest
/// and provider pollers all drive [`FleetRepo::apply_event`] against one pool,
/// so under load most applies lost the race; the caller then saw the session
/// state unchanged and re-enqueued the same observation, which is a
/// self-sustaining write storm (`fleet hook reduce failed` and hundreds of
/// `database is locked` lines per daemon log).
///
/// `BEGIN IMMEDIATE` takes the write lock at BEGIN, so there is no snapshot to
/// invalidate and ordinary contention IS covered by the pool's 10s
/// `busy_timeout`.
const IMMEDIATE_TRANSACTION: &str = "BEGIN IMMEDIATE";

/// How many times a write transaction is replayed before its error escapes.
///
/// Belt and braces to [`IMMEDIATE_TRANSACTION`]: taking the lock up front makes
/// the busy handler apply, and this covers the residue where even a 10s
/// `busy_timeout` expires. Five attempts spend under 100ms of backoff.
const WRITE_LOCK_ATTEMPTS: u32 = 5;

/// Run one write transaction, replaying it while `SQLite` reports lock
/// contention.
///
/// `attempt_once` must be self-contained (begin, write, commit): a rolled-back
/// transaction's reads are void, so a retry has to redo them, not just the write.
async fn with_write_lock_retry<F, Fut, T>(mut attempt_once: F) -> Result<T, FleetRepoError>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, FleetRepoError>>,
{
    let mut attempts: u32 = 0;
    loop {
        let outcome = attempt_once().await;
        let Err(FleetRepoError::Sql(error)) = &outcome else {
            return outcome;
        };
        attempts += 1;
        if attempts >= WRITE_LOCK_ATTEMPTS || !is_lock_contention(error) {
            return outcome;
        }
        tokio::time::sleep(retry_backoff(attempts)).await;
    }
}

/// Whether an error is transient lock contention rather than a real fault.
///
/// Matches on the EXTENDED result code sqlx surfaces: 5 `SQLITE_BUSY`,
/// 6 `SQLITE_LOCKED`, 261 `SQLITE_BUSY_RECOVERY`, 262 `SQLITE_LOCKED_SHAREDCACHE`,
/// 517 `SQLITE_BUSY_SNAPSHOT`. Everything else (constraint violations, decode
/// faults, corruption) must surface unchanged on the first attempt.
fn is_lock_contention(error: &sqlx::Error) -> bool {
    let Some(database) = error.as_database_error() else {
        return false;
    };
    let Some(code) = database.code() else {
        return false;
    };
    matches!(code.as_ref(), "5" | "6" | "261" | "262" | "517")
}

/// Jittered backoff before replaying a rolled-back write.
///
/// Doubles from 2ms; the jitter is what stops two contending daemon loops
/// re-colliding in lockstep on every attempt.
fn retry_backoff(attempt: u32) -> Duration {
    let base_ms = 1_u64 << attempt.min(5);
    Duration::from_millis(base_ms + rand::random::<u64>() % base_ms)
}

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
    /// Provider-reported model id, verbatim.
    pub model: Option<String>,
    /// Provider-reported reasoning effort, verbatim.
    pub reasoning_effort: Option<String>,
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
    /// Provider-reported model id, verbatim. `None` means never observed.
    pub model: Option<String>,
    /// Provider-reported reasoning effort, verbatim. `None` means never observed.
    pub reasoning_effort: Option<String>,
    /// Model group timestamp.
    pub model_updated_at: i64,
    /// Model group authority.
    pub model_authority: String,
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
        Self::apply_event_at_version(pool, event, None).await
    }

    /// Apply one normalized event only if the existing session has `expected_version`.
    ///
    /// Duplicate event ids remain idempotent and return before the version gate.
    /// This is for recovery mutations that must not overwrite a newer provider
    /// observation between inspection and commit.
    pub async fn apply_event_if_version(
        pool: &SqlitePool,
        event: &NewFleetEvent,
        expected_version: i64,
    ) -> Result<ApplyFleetEventResult, FleetRepoError> {
        Self::apply_event_at_version(pool, event, Some(expected_version)).await
    }

    async fn apply_event_at_version(
        pool: &SqlitePool,
        event: &NewFleetEvent,
        expected_version: Option<i64>,
    ) -> Result<ApplyFleetEventResult, FleetRepoError> {
        with_write_lock_retry(move || Self::apply_event_committed(pool, event, expected_version))
            .await
    }

    /// One IMMEDIATE transaction around [`Self::apply_event_in_tx`].
    ///
    /// The write lock is taken at BEGIN, before the first SELECT, so this
    /// transaction has no read snapshot to invalidate: see
    /// [`IMMEDIATE_TRANSACTION`] for why a DEFERRED begin here was the engine of
    /// the daemon's write storm.
    async fn apply_event_committed(
        pool: &SqlitePool,
        event: &NewFleetEvent,
        expected_version: Option<i64>,
    ) -> Result<ApplyFleetEventResult, FleetRepoError> {
        let mut tx = pool.begin_with(IMMEDIATE_TRANSACTION).await?;
        let result = Self::apply_event_in_tx(&mut tx, event, expected_version).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Apply one normalized event inside a CALLER-OWNED transaction, without
    /// committing it.
    ///
    /// This exists so a caller can make the `fleet_session` row and its
    /// provider-specific adjunct row commit TOGETHER. `fleet/acp_session_create`
    /// is that caller: an ACP session is one identity spread over two tables
    /// (`repo/fleet_acp_session.rs`), and a crash between two separate
    /// transactions would leave either a Fleet session no pool can drive or an
    /// ACP row the snapshot cannot see.
    ///
    /// # Caller contract: the transaction must already hold the write lock
    ///
    /// This function READS (`event_by_id`, `session_by_key_tx`) before it
    /// WRITES. A caller that hands it a DEFERRED transaction with no prior write
    /// makes those reads take a snapshot that the later INSERT must upgrade,
    /// which `SQLite` refuses with `SQLITE_BUSY`/`SQLITE_BUSY_SNAPSHOT` the
    /// moment any other connection commits in the window, uncovered by
    /// `busy_timeout`, see [`IMMEDIATE_TRANSACTION`]. Callers must therefore
    /// open with [`IMMEDIATE_TRANSACTION`] **or** issue their own write first.
    ///
    /// [`FleetAcpSessionRepo::insert_with_fleet_session`](crate::repo::fleet_acp_session::FleetAcpSessionRepo::insert_with_fleet_session)
    /// satisfies the contract the second way: its transaction's FIRST statement
    /// is the `fleet_acp_session` INSERT, so the write lock is already held by
    /// the time this runs and there is no snapshot to upgrade. That is why it is
    /// left on a plain `begin()` rather than converted here.
    pub(crate) async fn apply_event_in_tx(
        tx: &mut Transaction<'_, Sqlite>,
        event: &NewFleetEvent,
        expected_version: Option<i64>,
    ) -> Result<ApplyFleetEventResult, FleetRepoError> {
        if let Some(prior) = event_by_id(tx, &event.event_id).await? {
            if prior.session_key != event.session_key {
                return Err(FleetRepoError::EventIdCollision {
                    event_id: event.event_id.clone(),
                    existing_session: prior.session_key,
                });
            }
            let session = session_by_key_tx(tx, &event.session_key)
                .await?
                .expect("fleet event foreign key must resolve its session");
            return Ok(ApplyFleetEventResult {
                revision: prior.revision,
                session_version: prior.session_version,
                applied: prior.applied,
                duplicate: true,
                session,
            });
        }

        let prior = session_by_key_tx(tx, &event.session_key).await?;
        if let Some(expected) = expected_version {
            let actual = prior
                .as_ref()
                .ok_or_else(|| FleetRepoError::SessionNotFound {
                    session_key: event.session_key.clone(),
                })?
                .version;
            if actual != expected {
                return Err(FleetRepoError::StaleVersion {
                    session_key: event.session_key.clone(),
                    expected,
                    actual,
                });
            }
        }
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
            insert_session(tx, &session).await?;
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
        .execute(&mut **tx)
        .await?
        .last_insert_rowid();

        if changed {
            session.updated_revision = revision;
            update_session(tx, &session).await?;
        }

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
        with_write_lock_retry(move || {
            Self::supersede_session_committed(pool, legacy_key, managed_key, observed_at)
        })
        .await
    }

    /// One IMMEDIATE transaction performing the supersession.
    ///
    /// Same read-then-upgrade shape as [`Self::apply_event_committed`] (it
    /// reads the prior event and both sessions' rows before it writes), so it
    /// takes the write lock at BEGIN for the same reason. Replaying it is safe:
    /// the `event_id` is derived from the two keys, so a retry after a rollback
    /// re-runs the identical guards and inserts the same single event.
    async fn supersede_session_committed(
        pool: &SqlitePool,
        legacy_key: &str,
        managed_key: &str,
        observed_at: i64,
    ) -> Result<Option<i64>, FleetRepoError> {
        let event_id = format!("fleet-supersede:{legacy_key}:{managed_key}");
        let mut tx = pool.begin_with(IMMEDIATE_TRANSACTION).await?;
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

    /// Session keys a retention pass may demote out of the visible roster.
    ///
    /// "Archivable" is deliberately narrow: an `EXITED` row that is still
    /// visible, has never been superseded, and has gone unobserved past the
    /// caller's cutoff. Ordered oldest-first and capped so a caller drains a
    /// large backlog over several passes instead of one long write.
    ///
    /// # Errors
    /// Propagates the `SQLite` read failure.
    pub async fn list_archivable(
        pool: &SqlitePool,
        stale_before_ms: i64,
        limit: i64,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT session_key FROM fleet_session \
             WHERE visible = 1 AND superseded_by IS NULL \
               AND lifecycle_state = 'EXITED' AND last_observed_at < ? \
             ORDER BY last_observed_at ASC LIMIT ?",
        )
        .bind(stale_before_ms)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await
    }

    /// Demote one dead session out of the visible roster, keeping its identity.
    ///
    /// Modelled on [`Self::supersede_session`], with ONE deliberate difference:
    /// `superseded_by` stays NULL. That keeps the two hidden shapes separable
    /// forever — `visible = 0 AND superseded_by IS NULL` is archived,
    /// `superseded_by IS NOT NULL` is superseded — which is what lets
    /// [`apply_patch`]'s revival clause un-hide an archived row without ever
    /// resurrecting a superseded duplicate.
    ///
    /// Returns `None` when the row no longer satisfies the predicate, which is
    /// re-checked INSIDE the transaction: the candidate list is read outside it,
    /// so a hook can land in between and make the row live again.
    ///
    /// # Errors
    /// Propagates the `SQLite` write failure.
    pub async fn archive_session(
        pool: &SqlitePool,
        session_key: &str,
        observed_at: i64,
    ) -> Result<Option<i64>, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let version: Option<i64> = sqlx::query_scalar(
            "SELECT version FROM fleet_session WHERE session_key = ? \
             AND visible = 1 AND superseded_by IS NULL AND lifecycle_state = 'EXITED'",
        )
        .bind(session_key)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(version) = version else {
            tx.commit().await?;
            return Ok(None);
        };
        let next_version = version + 1;
        // Visibility ONLY. Unlike `supersede_session`, this deliberately does
        // not touch `last_observed_at`: archiving is the janitor noticing a
        // session, not anyone observing the SESSION, and stamping it with the
        // janitor's clock would destroy the one field that says when the thing
        // was last really alive — the field `list_archived` sorts on and an
        // operator reads. There is no other surviving record of it.
        sqlx::query("UPDATE fleet_session SET visible = 0, version = ? WHERE session_key = ?")
            .bind(next_version)
            .bind(session_key)
            .execute(&mut *tx)
            .await?;
        let revision = sqlx::query(
            "INSERT INTO fleet_event \
             (event_id, session_key, observed_at, authority, event_type, payload, \
              session_version, applied) VALUES (?, ?, ?, 'authoritative', \
              'session_archived', '{}', ?, 1)",
        )
        .bind(format!("fleet-archive:{session_key}:{next_version}"))
        .bind(session_key)
        .bind(observed_at)
        .bind(next_version)
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
        sqlx::query("UPDATE fleet_session SET updated_revision = ? WHERE session_key = ?")
            .bind(revision)
            .bind(session_key)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(revision))
    }

    /// Name the rows that were written before anything authored a
    /// `display_name`, using the caller's rule.
    ///
    /// A repair, not an observation: it fills a column that was NULL for the
    /// field's whole life without touching `version`, `updated_revision`, or any
    /// group's authority, so no client sees a state change and no event is
    /// logged for a name nobody changed. Rows the rule declines stay NULL and
    /// are retried on the next call.
    ///
    /// The rule is the CALLER's, so the label the operator sees has exactly one
    /// author and this layer stays free of presentation.
    ///
    /// # Errors
    /// Propagates the `SQLite` read or write failure.
    pub async fn backfill_display_names<F>(
        pool: &SqlitePool,
        derive: F,
    ) -> Result<usize, sqlx::Error>
    where
        F: Fn(&str) -> Option<String>,
    {
        let nameless: Vec<(String, String)> = sqlx::query_as(
            "SELECT session_key, cwd FROM fleet_session \
             WHERE display_name IS NULL OR display_name = ''",
        )
        .fetch_all(pool)
        .await?;
        let named: Vec<(String, String)> = nameless
            .into_iter()
            .filter_map(|(session_key, cwd)| Some((session_key, derive(&cwd)?)))
            .collect();
        if named.is_empty() {
            return Ok(0);
        }
        let mut tx = pool.begin().await?;
        for (session_key, display_name) in &named {
            sqlx::query("UPDATE fleet_session SET display_name = ? WHERE session_key = ?")
                .bind(display_name)
                .bind(session_key)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(named.len())
    }

    /// Read the archived roster, most recently observed first.
    ///
    /// Archiving hides a row from [`Self::snapshot`], it does not delete it, so
    /// this is the browse path that keeps a retired session reachable. Excludes
    /// superseded duplicates, which are hidden for an unrelated reason and have
    /// nothing to show an operator.
    ///
    /// # Errors
    /// Propagates the `SQLite` read failure.
    pub async fn list_archived(
        pool: &SqlitePool,
        limit: i64,
    ) -> Result<Vec<FleetSessionRow>, sqlx::Error> {
        let rows = sqlx::query(SESSION_SELECT_ARCHIVED).bind(limit.max(0)).fetch_all(pool).await?;
        rows.iter().map(session_from_row).collect()
    }

    /// The ERR roster: every visible, still-running session the read model
    /// currently projects as `attention_state = 'ERROR'`, key-ordered.
    ///
    /// `lifecycle_state != 'EXITED'` belongs in the predicate rather than in
    /// each caller, because an exited session's error is history: its pane is
    /// gone, so nothing can be typed into it and nothing about it will change
    /// again. Handing it to the daemon's retry sweep would spend that session's
    /// continue budget on sends that cannot land and end in an escalation
    /// naming a session the operator can no longer open.
    ///
    /// Deliberately unindexed. `attention_state` holds five values over a
    /// roster the archiver keeps in the low thousands, so a periodic scan is
    /// cheaper than an index every hook write would have to maintain.
    ///
    /// # Errors
    /// Propagates the `SQLite` read failure.
    pub async fn list_attention_error(
        pool: &SqlitePool,
    ) -> Result<Vec<FleetSessionRow>, sqlx::Error> {
        let rows = sqlx::query(SESSION_SELECT_ERRORING).fetch_all(pool).await?;
        rows.iter().map(session_from_row).collect()
    }

    /// The newest `limit` durable event payloads for one session, newest first.
    ///
    /// [`Self::events_for_session`] returns the WHOLE history, which is the
    /// right shape for a projection replay and the wrong one for a periodic
    /// scanner: `fleet_event` has been measured at 1.1M rows on a real host, so
    /// a caller that only needs "what did this session just say" would drag the
    /// entire log through memory every tick. Ordering by `revision DESC` walks
    /// `idx_fleet_event_session_revision` backwards, so the read touches `limit`
    /// index entries however long the session has been alive.
    ///
    /// # Errors
    /// Propagates the `SQLite` read failure.
    pub async fn recent_event_payloads(
        pool: &SqlitePool,
        session_key: &str,
        limit: i64,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT payload FROM fleet_event WHERE session_key = ? \
             ORDER BY revision DESC LIMIT ?",
        )
        .bind(session_key)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await
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

    /// Does this provider session still hold a request that is waiting on a
    /// human, an `ASK`/`APPROVAL` attention state with an identified request?
    ///
    /// The attention ingest's stale-ASK reconcile asks this before closing an
    /// open card. That reconcile infers "no longer asking" from the transcript,
    /// which cannot see an AskUserQuestion until the tool resolves, so on its own
    /// it would close a question the session is still blocked on. Fleet's own
    /// projection is the authority on whether a request is live.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn provider_session_holds_open_request(
        pool: &SqlitePool,
        provider_session_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let row = sqlx::query(
            "SELECT 1 FROM fleet_session \
             WHERE provider_session_id = ? \
               AND attention_state IN ('ASK', 'APPROVAL') \
               AND current_request_fingerprint IS NOT NULL \
             LIMIT 1",
        )
        .bind(provider_session_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.is_some())
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

    /// Read one session's durable event history in projection order.
    pub async fn events_for_session(
        pool: &SqlitePool,
        session_key: &str,
    ) -> Result<Vec<FleetEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT revision, event_id, session_key, observed_at, authority, event_type, \
                    payload, session_version, applied \
             FROM fleet_event WHERE session_key = ? ORDER BY revision ASC",
        )
        .bind(session_key)
        .fetch_all(pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    /// Read a bounded, payload-free Fleet timeline after a global revision.
    ///
    /// The query joins non-superseded Fleet sessions and filters the closed raw
    /// type allowlist before `LIMIT`, so excluded history can never consume a
    /// page or strand a caller's cursor.
    ///
    /// The join predicate is `superseded_by IS NULL`, NOT `visible = 1`. Those
    /// were the same set until archiving existed, and the intent was always the
    /// former: a superseded duplicate is hidden because its history is already
    /// reachable through the visible twin that replaced it, so showing it twice
    /// would be the bug. An ARCHIVED row has no twin — `visible = 1` here would
    /// erase its whole timeline, which is exactly the promise
    /// [`Self::list_archived`] makes to the operator.
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
             INNER JOIN fleet_session s \
                ON s.session_key = e.session_key AND s.superseded_by IS NULL \
             WHERE e.revision > ? \
               AND (? IS NULL OR e.session_key = ?) \
               AND e.event_type IN ( \
                   'SessionStart', 'UserPromptSubmit', 'PreToolUse', 'PostToolUse', \
                   'AskUserQuestion', 'PermissionRequest', 'Notification', 'Stop', \
                   'SubagentStop', 'StopFailure', 'SessionEnd', \
                   'codex_manager_unavailable', 'codex_manager_recovered', \
                   'codex_managed_tui_started', 'tmux_missing', 'tmux_unavailable', \
                   'tmux_available', 'tmux_discovered', 'session_superseded', \
                   'session_stale', 'session_archived' \
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
    transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision, \
    model, reasoning_effort, model_updated_at, model_authority \
    FROM fleet_session WHERE session_key = ?";

const SESSION_SELECT_ALL: &str = "SELECT session_key, provider, provider_session_id, \
    tmux_target, process_start_fingerprint, cwd, display_name, lifecycle_state, \
    attention_state, current_request_fingerprint, management_state, transport_health, capabilities, provenance, \
    confidence, discovered_at, last_observed_at, metadata_updated_at, \
    metadata_authority, lifecycle_updated_at, lifecycle_authority, \
    attention_updated_at, attention_authority, transport_updated_at, \
    transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision, \
    model, reasoning_effort, model_updated_at, model_authority \
    FROM fleet_session WHERE visible = 1 ORDER BY session_key ASC";

/// The ERR roster, read by [`FleetRepo::list_attention_error`]. Same column
/// list and same `visible = 1` gate as [`SESSION_SELECT_ALL`]: a superseded or
/// archived row is not a session anything may act on.
const SESSION_SELECT_ERRORING: &str = "SELECT session_key, provider, provider_session_id, \
    tmux_target, process_start_fingerprint, cwd, display_name, lifecycle_state, \
    attention_state, current_request_fingerprint, management_state, transport_health, capabilities, provenance, \
    confidence, discovered_at, last_observed_at, metadata_updated_at, \
    metadata_authority, lifecycle_updated_at, lifecycle_authority, \
    attention_updated_at, attention_authority, transport_updated_at, \
    transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision, \
    model, reasoning_effort, model_updated_at, model_authority \
    FROM fleet_session WHERE visible = 1 AND attention_state = 'ERROR' \
    AND lifecycle_state != 'EXITED' ORDER BY session_key ASC";

/// The archived roster: hidden by [`FleetRepo::archive_session`], NOT by
/// supersession. `superseded_by IS NULL` is the whole discriminator — the only
/// writer of that column is `supersede_session`.
const SESSION_SELECT_ARCHIVED: &str = "SELECT session_key, provider, provider_session_id, \
    tmux_target, process_start_fingerprint, cwd, display_name, lifecycle_state, \
    attention_state, current_request_fingerprint, management_state, transport_health, capabilities, provenance, \
    confidence, discovered_at, last_observed_at, metadata_updated_at, \
    metadata_authority, lifecycle_updated_at, lifecycle_authority, \
    attention_updated_at, attention_authority, transport_updated_at, \
    transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision, \
    model, reasoning_effort, model_updated_at, model_authority \
    FROM fleet_session WHERE visible = 0 AND superseded_by IS NULL \
    ORDER BY last_observed_at DESC LIMIT ?";

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
    let model_at = if event.patch.model.is_some() || event.patch.reasoning_effort.is_some() {
        event.observed_at
    } else {
        0
    };
    // Bound before the literal: the transport group MOVES `authority`, and the
    // model group is written after it.
    let authority_for_model = authority.clone();
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
        // Seeded from the patch like every other group: a session's FIRST event
        // is a real observation, and for a managed Codex thread it is the only
        // one that carries the pair until the settings feed fires.
        model: event.patch.model.clone(),
        reasoning_effort: event.patch.reasoning_effort.clone(),
        model_updated_at: model_at,
        model_authority: if model_at == 0 {
            "inferred".to_string()
        } else {
            authority_for_model
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

    // The model group is deliberately NOT folded into `has_metadata()`. The
    // metadata group is authoritative on every hook, so an inferred model
    // producer could never land against it; and a model-only observation must
    // not restamp an unrelated group's freshness.
    if (event.patch.model.is_some() || event.patch.reasoning_effort.is_some())
        && should_replace(
            authority,
            event.observed_at,
            &row.model_authority,
            row.model_updated_at,
        )
    {
        assign_option_if_some(&mut row.model, &event.patch.model);
        assign_option_if_some(&mut row.reasoning_effort, &event.patch.reasoning_effort);
        row.model_updated_at = event.observed_at;
        row.model_authority.clone_from(&authority_token);
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
            transport_authority, active_work_count, workload_updated_at, workload_authority, version, updated_revision, \
            model, reasoning_effort, model_updated_at, model_authority) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    );
    bind_session(query, row).execute(&mut **tx).await?;
    Ok(())
}

/// Persist one accepted patch, and REVIVE the row if it had been archived.
///
/// `visible = CASE WHEN superseded_by IS NULL THEN 1 ELSE visible END` is the
/// revival clause. `FleetRepo::archive_session` hides dead rows on a 24h clock;
/// without this, a session that comes back to life would stay invisible
/// forever. It must never un-hide a SUPERSEDED row, hence the guard on
/// `superseded_by` rather than a bare `visible = 1`.
///
/// Only reached when `apply_patch` returned `changed`, i.e. some state group
/// won its `should_replace` authority/recency check, and only for a NEW
/// `event_id` (a replayed duplicate returns early in `apply_event_in_tx`). So a
/// replay can never revive. Note the weaker half of that guarantee: the
/// comparison is against the winning GROUP's timestamp, not against the moment
/// of archiving, so an event older than the archive cutoff but newer than that
/// group can still revive a row. That is benign — visibility is not
/// correctness, and the next archive pass re-hides it.
async fn update_session(
    tx: &mut Transaction<'_, Sqlite>,
    row: &FleetSessionRow,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE fleet_session SET \
            visible = CASE WHEN superseded_by IS NULL THEN 1 ELSE visible END, \
            provider = ?, provider_session_id = ?, tmux_target = ?, \
            process_start_fingerprint = ?, cwd = ?, display_name = ?, \
            lifecycle_state = ?, attention_state = ?, current_request_fingerprint = ?, management_state = ?, \
            transport_health = ?, capabilities = ?, provenance = ?, confidence = ?, \
            discovered_at = ?, last_observed_at = ?, metadata_updated_at = ?, \
            metadata_authority = ?, lifecycle_updated_at = ?, lifecycle_authority = ?, \
            attention_updated_at = ?, attention_authority = ?, transport_updated_at = ?, \
            transport_authority = ?, active_work_count = ?, workload_updated_at = ?, workload_authority = ?, version = ?, updated_revision = ?, \
            model = ?, reasoning_effort = ?, model_updated_at = ?, model_authority = ? \
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
    .bind(&row.model)
    .bind(&row.reasoning_effort)
    .bind(row.model_updated_at)
    .bind(&row.model_authority)
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
        .bind(&row.model)
        .bind(&row.reasoning_effort)
        .bind(row.model_updated_at)
        .bind(&row.model_authority)
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
        model: row.try_get("model")?,
        reasoning_effort: row.try_get("reasoning_effort")?,
        model_updated_at: row.try_get("model_updated_at")?,
        model_authority: row.try_get("model_authority")?,
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

    /// The boot repair names rows written before `display_name` existed, and
    /// does it WITHOUT touching `version` or `updated_revision`.
    ///
    /// That invariant is the whole reason this is a repair rather than an
    /// observation: bumping either would mint a revision on every existing row
    /// at boot, and every connected client would re-snapshot a fleet-wide change
    /// for a name nobody actually changed. On the live host this pass covers
    /// 1724 rows, so the churn would not be small.
    #[tokio::test]
    async fn backfill_names_nameless_rows_without_minting_a_revision() {
        let (_dir, store) = store().await;
        for (key, cwd) in [
            ("claude:s-named", "/Users/dev/d/git/ai-coder-rules"),
            ("claude:s-declined", "/Users/dev"),
        ] {
            FleetRepo::apply_event(
                store.pool(),
                &event(
                    &format!("e-{key}"),
                    key,
                    100,
                    ObservationAuthority::Authoritative,
                    FleetSessionPatch {
                        provider: Some("claude".to_string()),
                        cwd: Some(cwd.to_string()),
                        ..FleetSessionPatch::default()
                    },
                ),
            )
            .await
            .unwrap();
        }

        let before = FleetRepo::get_session(store.pool(), "claude:s-named")
            .await
            .unwrap()
            .expect("seeded session");
        assert_eq!(before.display_name, None, "precondition: unnamed");

        // The caller owns the rule, exactly as the daemon passes its own.
        let derive = |cwd: &str| -> Option<String> {
            let trimmed = cwd.trim_end_matches('/');
            let path = std::path::Path::new(trimmed);
            if matches!(
                path.parent().and_then(std::path::Path::to_str),
                Some("/Users" | "/home")
            ) {
                return None;
            }
            path.file_name()
                .and_then(std::ffi::OsStr::to_str)
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        };

        let named = FleetRepo::backfill_display_names(store.pool(), derive).await.unwrap();
        assert_eq!(named, 1, "only the row the rule accepts is named");

        let after = FleetRepo::get_session(store.pool(), "claude:s-named")
            .await
            .unwrap()
            .expect("session survives");
        assert_eq!(after.display_name.as_deref(), Some("ai-coder-rules"));
        assert_eq!(
            (after.version, after.updated_revision),
            (before.version, before.updated_revision),
            "a repair must not mint a revision, or every client re-snapshots at boot"
        );

        let declined = FleetRepo::get_session(store.pool(), "claude:s-declined")
            .await
            .unwrap()
            .expect("declined session survives");
        assert_eq!(
            declined.display_name, None,
            "a cwd that would name the operator's home stays NULL rather than leaking identity"
        );

        // Idempotent: the named row is not rewritten, so a long-lived daemon
        // does no work per boot beyond the rows it still cannot name.
        let second = FleetRepo::backfill_display_names(store.pool(), derive).await.unwrap();
        assert_eq!(second, 0, "second boot names nothing new");
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
    async fn version_guard_rejects_recovery_after_newer_observation() {
        let (_dir, store) = store().await;
        let created = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-created",
                "claude:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    attention_state: Some("WAITING".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        let recovery = event(
            "e-recovery",
            "claude:s-1",
            200,
            ObservationAuthority::Authoritative,
            FleetSessionPatch {
                attention_state: Some("ASK".to_string()),
                ..FleetSessionPatch::default()
            },
        );

        assert!(matches!(
            FleetRepo::apply_event_if_version(
                store.pool(),
                &recovery,
                created.session_version + 1,
            )
            .await,
            Err(FleetRepoError::StaleVersion { .. })
        ));
        assert_eq!(
            FleetRepo::events_after(store.pool(), 0, 100).await.unwrap().len(),
            1,
            "stale recovery must append no event"
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
    async fn workload_update_does_not_replace_newer_lifecycle_state() {
        let (_dir, store) = store().await;
        FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-running",
                "codex:s-1",
                200,
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
                "e-workload",
                "codex:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    active_work_count: Some(1),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.session.lifecycle_state, "RUNNING");
        assert_eq!(result.session.lifecycle_updated_at, 200);
        assert_eq!(result.session.active_work_count, 1);
        assert_eq!(result.session.workload_updated_at, 100);
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

    /// Seed one session through a metadata patch and return its key.
    async fn seeded_session(store: &Store, key: &str, at: i64) {
        FleetRepo::apply_event(
            store.pool(),
            &event(
                &format!("e-seed-{key}"),
                key,
                at,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    provider: Some("claude".to_string()),
                    cwd: Some("/repo".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
    }

    /// The model group must be a state group of its own, not a rider on the
    /// metadata group's `has_metadata()` gate.
    ///
    /// A patch carrying ONLY a model is what every real producer emits — a hook
    /// that observed an effort, a transcript tail that observed a model. If it
    /// does not win a group, `apply_patch` returns `changed = false`, the row's
    /// `version` never moves and `updated_revision` is never set, so the macOS
    /// client (which re-snapshots per revision) never learns the model exists.
    /// The failure is total and completely silent.
    #[tokio::test]
    async fn model_only_patch_bumps_version_and_mints_revision() {
        let (_dir, store) = store().await;
        seeded_session(&store, "claude:s-1", 100).await;

        let result = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-model",
                "claude:s-1",
                200,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    model: Some("claude-opus-5".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert!(
            result.applied,
            "a model-only patch must win a state group of its own"
        );
        assert_eq!(result.session.version, 2, "the row version must advance");
        assert_eq!(
            result.session.updated_revision, result.revision,
            "the change must be pinned to a fresh revision, or no subscriber sees it"
        );
        assert_eq!(result.session.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(result.session.model_updated_at, 200);
        assert_eq!(result.session.model_authority, "authoritative");
    }

    /// A session's FIRST event takes the `new_session` path, which bypasses
    /// `apply_patch` entirely. Every other group seeds itself from the patch
    /// there; if the model group did not, a managed Codex thread seeded with
    /// its pair at spawn would land with an empty model and stay empty until
    /// the settings feed happened to fire.
    #[tokio::test]
    async fn model_on_a_first_event_seeds_the_new_row() {
        let (_dir, store) = store().await;
        let result = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-spawn",
                "codex:thread-1",
                400,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5.6-terra".to_string()),
                    reasoning_effort: Some("high".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert!(result.applied);
        assert_eq!(result.session.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(result.session.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(result.session.model_updated_at, 400);
        assert_eq!(result.session.model_authority, "authoritative");

        // And it is DURABLE, not just present on the returned row: the insert
        // and the re-read must agree.
        let stored = FleetRepo::get_session(store.pool(), "codex:thread-1")
            .await
            .unwrap()
            .expect("session persisted");
        assert_eq!(stored.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(stored.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(stored.model_updated_at, 400);
        assert_eq!(stored.model_authority, "authoritative");
    }

    /// A first event with no model leaves the group at its weakest prior, so
    /// the next observation of any authority can land.
    #[tokio::test]
    async fn first_event_without_model_leaves_the_group_unobserved() {
        let (_dir, store) = store().await;
        seeded_session(&store, "claude:s-1", 100).await;
        let stored = FleetRepo::get_session(store.pool(), "claude:s-1")
            .await
            .unwrap()
            .expect("session persisted");

        assert_eq!(stored.model, None);
        assert_eq!(stored.reasoning_effort, None);
        assert_eq!(stored.model_updated_at, 0);
        assert_eq!(stored.model_authority, "inferred");
    }

    /// The two fields share one group but are assigned independently: a later
    /// effort-only observation must not blank a model already known.
    #[tokio::test]
    async fn effort_only_patch_does_not_clear_model() {
        let (_dir, store) = store().await;
        seeded_session(&store, "claude:s-1", 100).await;
        FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-pair",
                "claude:s-1",
                200,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    model: Some("claude-opus-5".to_string()),
                    reasoning_effort: Some("high".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        let result = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-effort",
                "claude:s-1",
                300,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    reasoning_effort: Some("xhigh".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert!(result.applied);
        assert_eq!(
            result.session.model.as_deref(),
            Some("claude-opus-5"),
            "an effort-only observation says nothing about the model"
        );
        assert_eq!(result.session.reasoning_effort.as_deref(), Some("xhigh"));
        assert_eq!(result.session.model_updated_at, 300);
    }

    /// The whole reason the group carries its own authority: an inferred
    /// producer must never overwrite what a provider stated, however much later
    /// it observed.
    #[tokio::test]
    async fn inferred_model_cannot_clobber_authoritative() {
        let (_dir, store) = store().await;
        seeded_session(&store, "codex:s-1", 50).await;
        FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-auth-model",
                "codex:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    model: Some("gpt-5.6-terra".to_string()),
                    reasoning_effort: Some("high".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        let inferred = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-inferred-model",
                "codex:s-1",
                200,
                ObservationAuthority::Inferred,
                FleetSessionPatch {
                    model: Some("gpt-4".to_string()),
                    reasoning_effort: Some("low".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert!(!inferred.applied);
        assert_eq!(inferred.session.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(inferred.session.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(inferred.session.model_updated_at, 100);
        assert_eq!(inferred.session.model_authority, "authoritative");
    }

    /// Folding the model into `has_metadata()` would make every model-only
    /// observation restamp the metadata group's freshness, corrupting an
    /// unrelated group's authority clock. It must not.
    #[tokio::test]
    async fn model_group_does_not_disturb_metadata_freshness() {
        let (_dir, store) = store().await;
        seeded_session(&store, "claude:s-1", 100).await;

        let result = FleetRepo::apply_event(
            store.pool(),
            &event(
                "e-model",
                "claude:s-1",
                900,
                ObservationAuthority::Inferred,
                FleetSessionPatch {
                    model: Some("claude-opus-5".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert_eq!(result.session.model.as_deref(), Some("claude-opus-5"));
        assert_eq!(
            result.session.metadata_updated_at, 100,
            "the metadata group's clock belongs to the metadata group"
        );
        assert_eq!(
            result.session.metadata_authority, "authoritative",
            "an inferred model observation must not weaken metadata authority"
        );
        assert_eq!(result.session.cwd, "/repo");
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

    /// Two concurrent writers on ONE `Store` must never surface a lock error.
    ///
    /// This is the daemon's real shape: the tmux reconciler, the hook ingest and
    /// the provider poller all call [`FleetRepo::apply_event`] on the same pool
    /// for different sessions. With a DEFERRED `BEGIN` this test fails within a
    /// few dozen iterations with `(code: 517) database is locked`,
    /// `SQLITE_BUSY_SNAPSHOT`, raised when the sibling commits between this
    /// transaction's first SELECT and its first write, and NOT covered by
    /// `busy_timeout`. Every such failure is a dropped fleet event in production.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_writers_never_surface_a_lock_error() {
        /// Enough loops that the read-to-write window is crossed by the sibling
        /// many times over; the deferred version fails long before the end.
        const ITERATIONS: i64 = 150;

        let (_dir, store) = store().await;
        let writers: Vec<_> = (0..2)
            .map(|writer| {
                let pool = store.pool().clone();
                tokio::spawn(async move {
                    let mut errors = Vec::new();
                    for i in 0..ITERATIONS {
                        let lifecycle = if i % 2 == 0 { "RUNNING" } else { "IDLE" };
                        let input = event(
                            &format!("e-{writer}-{i}"),
                            &format!("claude:contended-{writer}"),
                            100 + i,
                            ObservationAuthority::Authoritative,
                            FleetSessionPatch {
                                provider: Some("claude".to_string()),
                                lifecycle_state: Some(lifecycle.to_string()),
                                ..FleetSessionPatch::default()
                            },
                        );
                        if let Err(error) = FleetRepo::apply_event(&pool, &input).await {
                            errors.push(format!("writer {writer} iteration {i}: {error}"));
                        }
                    }
                    errors
                })
            })
            .collect();

        let mut errors = Vec::new();
        for writer in writers {
            errors.extend(writer.await.unwrap());
        }
        assert!(
            errors.is_empty(),
            "{} of {} concurrent applies failed: {errors:#?}",
            errors.len(),
            ITERATIONS * 2
        );

        // The writes really landed: no silent no-op run that would pass trivially.
        for writer in 0..2 {
            let events =
                FleetRepo::events_for_session(store.pool(), &format!("claude:contended-{writer}"))
                    .await
                    .unwrap();
            assert_eq!(events.len(), usize::try_from(ITERATIONS).unwrap());
        }
    }

    /// A `SQLite` error carrying one extended result code, so the retry ladder
    /// is testable without racing a real database into a lock it no longer takes.
    #[derive(Debug)]
    struct CodedError(&'static str);

    impl std::fmt::Display for CodedError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "(code: {}) database is locked", self.0)
        }
    }

    impl std::error::Error for CodedError {}

    impl sqlx::error::DatabaseError for CodedError {
        fn message(&self) -> &str {
            "database is locked"
        }
        fn code(&self) -> Option<std::borrow::Cow<'_, str>> {
            Some(std::borrow::Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn coded(code: &'static str) -> FleetRepoError {
        FleetRepoError::Sql(sqlx::Error::Database(Box::new(CodedError(code))))
    }

    #[tokio::test]
    async fn write_lock_retry_replays_contention_then_gives_up() {
        for code in ["5", "6", "261", "262", "517"] {
            let mut attempts = 0_u32;
            let outcome: Result<(), _> = with_write_lock_retry(|| {
                attempts += 1;
                async move { Err(coded(code)) }
            })
            .await;
            assert_eq!(
                attempts, WRITE_LOCK_ATTEMPTS,
                "code {code} must be replayed"
            );
            assert!(outcome.is_err(), "the last error still escapes");
        }
    }

    #[tokio::test]
    async fn write_lock_retry_stops_on_the_first_success_and_never_replays_a_real_fault() {
        let mut attempts = 0_u32;
        let recovered = with_write_lock_retry(|| {
            attempts += 1;
            let contended = attempts < 3;
            async move {
                if contended {
                    Err(coded("517"))
                } else {
                    Ok(attempts)
                }
            }
        })
        .await;
        assert_eq!(recovered.unwrap(), 3, "the third attempt commits");

        // A constraint violation is a bug, not contention: it must surface at once.
        let mut faults = 0_u32;
        let outcome: Result<(), _> = with_write_lock_retry(|| {
            faults += 1;
            async move { Err(coded("1555")) }
        })
        .await;
        assert_eq!(faults, 1, "a non-lock error is never replayed");
        assert!(outcome.is_err());
    }

    /// Archiving a dead session must take it OUT of the roster every snapshot
    /// scans, while leaving it reachable by key and listed as archived.
    ///
    /// This is the whole point of the change: 1,440 of 1,472 visible rows on a
    /// measured profile were dead, and every 3s tick scanned all of them.
    #[tokio::test]
    async fn archiving_removes_a_dead_session_from_the_scanned_roster() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        for (id, key) in [("e-dead", "claude:dead"), ("e-live", "claude:live")] {
            FleetRepo::apply_event(
                pool,
                &event(
                    id,
                    key,
                    100,
                    ObservationAuthority::Authoritative,
                    FleetSessionPatch {
                        lifecycle_state: Some(
                            if key == "claude:dead" {
                                "EXITED"
                            } else {
                                "RUNNING"
                            }
                            .to_string(),
                        ),
                        ..FleetSessionPatch::default()
                    },
                ),
            )
            .await
            .unwrap();
        }

        let candidates = FleetRepo::list_archivable(pool, 200, 500).await.unwrap();
        assert_eq!(
            candidates,
            vec!["claude:dead".to_string()],
            "only the EXITED row is archivable"
        );

        let revision = FleetRepo::archive_session(pool, "claude:dead", 300).await.unwrap();
        assert!(revision.is_some(), "archiving must commit a revision");

        let scanned: Vec<String> = FleetRepo::snapshot(pool)
            .await
            .unwrap()
            .sessions
            .into_iter()
            .map(|row| row.session_key)
            .collect();
        assert_eq!(
            scanned,
            vec!["claude:live".to_string()],
            "the archived session must leave the roster SESSION_SELECT_ALL scans"
        );
        assert!(
            FleetRepo::get_session(pool, "claude:dead").await.unwrap().is_some(),
            "archived is not deleted — direct lookup by key must still resolve"
        );
        let archived: Vec<String> = FleetRepo::list_archived(pool, 50)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.session_key)
            .collect();
        assert_eq!(archived, vec!["claude:dead".to_string()]);
        assert_eq!(
            FleetRepo::list_archivable(pool, 400, 500).await.unwrap(),
            Vec::<String>::new(),
            "an archived row must never be re-archived"
        );
    }

    /// A session that comes back to life after being archived returns to the
    /// roster on its next real observation. Without this an archived session
    /// would be invisible forever.
    #[tokio::test]
    async fn a_new_observation_revives_an_archived_session() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        FleetRepo::apply_event(
            pool,
            &event(
                "e-dead",
                "claude:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("EXITED".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        FleetRepo::archive_session(pool, "claude:s-1", 300).await.unwrap();
        assert!(FleetRepo::snapshot(pool).await.unwrap().sessions.is_empty());

        let revived = FleetRepo::apply_event(
            pool,
            &event(
                "e-alive-again",
                "claude:s-1",
                400,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("RUNNING".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert!(revived.applied);
        let roster: Vec<String> = FleetRepo::snapshot(pool)
            .await
            .unwrap()
            .sessions
            .into_iter()
            .map(|row| row.session_key)
            .collect();
        assert_eq!(
            roster,
            vec!["claude:s-1".to_string()],
            "a revived session must return to the scanned roster"
        );
        assert!(
            FleetRepo::list_archived(pool, 50).await.unwrap().is_empty(),
            "and must leave the archived list"
        );
    }

    /// The revival clause must not resurrect a SUPERSEDED duplicate. Both
    /// shapes sit at `visible = 0`; only `superseded_by` tells them apart, and
    /// a superseded row still receives events (its history stays on its key).
    #[tokio::test]
    async fn revival_never_unhides_a_superseded_duplicate() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        for (id, key) in [
            ("e-legacy", "claude:legacy"),
            ("e-managed", "claude:managed"),
        ] {
            FleetRepo::apply_event(
                pool,
                &event(
                    id,
                    key,
                    100,
                    ObservationAuthority::Authoritative,
                    FleetSessionPatch {
                        management_state: Some("DEGRADED".to_string()),
                        ..FleetSessionPatch::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        FleetRepo::supersede_session(pool, "claude:legacy", "claude:managed", 200)
            .await
            .unwrap()
            .expect("supersede applies");

        FleetRepo::apply_event(
            pool,
            &event(
                "e-legacy-late",
                "claude:legacy",
                300,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("RUNNING".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        let roster: Vec<String> = FleetRepo::snapshot(pool)
            .await
            .unwrap()
            .sessions
            .into_iter()
            .map(|row| row.session_key)
            .collect();
        assert_eq!(
            roster,
            vec!["claude:managed".to_string()],
            "a superseded duplicate must stay hidden however many events it receives"
        );
        assert!(
            FleetRepo::list_archived(pool, 50).await.unwrap().is_empty(),
            "and must never be confused with an archived row"
        );
    }

    /// The candidate list is read outside the archiving transaction, so a hook
    /// can revive a row in between. The in-transaction re-check must catch it.
    #[tokio::test]
    async fn archiving_declines_a_session_revived_after_the_candidate_read() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        FleetRepo::apply_event(
            pool,
            &event(
                "e-dead",
                "claude:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("EXITED".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            FleetRepo::list_archivable(pool, 200, 500).await.unwrap().len(),
            1
        );

        // The hook that lands between the candidate read and the archive.
        FleetRepo::apply_event(
            pool,
            &event(
                "e-alive",
                "claude:s-1",
                200,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("RUNNING".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        assert_eq!(
            FleetRepo::archive_session(pool, "claude:s-1", 300).await.unwrap(),
            None,
            "a session that came back to life must not be archived on a stale candidate"
        );
        assert_eq!(FleetRepo::snapshot(pool).await.unwrap().sessions.len(), 1);
    }

    /// Archiving is a VISIBILITY change and must not rewrite when the session
    /// was last really seen.
    ///
    /// `last_observed_at` is the only surviving record of that moment once the
    /// row leaves the roster — it is what `list_archived` sorts on and what an
    /// operator reads to answer "when did this actually die". Stamping it with
    /// the janitor's clock would make every archived session claim it was alive
    /// until the sweep noticed it.
    #[tokio::test]
    async fn archiving_preserves_when_the_session_was_last_really_seen() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        FleetRepo::apply_event(
            pool,
            &event(
                "e-dead",
                "claude:s-1",
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    lifecycle_state: Some("EXITED".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        let last_alive = FleetRepo::get_session(pool, "claude:s-1").await.unwrap().unwrap();
        assert_eq!(last_alive.last_observed_at, 100);

        // The janitor runs a long time later, as it does on a 24h TTL.
        FleetRepo::archive_session(pool, "claude:s-1", 999_999).await.unwrap().unwrap();

        assert_eq!(
            FleetRepo::get_session(pool, "claude:s-1")
                .await
                .unwrap()
                .unwrap()
                .last_observed_at,
            100,
            "the janitor's clock must not overwrite the last real observation"
        );
        assert_eq!(
            FleetRepo::list_archived(pool, 50).await.unwrap()[0].last_observed_at,
            100,
            "and the archived listing must show that real time, not the sweep time"
        );
    }

    /// `discovered_at` records when the row was FIRST written and nothing may
    /// move it afterwards.
    ///
    /// Not a cosmetic property. The daemon breaks a tie between two rows
    /// claiming one tmux pane with `discovered_at` precisely because no sweep
    /// writes it (`fleet::pane_claim_rank`): `last_observed_at` cannot decide
    /// alone, since the missing-sweep's own write bumps the loser's to the
    /// winner's value and the resulting tie inverted pane ownership every tick.
    /// Making `discovered_at` mutable would silently restore that inversion from
    /// another crate, so the invariant is pinned here, where it lives.
    ///
    /// `update_session` rewrites the column on every applied event; this holds
    /// only because it binds the row's own value straight back and `apply_patch`
    /// never touches it.
    #[tokio::test]
    async fn discovered_at_records_first_sight_and_never_moves_again() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        FleetRepo::apply_event(
            pool,
            &event(
                "e-first",
                "claude:s-1",
                100,
                ObservationAuthority::Inferred,
                FleetSessionPatch {
                    tmux_target: Some("demo:1.1".to_string()),
                    lifecycle_state: Some("RUNNING".to_string()),
                    transport_health: Some("HEALTHY".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            FleetRepo::get_session(pool, "claude:s-1").await.unwrap().unwrap().discovered_at,
            100
        );

        // Every later authority, and the shape the missing-sweep writes.
        for (id, at, authority) in [
            ("e-hook", 500, ObservationAuthority::Authoritative),
            ("e-missing", 900, ObservationAuthority::Authoritative),
            ("e-inferred", 1_300, ObservationAuthority::Inferred),
        ] {
            FleetRepo::apply_event(
                pool,
                &event(
                    id,
                    "claude:s-1",
                    at,
                    authority,
                    FleetSessionPatch {
                        transport_health: Some(
                            if at == 900 { "UNAVAILABLE" } else { "HEALTHY" }.to_string(),
                        ),
                        ..FleetSessionPatch::default()
                    },
                ),
            )
            .await
            .unwrap();
        }

        // A backwards-dated event, which must not drag it down either.
        FleetRepo::apply_event(
            pool,
            &event(
                "e-late-arrival",
                "claude:s-1",
                50,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    display_name: Some("renamed".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();

        let row = FleetRepo::get_session(pool, "claude:s-1").await.unwrap().unwrap();
        assert_eq!(
            row.discovered_at, 100,
            "discovered_at is first sight, so no event may move it in either direction"
        );
        assert!(
            row.last_observed_at > row.discovered_at,
            "while last_observed_at does move, which is why it cannot break a pane tie alone"
        );

        // The two writers that bypass `apply_event` entirely.
        FleetRepo::apply_event(
            pool,
            &event(
                "e-managed",
                "claude:managed",
                1_400,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    management_state: Some("MANAGED".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        FleetRepo::apply_event(
            pool,
            &event(
                "e-legacy",
                "claude:legacy",
                1_500,
                ObservationAuthority::Inferred,
                FleetSessionPatch {
                    management_state: Some("DEGRADED".to_string()),
                    ..FleetSessionPatch::default()
                },
            ),
        )
        .await
        .unwrap();
        FleetRepo::supersede_session(pool, "claude:legacy", "claude:managed", 9_000)
            .await
            .unwrap()
            .expect("supersede applies");
        FleetRepo::archive_session(pool, "claude:s-1", 9_999).await.unwrap();

        for (key, expected) in [("claude:legacy", 1_500), ("claude:s-1", 100)] {
            assert_eq!(
                FleetRepo::get_session(pool, key).await.unwrap().unwrap().discovered_at,
                expected,
                "neither superseding nor archiving may restamp {key}"
            );
        }
    }

    /// An archived session keeps its browsable timeline; a superseded duplicate
    /// still does not get one of its own.
    ///
    /// `timeline_after` joined on `visible = 1`, which meant "not a superseded
    /// duplicate" right up until archiving made a second reason to be invisible.
    /// Getting this wrong silently empties the history of every archived
    /// session, contradicting what `list_archived` promises.
    #[tokio::test]
    async fn timeline_keeps_archived_history_and_still_hides_superseded_duplicates() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        for (id, key) in [
            ("e-archived", "claude:archived"),
            ("e-legacy", "claude:legacy"),
            ("e-managed", "claude:managed"),
        ] {
            let mut seed = event(
                id,
                key,
                100,
                ObservationAuthority::Authoritative,
                FleetSessionPatch {
                    management_state: Some("DEGRADED".to_string()),
                    lifecycle_state: Some("EXITED".to_string()),
                    ..FleetSessionPatch::default()
                },
            );
            seed.event_type = "SessionStart".to_string();
            FleetRepo::apply_event(pool, &seed).await.unwrap();
        }
        FleetRepo::archive_session(pool, "claude:archived", 300).await.unwrap().unwrap();
        FleetRepo::supersede_session(pool, "claude:legacy", "claude:managed", 300)
            .await
            .unwrap()
            .expect("supersede applies");

        let timeline = FleetRepo::timeline_after(pool, 0, None, 100).await.unwrap();
        let keys: std::collections::BTreeSet<&str> =
            timeline.iter().map(|row| row.session_key.as_str()).collect();

        assert!(
            keys.contains("claude:archived"),
            "an archived session's history must stay reachable — it has no visible twin"
        );
        assert!(
            !keys.contains("claude:legacy"),
            "a superseded duplicate stays filtered; its history lives on the twin"
        );
        assert!(keys.contains("claude:managed"));

        // Scoped to the archived key alone, the way a detail view asks.
        let scoped =
            FleetRepo::timeline_after(pool, 0, Some("claude:archived"), 100).await.unwrap();
        assert!(
            scoped.iter().any(|row| row.event_type == "session_archived"),
            "the archive itself must appear, or the timeline just stops with no reason given"
        );
    }
}
