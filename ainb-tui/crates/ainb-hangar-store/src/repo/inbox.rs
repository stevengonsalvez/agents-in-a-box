//! Typed repository wrapper over the `inbox_entry` table (e38.14).
//!
//! [`InboxRepo`] is a thin, stateless sqlx layer over the aggregated
//! notification inbox (migration 0021). Each row is one workspace-scoped
//! notification derived from a live issue / comment / task [`HangarEvent`]: the
//! daemon's inbox aggregator drains the event broker and calls [`InboxRepo::insert`]
//! for every matching event, so events that were once purely live now land
//! durably with an unread count.
//!
//! `read_at` (epoch milliseconds, nullable) is the whole unread model: `NULL` =
//! unread, a stamped value = read. [`InboxRepo::unread_count`] counts the `NULL`
//! rows; [`InboxRepo::mark_all_read`] stamps every currently-unread row so the
//! count drops to zero (the mark-read sweep). Every method is **workspace-scoped**
//! by the `workspace_id` column, mirroring the tenant isolation the issue/comment
//! repos enforce: a foreign tenant's inbox is never read or mutated.
//!
//! [`HangarEvent`]: ../../../ainb_hangar_proto/events/enum.HangarEvent.html

use sqlx::{Row, SqlitePool};

/// The entity family an [`InboxEntry`] is about — the `kind` column, CHECK-
/// constrained to these three by migration 0021.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxKind {
    /// An issue lifecycle event (created / updated / deleted).
    Issue,
    /// A comment was added to an issue.
    Comment,
    /// A task lifecycle event (queued / started / finished).
    Task,
}

impl InboxKind {
    /// The wire / column token for this kind (matches the migration's CHECK set).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Issue => "issue",
            Self::Comment => "comment",
            Self::Task => "task",
        }
    }

    /// Parse a stored `kind` token back into the enum.
    ///
    /// Returns `None` for an unrecognised token (only possible via raw SQL tamper —
    /// the column carries a CHECK constraint).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "issue" => Some(Self::Issue),
            "comment" => Some(Self::Comment),
            "task" => Some(Self::Task),
            _ => None,
        }
    }
}

/// Parameters for inserting one aggregated inbox entry.
///
/// `id` is minted by the caller (the daemon's [`IdGen`](ainb_hangar_core::idgen::IdGen),
/// like [`super::comment::NewComment`]) so the repo stays free of an
/// id-generation dependency. A fresh entry is always **unread** (`read_at`
/// defaults to `NULL`); the mark-read sweep is the only thing that stamps it.
#[derive(Debug, Clone)]
pub struct NewInboxEntry {
    /// Primary key (ULID string).
    pub id: String,
    /// The owning workspace's resolved row id (never the slug).
    pub workspace_id: String,
    /// The entity family the entry is about.
    pub kind: InboxKind,
    /// The wire event discriminant that produced this entry (e.g.
    /// `issue_created`, `comment_added`, `task_queued`).
    pub event: String,
    /// The id of the issue / comment / task the entry is about (deep-link target).
    pub subject_id: String,
    /// A short pre-rendered human line for the list row.
    pub summary: String,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// A fully-materialised `inbox_entry` row read back from the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxEntry {
    /// Primary key.
    pub id: String,
    /// The owning workspace.
    pub workspace_id: String,
    /// The entity family the entry is about.
    pub kind: InboxKind,
    /// The wire event discriminant.
    pub event: String,
    /// The subject id (issue / comment / task).
    pub subject_id: String,
    /// The pre-rendered summary line.
    pub summary: String,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
    /// When the entry was marked read (epoch milliseconds), or `None` when UNREAD.
    pub read_at: Option<i64>,
}

/// Stateless typed wrapper over the `inbox_entry` table.
pub struct InboxRepo;

