//! Durable raw provider-event ledger for Fleet projections, and (since 0079)
//! the ACP transcript store.
//!
//! ## RETENTION: two regimes, split by `source`.
//!
//! **Projection sources (`source <> 'acp'`): never trimmed.** Like
//! `activity_log` (0059) and unlike `dispatch_attempt` (0058, bounded to
//! 20/issue), these rows are kept forever, on purpose. `raw_payload` is the
//! verbatim provider envelope, and the whole point of keeping it is that a
//! FUTURE reducer can replay history and re-derive Fleet state that today's
//! reducer does not extract. Trimming would silently destroy exactly the rows
//! that make a re-reduction possible, oldest-first. On these rows
//! `projection_revision IS NULL` marks pending recovery work which the Codex
//! manager replays on startup, not garbage; nothing may ever delete such a
//! row. Their measured growth (roughly 21 rows per 2 days at a ~344 byte mean
//! payload, ~1.3 MB per YEAR) remains negligible.
//!
//! **ACP transcript rows (`source = 'acp'`): eligible for operator-invoked
//! export-then-delete via [`FleetProviderEventRepo::delete_acp_before`].**
//! These rows carry NO `projection_revision` recovery contract (no reducer
//! ever projects them; `NULL` is their steady state, not pending work), which
//! is exactly what makes deleting them safe. They also invalidate the old
//! growth measurement by two to three orders of magnitude: one ACP turn emits
//! tens to low hundreds of coalesced chunk rows, so five sessions at twenty
//! turns a day at fifty rows a turn is ~5,000 rows and roughly 1.8 MB per
//! DAY, on the order of 650 MB per year. The old "never trim" stance is
//! therefore scoped to the projection sources above; there is still NO
//! automatic sweep, only the explicit operator export-then-delete.
//!
//! Revisit trigger: `source='acp'` rows exceeding ~1M rows or ~100 MB on a
//! real profile without the export-then-delete command having shipped, or ACP
//! write volume visibly contending the single `SQLite` writer (watch the
//! commits-issued counter).
//!
//! The pending-recovery partial index keeps its predicate
//! (`WHERE projection_revision IS NULL`) across 0079; ACP rows are pushed out
//! of the recovery scan's way by the index KEY
//! (`source, provider, projection_revision`), never by a predicate the
//! planner could not prove for a bound `source = ?` parameter.

use blake3::Hasher;
use sqlx::{Row, SqlitePool};

/// One raw provider envelope awaiting or linked to Fleet projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFleetProviderEvent {
    /// Replay-safe identity minted by provider or source adapter.
    pub event_id: String,
    /// Provider token, for example `claude` or `codex`.
    pub provider: String,
    /// Source transport, for example `claude_hook` or `codex_app_server`.
    pub source: String,
    /// Fleet session identity when known.
    pub session_key: Option<String>,
    /// Provider-owned session identity when known.
    pub provider_session_id: Option<String>,
    /// Provider observation time in epoch milliseconds.
    pub observed_at: i64,
    /// Local source receipt time in epoch milliseconds.
    pub received_at: i64,
    /// Provider event discriminator.
    pub event_type: String,
    /// Exact source payload, never a normalized projection body.
    pub raw_payload: String,
}

/// Persisted raw provider envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetProviderEventRow {
    /// Monotonic receipt order assigned by SQLite.
    pub ingest_order: i64,
    /// Replay-safe event identity.
    pub event_id: String,
    /// Provider token.
    pub provider: String,
    /// Source transport.
    pub source: String,
    /// Fleet session identity when known.
    pub session_key: Option<String>,
    /// Provider session identity when known.
    pub provider_session_id: Option<String>,
    /// Provider observation time.
    pub observed_at: i64,
    /// Local receipt time.
    pub received_at: i64,
    /// Provider event discriminator.
    pub event_type: String,
    /// Exact source payload.
    pub raw_payload: String,
    /// Content digest of raw payload.
    pub raw_blake3: String,
    /// Fleet projection revision once reduced.
    pub projection_revision: Option<i64>,
}

