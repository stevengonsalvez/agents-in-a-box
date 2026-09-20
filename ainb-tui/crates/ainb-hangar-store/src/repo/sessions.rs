//! Typed repository wrapper over the `sessions` table (migration 0101, spec P6d, #1166).
//!
//! Replaces `~/.agents-in-a-box/sessions.json` with a daemon-owned SQLite table
//! behind RPC, imported once at boot.
//!
//! Carries all thirteen fields of `SessionMetadata` (session_manager.rs:94-124).

use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection, SqlitePool};

/// What [`SessionsRepo::upsert`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    /// The row was inserted or updated.
    Written,
    /// The tmux session name is bound to another session id; nothing changed.
    TmuxNameTaken {
        /// The session id that holds the name.
        holder: String,
    },
}

/// The completion marker of one `sessions.json` import (migration 0102).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportMarker {
    /// The file the import read.
    pub source_path: String,
    /// Unix milliseconds when the import finished.
    pub completed_at: i64,
    /// Rows written.
    pub imported: i64,
    /// Records whose id or tmux name was already present.
    pub skipped: i64,
    /// Records that failed validation and stayed in the file only.
    pub rejected: i64,
}

/// What [`SessionsRepo::complete_import`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportOutcome {
    /// This call imported and wrote the marker.
    Completed(ImportMarker),
    /// A marker for the source already existed; nothing was written.
    AlreadyCompleted,
}

/// Which table rows a reconcile pass may delete, which is the one thing the
/// flip changes about a pass.
///
/// Before the flip the FILE is the authority on which sessions exist and the
/// table on their contents (the orchestrator's rule on #1250): only the file
/// was written by every surface, so a row the file lacks is a row a client
/// deleted from the file and could not delete from the table. After the flip
/// the TABLE is the authority and the file is a mirror kept row by row, so a
/// row the file lacks is far more likely a mirror that has not caught up than
/// a session someone ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Deletes {
    /// Pre-flip: every table row whose session the file does not have.
    EveryRowTheFileLacks,
    /// Post-flip, with a file that exists and parses: only rows the file has
    /// had a chance to carry, that is rows created no later than the file's
    /// own last write. An older binary's `ainb kill` is still reconciled,
    /// and a row a new writer created between two passes is never taken for
    /// one the file dropped.
    RowsNoNewerThan(i64),
    /// Post-flip, with no file to read: the mirror was lost, not emptied.
    /// Nothing is deleted and the pass still commits its marker, so readers
    /// are served from the table rather than waiting for a file that may
    /// never come back.
    Nothing,
}

impl Deletes {
    /// Whether a row created at `created_at` may go.
    #[must_use]
    pub const fn may_delete(self, created_at: i64) -> bool {
        match self {
            Self::EveryRowTheFileLacks => true,
            Self::RowsNoNewerThan(stamp) => created_at <= stamp,
            Self::Nothing => false,
        }
    }
}

/// One `sessions.json` record offered to [`SessionsRepo::complete_reconcile`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSession {
    /// The record as a table row.
    pub row: SessionRow,
    /// The record had no `session_id` and the reader minted one. A minted id
    /// is new on every read, so for such a record the tmux name, not the id,
    /// says whether the table already holds it.
    pub id_minted: bool,
}

/// A file session not inserted because its tmux name is bound to another id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameConflict {
    /// The file record's session id.
    pub session_id: String,
    /// The tmux name both claim.
    pub tmux_session_name: String,
    /// The session id the table binds the name to (the table wins).
    pub holder: String,
}

/// What one [`SessionsRepo::complete_reconcile`] pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconcileOutcome {
    /// The `<path>#reconcile` marker as this pass wrote it. `imported` counts
    /// rows inserted, `skipped` name conflicts, `rejected` the caller's count
    /// of records that failed validation.
    pub marker: ImportMarker,
    /// Every name conflict of this pass, for the caller to report.
    pub conflicts: Vec<NameConflict>,
    /// Table rows this pass deleted because the file no longer has them (the
    /// file is the authority on which sessions exist until the flip). Not a
    /// marker column: that would take a migration, which the goal rules out.
    pub deleted: Vec<String>,
}