impl InboxRepo {
    /// Insert one aggregated inbox entry (always unread — `read_at` is `NULL`).
    ///
    /// Unlike the comment repo, the inbox is NOT scoped through a join to another
    /// table: the daemon's aggregator already resolved the workspace row id before
    /// calling here, so the entry rides directly on `workspace_id`. The FK to
    /// `workspace(id)` keeps a dangling-workspace insert from landing.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — e.g. a `kind` CHECK
    /// violation (impossible through [`InboxKind`], only via raw SQL) or a
    /// workspace FK violation.
    pub async fn insert(pool: &SqlitePool, entry: &NewInboxEntry) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO inbox_entry \
             (id, workspace_id, kind, event, subject_id, summary, created_at, read_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, NULL)",
        )
        .bind(&entry.id)
        .bind(&entry.workspace_id)
        .bind(entry.kind.as_str())
        .bind(&entry.event)
        .bind(&entry.subject_id)
        .bind(&entry.summary)
        .bind(entry.created_at)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// List a workspace's inbox entries, newest first, capped at `limit`.
    ///
    /// Workspace-scoped by the `workspace_id` column: a foreign tenant's entries
    /// are never returned, and an unknown workspace yields an empty list. Ordered
    /// `created_at DESC, id DESC` so the newest notification is the first row (the
    /// `id` tiebreak keeps two same-millisecond entries deterministic).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a stored `kind` token is
    /// malformed (impossible given the CHECK constraint).
    pub async fn list(
        pool: &SqlitePool,
        workspace_id: &str,
        limit: i64,
    ) -> Result<Vec<InboxEntry>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, workspace_id, kind, event, subject_id, summary, created_at, read_at \
             FROM inbox_entry \
             WHERE workspace_id = ? \
             ORDER BY created_at DESC, id DESC \
             LIMIT ?",
        )
        .bind(workspace_id)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        rows.iter().map(entry_from_row).collect()
    }

    /// Count a workspace's UNREAD entries (`read_at IS NULL`).
    ///
    /// Workspace-scoped: a foreign tenant's unread entries never count. This is the
    /// figure the inbox badge renders and the mark-read sweep drives to zero.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the count query fails.
    pub async fn unread_count(pool: &SqlitePool, workspace_id: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM inbox_entry WHERE workspace_id = ? AND read_at IS NULL",
        )
        .bind(workspace_id)
        .fetch_one(pool)
        .await
    }

    /// Mark every currently-unread entry in a workspace as read, stamping
    /// `read_at = now_ms`. Returns the number of rows flipped (which equals the
    /// unread count before the sweep).
    ///
    /// Idempotent: a second sweep touches no row (every entry already has a
    /// `read_at`), so the count stays at zero. Only `NULL`-`read_at` rows are
    /// stamped, so an already-read entry keeps its original read timestamp.
    /// Workspace-scoped: a foreign tenant's entries are never touched.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the update fails.
    pub async fn mark_all_read(
        pool: &SqlitePool,
        workspace_id: &str,
        now_ms: i64,
    ) -> Result<u64, sqlx::Error> {
        let res = sqlx::query(
            "UPDATE inbox_entry SET read_at = ? \
             WHERE workspace_id = ? AND read_at IS NULL",
        )
        .bind(now_ms)
        .bind(workspace_id)
        .execute(pool)
        .await?;
        Ok(res.rows_affected())
    }
}