/// Source-ledger failures that must stop a replay cursor.
#[derive(Debug, thiserror::Error)]
pub enum FleetProviderEventError {
    /// Existing ID points to a different source envelope.
    #[error("provider event id {event_id:?} conflicts with a different envelope")]
    EventIdCollision {
        /// Conflicting event identity.
        event_id: String,
    },
    /// The requested source event is absent from the durable ledger.
    #[error("provider event {event_id:?} was not found")]
    EventNotFound {
        /// Missing event identity.
        event_id: String,
    },
    /// SQLite failed.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

/// Typed access to the raw provider-event ledger.
pub struct FleetProviderEventRepo;

impl FleetProviderEventRepo {
    /// Insert one source envelope. Exact replays return its original row.
    pub async fn append(
        pool: &SqlitePool,
        event: &NewFleetProviderEvent,
    ) -> Result<FleetProviderEventRow, FleetProviderEventError> {
        let digest = digest(&event.raw_payload);
        let result = sqlx::query(
            "INSERT INTO fleet_provider_event (event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(event_id) DO NOTHING",
        )
        .bind(&event.event_id)
        .bind(&event.provider)
        .bind(&event.source)
        .bind(&event.session_key)
        .bind(&event.provider_session_id)
        .bind(event.observed_at)
        .bind(event.received_at)
        .bind(&event.event_type)
        .bind(&event.raw_payload)
        .bind(&digest)
        .execute(pool)
        .await?;
        let row = Self::get(pool, &event.event_id).await?.ok_or_else(|| {
            FleetProviderEventError::EventNotFound {
                event_id: event.event_id.clone(),
            }
        })?;
        if result.rows_affected() == 0 && !matches_event(&row, event, &digest) {
            return Err(FleetProviderEventError::EventIdCollision {
                event_id: event.event_id.clone(),
            });
        }
        Ok(row)
    }

    /// Insert a whole batch of source envelopes in ONE transaction, returning
    /// the highest `ingest_order` the batch owns.
    ///
    /// This exists for the ACP transcript hot path (plan Phase 4 commit
    /// cadence). Per-row [`Self::append`] means one transaction per chunk, and
    /// every transaction takes the single `SQLite` write lock shared with the
    /// fleet event log, the claim loop and the outbox drain (`store.rs:77-90`,
    /// whose comment warns a contended writer can exhaust its 10 s
    /// `busy_timeout`). Coalescing bounds the ROW count; only a batched commit
    /// bounds the COMMIT count.
    ///
    /// `ON CONFLICT DO UPDATE SET event_id = excluded.event_id` is a deliberate
    /// no-op write: it changes no column value (the conflict key equals the
    /// excluded key) but makes `RETURNING` fire on the replay leg too, so a
    /// batch containing an already-committed `event_id` still reports that
    /// row's true `ingest_order` instead of a stale `last_insert_rowid`.
    ///
    /// The returned row is then compared against the incoming envelope, so
    /// BOTH doors into this ledger enforce one contract: an exact replay is a
    /// no-op, and the same `event_id` carrying a DIFFERENT envelope is
    /// [`FleetProviderEventError::EventIdCollision`], never a silently
    /// discarded payload. The whole batch's transaction rolls back with it.
    pub async fn append_batch(
        pool: &SqlitePool,
        events: &[NewFleetProviderEvent],
    ) -> Result<Option<i64>, FleetProviderEventError> {
        if events.is_empty() {
            return Ok(None);
        }
        let mut tx = pool.begin().await?;
        let mut high_water: Option<i64> = None;
        for event in events {
            let digest = digest(&event.raw_payload);
            let stored = sqlx::query(
                "INSERT INTO fleet_provider_event (event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(event_id) DO UPDATE SET event_id = excluded.event_id \
                 RETURNING ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision",
            )
            .bind(&event.event_id)
            .bind(&event.provider)
            .bind(&event.source)
            .bind(&event.session_key)
            .bind(&event.provider_session_id)
            .bind(event.observed_at)
            .bind(event.received_at)
            .bind(&event.event_type)
            .bind(&event.raw_payload)
            .bind(&digest)
            .fetch_one(&mut *tx)
            .await?;
            let row = row_from(&stored)?;
            if !matches_event(&row, event, &digest) {
                return Err(FleetProviderEventError::EventIdCollision {
                    event_id: event.event_id.clone(),
                });
            }
            let order = row.ingest_order;
            high_water = Some(high_water.map_or(order, |current: i64| current.max(order)));
        }
        tx.commit().await?;
        Ok(high_water)
    }

