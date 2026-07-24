//! Typed repository wrapper over the `agent_invocation_target` table (migration
//! 0047, gap #8 — multica migration 130 parity).
//!
//! An [`InvocationTarget`] is one allow-list row for a `public_to` agent: it
//! admits a whole `workspace`, a specific `member` (by user id), or a reserved
//! `team` (inert in V1). The table is **FK-less by design** — matching the
//! `(actor_type, actor_id)` convention in [`crate::repo`] and the hot queue tables
//! — so `agent_id` / `created_by` / member `target_id` referential integrity is a
//! service-layer concern.
//!
//! The `UNIQUE(agent_id, target_type, target_id)` constraint dedups, so
//! [`AgentInvocationTargetRepo::add`] is idempotent (a duplicate is a silent
//! no-op, never a duplicate row or a hard error).
//!
//! Every mutation re-derives the legacy `agent.visibility` label via
//! [`crate::repo::agent::AgentRepo::rederive_visibility`] so the two stay
//! consistent (a workspace target present ⇒ `visibility = 'workspace'`).

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_core::idgen::IdGen;
use sqlx::SqlitePool;

use crate::repo::agent::AgentRepo;

/// One invocation-target allow-list row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationTarget {
    /// Primary key (ULID / random-hex string).
    pub id: String,
    /// The agent this row grants invocation on (`agent.id`; FK-less).
    pub agent_id: String,
    /// The kind of principal admitted: `"workspace"`, `"member"`, or `"team"`.
    pub target_type: String,
    /// The admitted principal's id: a `workspace_id`, a `user_id`, or a future
    /// team id (polymorphic, keyed by `target_type`).
    pub target_id: String,
    /// The user who created the row (`user.id`), or `None` when unattributed.
    pub created_by: Option<String>,
    /// Creation timestamp (epoch millis).
    pub created_at: i64,
}

/// Stateless typed wrapper over the `agent_invocation_target` table.
pub struct AgentInvocationTargetRepo;

impl AgentInvocationTargetRepo {
    /// Add one allow-list row, idempotent on `(agent_id, target_type, target_id)`.
    ///
    /// `target_type` must be `"workspace"`, `"member"`, or `"team"` (the schema
    /// `CHECK` is the last line of defence). A duplicate row is a silent no-op
    /// (`ON CONFLICT DO NOTHING`), so re-granting the same target is safe. After
    /// the insert, the agent's legacy `visibility` label is re-derived to stay in
    /// sync (a workspace target flips it to `'workspace'`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert or the visibility re-derivation
    /// fails (e.g. a `CHECK` violation on an out-of-set `target_type`).
    pub async fn add(
        pool: &SqlitePool,
        idgen: &impl IdGen,
        clock: &impl HangarClock,
        agent_id: &str,
        target_type: &str,
        target_id: &str,
        created_by: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO agent_invocation_target \
             (id, agent_id, target_type, target_id, created_by, created_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT (agent_id, target_type, target_id) DO NOTHING",
        )
        .bind(idgen.new_ulid())
        .bind(agent_id)
        .bind(target_type)
        .bind(target_id)
        .bind(created_by)
        .bind(clock.now_ms())
        .execute(pool)
        .await?;
        AgentRepo::rederive_visibility(pool, agent_id).await?;
        Ok(())
    }

