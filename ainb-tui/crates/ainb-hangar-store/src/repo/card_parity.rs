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

use ainb_hangar_core::agent_kind::{AgentKind, resolve_agent_cascade};
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
    /// Persist a card's repo and/or agent onto its issue row (the durable card
    /// state), scoped to `workspace_id` so the write is self-guarded at the SQL
    /// boundary (a foreign-tenant issue id matches no row — never a cross-tenant
    /// write).
    ///
    /// This is a PARTIAL update: only the columns whose `Option` is `Some` are
    /// written; a `None` field is LEFT UNCHANGED (not cleared to NULL). So an
    /// agent-only edit never drops the card's repo, and a repo-only edit never
    /// drops its chosen provider — the two are independently editable. Passing both
    /// `None` is a no-op (`Ok(false)`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault. Returns `Ok(false)` when no
    /// `(issue_id, workspace_id)` row matched (an unknown / foreign issue), or when
    /// both fields are `None` (nothing to write).
    pub async fn set_issue_repo_agent(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
        repo_ref: Option<&str>,
        agent_kind: Option<AgentKind>,
    ) -> Result<bool, sqlx::Error> {
        // Build the SET clause from only the provided columns (mirrors
        // `IssueRepo::update_fields`): a `None` column is left untouched.
        let mut sets: Vec<&str> = Vec::new();
        if repo_ref.is_some() {
            sets.push("repo_ref = ?");
        }
        if agent_kind.is_some() {
            sets.push("agent_kind = ?");
        }
        if sets.is_empty() {
            return Ok(false);
        }
        let sql = format!(
            "UPDATE issue SET {} WHERE id = ? AND workspace_id = ?",
            sets.join(", ")
        );
        let mut query = sqlx::query(&sql);
        if let Some(repo) = repo_ref {
            query = query.bind(repo);
        }
        if let Some(agent) = agent_kind {
            query = query.bind(agent.as_str());
        }
        let res = query.bind(issue_id).bind(workspace_id).execute(pool).await?;
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

    /// Assign (or clear, with `None`) a card's SQUAD onto its issue row (tcp T4 /
    /// F7). Workspace-scoped so the write is self-guarded at the SQL boundary (a
    /// foreign-tenant issue id matches no row — never a cross-tenant write).
    ///
    /// A `Some(squad_id)` makes a card RUN fan out across the whole squad; `None`
    /// clears the squad, reverting the card to a single-agent run. The squad ref's
    /// existence in the workspace is checked by the run handler at fan-out time
    /// (the column carries no FK), so a stale squad id here fails the run cleanly
    /// rather than corrupting the card.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault. Returns `Ok(false)` when no
    /// `(issue_id, workspace_id)` row matched (an unknown / foreign issue).
    pub async fn set_issue_squad(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        squad_id: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("UPDATE issue SET squad_id = ? WHERE id = ? AND workspace_id = ?")
            .bind(squad_id)
            .bind(issue_id)
            .bind(workspace.as_str())
            .execute(pool)
            .await?;
        Ok(res.rows_affected() == 1)
    }

    /// Read a card's assigned squad id (`None` when unassigned / issue absent) —
    /// the run handler reads this to decide single-agent vs squad fan-out.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on a store fault.
    pub async fn get_issue_squad(
        pool: &SqlitePool,
        issue_id: &str,
    ) -> Result<Option<String>, sqlx::Error> {
        let raw: Option<Option<String>> =
            sqlx::query_scalar("SELECT squad_id FROM issue WHERE id = ?")
                .bind(issue_id)
                .fetch_optional(pool)
                .await?;
        Ok(raw.flatten())
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
        let res =
            sqlx::query("UPDATE board SET default_agent = ? WHERE id = ? AND workspace_id = ?")
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
        let raw: Option<Option<String>> =
            sqlx::query_scalar("SELECT default_agent FROM board WHERE id = ? AND workspace_id = ?")
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
        Ok(resolve_agent_cascade(
            last_used,
            board,
            workspace_default,
            global,
        ))
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
            "ws-a",
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

    /// `set_issue_repo_agent` is a PARTIAL, workspace-scoped write: an agent-only
    /// edit never drops the repo, a repo-only edit never drops the agent, and a
    /// foreign-workspace write matches no row.
    #[tokio::test]
    async fn set_issue_repo_agent_is_partial_and_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        // Seed both fields.
        CardParityRepo::set_issue_repo_agent(
            pool,
            "ws-a",
            "issue-1",
            Some("/repos/app"),
            Some(AgentKind::Codex),
        )
        .await
        .unwrap();

        // Agent-only edit leaves the repo untouched.
        let ok = CardParityRepo::set_issue_repo_agent(
            pool,
            "ws-a",
            "issue-1",
            None,
            Some(AgentKind::Claude),
        )
        .await
        .unwrap();
        assert!(ok);
        assert_eq!(
            CardParityRepo::get_issue_repo_agent(pool, "issue-1").await.unwrap(),
            Some((Some("/repos/app".to_string()), Some(AgentKind::Claude))),
            "an agent-only edit must not drop the repo"
        );

        // Repo-only edit leaves the agent untouched.
        let ok =
            CardParityRepo::set_issue_repo_agent(pool, "ws-a", "issue-1", Some("scratch"), None)
                .await
                .unwrap();
        assert!(ok);
        assert_eq!(
            CardParityRepo::get_issue_repo_agent(pool, "issue-1").await.unwrap(),
            Some((Some("scratch".to_string()), Some(AgentKind::Claude))),
            "a repo-only edit must not drop the agent"
        );

        // Both-None is a no-op; a foreign-workspace write matches no row.
        assert!(
            !CardParityRepo::set_issue_repo_agent(pool, "ws-a", "issue-1", None, None)
                .await
                .unwrap()
        );
        assert!(
            !CardParityRepo::set_issue_repo_agent(pool, "ws-b", "issue-1", Some("/x"), None)
                .await
                .unwrap(),
            "a cross-tenant repo/agent write must miss"
        );
        assert_eq!(
            CardParityRepo::get_issue_repo_agent(pool, "issue-1").await.unwrap(),
            Some((Some("scratch".to_string()), Some(AgentKind::Claude))),
            "the no-op + cross-tenant writes left the row unchanged"
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
        sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u','u@e.com',0)")
            .execute(pool)
            .await
            .unwrap();
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
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1"))
                .await
                .unwrap(),
            AgentKind::Claude
        );

        // Global only.
        DaemonConfigRepo::set(pool, AGENT_GLOBAL_DEFAULT_KEY, "codex").await.unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1"))
                .await
                .unwrap(),
            AgentKind::Codex
        );

        // Workspace beats global.
        CardParityRepo::set_workspace_default_agent(pool, &ws("ws-a"), Some(AgentKind::Copilot))
            .await
            .unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1"))
                .await
                .unwrap(),
            AgentKind::Copilot
        );

        // Board beats workspace.
        CardParityRepo::set_board_default_agent(pool, &ws("ws-a"), "b1", Some(AgentKind::Claude))
            .await
            .unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1"))
                .await
                .unwrap(),
            AgentKind::Claude
        );

        // Last-used beats everything.
        CardParityRepo::set_last_used_agent(pool, AgentKind::Codex).await.unwrap();
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), Some("b1"))
                .await
                .unwrap(),
            AgentKind::Codex
        );

        // Without a board id the board tier is skipped (workspace default wins
        // over global, last-used still on top).
        assert_eq!(
            CardParityRepo::resolve_agent_cascade(pool, &ws("ws-a"), None).await.unwrap(),
            AgentKind::Codex
        );
    }

    /// A card's squad assignment round-trips, defaults None, clears with None, and
    /// is workspace-scoped.
    #[tokio::test]
    async fn issue_squad_round_trips_and_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        seed_issue(pool, "ws-a", "issue-1").await;

        // Unassigned by default; a missing issue reads None.
        assert_eq!(
            CardParityRepo::get_issue_squad(pool, "issue-1").await.unwrap(),
            None
        );
        assert_eq!(
            CardParityRepo::get_issue_squad(pool, "nope").await.unwrap(),
            None
        );

        // Assign a squad.
        assert!(
            CardParityRepo::set_issue_squad(pool, &ws("ws-a"), "issue-1", Some("sq-1"))
                .await
                .unwrap()
        );
        assert_eq!(
            CardParityRepo::get_issue_squad(pool, "issue-1").await.unwrap(),
            Some("sq-1".to_string())
        );

        // A cross-tenant write misses (leaves the assignment intact).
        assert!(
            !CardParityRepo::set_issue_squad(pool, &ws("ws-b"), "issue-1", Some("sq-x"))
                .await
                .unwrap()
        );
        assert_eq!(
            CardParityRepo::get_issue_squad(pool, "issue-1").await.unwrap(),
            Some("sq-1".to_string())
        );

        // Clear it.
        assert!(
            CardParityRepo::set_issue_squad(pool, &ws("ws-a"), "issue-1", None)
                .await
                .unwrap()
        );
        assert_eq!(
            CardParityRepo::get_issue_squad(pool, "issue-1").await.unwrap(),
            None
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