    /// Link a source envelope to the revision that reduced it. Replays preserve
    /// the first committed projection, but a conflicting revision is rejected.
    pub async fn mark_projected(
        pool: &SqlitePool,
        event_id: &str,
        revision: i64,
    ) -> Result<(), FleetProviderEventError> {
        let result = sqlx::query(
            "UPDATE fleet_provider_event SET projection_revision = ? WHERE event_id = ? AND (projection_revision IS NULL OR projection_revision = ?)",
        )
        .bind(revision)
        .bind(event_id)
        .bind(revision)
        .execute(pool)
        .await?;
        if result.rows_affected() == 0 {
            if Self::get(pool, event_id).await?.is_none() {
                return Err(FleetProviderEventError::EventNotFound {
                    event_id: event_id.to_string(),
                });
            }
            return Err(FleetProviderEventError::EventIdCollision {
                event_id: event_id.to_string(),
            });
        }
        Ok(())
    }

    /// Fetch one persisted source envelope.
    pub async fn get(
        pool: &SqlitePool,
        event_id: &str,
    ) -> Result<Option<FleetProviderEventRow>, sqlx::Error> {
        sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision FROM fleet_provider_event WHERE event_id = ?",
        )
        .bind(event_id)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(row_from)
        .transpose()
    }

    /// List source envelopes that still require projection, oldest receipt first.
    pub async fn unprojected(
        pool: &SqlitePool,
        provider: &str,
        source: &str,
        limit: i64,
    ) -> Result<Vec<FleetProviderEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event WHERE provider = ? AND source = ? AND projection_revision IS NULL \
             ORDER BY ingest_order ASC LIMIT ?",
        )
        .bind(provider)
        .bind(source)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from).collect()
    }

    /// Newest committed `ingest_order` for one session, or `None` on an empty
    /// transcript. The subscribe acknowledgement's head cursor.
    pub async fn head_order_for_session(
        pool: &SqlitePool,
        session_key: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT MAX(ingest_order) FROM fleet_provider_event WHERE session_key = ?",
        )
        .bind(session_key)
        .fetch_one(pool)
        .await
    }

    /// Page one session's rows after an `ingest_order` cursor, oldest first
    /// (the transcript read model; rides `idx_fleet_provider_event_session_order`).
    pub async fn list_by_session_after(
        pool: &SqlitePool,
        session_key: &str,
        after_order: i64,
        limit: i64,
    ) -> Result<Vec<FleetProviderEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event WHERE session_key = ? AND ingest_order > ? \
             ORDER BY ingest_order ASC LIMIT ?",
        )
        .bind(session_key)
        .bind(after_order)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from).collect()
    }

    /// The operator export-then-delete leg: remove one session's ACP
    /// transcript rows below `before_ingest_order`, returning the count.
    ///
    /// Two refusal rules, both enforced by the statement itself:
    /// - a row with `source <> 'acp'` is never touched, regardless of the
    ///   range asked for;
    /// - therefore no row carrying the `projection_revision IS NULL` pending
    ///   recovery contract is ever touched, because that contract exists only
    ///   on the non-ACP projection sources (see the header: on ACP rows NULL
    ///   is the steady state, which is what makes them deletable at all).
    pub async fn delete_acp_before(
        pool: &SqlitePool,
        session_key: &str,
        before_ingest_order: i64,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM fleet_provider_event \
             WHERE session_key = ? AND ingest_order < ? AND source = 'acp'",
        )
        .bind(session_key)
        .bind(before_ingest_order)
        .execute(pool)
        .await?;
        Ok(result.rows_affected())
    }
}

