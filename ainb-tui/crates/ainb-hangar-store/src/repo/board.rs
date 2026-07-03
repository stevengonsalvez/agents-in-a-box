//! Typed repository wrapper over the `board` × `board_column` × `board_card`
//! tables (P4 / D8 — user-defined kanban boards).
//!
//! A [`Board`] is a workspace-scoped kanban board with an ordered set of
//! user-defined [`BoardColumnRow`]s and a set of [`BoardCard`] placements
//! (`card = issue`, §4.5). This repo is the only mutation + read surface over the
//! three tables: create/rename/retune/delete a board, add/rename/retune/delete/
//! reorder its columns, place/move a card, and — crucially — the AUTO-MOVE hook
//! ([`BoardRepo::auto_move_on_state`]) the daemon fires on every task FSM
//! transition.
//!
//! # Auto-move (the dispatch hook)
//!
//! When a task reaches a new FSM state, the daemon resolves the task's issue and
//! calls [`BoardRepo::auto_move_on_state`]. For every board in the workspace that
//! carries that issue as a card, the card jumps to the column whose `fsm_state`
//! matches the new status — but ONLY when the board's `auto_move` master toggle
//! AND the target column's `auto_move` flag are both on (D8 "column↔FSM-state
//! mapping + per-board auto-move toggle"). The moved placements are returned so
//! the caller can emit events; the TUI reflects the move on the next
//! `hangar/boards_list` snapshot.
//!
//! # Workspace scoping
//!
//! **Every method takes a [`WorkspaceId`] and enforces it in SQL.** A board id
//! from another tenant resolves to no row (a [`BoardRepoError::NotFound`], never a
//! cross-tenant edit), and listing a foreign / unknown workspace yields an empty
//! vec. The `(workspace_id, name)` UNIQUE index is the resolve-or-reject guard:
//! creating (or renaming into) a board name that already exists in the workspace
//! is rejected with [`BoardRepoError::DuplicateName`].
//!
//! `PRAGMA foreign_keys` is off in this crate (mirroring the rest of the repos),
//! so the `(workspace_id, name)` UNIQUE index, the `(board_id, issue_id)`
//! composite PK, and the `board_id`/`column_id` FKs are the engine-enforced
//! invariants; the workspace-scope guard, the `ord` contiguity, and the
//! card-reparking on column delete are enforced here in application code.

use ainb_hangar_core::ids::WorkspaceId;
use sqlx::{Row, SqlitePool};

/// One board with its columns and card placements (the list row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    /// The board's id (`board.id`).
    pub id: String,
    /// The board's name (unique within its workspace).
    pub name: String,
    /// The per-board auto-move master toggle. When off, no card on this board
    /// auto-moves regardless of the columns' own flags.
    pub auto_move: bool,
    /// The board's columns, ordered left-to-right by `ord`.
    pub columns: Vec<BoardColumnRow>,
    /// The board's card placements (`issue_id` → `column_id`). A card with
    /// `column_id = None` is UNMAPPED (parked after its column was deleted).
    pub cards: Vec<BoardCard>,
}

/// One user-defined board column (a pure read row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardColumnRow {
    /// The column's stable surrogate id (`board_column.id`) — what a card's
    /// `column_id` and a reorder key off (never the volatile `ord`).
    pub id: String,
    /// The column's left-to-right position (contiguous `0..n`, repo-maintained).
    pub ord: i64,
    /// The column's display name.
    pub name: String,
    /// The task-FSM status this column represents (`queued`/`running`/`done`/…),
    /// or `None` for a purely manual column. The auto-move target key.
    pub fsm_state: Option<String>,
    /// Whether a task reaching this column's `fsm_state` auto-moves the card here.
    pub auto_move: bool,
}

/// One card placement on a board: an issue in a column (or unmapped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardCard {
    /// The placed issue's id (`issue.id`).
    pub issue_id: String,
    /// The column the card sits in, or `None` when unmapped (its column was
    /// deleted — the card is parked, never lost).
    pub column_id: Option<String>,
    /// When the card was added (epoch ms).
    pub added_at: i64,
}

/// One card the auto-move hook actually moved: which board, which issue, and the
/// column it landed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMoved {
    /// The board whose card moved.
    pub board_id: String,
    /// The issue (card) that moved.
    pub issue_id: String,
    /// The column the card landed in (the `fsm_state`-matched auto-move target).
    pub column_id: String,
}

