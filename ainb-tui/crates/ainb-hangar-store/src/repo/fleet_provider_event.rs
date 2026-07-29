//! Durable raw provider-event ledger for Fleet projections.

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
        let row = Self::get(pool, &event.event_id)
            .await?
            .expect("successful insert or conflict must leave provider event row");
        if result.rows_affected() == 0 && !matches_event(&row, event, &digest) {
            return Err(FleetProviderEventError::EventIdCollision {
                event_id: event.event_id.clone(),
            });
        }
        Ok(row)
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
}
