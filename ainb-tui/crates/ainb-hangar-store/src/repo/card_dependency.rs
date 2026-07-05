//! Typed repository over the `card_dependency` edge table (tcp T4 / F7).
//!
//! A card dependency is a directed edge `dependent -> blocker`: the DEPENDENT
//! card is blocked until the BLOCKER card finishes. Card = issue (§4.5), so both
//! endpoints are `issue.id`s in one workspace. This repo is the only mutation +
//! read surface over the table:
//!
//!   - [`CardDependencyRepo::add_edge`] — insert one edge, rejecting a self-edge
//!     and any edge that would create a CYCLE (a DFS over the existing edges runs
//!     BEFORE the write, so the graph is acyclic by construction);
//!   - [`CardDependencyRepo::remove_edge`] — idempotent delete of one edge;
//!   - [`CardDependencyRepo::blockers_of`] / [`CardDependencyRepo::dependents_of`]
//!     — the forward / reverse adjacency;
//!   - [`CardDependencyRepo::unfinished_blockers_of`] — the refuse-run guard: the
//!     dependent's blockers that have not FINISHED (their active set drained with a
//!     `done`; a card with any unfinished blocker refuses to run and is never
//!     auto-dispatched);
//!   - [`CardDependencyRepo::edges_of_workspace`] — every edge in the workspace,
//!     for the board snapshot's 🔒 blocked-state render;
//!   - [`CardDependencyRepo::set_auto_run`] / [`CardDependencyRepo::get_auto_run`]
//!     — the per-card auto-run flag (default OFF) the finalize seam consults when
//!     the last blocker completes.
//!
//! # Workspace scoping
//!
//! Every mutation takes a [`WorkspaceId`] and resolves BOTH endpoints inside it
//! before writing (there is no FK — `PRAGMA foreign_keys` is off in this crate),
//! so a dependency can never reference a foreign-tenant / nonexistent issue or
//! cross a workspace boundary. The composite PK `(dependent, blocker)` is the sole
//! engine-enforced invariant; existence, scoping, and acyclicity are enforced
//! here in application code (mirroring `BoardRepo`).
//!
//! # "Finished" = the whole active set drained, with a success
//!
//! A blocker is FINISHED when EVERY task on its issue is terminal (its active set
//! has drained) AND at least one of them reached `done`. A squad blocker fans out
//! N tasks onto one issue, so keying on the whole set — not just the latest task —
//! is what stops a dependent unblocking while the leader / other members still run
//! (tcp T4 / FANOUT-SEMANTICS). A blocker with no task, with any still-active task,
//! or whose tasks all ended `failed`/`cancelled` (no `done`) is UNFINISHED, so the
//! dependent stays blocked. See [`CardDependencyRepo::unfinished_blockers_of`].

use ainb_hangar_core::ids::WorkspaceId;
use sqlx::{Row, SqlitePool};

/// Stateless typed wrapper over the `card_dependency` table + the `issue.auto_run`
/// flag (tcp T4 / F7).
pub struct CardDependencyRepo;