/// Stateless typed wrapper over the `board` + `board_column` + `board_card` tables.
pub struct BoardRepo;

impl BoardRepo {
    /// Create an EMPTY board named `name` in `workspace`, returning its id.
    ///
    /// Columns are added afterwards via [`BoardRepo::column_add`]. The board's
    /// `auto_move` master toggle defaults ON.
    ///
    /// # Errors
    ///
    /// Returns [`BoardRepoError::DuplicateName`] when the name is taken in the
    /// workspace (the `(workspace_id, name)` UNIQUE index), or
    /// [`BoardRepoError::Db`] on a store failure.
    pub async fn create(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &str,
        name: &str,
        created_at: i64,
    ) -> Result<String, BoardRepoError> {
        let res = sqlx::query(
            "INSERT INTO board (id, workspace_id, name, auto_move, created_at) \
             VALUES (?, ?, ?, 1, ?)",
        )
        .bind(id)
        .bind(workspace.as_str())
        .bind(name)
        .bind(created_at)
        .execute(pool)
        .await;
        match res {
            Ok(_) => Ok(id.to_string()),
            Err(e) if is_unique_violation(&e) => Err(BoardRepoError::DuplicateName),
            Err(e) => Err(BoardRepoError::Db(e)),
        }
    }

    /// List every board of `workspace` with its columns (ordered) and cards.
    ///
    /// Workspace-scoped: a foreign / unknown workspace yields an empty vec.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a query fails.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<Board>, sqlx::Error> {
        let board_rows = sqlx::query(
            "SELECT id, name, auto_move FROM board WHERE workspace_id = ? ORDER BY name, id",
        )
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;