/// Map one raw `inbox_entry` row into an [`InboxEntry`].
fn entry_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<InboxEntry, sqlx::Error> {
    let kind_token: String = row.try_get("kind")?;
    let kind = InboxKind::parse(&kind_token).ok_or_else(|| sqlx::Error::ColumnDecode {
        index: "kind".to_string(),
        source: format!("unknown inbox kind {kind_token:?}").into(),
    })?;
    Ok(InboxEntry {
        id: row.try_get("id")?,
        workspace_id: row.try_get("workspace_id")?,
        kind,
        event: row.try_get("event")?,
        subject_id: row.try_get("subject_id")?,
        summary: row.try_get("summary")?,
        created_at: row.try_get("created_at")?,
        read_at: row.try_get("read_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    /// Seed one workspace so the FK-scoped inserts resolve.
    async fn seed_workspace(pool: &SqlitePool, ws: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(ws)
            .bind(ws)
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    fn entry(id: &str, ws: &str, kind: InboxKind, subject: &str, ts: i64) -> NewInboxEntry {
        NewInboxEntry {
            id: id.into(),
            workspace_id: ws.into(),
            kind,
            event: "issue_created".into(),
            subject_id: subject.into(),
            summary: format!("entry {id}"),
            created_at: ts,
        }
    }

    #[tokio::test]
    async fn insert_then_list_newest_first_all_unread() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;

        for (id, ts) in [("e1", 1000_i64), ("e2", 2000), ("e3", 3000)] {
            InboxRepo::insert(
                store.pool(),
                &entry(id, "ws-a", InboxKind::Issue, "issue-1", ts),
            )
            .await
            .unwrap();
        }

        let list = InboxRepo::list(store.pool(), "ws-a", 100).await.unwrap();
        let ids: Vec<_> = list.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["e3", "e2", "e1"], "newest first");
        assert!(
            list.iter().all(|e| e.read_at.is_none()),
            "fresh entries are unread"
        );
        assert_eq!(
            InboxRepo::unread_count(store.pool(), "ws-a").await.unwrap(),
            3
        );
    }

    #[tokio::test]
    async fn mark_all_read_drops_unread_to_zero_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;
        for (id, ts) in [("e1", 1000_i64), ("e2", 2000)] {
            InboxRepo::insert(
                store.pool(),
                &entry(id, "ws-a", InboxKind::Comment, "issue-1", ts),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            InboxRepo::unread_count(store.pool(), "ws-a").await.unwrap(),
            2
        );

        let flipped = InboxRepo::mark_all_read(store.pool(), "ws-a", 5000).await.unwrap();
        assert_eq!(flipped, 2, "both unread rows flip");
        assert_eq!(
            InboxRepo::unread_count(store.pool(), "ws-a").await.unwrap(),
            0,
            "unread count drops to zero after the sweep"
        );
        // The read entries kept their stamp.
        let list = InboxRepo::list(store.pool(), "ws-a", 100).await.unwrap();
        assert!(list.iter().all(|e| e.read_at == Some(5000)));

        // A second sweep touches no row (idempotent).
        let again = InboxRepo::mark_all_read(store.pool(), "ws-a", 9999).await.unwrap();
        assert_eq!(again, 0, "a re-sweep flips nothing");
        let list = InboxRepo::list(store.pool(), "ws-a", 100).await.unwrap();
        assert!(
            list.iter().all(|e| e.read_at == Some(5000)),
            "already-read entries keep their original read timestamp"
        );
    }

    #[tokio::test]
    async fn list_and_unread_count_are_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;
        seed_workspace(store.pool(), "ws-b").await;
        InboxRepo::insert(
            store.pool(),
            &entry("a1", "ws-a", InboxKind::Issue, "i1", 1000),
        )
        .await
        .unwrap();
        InboxRepo::insert(
            store.pool(),
            &entry("b1", "ws-b", InboxKind::Task, "t1", 1000),
        )
        .await
        .unwrap();

        // ws-a sees only its own entry, and its unread count is its own.
        let list_a = InboxRepo::list(store.pool(), "ws-a", 100).await.unwrap();
        assert_eq!(list_a.len(), 1);
        assert_eq!(list_a[0].id, "a1");
        assert_eq!(
            InboxRepo::unread_count(store.pool(), "ws-a").await.unwrap(),
            1
        );

        // Marking ws-a read does not touch ws-b's unread entry.
        InboxRepo::mark_all_read(store.pool(), "ws-a", 5000).await.unwrap();
        assert_eq!(
            InboxRepo::unread_count(store.pool(), "ws-a").await.unwrap(),
            0
        );
        assert_eq!(
            InboxRepo::unread_count(store.pool(), "ws-b").await.unwrap(),
            1,
            "a sibling tenant's inbox is untouched by another's mark-read"
        );

        // An unknown workspace yields an empty list + zero unread.
        assert!(InboxRepo::list(store.pool(), "ws-nope", 100).await.unwrap().is_empty());
        assert_eq!(
            InboxRepo::unread_count(store.pool(), "ws-nope").await.unwrap(),
            0
        );
    }
}
