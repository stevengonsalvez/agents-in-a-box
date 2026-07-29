//! Durable active provider child-work projection.

use sqlx::SqlitePool;

/// One provider child-work state transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetWorkUpdate {
    /// Provider token.
    pub provider: String,
    /// Parent Fleet session identity.
    pub session_key: String,
    /// Stable child identity supplied by provider.
    pub work_key: String,
    /// Subagent, task, or child-thread.
    pub kind: String,
    /// True at start, false at completion.
    pub active: bool,
    /// Source event identity for replay.
    pub event_id: String,
    /// Observation time.
    pub observed_at: i64,
}

/// Fleet child-work repository.
pub struct FleetWorkRepo;

impl FleetWorkRepo {
    /// Apply one transition and return parent active-work count.
    pub async fn apply(pool: &SqlitePool, update: &FleetWorkUpdate) -> Result<i64, sqlx::Error> {
        if update.active {
            sqlx::query(
                "INSERT INTO fleet_work_item (provider, session_key, work_key, kind, state, started_at, last_event_id) VALUES (?, ?, ?, ?, 'ACTIVE', ?, ?) \
                 ON CONFLICT(provider, session_key, work_key) DO UPDATE SET kind = excluded.kind, state = 'ACTIVE', started_at = excluded.started_at, completed_at = NULL, last_event_id = excluded.last_event_id \
                 WHERE fleet_work_item.last_event_id != excluded.last_event_id",
            )
            .bind(&update.provider)
            .bind(&update.session_key)
            .bind(&update.work_key)
            .bind(&update.kind)
            .bind(update.observed_at)
            .bind(&update.event_id)
            .execute(pool)
            .await?;
        } else {
            sqlx::query(
                "UPDATE fleet_work_item SET state = 'COMPLETE', completed_at = ?, last_event_id = ? \
                 WHERE provider = ? AND session_key = ? AND work_key = ? AND last_event_id != ?",
            )
            .bind(update.observed_at)
            .bind(&update.event_id)
            .bind(&update.provider)
            .bind(&update.session_key)
            .bind(&update.work_key)
            .bind(&update.event_id)
            .execute(pool)
            .await?;
        }
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM fleet_work_item WHERE session_key = ? AND state = 'ACTIVE'",
        )
        .bind(&update.session_key)
        .fetch_one(pool)
        .await
    }

    /// Complete every active parent relationship for one provider child key.
    /// Thread IDs are provider-global, so close events need no parent ID.
    pub async fn complete_by_work_key(
        pool: &SqlitePool,
        provider: &str,
        work_key: &str,
        event_id: &str,
        observed_at: i64,
    ) -> Result<Vec<(String, i64)>, sqlx::Error> {
        let session_keys: Vec<String> = sqlx::query_scalar(
            "SELECT session_key FROM fleet_work_item WHERE provider = ? AND work_key = ? AND (state = 'ACTIVE' OR last_event_id = ?)",
        )
        .bind(provider)
        .bind(work_key)
        .bind(event_id)
        .fetch_all(pool)
        .await?;
        let mut completed = Vec::with_capacity(session_keys.len());
        for session_key in session_keys {
            let count = Self::apply(
                pool,
                &FleetWorkUpdate {
                    provider: provider.to_string(),
                    session_key: session_key.clone(),
                    work_key: work_key.to_string(),
                    kind: "child_thread".to_string(),
                    active: false,
                    event_id: event_id.to_string(),
                    observed_at,
                },
            )
            .await?;
            completed.push((session_key, count));
        }
        Ok(completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn update(event_id: &str, active: bool) -> FleetWorkUpdate {
        FleetWorkUpdate {
            provider: "codex".to_string(),
            session_key: "codex:parent".to_string(),
            work_key: "codex:child".to_string(),
            kind: "child_thread".to_string(),
            active,
            event_id: event_id.to_string(),
            observed_at: 100,
        }
    }

    #[tokio::test]
    async fn child_close_without_parent_completes_active_relationship_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert_eq!(
            FleetWorkRepo::apply(store.pool(), &update("start", true)).await.unwrap(),
            1
        );

        let first =
            FleetWorkRepo::complete_by_work_key(store.pool(), "codex", "codex:child", "close", 200)
                .await
                .unwrap();
        assert_eq!(first, vec![("codex:parent".to_string(), 0)]);
        let replay =
            FleetWorkRepo::complete_by_work_key(store.pool(), "codex", "codex:child", "close", 200)
                .await
                .unwrap();
        assert_eq!(replay, first);
    }
}