        let mut boards = Vec::with_capacity(board_rows.len());
        for r in &board_rows {
            let id: String = r.try_get("id")?;
            let columns = Self::columns_of(pool, &id).await?;
            let cards = Self::cards_of(pool, &id).await?;
            boards.push(Board {
                id,
                name: r.try_get("name")?,
                auto_move: r.try_get::<i64, _>("auto_move")? != 0,
                columns,
                cards,
            });
        }
        Ok(boards)
    }

    /// Update a board's `name` and/or `auto_move` master toggle. `None` fields are
    /// left unchanged.
    ///
    /// Workspace-scoped: a board not in `workspace` is rejected with
    /// [`BoardRepoError::NotFound`]. A rename that collides with another board's
    /// name in the workspace is rejected with [`BoardRepoError::DuplicateName`].
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] / [`BoardRepoError::DuplicateName`] /
    /// [`BoardRepoError::Db`].
    pub async fn update(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        name: Option<&str>,
        auto_move: Option<bool>,
    ) -> Result<(), BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_board_in_ws(&mut tx, workspace, board_id).await?;
        if let Some(n) = name {
            let res = sqlx::query("UPDATE board SET name = ? WHERE id = ?")
                .bind(n)
                .bind(board_id)
                .execute(&mut *tx)
                .await;
            if let Err(e) = res {
                return Err(if is_unique_violation(&e) {
                    BoardRepoError::DuplicateName
                } else {
                    BoardRepoError::Db(e)
                });
            }
        }
        if let Some(am) = auto_move {
            sqlx::query("UPDATE board SET auto_move = ? WHERE id = ?")
                .bind(i64::from(am))
                .bind(board_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Delete a board and all of its columns + card placements.
    ///
    /// Workspace-scoped: a board not in `workspace` is rejected with
    /// [`BoardRepoError::NotFound`] (never a cross-tenant delete).
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] / [`BoardRepoError::Db`].
    pub async fn delete(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
    ) -> Result<(), BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_board_in_ws(&mut tx, workspace, board_id).await?;
        sqlx::query("DELETE FROM board_card WHERE board_id = ?")
            .bind(board_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM board_column WHERE board_id = ?")
            .bind(board_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM board WHERE id = ?")
            .bind(board_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Append a column to a board at the next `ord`, returning its id.
    ///
    /// `fsm_state` (a task-FSM status token, or `None`) and `auto_move` set the
    /// column's auto-move mapping. Workspace-scoped via the board.
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] when the board is not in `workspace`, or
    /// [`BoardRepoError::Db`].
    pub async fn column_add(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        column_id: &str,
        name: &str,
        fsm_state: Option<&str>,
        auto_move: bool,
    ) -> Result<String, BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_board_in_ws(&mut tx, workspace, board_id).await?;
        // An auto-move column must be the sole target for its state on the board
        // (else the auto-move hook is ambiguous — see `auto_move_conflict`).
        if auto_move {
            if let Some(fs) = fsm_state {
                if Self::auto_move_conflict(&mut tx, board_id, fs, None).await? {
                    return Err(BoardRepoError::DuplicateAutoMove);
                }
            }
        }
        let next_ord: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(ord) + 1, 0) FROM board_column WHERE board_id = ?")
                .bind(board_id)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query(
            "INSERT INTO board_column (id, board_id, ord, name, fsm_state, auto_move) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(column_id)
        .bind(board_id)
        .bind(next_ord)
        .bind(name)
        .bind(fsm_state)
        .bind(i64::from(auto_move))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(column_id.to_string())
    }

    /// Update a column's `name`, `fsm_state`, and/or `auto_move`.
    ///
    /// `name` / `auto_move`: `None` leaves unchanged. `fsm_state`: the OUTER
    /// `None` leaves it unchanged; `Some(None)` clears it to NULL (a manual
    /// column); `Some(Some(s))` sets it.
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] when the column is not in `workspace`'s board,
    /// or [`BoardRepoError::Db`].
    pub async fn column_update(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        column_id: &str,
        name: Option<&str>,
        fsm_state: Option<Option<&str>>,
        auto_move: Option<bool>,
    ) -> Result<(), BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_column_in_ws(&mut tx, workspace, board_id, column_id).await?;
        // Compute the column's EFFECTIVE (auto_move, fsm_state) after this partial
        // update, then reject it if that would give the board two auto-move columns
        // for one state (the ambiguity guard — see `auto_move_conflict`). Read the
        // current row so an update that touches only one of the two fields still
        // sees the other's live value.
        let cur = sqlx::query("SELECT fsm_state, auto_move FROM board_column WHERE id = ?")
            .bind(column_id)
            .fetch_one(&mut *tx)
            .await?;
        let cur_fsm: Option<String> = cur.try_get("fsm_state")?;
        let cur_auto: bool = cur.try_get::<i64, _>("auto_move")? != 0;
        let res_fsm: Option<String> = match fsm_state {
            None => cur_fsm,
            Some(inner) => inner.map(str::to_string),
        };
        let res_auto = auto_move.unwrap_or(cur_auto);
        if res_auto {
            if let Some(state) = res_fsm.as_deref() {
                if Self::auto_move_conflict(&mut tx, board_id, state, Some(column_id)).await? {
                    return Err(BoardRepoError::DuplicateAutoMove);
                }
            }
        }
        if let Some(n) = name {
            sqlx::query("UPDATE board_column SET name = ? WHERE id = ?")
                .bind(n)
                .bind(column_id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(fs) = fsm_state {
            sqlx::query("UPDATE board_column SET fsm_state = ? WHERE id = ?")
                .bind(fs)
                .bind(column_id)
                .execute(&mut *tx)
                .await?;
        }
        if let Some(am) = auto_move {
            sqlx::query("UPDATE board_column SET auto_move = ? WHERE id = ?")
                .bind(i64::from(am))
                .bind(column_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Delete a column, PARKING its cards at `column_id = NULL` (the unmapped
    /// pool — no data loss), then renormalising the remaining columns' `ord` to a
    /// contiguous `0..n`.
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] when the column is not in `workspace`'s board,
    /// or [`BoardRepoError::Db`].
    pub async fn column_delete(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        column_id: &str,
    ) -> Result<(), BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_column_in_ws(&mut tx, workspace, board_id, column_id).await?;
        // Park the deleted column's cards at NULL (the unmapped pool).
        sqlx::query("UPDATE board_card SET column_id = NULL WHERE column_id = ?")
            .bind(column_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM board_column WHERE id = ?")
            .bind(column_id)
            .execute(&mut *tx)
            .await?;
        Self::renumber_ords(&mut tx, board_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Reorder a board's columns to match `ordered_ids` (index becomes `ord`).
    ///
    /// `ordered_ids` must be exactly the board's current column ids (a permutation
    /// of them); any other set — a missing id, an extra id, or a foreign id — is
    /// rejected with [`BoardRepoError::BadReorder`] and nothing is written. Since
    /// cards reference the stable `column_id`, a reorder never moves a card.
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] / [`BoardRepoError::BadReorder`] /
    /// [`BoardRepoError::Db`].
    pub async fn column_reorder(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_board_in_ws(&mut tx, workspace, board_id).await?;
        // The reorder set must be exactly the board's current columns.
        let current: Vec<String> =
            sqlx::query_scalar("SELECT id FROM board_column WHERE board_id = ?")
                .bind(board_id)
                .fetch_all(&mut *tx)
                .await?;
        use std::collections::HashSet;
        let current_set: HashSet<&str> = current.iter().map(String::as_str).collect();
        let given_set: HashSet<&str> = ordered_ids.iter().map(String::as_str).collect();
        if current.len() != ordered_ids.len() || current_set != given_set {
            return Err(BoardRepoError::BadReorder);
        }
        for (i, col) in ordered_ids.iter().enumerate() {
            sqlx::query("UPDATE board_column SET ord = ? WHERE id = ? AND board_id = ?")
                .bind(i64::try_from(i).unwrap_or(i64::MAX))
                .bind(col)
                .bind(board_id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Place `issue_id` on `board_id` in `column_id` (or unmapped when `None`),
    /// idempotently — re-adding the same issue re-targets its column.
    ///
    /// Workspace-scoped via the board; a `column_id` that is `Some` must belong to
    /// the board, and `issue_id` must name an issue that lives in the SAME
    /// workspace (else [`BoardRepoError::NotFound`]). `board_card.issue_id` carries
    /// no FK (`PRAGMA foreign_keys` is off), so this service-layer check is the
    /// only guard against persisting a card for a nonexistent or sibling-tenant
    /// issue — the "card = issue, workspace-scoped" invariant (D8/§4.5).
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] / [`BoardRepoError::Db`].
    pub async fn card_add(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        issue_id: &str,
        column_id: Option<&str>,
        added_at: i64,
    ) -> Result<(), BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_board_in_ws(&mut tx, workspace, board_id).await?;
        Self::ensure_issue_in_ws(&mut tx, workspace, issue_id).await?;
        if let Some(col) = column_id {
            Self::ensure_column_of_board(&mut tx, board_id, col).await?;
        }
        sqlx::query(
            "INSERT INTO board_card (board_id, issue_id, column_id, added_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (board_id, issue_id) DO UPDATE SET column_id = excluded.column_id",
        )
        .bind(board_id)
        .bind(issue_id)
        .bind(column_id)
        .bind(added_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Move an existing card to `column_id` (or unmapped when `None`).
    ///
    /// Workspace-scoped; the card must already exist on the board (else
    /// [`BoardRepoError::NotFound`]), and a `Some` `column_id` must belong to the
    /// board.
    ///
    /// # Errors
    ///
    /// [`BoardRepoError::NotFound`] / [`BoardRepoError::Db`].
    pub async fn card_move(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        board_id: &str,
        issue_id: &str,
        column_id: Option<&str>,
    ) -> Result<(), BoardRepoError> {
        let mut tx = pool.begin().await?;
        Self::ensure_board_in_ws(&mut tx, workspace, board_id).await?;
        if let Some(col) = column_id {
            Self::ensure_column_of_board(&mut tx, board_id, col).await?;
        }
        let affected = sqlx::query(
            "UPDATE board_card SET column_id = ? WHERE board_id = ? AND issue_id = ?",
        )
        .bind(column_id)
        .bind(board_id)
        .bind(issue_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(BoardRepoError::NotFound);
        }
        tx.commit().await?;
        Ok(())
    }

    /// THE AUTO-MOVE HOOK. For every board in `workspace` that carries `issue_id`
    /// as a card, move the card to the column whose `fsm_state == new_state` when
    /// the board's master `auto_move` AND the target column's `auto_move` are both
    /// on. Returns each card actually moved (skips cards already in the target).
    ///
    /// Called by the daemon on every task FSM transition (the task's issue +
    /// workspace resolve `issue_id` + `workspace`). A card in a board whose master
    /// toggle is off, or with no matching auto-move column, is left where it is.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if a query fails.
    pub async fn auto_move_on_state(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        issue_id: &str,
        new_state: &str,
    ) -> Result<Vec<AutoMoved>, sqlx::Error> {
        // Resolve, in one query, every (board, target_column) pair where: the
        // board is in the workspace + auto-move is on, the board carries this
        // issue as a card, and a column of that board has auto_move on and
        // fsm_state = new_state and is NOT already where the card sits.
        let rows = sqlx::query(
            "SELECT c.board_id AS board_id, col.id AS column_id \
             FROM board_card c \
             JOIN board b ON b.id = c.board_id \
             JOIN board_column col ON col.board_id = c.board_id \
             WHERE b.workspace_id = ? \
               AND b.auto_move = 1 \
               AND c.issue_id = ? \
               AND col.auto_move = 1 \
               AND col.fsm_state = ? \
               AND (c.column_id IS NULL OR c.column_id <> col.id)",
        )
        .bind(workspace.as_str())
        .bind(issue_id)
        .bind(new_state)
        .fetch_all(pool)
        .await?;

        let mut moved = Vec::with_capacity(rows.len());
        for r in &rows {
            let board_id: String = r.try_get("board_id")?;
            let column_id: String = r.try_get("column_id")?;
            sqlx::query("UPDATE board_card SET column_id = ? WHERE board_id = ? AND issue_id = ?")
                .bind(&column_id)
                .bind(&board_id)
                .bind(issue_id)
                .execute(pool)
                .await?;
            moved.push(AutoMoved {
                board_id,
                issue_id: issue_id.to_string(),
                column_id,
            });
        }
        Ok(moved)
    }

    /// Read a board's columns, ordered by `ord` then `id`.
    async fn columns_of(
        pool: &SqlitePool,
        board_id: &str,
    ) -> Result<Vec<BoardColumnRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, ord, name, fsm_state, auto_move FROM board_column \
             WHERE board_id = ? ORDER BY ord, id",
        )
        .bind(board_id)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(BoardColumnRow {
                    id: r.try_get("id")?,
                    ord: r.try_get("ord")?,
                    name: r.try_get("name")?,
                    fsm_state: r.try_get("fsm_state")?,
                    auto_move: r.try_get::<i64, _>("auto_move")? != 0,
                })
            })
            .collect()
    }

    /// Read a board's card placements, ordered by `added_at` then `issue_id`.
    async fn cards_of(pool: &SqlitePool, board_id: &str) -> Result<Vec<BoardCard>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT issue_id, column_id, added_at FROM board_card \
             WHERE board_id = ? ORDER BY added_at, issue_id",
        )
        .bind(board_id)
        .fetch_all(pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(BoardCard {
                    issue_id: r.try_get("issue_id")?,
                    column_id: r.try_get("column_id")?,
                    added_at: r.try_get("added_at")?,
                })
            })
            .collect()
    }

    /// Renumber a board's columns to a contiguous `0..n` by their current order.
    async fn renumber_ords(
        tx: &mut sqlx::SqliteConnection,
        board_id: &str,
    ) -> Result<(), sqlx::Error> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM board_column WHERE board_id = ? ORDER BY ord, id")
                .bind(board_id)
                .fetch_all(&mut *tx)
                .await?;
        for (i, id) in ids.iter().enumerate() {
            sqlx::query("UPDATE board_column SET ord = ? WHERE id = ?")
                .bind(i64::try_from(i).unwrap_or(i64::MAX))
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        Ok(())
    }

    /// Confirm `board_id` belongs to `workspace` inside `tx`.
    async fn ensure_board_in_ws(
        tx: &mut sqlx::SqliteConnection,
        workspace: &WorkspaceId,
        board_id: &str,
    ) -> Result<(), BoardRepoError> {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM board WHERE id = ? AND workspace_id = ?")
                .bind(board_id)
                .bind(workspace.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(BoardRepoError::NotFound);
        }
        Ok(())
    }

    /// Confirm `column_id` belongs to `board_id`, which belongs to `workspace`.
    async fn ensure_column_in_ws(
        tx: &mut sqlx::SqliteConnection,
        workspace: &WorkspaceId,
        board_id: &str,
        column_id: &str,
    ) -> Result<(), BoardRepoError> {
        Self::ensure_board_in_ws(tx, workspace, board_id).await?;
        Self::ensure_column_of_board(tx, board_id, column_id).await
    }

    /// Confirm `column_id` belongs to `board_id`.
    async fn ensure_column_of_board(
        tx: &mut sqlx::SqliteConnection,
        board_id: &str,
        column_id: &str,
    ) -> Result<(), BoardRepoError> {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM board_column WHERE id = ? AND board_id = ?")
                .bind(column_id)
                .bind(board_id)
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(BoardRepoError::NotFound);
        }
        Ok(())
    }

    /// Confirm `issue_id` names an issue that lives in `workspace`. The
    /// `board_card.issue_id` column has no FK, so this is the sole guard that a
    /// card references a real, same-workspace issue (rejecting a nonexistent or
    /// sibling-tenant id with [`BoardRepoError::NotFound`]).
    async fn ensure_issue_in_ws(
        tx: &mut sqlx::SqliteConnection,
        workspace: &WorkspaceId,
        issue_id: &str,
    ) -> Result<(), BoardRepoError> {
        let exists: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM issue WHERE id = ? AND workspace_id = ?")
                .bind(issue_id)
                .bind(workspace.as_str())
                .fetch_optional(&mut *tx)
                .await?;
        if exists.is_none() {
            return Err(BoardRepoError::NotFound);
        }
        Ok(())
    }

    /// Whether another auto-move column on `board_id` already claims `fsm_state`
    /// (an `auto_move = 1` column with the same `fsm_state`, excluding
    /// `exclude_column_id`). This is the ambiguity guard for the AUTO-MOVE hook:
    /// [`Self::auto_move_on_state`] selects EVERY matching column, so two columns
    /// mapping the same state would move a card twice with unordered final
    /// placement. One auto-move target per `(board_id, fsm_state)` keeps the hook
    /// deterministic.
    async fn auto_move_conflict(
        tx: &mut sqlx::SqliteConnection,
        board_id: &str,
        fsm_state: &str,
        exclude_column_id: Option<&str>,
    ) -> Result<bool, BoardRepoError> {
        let exists: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM board_column \
             WHERE board_id = ? AND fsm_state = ? AND auto_move = 1 \
               AND (? IS NULL OR id <> ?) LIMIT 1",
        )
        .bind(board_id)
        .bind(fsm_state)
        .bind(exclude_column_id)
        .bind(exclude_column_id)
        .fetch_optional(&mut *tx)
        .await?;
        Ok(exists.is_some())
    }
}