/// Why a dependency edge could not be added.
#[derive(Debug, thiserror::Error)]
pub enum CardDependencyError {
    /// The dependent and blocker are the same card — a card cannot depend on
    /// itself (the trivial cycle). Rejected.
    #[error("a card cannot depend on itself")]
    SelfDependency,
    /// The edge would create a dependency CYCLE (the blocker already depends,
    /// transitively, on the dependent). Rejected so the graph stays acyclic and a
    /// card can never wait on itself through a chain.
    #[error("that dependency would create a cycle")]
    Cycle,
    /// One (or both) endpoints is not an issue in this workspace — an unknown id
    /// or a sibling-tenant one. Rejected rather than storing a dangling edge.
    #[error("dependent or blocker card not found in this workspace")]
    NotFound,
    /// An underlying `sqlx` failure (IO, decode, …).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

impl CardDependencyRepo {
    /// Add a `dependent -> blocker` edge in `workspace`, idempotently.
    ///
    /// Both endpoints must be issues in `workspace`. A self-edge is rejected
    /// ([`CardDependencyError::SelfDependency`]); an edge that would close a cycle
    /// is rejected ([`CardDependencyError::Cycle`]) — the cycle check runs inside
    /// the same transaction as the insert, so two concurrent adds cannot race a
    /// cycle into existence. Re-adding an existing edge is a no-op (the composite
    /// PK's `ON CONFLICT DO NOTHING`).
    ///
    /// # Errors
    ///
    /// [`CardDependencyError::SelfDependency`] / [`CardDependencyError::Cycle`] /
    /// [`CardDependencyError::NotFound`] / [`CardDependencyError::Db`].
    pub async fn add_edge(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        dependent_issue_id: &str,
        blocker_issue_id: &str,
        created_at: i64,
    ) -> Result<(), CardDependencyError> {
        if dependent_issue_id == blocker_issue_id {
            return Err(CardDependencyError::SelfDependency);
        }
        let mut tx = pool.begin().await?;
        Self::ensure_issue_in_ws(&mut tx, workspace, dependent_issue_id).await?;
        Self::ensure_issue_in_ws(&mut tx, workspace, blocker_issue_id).await?;

        // Cycle guard: the new edge says "dependent depends on blocker". A cycle
        // exists iff `blocker` can already reach `dependent` by following
        // depends-on edges (blocker -> ... -> dependent) — adding
        // dependent -> blocker would then close the loop. DFS from `blocker` over
        // its own blockers; if it reaches `dependent`, reject.
        if Self::reaches(&mut tx, blocker_issue_id, dependent_issue_id).await? {
            return Err(CardDependencyError::Cycle);
        }

        sqlx::query(
            "INSERT INTO card_dependency \
             (workspace_id, dependent_issue_id, blocker_issue_id, created_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (dependent_issue_id, blocker_issue_id) DO NOTHING",
        )
        .bind(workspace.as_str())
        .bind(dependent_issue_id)
        .bind(blocker_issue_id)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Remove the `dependent -> blocker` edge, idempotently (removing an absent
    /// edge is a no-op). Workspace-scoped so a foreign-tenant edge is never
    /// touched.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn remove_edge(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        dependent_issue_id: &str,
        blocker_issue_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "DELETE FROM card_dependency \
             WHERE workspace_id = ? AND dependent_issue_id = ? AND blocker_issue_id = ?",
        )
        .bind(workspace.as_str())
        .bind(dependent_issue_id)
        .bind(blocker_issue_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// The blocker issue ids the dependent card depends on (its direct blockers),
    /// ordered by insertion time then id for a stable render.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn blockers_of(
        pool: &SqlitePool,
        dependent_issue_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT blocker_issue_id FROM card_dependency \
             WHERE dependent_issue_id = ? ORDER BY created_at, blocker_issue_id",
        )
        .bind(dependent_issue_id)
        .fetch_all(pool)
        .await
    }

