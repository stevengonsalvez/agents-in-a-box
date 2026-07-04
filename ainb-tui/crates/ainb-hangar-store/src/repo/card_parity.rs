//! Targeted read/write surface for the task-create parity fields (migration
//! 0032, spec F1-F5) and the F4 agent default cascade.
//!
//! These columns (`issue.repo_ref` / `issue.agent_kind`, `agent_task_queue.
//! repo_ref` / `agent_task_queue.agent_kind`, `board.default_agent`,
//! `workspace.default_agent`) hang off tables that already have rich typed
//! repos ([`super::issue`], [`super::task`], [`super::board`],
//! [`super::workspace`]). Rather than widen those read structs — and every
//! literal + fixture that builds them — this module adds the small, focused
//! accessors the card-create/run + cascade paths need, leaving the existing
//! readers byte-identical.
//!
//! # The cascade ([`CardParityRepo::resolve_agent_cascade`])
//!
//! F4's precedence is `last-used → board default → workspace default → global
//! config → claude`. This repo reads the four sources (last-used + global live
//! in the [`super::daemon_config`] KV table; board + workspace defaults are the
//! new columns) and delegates the precedence to the pure
//! [`ainb_hangar_core::agent_kind::resolve_agent_cascade`].

use ainb_hangar_core::agent_kind::{resolve_agent_cascade, AgentKind};
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::SqlitePool;

use super::daemon_config::DaemonConfigRepo;

/// `daemon_config` key holding the host-wide default card agent (F4 global tier).
pub const AGENT_GLOBAL_DEFAULT_KEY: &str = "card_agent.default";
/// `daemon_config` key holding the most-recently-picked card agent (F4 last-used
/// tier). Updated on every card create/run so the next overlay pre-selects it.
pub const AGENT_LAST_USED_KEY: &str = "card_agent.last_used";

/// Stateless accessor over the migration-0032 card-parity columns + the F4
/// cascade sources.
pub struct CardParityRepo;

impl CardParityRepo {
    /// Persist a card's repo + agent onto its issue row (the durable card state).
    /// `repo_ref` is an absolute path or the literal `scratch`; `agent_kind` is
    /// the picked provider, or `None` to leave the run to resolve via the cascade.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault. Returns `Ok(false)` when no
    /// issue with that id exists (matched no row).
    pub async fn set_issue_repo_agent(
        pool: &SqlitePool,
        issue_id: &str,
        repo_ref: Option<&str>,
        agent_kind: Option<AgentKind>,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE issue SET repo_ref = ?, agent_kind = ? WHERE id = ?")
            .bind(repo_ref)
            .bind(agent_kind.map(|a| a.as_str()))
            .bind(issue_id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Read a card's persisted `(repo_ref, agent_kind)` from its issue, or `None`
    /// when the issue does not exist. A stored `agent_kind` token that no longer
    /// parses reads back as `None` (the cascade then decides), never an error.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn get_issue_repo_agent(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<Option<(Option<String>, Option<AgentKind>)>, sqlx::Error> {
        let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
            "SELECT repo_ref, agent_kind FROM issue WHERE id = ?",
        )
        .bind(issue_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(|(repo, agent)| (repo, agent.as_deref().and_then(AgentKind::parse))))
    }