    /// List every allow-list row for `agent_id`, ordered by `(target_type,
    /// target_id)` for a stable readout.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list(
        pool: &SqlitePool,
        agent_id: &str,
    ) -> Result<Vec<InvocationTarget>, sqlx::Error> {
        use sqlx::Row;
        let rows = sqlx::query(
            "SELECT id, agent_id, target_type, target_id, created_by, created_at \
             FROM agent_invocation_target WHERE agent_id = ? \
             ORDER BY target_type, target_id",
        )
        .bind(agent_id)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(InvocationTarget {
                    id: r.try_get("id")?,
                    agent_id: r.try_get("agent_id")?,
                    target_type: r.try_get("target_type")?,
                    target_id: r.try_get("target_id")?,
                    created_by: r.try_get("created_by")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Remove one allow-list row, returning `true` when a row was deleted, `false`
    /// when the `(agent_id, target_type, target_id)` triple matched nothing. The
    /// agent's legacy `visibility` label is re-derived afterwards.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the delete or the re-derivation fails.
    pub async fn remove(
        pool: &SqlitePool,
        agent_id: &str,
        target_type: &str,
        target_id: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "DELETE FROM agent_invocation_target \
             WHERE agent_id = ? AND target_type = ? AND target_id = ?",
        )
        .bind(agent_id)
        .bind(target_type)
        .bind(target_id)
        .execute(pool)
        .await?;
        AgentRepo::rederive_visibility(pool, agent_id).await?;
        Ok(res.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use crate::bootstrap;
    use crate::repo::agent::AgentRepo;
    use ainb_hangar_core::clock::SystemClock;
    use ainb_hangar_core::idgen::SystemIdGen;

    /// Boot a fresh store with the default workspace + a private agent, returning
    /// `(store, workspace_id, agent_id)`. The store is kept alive by the leaked
    /// tempdir so the sqlite file outlives the test.
    async fn seed() -> (Store, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let dir = Box::leak(Box::new(dir));
        let store = Store::open_in(dir.path()).await.unwrap();
        let ws = bootstrap::ensure_default_workspace(store.pool()).await.unwrap();
        let agent =
            bootstrap::create_agent(store.pool(), &ws, "bot", "claude", None).await.unwrap();
        (store, ws, agent.id)
    }

    /// `add` is idempotent on the `(agent_id, target_type, target_id)` UNIQUE key —
    /// re-granting the same target writes no second row and is not an error.
    #[tokio::test]
    async fn add_is_idempotent_on_the_unique_key() {
        let (store, _ws, agent_id) = seed().await;
        let pool = store.pool();

        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "member",
            "u-bob",
            None,
        )
        .await
        .unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "member",
            "u-bob",
            Some("u-owner"),
        )
        .await
        .unwrap();

        let targets = AgentInvocationTargetRepo::list(pool, &agent_id).await.unwrap();
        assert_eq!(
            targets.len(),
            1,
            "a duplicate grant must not add a second row"
        );
        assert_eq!(targets[0].target_type, "member");
        assert_eq!(targets[0].target_id, "u-bob");
    }

    /// `remove` reports whether a row was actually deleted.
    #[tokio::test]
    async fn remove_reports_whether_a_row_was_deleted() {
        let (store, _ws, agent_id) = seed().await;
        let pool = store.pool();

        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "member",
            "u-bob",
            None,
        )
        .await
        .unwrap();

        assert!(
            AgentInvocationTargetRepo::remove(pool, &agent_id, "member", "u-bob")
                .await
                .unwrap(),
            "removing an existing target returns true"
        );
        assert!(
            !AgentInvocationTargetRepo::remove(pool, &agent_id, "member", "u-bob")
                .await
                .unwrap(),
            "removing a non-existent target returns false"
        );
        assert!(AgentInvocationTargetRepo::list(pool, &agent_id).await.unwrap().is_empty());
    }

    /// `list` orders rows by `(target_type, target_id)` for a stable readout.
    #[tokio::test]
    async fn list_is_ordered() {
        let (store, ws, agent_id) = seed().await;
        let pool = store.pool();

        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "member",
            "u-zeb",
            None,
        )
        .await
        .unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "member",
            "u-abe",
            None,
        )
        .await
        .unwrap();
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "workspace",
            &ws,
            None,
        )
        .await
        .unwrap();

        let targets = AgentInvocationTargetRepo::list(pool, &agent_id).await.unwrap();
        let ordered: Vec<(&str, &str)> =
            targets.iter().map(|t| (t.target_type.as_str(), t.target_id.as_str())).collect();
        assert_eq!(
            ordered,
            vec![
                ("member", "u-abe"),
                ("member", "u-zeb"),
                ("workspace", ws.as_str())
            ],
        );
    }

    /// `set_permission_mode` + target writes re-derive the legacy `visibility`:
    /// public_to with a workspace target → `'workspace'`; member-only → `'private'`;
    /// private → `'private'`.
    #[tokio::test]
    async fn visibility_is_rederived_from_mode_and_targets() {
        let (store, ws, agent_id) = seed().await;
        let pool = store.pool();

        // Flip to public_to but with only a MEMBER target → visibility stays private
        // (no widening for legacy readers).
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "member",
            "u-bob",
            None,
        )
        .await
        .unwrap();
        AgentRepo::set_permission_mode(pool, &agent_id, "public_to").await.unwrap();
        let vis: String = sqlx::query_scalar("SELECT visibility FROM agent WHERE id = ?")
            .bind(&agent_id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(
            vis, "private",
            "public_to with member-only target derives 'private'"
        );

        // Add a WORKSPACE target → visibility becomes 'workspace'.
        AgentInvocationTargetRepo::add(
            pool,
            &SystemIdGen,
            &SystemClock,
            &agent_id,
            "workspace",
            &ws,
            None,
        )
        .await
        .unwrap();
        let vis: String = sqlx::query_scalar("SELECT visibility FROM agent WHERE id = ?")
            .bind(&agent_id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(vis, "workspace", "a workspace target derives 'workspace'");

        // Back to private → visibility 'private' regardless of leftover targets.
        AgentRepo::set_permission_mode(pool, &agent_id, "private").await.unwrap();
        let vis: String = sqlx::query_scalar("SELECT visibility FROM agent WHERE id = ?")
            .bind(&agent_id)
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(vis, "private", "private mode always derives 'private'");
    }
}