    /// The dependent issue ids that depend on the blocker card (its direct
    /// dependents) — the finalize-seam reverse lookup when the blocker completes.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn dependents_of(
        pool: &SqlitePool,
        blocker_issue_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT dependent_issue_id FROM card_dependency \
             WHERE blocker_issue_id = ? ORDER BY created_at, dependent_issue_id",
        )
        .bind(blocker_issue_id)
        .fetch_all(pool)
        .await
    }

    /// The dependent card's UNFINISHED blockers. Empty ⇒ the card is runnable. This
    /// is the refuse-run guard AND the "blocker completed" check the finalize seam
    /// re-evaluates.
    ///
    /// # "Finished" = the blocker's WHOLE active set drained, with a success
    ///
    /// A blocker is FINISHED (drops off this list, satisfying the dependency) iff
    /// EVERY task on its issue is terminal (its active set has drained) AND at least
    /// one of those tasks reached `done`. It is UNFINISHED (kept here, the dependent
    /// stays blocked) iff:
    ///   - it has NO task at all (never ran), OR
    ///   - ANY of its tasks is still active (`queued`/`dispatched`/`running`), OR
    ///   - all its tasks are terminal but NONE reached `done`.
    ///
    /// This is the tcp T4 / FANOUT-SEMANTICS fix: a squad blocker fans out N tasks
    /// onto one issue, so the old "latest task is `done`" test unblocked the
    /// dependent the instant the last-inserted sibling finished while the leader +
    /// other members still ran. Keying on the WHOLE set closes that. A consequence,
    /// by design: a fully-failed or fully-cancelled blocker (no `done`) NEVER
    /// unblocks its dependents, and a blocker that is re-running (fresh active tasks)
    /// re-blocks them until it drains again.
    ///
    /// # Single run generation (known limitation)
    ///
    /// The "any `done`" test spans EVERY task the blocker has ever had, so it assumes
    /// one run generation. A blocker that succeeded once, then was RE-RUN and failed,
    /// keeps an old `done` row and so stays "finished" here. Scoping to the latest run
    /// needs a run-generation marker (follow-up); the concurrent-fan-out contract is
    /// unaffected.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn unfinished_blockers_of(
        pool: &SqlitePool,
        dependent_issue_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        // Keep a blocker UNLESS its active set has drained (no active task left) AND
        // it has at least one `done` task. The two EXISTS probes are indexed lookups
        // on `issue_id`, cheap even with a full task history per blocker.
        sqlx::query_scalar(
            "SELECT d.blocker_issue_id FROM card_dependency d \
             WHERE d.dependent_issue_id = ? \
               AND NOT ( \
                     EXISTS ( \
                       SELECT 1 FROM agent_task_queue t \
                       WHERE t.issue_id = d.blocker_issue_id AND t.status = 'done' \
                     ) \
                     AND NOT EXISTS ( \
                       SELECT 1 FROM agent_task_queue t \
                       WHERE t.issue_id = d.blocker_issue_id \
                         AND t.status IN ('queued','dispatched','running') \
                     ) \
                   ) \
             ORDER BY d.created_at, d.blocker_issue_id",
        )
        .bind(dependent_issue_id)
        .fetch_all(pool)
        .await
    }

    /// Every dependency edge in `workspace` as `(dependent, blocker)` pairs, for
    /// the board snapshot's blocked-state render. Ordered by dependent then blocker
    /// for a deterministic snapshot.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn edges_of_workspace(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<(String, String)>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT dependent_issue_id, blocker_issue_id FROM card_dependency \
             WHERE workspace_id = ? ORDER BY dependent_issue_id, blocker_issue_id",
        )
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("dependent_issue_id")?, r.try_get("blocker_issue_id")?)))
            .collect()
    }

    /// Set the per-card auto-run flag (F7): when `true`, the card auto-launches the
    /// instant its last blocker completes. Workspace-scoped; a foreign / unknown
    /// issue matches no row (`Ok(false)`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_auto_run(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        auto_run: bool,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE issue SET auto_run = ? WHERE id = ? AND workspace_id = ?")
            .bind(i64::from(auto_run))
            .bind(issue_id)
            .bind(workspace.as_str())
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Read a card's auto-run flag (`false` when unset / issue absent).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn get_auto_run(pool: &SqlitePool, issue_id: &str) -> Result<bool, sqlx::Error> {
        let raw: Option<i64> = sqlx::query_scalar("SELECT auto_run FROM issue WHERE id = ?")
            .bind(issue_id)
            .fetch_optional(pool)
            .await?;
        Ok(raw.unwrap_or(0) != 0)
    }

    /// Whether `start` can reach `target` by following depends-on edges
    /// (`start -> ... -> target`). An iterative DFS over the `card_dependency`
    /// edges inside `tx`; the `visited` set makes it terminate even on a
    /// pre-existing (never-happens) cycle, and bounds it to O(V+E).
    async fn reaches(
        tx: &mut sqlx::SqliteConnection,
        start: &str,
        target: &str,
    ) -> Result<bool, sqlx::Error> {
        use std::collections::HashSet;
        let mut visited: HashSet<String> = HashSet::new();
        let mut stack: Vec<String> = vec![start.to_string()];
        while let Some(node) = stack.pop() {
            if node == target {
                return Ok(true);
            }
            if !visited.insert(node.clone()) {
                continue;
            }
            let next: Vec<String> = sqlx::query_scalar(
                "SELECT blocker_issue_id FROM card_dependency WHERE dependent_issue_id = ?",
            )
            .bind(&node)
            .fetch_all(&mut *tx)
            .await?;
            stack.extend(next);
        }
        Ok(false)
    }

    /// Confirm `issue_id` names an issue that lives in `workspace` (the sole guard
    /// against a dangling / cross-tenant endpoint, since the edge carries no FK).
    async fn ensure_issue_in_ws(
        tx: &mut sqlx::SqliteConnection,
        workspace: &WorkspaceId,
        issue_id: &str,
    ) -> Result<(), CardDependencyError> {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(CardDependencyError::NotFound);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id.to_string()).unwrap()
    }

    async fn seed_ws(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, 0)")
            .bind(id)
            .bind(id)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn seed_issue(pool: &SqlitePool, ws: &str, id: &str) {
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, 'member', 'm1', 0)",
        )
        .bind(id)
        .bind(ws)
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Seed a minimal user/runtime/agent so a task row's FK chain holds, then a
    /// task on `issue_id` with `status`. Used to drive the blocker-finished check.
    async fn seed_task_on_issue(pool: &SqlitePool, ws: &str, issue_id: &str, task_id: &str, status: &str) {
        sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES ('u','u@e.com',0)")
            .execute(pool).await.unwrap();
        sqlx::query("INSERT OR IGNORE INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) VALUES ('rt', ?, 'd','claude','local','online')")
            .bind(ws).execute(pool).await.unwrap();
        sqlx::query("INSERT OR IGNORE INTO agent (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) VALUES ('ag', ?, 'A','rt','x','workspace','u')")
            .bind(ws).execute(pool).await.unwrap();
        sqlx::query(
            "INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, issue_id, status, created_at) \
             VALUES (?, ?, 'rt', 'ag', ?, ?, 0)",
        )
        .bind(task_id).bind(ws).bind(issue_id).bind(status)
        .execute(pool).await.unwrap();
    }

    /// Transition an existing task row's status in place (a task's own lifecycle
    /// move `running -> done`), so the active-set semantics are exercised on real
    /// transitions rather than by inserting a new sibling.
    async fn set_task_status(pool: &SqlitePool, task_id: &str, status: &str) {
        sqlx::query("UPDATE agent_task_queue SET status = ? WHERE id = ?")
            .bind(status).bind(task_id)
            .execute(pool).await.unwrap();
    }

    async fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        (dir, store)
    }

    /// add_edge builds the adjacency both ways and blockers_of / dependents_of
    /// read it back; a re-add is an idempotent no-op.
    #[tokio::test]
    async fn add_edge_builds_adjacency_and_is_idempotent() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        for id in ["a", "b", "c"] {
            seed_issue(pool, "ws-a", id).await;
        }
        // b depends on a; c depends on a.
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "b", "a", 1).await.unwrap();
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "c", "a", 2).await.unwrap();
        // Idempotent re-add.
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "b", "a", 3).await.unwrap();

        assert_eq!(CardDependencyRepo::blockers_of(pool, "b").await.unwrap(), vec!["a"]);
        let mut deps = CardDependencyRepo::dependents_of(pool, "a").await.unwrap();
        deps.sort();
        assert_eq!(deps, vec!["b", "c"]);
        assert_eq!(
            CardDependencyRepo::edges_of_workspace(pool, &ws("ws-a")).await.unwrap().len(),
            2,
            "the idempotent re-add did not double the edge"
        );
    }

    /// A self-edge is rejected.
    #[tokio::test]
    async fn add_edge_rejects_self_dependency() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "a").await;
        let err = CardDependencyRepo::add_edge(pool, &ws("ws-a"), "a", "a", 1).await.unwrap_err();
        assert!(matches!(err, CardDependencyError::SelfDependency), "got {err:?}");
    }

    /// A direct cycle (a↔b) and a transitive cycle (a→b→c→a) are both rejected,
    /// leaving the graph acyclic.
    #[tokio::test]
    async fn add_edge_rejects_direct_and_transitive_cycles() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        for id in ["a", "b", "c"] {
            seed_issue(pool, "ws-a", id).await;
        }
        // a depends on b, b depends on c (a chain a -> b -> c).
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "a", "b", 1).await.unwrap();
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "b", "c", 2).await.unwrap();

        // Direct cycle: b depends on a would close a<->b (a already depends on b).
        let direct = CardDependencyRepo::add_edge(pool, &ws("ws-a"), "b", "a", 3).await.unwrap_err();
        assert!(matches!(direct, CardDependencyError::Cycle), "got {direct:?}");

        // Transitive cycle: c depends on a would close a -> b -> c -> a.
        let trans = CardDependencyRepo::add_edge(pool, &ws("ws-a"), "c", "a", 4).await.unwrap_err();
        assert!(matches!(trans, CardDependencyError::Cycle), "got {trans:?}");

        // The two rejected edges were never written.
        assert_eq!(
            CardDependencyRepo::edges_of_workspace(pool, &ws("ws-a")).await.unwrap().len(),
            2
        );
    }

    /// An edge to a nonexistent or sibling-tenant issue is rejected.
    #[tokio::test]
    async fn add_edge_is_workspace_scoped() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_issue(pool, "ws-a", "a").await;
        seed_issue(pool, "ws-b", "foreign").await;

        // Unknown blocker.
        let unknown = CardDependencyRepo::add_edge(pool, &ws("ws-a"), "a", "ghost", 1).await.unwrap_err();
        assert!(matches!(unknown, CardDependencyError::NotFound), "got {unknown:?}");
        // Cross-tenant blocker (exists, but in ws-b).
        let foreign = CardDependencyRepo::add_edge(pool, &ws("ws-a"), "a", "foreign", 2).await.unwrap_err();
        assert!(matches!(foreign, CardDependencyError::NotFound), "got {foreign:?}");
        assert!(CardDependencyRepo::edges_of_workspace(pool, &ws("ws-a")).await.unwrap().is_empty());
    }

    /// unfinished_blockers_of returns blockers whose latest task is not `done`;
    /// a done blocker drops off, making the dependent runnable.
    #[tokio::test]
    async fn unfinished_blockers_track_the_done_state() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        for id in ["a", "b", "dep"] {
            seed_issue(pool, "ws-a", id).await;
        }
        // dep depends on both a and b.
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "dep", "a", 1).await.unwrap();
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "dep", "b", 2).await.unwrap();

        // No tasks yet → both blockers unfinished.
        let mut u = CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap();
        u.sort();
        assert_eq!(u, vec!["a", "b"], "never-ran blockers are unfinished");

        // a runs to done; b still running.
        seed_task_on_issue(pool, "ws-a", "a", "t-a", "done").await;
        seed_task_on_issue(pool, "ws-a", "b", "t-b", "running").await;
        assert_eq!(
            CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(),
            vec!["b"],
            "a is done (finished); b is still unfinished"
        );

        // b's task finishes too (its own running -> done) → the dependent has no
        // unfinished blockers (runnable).
        set_task_status(pool, "t-b", "done").await;
        assert!(
            CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap().is_empty(),
            "both blockers done → the dependent is runnable"
        );
    }

    /// A SQUAD blocker fans out several tasks onto one issue: the dependent stays
    /// blocked until the WHOLE set drains — the last-inserted sibling finishing
    /// `done` while others still run must NOT unblock it (the tcp T4 fan-out bug).
    #[tokio::test]
    async fn a_squad_blocker_unblocks_only_when_the_whole_set_drains() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "sq").await;
        seed_issue(pool, "ws-a", "dep").await;
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "dep", "sq", 1).await.unwrap();

        // Fan-out: leader + two members, all running.
        seed_task_on_issue(pool, "ws-a", "sq", "leader", "running").await;
        seed_task_on_issue(pool, "ws-a", "sq", "m1", "running").await;
        seed_task_on_issue(pool, "ws-a", "sq", "m2", "running").await;

        // The LAST-inserted member finishes done — the old latest-wins logic would
        // have unblocked here. The whole-set contract keeps dep blocked.
        set_task_status(pool, "m2", "done").await;
        assert_eq!(
            CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(),
            vec!["sq"],
            "one member done but the leader + m1 still run — the set has not drained"
        );

        // Leader + m1 finish too → the set has drained with a success → runnable.
        set_task_status(pool, "leader", "done").await;
        set_task_status(pool, "m1", "done").await;
        assert!(
            CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap().is_empty(),
            "the whole squad drained with a done → the dependent is runnable"
        );
    }

    /// A blocker whose whole set ended without a single `done` (all failed, all
    /// cancelled, or a failed+cancelled mix) NEVER unblocks its dependent.
    #[tokio::test]
    async fn a_fully_failed_or_cancelled_blocker_never_unblocks() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        for id in ["blk", "dep"] {
            seed_issue(pool, "ws-a", id).await;
        }
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "dep", "blk", 1).await.unwrap();

        // A squad that ended failed + cancelled, no `done` → drained but no success.
        seed_task_on_issue(pool, "ws-a", "blk", "t1", "failed").await;
        seed_task_on_issue(pool, "ws-a", "blk", "t2", "cancelled").await;
        assert_eq!(
            CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(),
            vec!["blk"],
            "a drained-but-never-succeeded blocker does not satisfy the dependency"
        );

        // A retry that finally succeeds flips it finished (the set now has a done).
        seed_task_on_issue(pool, "ws-a", "blk", "t3", "done").await;
        assert!(
            CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap().is_empty(),
            "a later done in the set unblocks the dependent"
        );
    }

    /// A failed / cancelled blocker does NOT satisfy the dependency (only `done`
    /// does): the latest task status must be `done`.
    #[tokio::test]
    async fn a_failed_blocker_is_still_unfinished() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "a").await;
        seed_issue(pool, "ws-a", "dep").await;
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "dep", "a", 1).await.unwrap();

        seed_task_on_issue(pool, "ws-a", "a", "t1", "failed").await;
        assert_eq!(
            CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(),
            vec!["a"],
            "a failed blocker has not completed — the dependent stays blocked"
        );
        // A newer done task on the same blocker flips it finished (latest wins).
        seed_task_on_issue(pool, "ws-a", "a", "t2", "done").await;
        assert!(CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap().is_empty());
    }

    /// remove_edge is idempotent and unblocks the dependent.
    #[tokio::test]
    async fn remove_edge_unblocks_and_is_idempotent() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "a").await;
        seed_issue(pool, "ws-a", "dep").await;
        CardDependencyRepo::add_edge(pool, &ws("ws-a"), "dep", "a", 1).await.unwrap();
        assert_eq!(CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap(), vec!["a"]);

        CardDependencyRepo::remove_edge(pool, &ws("ws-a"), "dep", "a").await.unwrap();
        assert!(CardDependencyRepo::unfinished_blockers_of(pool, "dep").await.unwrap().is_empty());
        // Idempotent second remove.
        CardDependencyRepo::remove_edge(pool, &ws("ws-a"), "dep", "a").await.unwrap();
    }

    /// The auto-run flag round-trips, defaults OFF, and is workspace-scoped.
    #[tokio::test]
    async fn auto_run_flag_round_trips() {
        let (_d, store) = open().await;
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_issue(pool, "ws-a", "a").await;

        assert!(!CardDependencyRepo::get_auto_run(pool, "a").await.unwrap(), "defaults OFF");
        assert!(CardDependencyRepo::set_auto_run(pool, &ws("ws-a"), "a", true).await.unwrap());
        assert!(CardDependencyRepo::get_auto_run(pool, "a").await.unwrap());
        // A cross-tenant write misses.
        assert!(!CardDependencyRepo::set_auto_run(pool, &ws("ws-b"), "a", false).await.unwrap());
        assert!(CardDependencyRepo::get_auto_run(pool, "a").await.unwrap(), "cross-tenant write left it on");
    }
}