    /// Persist the resolved repo + agent onto a task row, WITHIN an existing
    /// transaction (so the card-run enqueue + this write commit atomically and
    /// the claim loop can never observe a task without its dispatch fields).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_task_repo_agent_in_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        task_id: &str,
        repo_ref: Option<&str>,
        agent_kind: AgentKind,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE agent_task_queue SET repo_ref = ?, agent_kind = ? WHERE id = ?")
            .bind(repo_ref)
            .bind(agent_kind.as_str())
            .bind(task_id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Read a task's `(repo_ref, agent_kind)`. The agent defaults to
    /// [`AgentKind::DEFAULT`] when the stored token is unset/unparseable (the
    /// column is NOT NULL DEFAULT `claude`, so this only guards a corrupt value).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault. Returns `None` when the task
    /// does not exist.
    pub async fn get_task_repo_agent(
        pool: &SqlitePool,
        task_id: &str,
    ) -> Result<Option<(Option<String>, AgentKind)>, sqlx::Error> {
        let row = sqlx::query_as::<_, (Option<String>, String)>(
            "SELECT repo_ref, agent_kind FROM agent_task_queue WHERE id = ?",
        )
        .bind(task_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(|(repo, agent)| (repo, AgentKind::parse(&agent).unwrap_or(AgentKind::DEFAULT))))
    }

    /// Set (or clear, with `None`) a board's F4 default agent. Workspace-scoped:
    /// a board not in `workspace` matches no row (`Ok(false)`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_board_default_agent(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        agent_kind: Option<AgentKind>,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE board SET default_agent = ? WHERE id = ? AND workspace_id = ?",
        )
        .bind(agent_kind.map(|a| a.as_str()))
        .bind(board_id)
        .bind(workspace.as_str())
        .execute(pool)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Read a board's F4 default agent (`None` when unset / board absent /
    /// unparseable token). Workspace-scoped.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn get_board_default_agent(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
    ) -> Result<Option<AgentKind>, sqlx::Error> {
        let raw: Option<Option<String>> = sqlx::query_scalar(
            "SELECT default_agent FROM board WHERE id = ? AND workspace_id = ?",
        )
        .bind(board_id)
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        Ok(raw.flatten().as_deref().and_then(AgentKind::parse))
    }

    /// Set (or clear, with `None`) a workspace's F4 default agent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_workspace_default_agent(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        agent_kind: Option<AgentKind>,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE workspace SET default_agent = ? WHERE id = ?")
            .bind(agent_kind.map(|a| a.as_str()))
            .bind(workspace.as_str())
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Read a workspace's F4 default agent (`None` when unset / unparseable).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn get_workspace_default_agent(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Option<AgentKind>, sqlx::Error> {
        let raw: Option<Option<String>> =
            sqlx::query_scalar("SELECT default_agent FROM workspace WHERE id = ?")
                .bind(workspace.as_str())
                .fetch_optional(pool)
                .await?;
        Ok(raw.flatten().as_deref().and_then(AgentKind::parse))
    }

    /// Read the host-wide global default agent from `daemon_config` (`None` when
    /// unset / unparseable).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn get_global_default_agent(
        pool: &SqlitePool,
    ) -> Result<Option<AgentKind>, sqlx::Error> {
        Ok(DaemonConfigRepo::get(pool, AGENT_GLOBAL_DEFAULT_KEY)
            .await?
            .as_deref()
            .and_then(AgentKind::parse))
    }

    /// Read the most-recently-picked agent from `daemon_config` (F4 last-used
    /// tier; `None` when never picked / unparseable).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn get_last_used_agent(pool: &SqlitePool) -> Result<Option<AgentKind>, sqlx::Error> {
        Ok(DaemonConfigRepo::get(pool, AGENT_LAST_USED_KEY)
            .await?
            .as_deref()
            .and_then(AgentKind::parse))
    }

    /// Record the just-picked agent as the F4 last-used default (upsert).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn set_last_used_agent(
        pool: &SqlitePool,
        agent_kind: AgentKind,
    ) -> Result<(), sqlx::Error> {
        DaemonConfigRepo::set(pool, AGENT_LAST_USED_KEY, agent_kind.as_str()).await
    }

    /// Resolve the F4 default agent for a `(workspace, board?)` context: reads
    /// last-used, the board default (when a board is given), the workspace
    /// default, and the global default, then applies the pure precedence rule.
    ///
    /// `board_id = None` skips the board tier (a create not scoped to one board).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn resolve_agent_cascade(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: Option<&str>,
    ) -> Result<AgentKind, sqlx::Error> {
        let last_used = Self::get_last_used_agent(pool).await?;
        let board = match board_id {
            Some(b) => Self::get_board_default_agent(pool, workspace, b).await?,
            None => None,
        };
        let workspace_default = Self::get_workspace_default_agent(pool, workspace).await?;
        let global = Self::get_global_default_agent(pool).await?;
        Ok(resolve_agent_cascade(last_used, board, workspace_default, global))
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

    async fn seed_board(pool: &SqlitePool, ws: &str, id: &str) {
        sqlx::query("INSERT INTO board (id, workspace_id, name, auto_move, created_at) VALUES (?, ?, ?, 1, 0)")
            .bind(id)
            .bind(ws)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Issue repo/agent round-trips; an unparseable stored agent reads back None.
    #[tokio::test]
    async fn issue_repo_agent_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        // Unset by default.
        assert_eq!(
            CardParityRepo::get_issue_repo_agent(pool, "issue-1").await.unwrap(),
            Some((None, None))
        );
        // A missing issue → None.
        assert_eq!(
            CardParityRepo::get_issue_repo_agent(pool, "nope").await.unwrap(),
            None
        );

        let ok = CardParityRepo::set_issue_repo_agent(
            pool,
            "issue-1",
            Some("/repos/app"),
            Some(AgentKind::Codex),
        )
        .await
        .unwrap();
        assert!(ok);
        assert_eq!(
            CardParityRepo::get_issue_repo_agent(pool, "issue-1").await.unwrap(),
            Some((Some("/repos/app".to_string()), Some(AgentKind::Codex)))
        );

        // A corrupt stored token reads back None (cascade decides), never errors.
        sqlx::query("UPDATE issue SET agent_kind = 'gemini' WHERE id = 'issue-1'")
            .execute(pool)
            .await
            .unwrap();
        assert_eq!(
            CardParityRepo::get_issue_repo_agent(pool, "issue-1").await.unwrap(),
            Some((Some("/repos/app".to_string()), None))
        );
    }

    /// Task repo/agent round-trips inside a tx; agent defaults to claude.
    #[tokio::test]
    async fn task_repo_agent_defaults_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        // Minimal user + runtime + agent + task so the FK chain holds.
        sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u','u@e.com',0)").execute(pool).await.unwrap();
        sqlx::query("INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) VALUES ('rt','ws-a','d','claude','local','online')").execute(pool).await.unwrap();
        sqlx::query("INSERT INTO agent (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) VALUES ('ag','ws-a','A','rt','x','workspace','u')").execute(pool).await.unwrap();
        sqlx::query("INSERT INTO agent_task_queue (id, workspace_id, runtime_id, agent_id, status, created_at) VALUES ('t1','ws-a','rt','ag','queued',0)").execute(pool).await.unwrap();

        // Default agent_kind is claude (the column default).
        assert_eq!(
            CardParityRepo::get_task_repo_agent(pool, "t1").await.unwrap(),
            Some((None, AgentKind::Claude))
        );

        let mut tx = pool.begin().await.unwrap();
        CardParityRepo::set_task_repo_agent_in_tx(&mut tx, "t1", Some("scratch"), AgentKind::Codex)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            CardParityRepo::get_task_repo_agent(pool, "t1").await.unwrap(),
            Some((Some("scratch".to_string()), AgentKind::Codex))
        );
    }

    /// The full F4 cascade honours the precedence and last-used wins.
    #[tokio::test]
    async fn cascade_reads_all_four_tiers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_board(pool, "ws-a", "b1").await;

        // Nothing set → falls back to the hard default (claude).
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1")).await.unwrap(),
            AgentKind::Claude
        );

        // Global only.
        DaemonConfigRepo::set(pool, AGENT_GLOBAL_DEFAULT_KEY, "codex").await.unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1")).await.unwrap(),
            AgentKind::Codex
        );

        // Workspace beats global.
        CardParityRepo::set_workspace_default_agent(pool, &ws("ws-a"), Some(AgentKind::Copilot))
            .await
            .unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1")).await.unwrap(),
            AgentKind::Copilot
        );

        // Board beats workspace.
        CardParityRepo::set_board_default_agent(pool, &ws("ws-a"), "b1", Some(AgentKind::Claude))
            .await
            .unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1")).await.unwrap(),
            AgentKind::Claude
        );

        // Last-used beats everything.
        CardParityRepo::set_last_used_agent(pool, AgentKind::Codex).await.unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1")).await.unwrap(),
            AgentKind::Codex
        );

        // Without a board id the board tier is skipped (workspace default wins
        // over global, last-used still on top).
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), None).await.unwrap(),
            AgentKind::Codex
        );
    }

    /// Board default is workspace-scoped: a foreign workspace neither reads nor
    /// writes the board's default.
    #[tokio::test]
    async fn board_default_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_board(pool, "ws-a", "b1").await;

        // A write scoped to the wrong workspace matches no row.
        let wrote = CardParityRepo::set_board_default_agent(
            pool,
            &ws("ws-b"),
            "b1",
            Some(AgentKind::Codex),
        )
        .await
        .unwrap();
        assert!(!wrote, "cross-tenant board default write must miss");
        // And a read scoped to the wrong workspace sees nothing.
        assert_eq!(
            CardParityRepo::get_board_default_agent(pool, &ws("ws-b"), "b1").await.unwrap(),
            None
        );
    }
}
