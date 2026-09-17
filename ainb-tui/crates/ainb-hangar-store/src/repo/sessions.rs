//! Typed repository wrapper over the `sessions` table (migration 0101, spec P6d, #1166).
//!
//! Replaces `~/.agents-in-a-box/sessions.json` with a daemon-owned SQLite table
//! behind RPC, imported once at boot.
//!
//! Carries all thirteen fields of `SessionMetadata` (session_manager.rs:94-124).

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};

/// One session row in the `sessions` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRow {
    /// Unique session ID (UUID string).
    pub session_id: String,
    /// Tmux session name (unique).
    pub tmux_session_name: String,
    /// Worktree path string.
    pub worktree_path: String,
    /// Owning workspace slug or name.
    pub workspace_name: String,
    /// Creation timestamp in epoch milliseconds.
    pub created_at: i64,
    /// Agent type string (e.g. "Claude", "Codex", "Shell").
    pub agent_type: String,
    /// Whether headroom proxy is enabled.
    pub headroom_enabled: bool,
    /// Whether RTK hooks are enabled.
    pub rtk_enabled: bool,
    /// Optional skip_permissions flag (None means default/legacy).
    pub skip_permissions: Option<bool>,
    /// Model name or ID override.
    pub model: Option<String>,
    /// Model source string ("LegacyTyped", "Raw").
    pub model_source: String,
    /// Legacy Codex model name.
    pub codex_model: Option<String>,
    /// Shared Codex app-server remote thread ID.
    pub codex_thread_id: Option<String>,
}

impl SessionRow {
    fn from_row(row: sqlx::sqlite::SqliteRow) -> Self {
        let headroom_i64: i64 = row.get("headroom_enabled");
        let rtk_i64: i64 = row.get("rtk_enabled");
        let skip_permissions_i64: Option<i64> = row.get("skip_permissions");

        Self {
            session_id: row.get("session_id"),
            tmux_session_name: row.get("tmux_session_name"),
            worktree_path: row.get("worktree_path"),
            workspace_name: row.get("workspace_name"),
            created_at: row.get("created_at"),
            agent_type: row.get("agent_type"),
            headroom_enabled: headroom_i64 != 0,
            rtk_enabled: rtk_i64 != 0,
            skip_permissions: skip_permissions_i64.map(|v| v != 0),
            model: row.get("model"),
            model_source: row.get("model_source"),
            codex_model: row.get("codex_model"),
            codex_thread_id: row.get("codex_thread_id"),
        }
    }
}

/// Stateless typed repository over the `sessions` table.
pub struct SessionsRepo;

impl SessionsRepo {
    /// List all sessions, optionally filtered by workspace name, newest first.
    pub async fn list(
        pool: &SqlitePool,
        workspace_name: Option<&str>,
    ) -> Result<Vec<SessionRow>, sqlx::Error> {
        let rows = if let Some(ws) = workspace_name {
            sqlx::query(
                "SELECT session_id, tmux_session_name, worktree_path, workspace_name, \
                 created_at, agent_type, headroom_enabled, rtk_enabled, skip_permissions, \
                 model, model_source, codex_model, codex_thread_id \
                 FROM sessions WHERE workspace_name = ? ORDER BY created_at DESC",
            )
            .bind(ws)
            .fetch_all(pool)
            .await?
        } else {
            sqlx::query(
                "SELECT session_id, tmux_session_name, worktree_path, workspace_name, \
                 created_at, agent_type, headroom_enabled, rtk_enabled, skip_permissions, \
                 model, model_source, codex_model, codex_thread_id \
                 FROM sessions ORDER BY created_at DESC",
            )
            .fetch_all(pool)
            .await?
        };

        Ok(rows.into_iter().map(SessionRow::from_row).collect())
    }

    /// Read a session by its UUID string.
    pub async fn get_by_id(
        pool: &SqlitePool,
        session_id: &str,
    ) -> Result<Option<SessionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT session_id, tmux_session_name, worktree_path, workspace_name, \
             created_at, agent_type, headroom_enabled, rtk_enabled, skip_permissions, \
             model, model_source, codex_model, codex_thread_id \
             FROM sessions WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(pool)
        .await?;

        Ok(row.map(SessionRow::from_row))
    }

    /// Read a session by its tmux session name.
    pub async fn get_by_tmux_name(
        pool: &SqlitePool,
        tmux_name: &str,
    ) -> Result<Option<SessionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT session_id, tmux_session_name, worktree_path, workspace_name, \
             created_at, agent_type, headroom_enabled, rtk_enabled, skip_permissions, \
             model, model_source, codex_model, codex_thread_id \
             FROM sessions WHERE tmux_session_name = ?",
        )
        .bind(tmux_name)
        .fetch_optional(pool)
        .await?;

        Ok(row.map(SessionRow::from_row))
    }

    /// Upsert a session into the table atomically.
    pub async fn upsert(pool: &SqlitePool, session: &SessionRow) -> Result<(), sqlx::Error> {
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM sessions WHERE session_id = ? OR tmux_session_name = ?")
            .bind(&session.session_id)
            .bind(&session.tmux_session_name)
            .execute(&mut *tx)
            .await?;

        sqlx::query(
            "INSERT INTO sessions ( \
             session_id, tmux_session_name, worktree_path, workspace_name, \
             created_at, agent_type, headroom_enabled, rtk_enabled, \
             skip_permissions, model, model_source, codex_model, codex_thread_id \
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&session.session_id)
        .bind(&session.tmux_session_name)
        .bind(&session.worktree_path)
        .bind(&session.workspace_name)
        .bind(session.created_at)
        .bind(&session.agent_type)
        .bind(i64::from(session.headroom_enabled))
        .bind(i64::from(session.rtk_enabled))
        .bind(session.skip_permissions.map(i64::from))
        .bind(&session.model)
        .bind(&session.model_source)
        .bind(&session.codex_model)
        .bind(&session.codex_thread_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Delete a session by its UUID string.
    pub async fn delete_by_id(pool: &SqlitePool, session_id: &str) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("DELETE FROM sessions WHERE session_id = ?")
            .bind(session_id)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Delete a session by its tmux session name.
    pub async fn delete_by_tmux_name(
        pool: &SqlitePool,
        tmux_name: &str,
    ) -> Result<bool, sqlx::Error> {
        let res = sqlx::query("DELETE FROM sessions WHERE tmux_session_name = ?")
            .bind(tmux_name)
            .execute(pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }
}