/// The `session_import` key of the repeatable reconcile of `source_path`
/// (P6e). It sits beside the one-time import's `source_path` row in the same
/// table, so no migration widens the primary key.
#[must_use]
pub fn reconcile_key(source_path: &str) -> String {
    format!("{source_path}#reconcile")
}

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
    /// List at most `limit` sessions, optionally filtered by workspace name,
    /// newest first.
    ///
    /// The caller picks the bound: the RPC handler asks for one row past its
    /// cap so it can tell the client the answer was truncated.
    pub async fn list(
        pool: &SqlitePool,
        workspace_name: Option<&str>,
        limit: u32,
    ) -> Result<Vec<SessionRow>, sqlx::Error> {
        let rows = if let Some(ws) = workspace_name {
            sqlx::query(
                "SELECT session_id, tmux_session_name, worktree_path, workspace_name, \
                 created_at, agent_type, headroom_enabled, rtk_enabled, skip_permissions, \
                 model, model_source, codex_model, codex_thread_id \
                 FROM sessions WHERE workspace_name = ? \
                 ORDER BY created_at DESC, session_id LIMIT ?",
            )
            .bind(ws)
            .bind(i64::from(limit))
            .fetch_all(pool)
            .await?
        } else {
            sqlx::query(
                "SELECT session_id, tmux_session_name, worktree_path, workspace_name, \
                 created_at, agent_type, headroom_enabled, rtk_enabled, skip_permissions, \
                 model, model_source, codex_model, codex_thread_id \
                 FROM sessions ORDER BY created_at DESC, session_id LIMIT ?",
            )
            .bind(i64::from(limit))
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

    /// Insert or update the row keyed by `session.session_id`.
    ///
    /// A tmux session name is bound to one session id at a time. When another
    /// session id already holds `session.tmux_session_name` the write is
    /// refused with [`UpsertOutcome::TmuxNameTaken`] and neither row changes:
    /// rebinding a name to a new identity is a delete followed by an upsert,
    /// done deliberately by the caller, never a side effect of an upsert.
    pub async fn upsert(
        pool: &SqlitePool,
        session: &SessionRow,
    ) -> Result<UpsertOutcome, sqlx::Error> {
        // IMMEDIATE takes the write lock before the name check, so no other
        // writer can bind the name between the check and the insert. A
        // dropped transaction rolls back.
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;
        let outcome = Self::upsert_on(&mut tx, session).await?;
        if outcome == UpsertOutcome::Written {
            tx.commit().await?;
        }
        Ok(outcome)
    }

    async fn upsert_on(
        conn: &mut SqliteConnection,
        session: &SessionRow,
    ) -> Result<UpsertOutcome, sqlx::Error> {
        let holder: Option<String> = sqlx::query_scalar(
            "SELECT session_id FROM sessions WHERE tmux_session_name = ? AND session_id != ?",
        )
        .bind(&session.tmux_session_name)
        .bind(&session.session_id)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(holder) = holder {
            return Ok(UpsertOutcome::TmuxNameTaken { holder });
        }

        sqlx::query(
            "INSERT INTO sessions ( \
             session_id, tmux_session_name, worktree_path, workspace_name, \
             created_at, agent_type, headroom_enabled, rtk_enabled, \
             skip_permissions, model, model_source, codex_model, codex_thread_id \
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(session_id) DO UPDATE SET \
             tmux_session_name = excluded.tmux_session_name, \
             worktree_path = excluded.worktree_path, \
             workspace_name = excluded.workspace_name, \
             created_at = excluded.created_at, \
             agent_type = excluded.agent_type, \
             headroom_enabled = excluded.headroom_enabled, \
             rtk_enabled = excluded.rtk_enabled, \
             skip_permissions = excluded.skip_permissions, \
             model = excluded.model, \
             model_source = excluded.model_source, \
             codex_model = excluded.codex_model, \
             codex_thread_id = excluded.codex_thread_id",
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
        .execute(&mut *conn)
        .await?;

        Ok(UpsertOutcome::Written)
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

    /// The completion marker for the import of `source_path`, if that import
    /// has finished (migration 0102).
    pub async fn import_marker(
        pool: &SqlitePool,
        source_path: &str,
    ) -> Result<Option<ImportMarker>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT source_path, completed_at, imported, skipped, rejected \
             FROM session_import WHERE source_path = ?",
        )
        .bind(source_path)
        .fetch_optional(pool)
        .await?;
        Ok(row.map(|row| ImportMarker {
            source_path: row.get("source_path"),
            completed_at: row.get("completed_at"),
            imported: row.get("imported"),
            skipped: row.get("skipped"),
            rejected: row.get("rejected"),
        }))
    }

    /// Whether the table is authoritative for `source_path`: its one-time
    /// import AND at least one reconcile pass have completed.
    ///
    /// The P6d import row alone is not enough. Sessions created while P6d was
    /// dark are in the file only, so until a reconcile pass has inserted them
    /// the table is missing rows a client would otherwise stop seeing.
    pub async fn import_complete_for(
        pool: &SqlitePool,
        source_path: &str,
    ) -> Result<bool, sqlx::Error> {
        let n: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM session_import WHERE source_path IN (?, ?)")
                .bind(source_path)
                .bind(reconcile_key(source_path))
                .fetch_one(pool)
                .await?;
        Ok(n == 2)
    }

    /// Make the table hold the file's sessions, and record the pass on the
    /// `<path>#reconcile` marker, in one `IMMEDIATE` transaction.
    ///
    /// Repeatable, unlike [`Self::complete_import`]. A pass:
    /// - deletes the table rows `deletes` allows it to, first, returned in
    ///   `deleted`;
    /// - then inserts every file session the table lacks;
    /// - leaves a row the file and the table both have untouched: the table
    ///   wins on contents.
    ///
    /// Which rows may be deleted is the difference the flip makes, and the
    /// caller decides it from its own compiled catalogue; see [`Deletes`].
    ///
    /// A record with a minted id matches by tmux name. A file session whose
    /// tmux name is still bound to another id after the deletes is skipped
    /// and returned as a [`NameConflict`]. Sound only while every writer
    /// writes the file row before the table row, which every pre-flip writer
    /// does. On any error the transaction rolls back and the marker keeps its
    /// previous value, or stays absent.
    pub async fn complete_reconcile(
        pool: &SqlitePool,
        source_path: &str,
        sessions: &[FileSession],
        rejected: i64,
        completed_at: i64,
        deletes: Deletes,
    ) -> Result<ReconcileOutcome, sqlx::Error> {
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;

        let file_ids: std::collections::HashSet<&str> = sessions
            .iter()
            .filter(|s| !s.id_minted)
            .map(|s| s.row.session_id.as_str())
            .collect();
        let minted_names: std::collections::HashSet<&str> = sessions
            .iter()
            .filter(|s| s.id_minted)
            .map(|s| s.row.tmux_session_name.as_str())
            .collect();
        let table: Vec<(String, String, i64)> =
            sqlx::query_as("SELECT session_id, tmux_session_name, created_at FROM sessions")
                .fetch_all(&mut *tx)
                .await?;
        let mut deleted = Vec::new();
        for (session_id, tmux_name, created_at) in table {
            if file_ids.contains(session_id.as_str()) || minted_names.contains(tmux_name.as_str()) {
                continue;
            }
            if !deletes.may_delete(created_at) {
                continue;
            }
            sqlx::query("DELETE FROM sessions WHERE session_id = ?")
                .bind(&session_id)
                .execute(&mut *tx)
                .await?;
            deleted.push(session_id);
        }

        let mut imported = 0_i64;
        let mut conflicts = Vec::new();
        for session in sessions {
            let row = &session.row;
            let present: Option<String> = if session.id_minted {
                sqlx::query_scalar("SELECT session_id FROM sessions WHERE tmux_session_name = ?")
                    .bind(&row.tmux_session_name)
                    .fetch_optional(&mut *tx)
                    .await?
            } else {
                sqlx::query_scalar("SELECT session_id FROM sessions WHERE session_id = ?")
                    .bind(&row.session_id)
                    .fetch_optional(&mut *tx)
                    .await?
            };
            if present.is_some() {
                continue;
            }
            match Self::upsert_on(&mut tx, row).await? {
                UpsertOutcome::Written => imported += 1,
                UpsertOutcome::TmuxNameTaken { holder } => conflicts.push(NameConflict {
                    session_id: row.session_id.clone(),
                    tmux_session_name: row.tmux_session_name.clone(),
                    holder,
                }),
            }
        }

        let marker = ImportMarker {
            source_path: reconcile_key(source_path),
            completed_at,
            imported,
            skipped: i64::try_from(conflicts.len()).unwrap_or(i64::MAX),
            rejected,
        };
        sqlx::query(
            "INSERT INTO session_import (source_path, completed_at, imported, skipped, rejected) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(source_path) DO UPDATE SET \
             completed_at = excluded.completed_at, imported = excluded.imported, \
             skipped = excluded.skipped, rejected = excluded.rejected",
        )
        .bind(&marker.source_path)
        .bind(marker.completed_at)
        .bind(marker.imported)
        .bind(marker.skipped)
        .bind(marker.rejected)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(ReconcileOutcome {
            marker,
            conflicts,
            deleted,
        })
    }

    /// Write the imported `rows` and the completion marker for `source_path`
    /// in one `IMMEDIATE` transaction.
    ///
    /// A row whose session id or tmux name is already present is skipped and
    /// counted, never overwritten. When a marker for `source_path` already
    /// exists nothing is written and [`ImportOutcome::AlreadyCompleted`] is
    /// returned, so two daemons booting on one home import once.
    /// `rejected` is the caller's count of records that failed validation;
    /// it is stored on the marker so the failure stays visible.
    pub async fn complete_import(
        pool: &SqlitePool,
        source_path: &str,
        rows: &[SessionRow],
        rejected: i64,
        completed_at: i64,
    ) -> Result<ImportOutcome, sqlx::Error> {
        let mut tx = pool.begin_with(crate::repo::fleet::IMMEDIATE_TRANSACTION).await?;
        let outcome =
            Self::complete_import_on(&mut tx, source_path, rows, rejected, completed_at).await?;
        if matches!(outcome, ImportOutcome::Completed(_)) {
            tx.commit().await?;
        }
        Ok(outcome)
    }

    async fn complete_import_on(
        conn: &mut SqliteConnection,
        source_path: &str,
        rows: &[SessionRow],
        rejected: i64,
        completed_at: i64,
    ) -> Result<ImportOutcome, sqlx::Error> {
        let done: Option<String> =
            sqlx::query_scalar("SELECT source_path FROM session_import WHERE source_path = ?")
                .bind(source_path)
                .fetch_optional(&mut *conn)
                .await?;
        if done.is_some() {
            return Ok(ImportOutcome::AlreadyCompleted);
        }

        let mut imported = 0_i64;
        let mut skipped = 0_i64;
        for row in rows {
            let present: Option<String> = sqlx::query_scalar(
                "SELECT session_id FROM sessions WHERE session_id = ? OR tmux_session_name = ?",
            )
            .bind(&row.session_id)
            .bind(&row.tmux_session_name)
            .fetch_optional(&mut *conn)
            .await?;
            if present.is_some() {
                skipped += 1;
                continue;
            }
            match Self::upsert_on(conn, row).await? {
                UpsertOutcome::Written => imported += 1,
                UpsertOutcome::TmuxNameTaken { .. } => skipped += 1,
            }
        }

        sqlx::query(
            "INSERT INTO session_import (source_path, completed_at, imported, skipped, rejected) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(source_path)
        .bind(completed_at)
        .bind(imported)
        .bind(skipped)
        .bind(rejected)
        .execute(&mut *conn)
        .await?;

        Ok(ImportOutcome::Completed(ImportMarker {
            source_path: source_path.to_string(),
            completed_at,
            imported,
            skipped,
            rejected,
        }))
    }
}