/// Whether a `sqlx` error is a `SQLite` UNIQUE-constraint violation (the
/// `(workspace_id, name)` resolve-or-reject guard fired).
fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.code().as_deref() == Some("2067"))
}

/// Error surface for [`BoardRepo`].
#[derive(Debug, thiserror::Error)]
pub enum BoardRepoError {
    /// A board with that name already exists in the workspace (the
    /// `(workspace_id, name)` UNIQUE index fired). The create / rename is rejected.
    #[error("a board with that name already exists in this workspace")]
    DuplicateName,
    /// Another auto-move column on the board already maps this `fsm_state`. Two
    /// auto-move columns for one state make [`BoardRepo::auto_move_on_state`]
    /// ambiguous (it moves the card once per match, final placement unordered), so
    /// the second mapping is rejected. Only one auto-move target per
    /// `(board_id, fsm_state)` is allowed.
    #[error("another auto-move column already maps this task state on this board")]
    DuplicateAutoMove,
    /// The target board / column / card does not belong to the supplied workspace
    /// (covers an unknown id too). The mutation is rejected, nothing written.
    #[error("board, column, or card not found in this workspace")]
    NotFound,
    /// A reorder set that is not exactly the board's current columns. Rejected.
    #[error("reorder must list exactly the board's current columns")]
    BadReorder,
    /// An underlying `sqlx` failure (IO, decode, …).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id.to_string()).unwrap()
    }

    async fn seed_ws(pool: &SqlitePool, id: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(id)
            .bind(id)
            .bind(0_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Seed a minimal issue in `ws` so a card can reference it (card_add now
    /// requires the issue to exist in the same workspace).
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

    /// create + column_add + card_add build a board; list reads it back with
    /// columns ordered and the card placed.
    #[tokio::test]
    async fn create_columns_cards_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        seed_issue(pool, "ws-a", "issue-1").await;
        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c1", "Todo", None, false).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c2", "Done", Some("done"), true).await.unwrap();
        BoardRepo::card_add(pool, &ws("ws-a"), "b1", "issue-1", Some("c1"), 10).await.unwrap();

        let boards = BoardRepo::list(pool, &ws("ws-a")).await.unwrap();
        assert_eq!(boards.len(), 1);
        let b = &boards[0];
        assert_eq!(b.name, "Sprint");
        assert!(b.auto_move, "board auto-move defaults on");
        assert_eq!(b.columns.len(), 2);
        assert_eq!(b.columns[0].name, "Todo");
        assert_eq!(b.columns[0].ord, 0);
        assert_eq!(b.columns[1].name, "Done");
        assert_eq!(b.columns[1].fsm_state.as_deref(), Some("done"));
        assert!(b.columns[1].auto_move);
        assert_eq!(b.cards.len(), 1);
        assert_eq!(b.cards[0].issue_id, "issue-1");
        assert_eq!(b.cards[0].column_id.as_deref(), Some("c1"));
    }

    /// A duplicate board name in the same workspace is rejected; the same name in
    /// a DIFFERENT workspace is allowed, and list never leaks a sibling tenant.
    #[tokio::test]
    async fn create_rejects_duplicate_and_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;

        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();
        let dup = BoardRepo::create(pool, &ws("ws-a"), "b2", "Sprint", 2).await.unwrap_err();
        assert!(matches!(dup, BoardRepoError::DuplicateName), "got {dup:?}");
        // Same name, other tenant: allowed.
        BoardRepo::create(pool, &ws("ws-b"), "b3", "Sprint", 3).await.unwrap();

        assert_eq!(BoardRepo::list(pool, &ws("ws-a")).await.unwrap().len(), 1);
        let b = BoardRepo::list(pool, &ws("ws-b")).await.unwrap();
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].id, "b3");
    }

    /// Reorder rewrites `ord` without moving any card; a bad set is rejected.
    #[tokio::test]
    async fn column_reorder_rewrites_ord_without_touching_cards() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c1", "A", None, false).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c2", "B", None, false).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c3", "C", None, false).await.unwrap();
        seed_issue(pool, "ws-a", "issue-1").await;
        BoardRepo::card_add(pool, &ws("ws-a"), "b1", "issue-1", Some("c2"), 10).await.unwrap();

        BoardRepo::column_reorder(pool, &ws("ws-a"), "b1", &["c3".into(), "c1".into(), "c2".into()])
            .await
            .unwrap();
        let b = &BoardRepo::list(pool, &ws("ws-a")).await.unwrap()[0];
        assert_eq!(
            b.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            vec!["C", "A", "B"],
            "columns follow the new order"
        );
        // The card still sits in c2 — reorder never moved it.
        assert_eq!(b.cards[0].column_id.as_deref(), Some("c2"));

        // A reorder set that is not exactly the current columns is rejected.
        let bad = BoardRepo::column_reorder(pool, &ws("ws-a"), "b1", &["c1".into(), "c2".into()])
            .await
            .unwrap_err();
        assert!(matches!(bad, BoardRepoError::BadReorder), "got {bad:?}");
    }

    /// Deleting a column parks its cards at NULL (unmapped) and renumbers the rest
    /// — no data loss.
    #[tokio::test]
    async fn column_delete_parks_cards_unmapped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c1", "A", None, false).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c2", "B", None, false).await.unwrap();
        seed_issue(pool, "ws-a", "issue-1").await;
        BoardRepo::card_add(pool, &ws("ws-a"), "b1", "issue-1", Some("c1"), 10).await.unwrap();

        BoardRepo::column_delete(pool, &ws("ws-a"), "b1", "c1").await.unwrap();
        let b = &BoardRepo::list(pool, &ws("ws-a")).await.unwrap()[0];
        assert_eq!(b.columns.len(), 1, "one column remains");
        assert_eq!(b.columns[0].ord, 0, "renumbered contiguous");
        // The card survived, now unmapped.
        assert_eq!(b.cards.len(), 1);
        assert_eq!(b.cards[0].column_id, None, "card parked unmapped, not lost");
    }

    /// The auto-move hook moves a card to the matching auto-move column, honours
    /// the board master toggle, and is a no-op when no column matches.
    #[tokio::test]
    async fn auto_move_on_state_honours_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c1", "Todo", None, false).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c2", "Done", Some("done"), true).await.unwrap();
        seed_issue(pool, "ws-a", "issue-1").await;
        BoardRepo::card_add(pool, &ws("ws-a"), "b1", "issue-1", Some("c1"), 10).await.unwrap();

        // A non-matching state moves nothing.
        let none = BoardRepo::auto_move_on_state(pool, &ws("ws-a"), "issue-1", "running").await.unwrap();
        assert!(none.is_empty(), "no column maps `running`");

        // `done` moves the card to c2 (the auto-move target).
        let moved = BoardRepo::auto_move_on_state(pool, &ws("ws-a"), "issue-1", "done").await.unwrap();
        assert_eq!(moved.len(), 1);
        assert_eq!(moved[0].column_id, "c2");
        let b = &BoardRepo::list(pool, &ws("ws-a")).await.unwrap()[0];
        assert_eq!(b.cards[0].column_id.as_deref(), Some("c2"), "card auto-moved to Done");

        // Re-firing `done` is a no-op (already there).
        let again = BoardRepo::auto_move_on_state(pool, &ws("ws-a"), "issue-1", "done").await.unwrap();
        assert!(again.is_empty(), "already in the target column");

        // With the board master toggle OFF, auto-move does nothing.
        seed_issue(pool, "ws-a", "issue-2").await;
        BoardRepo::card_add(pool, &ws("ws-a"), "b1", "issue-2", Some("c1"), 11).await.unwrap();
        BoardRepo::update(pool, &ws("ws-a"), "b1", None, Some(false)).await.unwrap();
        let off = BoardRepo::auto_move_on_state(pool, &ws("ws-a"), "issue-2", "done").await.unwrap();
        assert!(off.is_empty(), "master toggle off suppresses auto-move");
    }

    /// Cross-tenant mutations are rejected (NotFound), never touching a row.
    #[tokio::test]
    async fn mutations_are_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();

        let err = BoardRepo::column_add(pool, &ws("ws-b"), "b1", "c1", "X", None, false).await.unwrap_err();
        assert!(matches!(err, BoardRepoError::NotFound), "got {err:?}");
        let err = BoardRepo::delete(pool, &ws("ws-b"), "b1").await.unwrap_err();
        assert!(matches!(err, BoardRepoError::NotFound), "got {err:?}");
        // The board is untouched.
        assert_eq!(BoardRepo::list(pool, &ws("ws-a")).await.unwrap().len(), 1);
    }

    /// A card for an issue that does not exist, or that lives in a sibling
    /// workspace, is rejected — a card must reference a real, same-workspace issue.
    #[tokio::test]
    async fn card_add_rejects_unknown_and_foreign_issue() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        seed_ws(pool, "ws-b").await;
        // issue-b belongs to ws-b, not ws-a.
        seed_issue(pool, "ws-b", "issue-b").await;
        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c1", "Todo", None, false).await.unwrap();

        // No such issue anywhere.
        let missing = BoardRepo::card_add(pool, &ws("ws-a"), "b1", "nope", Some("c1"), 10)
            .await
            .unwrap_err();
        assert!(matches!(missing, BoardRepoError::NotFound), "got {missing:?}");

        // The issue exists, but in a sibling workspace — still rejected.
        let foreign = BoardRepo::card_add(pool, &ws("ws-a"), "b1", "issue-b", Some("c1"), 10)
            .await
            .unwrap_err();
        assert!(matches!(foreign, BoardRepoError::NotFound), "got {foreign:?}");

        // Nothing was persisted.
        let b = &BoardRepo::list(pool, &ws("ws-a")).await.unwrap()[0];
        assert!(b.cards.is_empty(), "no card persisted for a bad issue");
    }

    /// One auto-move target per (board, fsm_state): a second auto-move column for
    /// the same state is rejected on add, and on an update that would create the
    /// clash (flipping auto-move on, or re-mapping an existing auto-move column
    /// onto a state another column already owns).
    #[tokio::test]
    async fn auto_move_target_is_unique_per_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;
        BoardRepo::create(pool, &ws("ws-a"), "b1", "Sprint", 1).await.unwrap();
        // First auto-move `done` column: fine.
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c1", "Done", Some("done"), true).await.unwrap();

        // Second auto-move `done` column on the same board: rejected.
        let dup = BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c2", "Done2", Some("done"), true)
            .await
            .unwrap_err();
        assert!(matches!(dup, BoardRepoError::DuplicateAutoMove), "got {dup:?}");

        // A NON-auto-move `done` column is allowed (no ambiguity).
        BoardRepo::column_add(pool, &ws("ws-a"), "b1", "c3", "Done-manual", Some("done"), false).await.unwrap();

        // Flipping the manual `done` column's auto-move ON now clashes with c1: rejected.
        let flip = BoardRepo::column_update(pool, &ws("ws-a"), "b1", "c3", None, None, Some(true))
            .await
            .unwrap_err();
        assert!(matches!(flip, BoardRepoError::DuplicateAutoMove), "got {flip:?}");

        // Re-mapping c1 onto its OWN state (a no-op clash with itself) is allowed —
        // the conflict check excludes the column being updated.
        BoardRepo::column_update(pool, &ws("ws-a"), "b1", "c1", None, Some(Some("done")), Some(true))
            .await
            .unwrap();
    }
}