fn digest(payload: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(payload.as_bytes());
    hasher.finalize().to_hex().to_string()
}

fn matches_event(
    row: &FleetProviderEventRow,
    event: &NewFleetProviderEvent,
    raw_blake3: &str,
) -> bool {
    row.provider == event.provider
        && row.source == event.source
        && row.session_key == event.session_key
        && row.provider_session_id == event.provider_session_id
        && row.observed_at == event.observed_at
        && row.event_type == event.event_type
        && row.raw_payload == event.raw_payload
        && row.raw_blake3 == raw_blake3
}

fn row_from(row: &sqlx::sqlite::SqliteRow) -> Result<FleetProviderEventRow, sqlx::Error> {
    Ok(FleetProviderEventRow {
        ingest_order: row.try_get("ingest_order")?,
        event_id: row.try_get("event_id")?,
        provider: row.try_get("provider")?,
        source: row.try_get("source")?,
        session_key: row.try_get("session_key")?,
        provider_session_id: row.try_get("provider_session_id")?,
        observed_at: row.try_get("observed_at")?,
        received_at: row.try_get("received_at")?,
        event_type: row.try_get("event_type")?,
        raw_payload: row.try_get("raw_payload")?,
        raw_blake3: row.try_get("raw_blake3")?,
        projection_revision: row.try_get("projection_revision")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn event(id: &str, payload: &str) -> NewFleetProviderEvent {
        NewFleetProviderEvent {
            event_id: id.to_string(),
            provider: "claude".to_string(),
            source: "claude_hook".to_string(),
            session_key: Some("claude:session-1".to_string()),
            provider_session_id: Some("session-1".to_string()),
            observed_at: 100,
            received_at: 101,
            event_type: "PostToolUse".to_string(),
            raw_payload: payload.to_string(),
        }
    }

    #[tokio::test]
    async fn source_event_replay_preserves_exact_raw_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let input = event("source-1", r#"{\"large\":\"payload\"}"#);
        let first = FleetProviderEventRepo::append(store.pool(), &input).await.unwrap();
        let replay = FleetProviderEventRepo::append(store.pool(), &input).await.unwrap();

        assert_eq!(first, replay);
        assert_eq!(first.raw_payload, input.raw_payload);
        assert_eq!(first.ingest_order, 1);
    }

    #[tokio::test]
    async fn append_batch_commits_once_and_reports_the_high_water_mark() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let batch: Vec<NewFleetProviderEvent> =
            (0..5).map(|index| event(&format!("acp-{index}"), "{}")).collect();

        let high_water = FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();

        assert_eq!(high_water, Some(5));
        let rows =
            FleetProviderEventRepo::list_by_session_after(store.pool(), "claude:session-1", 0, 100)
                .await
                .unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|row| row.raw_blake3.len() == 64));
    }

    #[tokio::test]
    async fn append_batch_replay_is_a_no_op_that_still_reports_the_original_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let batch = vec![event("acp-1", "first")];

        let first = FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();
        // The `DO UPDATE` is a no-op assignment, present only so RETURNING
        // fires on the replay leg.
        let replay = FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();

        assert_eq!(first, replay);
        let stored = FleetProviderEventRepo::get(store.pool(), "acp-1").await.unwrap().unwrap();
        assert_eq!(stored.raw_payload, "first");
    }

    /// Both doors enforce one contract: the batch leg rejects a reused
    /// `event_id` carrying a different envelope exactly as
    /// `source_event_id_rejects_different_raw_payload` pins for `append`,
    /// and the whole batch rolls back with it.
    #[tokio::test]
    async fn append_batch_rejects_a_reused_event_id_with_a_different_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        FleetProviderEventRepo::append_batch(store.pool(), &[event("acp-1", "first")])
            .await
            .unwrap();

        let collision = FleetProviderEventRepo::append_batch(
            store.pool(),
            &[event("acp-2", "fresh"), event("acp-1", "second")],
        )
        .await;

        assert!(matches!(
            collision,
            Err(FleetProviderEventError::EventIdCollision { .. })
        ));
        let stored = FleetProviderEventRepo::get(store.pool(), "acp-1").await.unwrap().unwrap();
        assert_eq!(stored.raw_payload, "first", "the stored payload is intact");
        assert!(
            FleetProviderEventRepo::get(store.pool(), "acp-2").await.unwrap().is_none(),
            "the batch's other row rolled back with the collision"
        );
    }

    #[tokio::test]
    async fn append_batch_of_nothing_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert_eq!(
            FleetProviderEventRepo::append_batch(store.pool(), &[]).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn source_event_replay_ignores_local_receipt_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let first = FleetProviderEventRepo::append(store.pool(), &event("source-1", "{}"))
            .await
            .unwrap();
        let mut replay = event("source-1", "{}");
        replay.received_at = 9_999;

        assert_eq!(
            FleetProviderEventRepo::append(store.pool(), &replay).await.unwrap(),
            first,
        );
    }

    #[tokio::test]
    async fn source_event_id_rejects_different_raw_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        FleetProviderEventRepo::append(store.pool(), &event("source-1", "first"))
            .await
            .unwrap();

        assert!(matches!(
            FleetProviderEventRepo::append(store.pool(), &event("source-1", "second")).await,
            Err(FleetProviderEventError::EventIdCollision { .. })
        ));
    }

    #[tokio::test]
    async fn unknown_projection_event_is_not_a_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert!(matches!(
            FleetProviderEventRepo::mark_projected(store.pool(), "missing", 1).await,
            Err(FleetProviderEventError::EventNotFound { .. })
        ));
    }

    fn acp_event(id: &str, session_key: &str) -> NewFleetProviderEvent {
        NewFleetProviderEvent {
            event_id: id.to_string(),
            provider: "claude-agent-acp".to_string(),
            source: "acp".to_string(),
            session_key: Some(session_key.to_string()),
            provider_session_id: Some("adapter-1".to_string()),
            observed_at: 100,
            received_at: 101,
            event_type: "acp.message".to_string(),
            raw_payload: format!("{{\"chunk\":\"{id}\"}}"),
        }
    }

    /// Seed both ACP transcript rows and the classic projection-source rows,
    /// so the plans and deletions below are asserted against mixed content.
    async fn seed_mixed(store: &Store) {
        for i in 0..5 {
            FleetProviderEventRepo::append(
                store.pool(),
                &acp_event(&format!("acp-{i}"), "acp:s-1"),
            )
            .await
            .unwrap();
        }
        FleetProviderEventRepo::append(store.pool(), &event("codex-1", "{}"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn every_row_carries_a_valid_raw_blake3_digest() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let row = FleetProviderEventRepo::append(store.pool(), &acp_event("acp-1", "acp:s-1"))
            .await
            .unwrap();
        assert_eq!(row.raw_blake3.len(), 64);
        assert_eq!(row.raw_blake3, digest(&row.raw_payload));
    }

    /// I10: the REAL consumer query still plans onto the recreated partial
    /// index with ACP rows present. Asserting the index merely EXISTS is not
    /// enough; only the plan proves the recovery scan did not degrade to a
    /// full table scan over the table ACP transcripts inflate.
    #[tokio::test]
    async fn recovery_scan_plans_onto_the_projection_index_with_acp_rows_present() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_mixed(&store).await;

        // The exact SQL `unprojected` runs, prefixed with EXPLAIN QUERY PLAN.
        let rows = sqlx::query(
            "EXPLAIN QUERY PLAN \
             SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event WHERE provider = ? AND source = ? AND projection_revision IS NULL \
             ORDER BY ingest_order ASC LIMIT ?",
        )
        .bind("codex")
        .bind("codex_app_server")
        .bind(10_i64)
        .fetch_all(store.pool())
        .await
        .unwrap();
        let detail = rows
            .iter()
            .map(|row| row.try_get::<String, _>("detail").unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            detail.contains("idx_fleet_provider_event_projection"),
            "the consumer query must use the partial index, plan was:\n{detail}"
        );
    }

    #[tokio::test]
    async fn list_by_session_after_pages_one_session_by_ingest_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_mixed(&store).await;
        FleetProviderEventRepo::append(store.pool(), &acp_event("acp-other", "acp:s-2"))
            .await
            .unwrap();

        let page = FleetProviderEventRepo::list_by_session_after(store.pool(), "acp:s-1", 2, 2)
            .await
            .unwrap();
        assert_eq!(
            page.iter().map(|r| r.event_id.as_str()).collect::<Vec<_>>(),
            vec!["acp-2", "acp-3"],
            "cursor-exclusive, limit-bounded, single-session"
        );
    }

    /// The export-then-delete leg removes ONLY `source='acp'` rows below the
    /// watermark; every row carrying the pending-recovery contract survives.
    #[tokio::test]
    async fn delete_acp_before_removes_only_acp_rows_below_the_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_mixed(&store).await;
        let watermark = FleetProviderEventRepo::get(store.pool(), "acp-3")
            .await
            .unwrap()
            .unwrap()
            .ingest_order;

        let deleted = FleetProviderEventRepo::delete_acp_before(store.pool(), "acp:s-1", watermark)
            .await
            .unwrap();
        assert_eq!(deleted, 3, "acp-0..2 fall below the watermark");
        assert!(FleetProviderEventRepo::get(store.pool(), "acp-0").await.unwrap().is_none());
        assert!(FleetProviderEventRepo::get(store.pool(), "acp-3").await.unwrap().is_some());
        assert!(FleetProviderEventRepo::get(store.pool(), "acp-4").await.unwrap().is_some());
    }

    /// Refusal rule one: a non-ACP row is never touched, even when the caller
    /// hands a range that covers it.
    #[tokio::test]
    async fn delete_acp_before_refuses_a_non_acp_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let codex = FleetProviderEventRepo::append(store.pool(), &event("codex-1", "{}"))
            .await
            .unwrap();

        let deleted = FleetProviderEventRepo::delete_acp_before(
            store.pool(),
            codex.session_key.as_deref().unwrap(),
            codex.ingest_order + 1,
        )
        .await
        .unwrap();
        assert_eq!(deleted, 0);
        assert!(
            FleetProviderEventRepo::get(store.pool(), "codex-1").await.unwrap().is_some(),
            "a non-acp row survives an in-range delete"
        );
    }

    /// Refusal rule two: a row marking pending recovery work
    /// (`projection_revision IS NULL` on a projection source) is never
    /// touched. On ACP rows NULL is the steady state, not pending work, so the
    /// guard is the `source` filter itself; this pins the recovery-contract
    /// half of it.
    #[tokio::test]
    async fn delete_acp_before_refuses_a_pending_recovery_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pending = FleetProviderEventRepo::append(store.pool(), &event("codex-pending", "{}"))
            .await
            .unwrap();
        assert_eq!(pending.projection_revision, None, "seeded as pending work");

        let deleted = FleetProviderEventRepo::delete_acp_before(
            store.pool(),
            pending.session_key.as_deref().unwrap(),
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(deleted, 0);
        assert!(
            FleetProviderEventRepo::get(store.pool(), "codex-pending")
                .await
                .unwrap()
                .is_some(),
            "pending recovery work survives any delete range"
        );
    }
}
