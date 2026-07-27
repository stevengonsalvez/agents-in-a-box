//! Typed repository wrapper over the `comment` table (e38.5).
//!
//! [`CommentRepo`] is a thin, stateless sqlx wrapper mirroring [`super::issue`].
//! A comment's author is the same **polymorphic actor** the issue's
//! assignee/creator use: an [`ActorRef`] in the Rust API split into the two TEXT
//! columns the schema stores (`author_type` / `author_id`) at the SQL boundary.
//!
//! # Workspace scoping without a `workspace_id` column
//!
//! The `comment` table (migration 0003) has no `workspace_id` of its own — a
//! comment belongs to an issue, and the issue carries the workspace. So both the
//! insert and the list are **scoped through a join to `issue`**: the insert only
//! lands a row when the target `(issue_id, workspace_id)` pair resolves to a real
//! issue in that tenant, and the list returns a foreign tenant's comments never.
//! This is the same FK-less-by-design tenant isolation the issue repo enforces
//! (per the reference architecture review §7); the daemon resolves the workspace
//! row id before calling here.

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use sqlx::{Row, SqlitePool};

/// Parameters for inserting a new `comment` row.
///
/// `id` is minted by the caller (the daemon's [`IdGen`](ainb_hangar_core::idgen::IdGen),
/// like [`super::issue::NewIssue`]) so the repo stays free of an id-generation
/// dependency. `author` is the polymorphic `(type, id)` actor that wrote it.
#[derive(Debug, Clone)]
pub struct NewComment {
    /// Primary key (ULID string).
    pub id: String,
    /// The issue this comment belongs to (`issue.id`).
    pub issue_id: String,
    /// The actor that authored the comment (mandatory).
    pub author: ActorRef,
    /// The comment body.
    pub body: String,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// A fully-materialised `comment` row read back from the database.
///
/// The polymorphic author columns are re-assembled into an [`ActorRef`] on read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Primary key.
    pub id: String,
    /// The issue this comment belongs to.
    pub issue_id: String,
    /// The authoring actor.
    pub author: ActorRef,
    /// The comment body.
    pub body: String,
    /// Creation timestamp (epoch milliseconds).
    pub created_at: i64,
}

/// Stateless typed wrapper over the `comment` table.
pub struct CommentRepo;

impl CommentRepo {
    /// Insert one comment, scoped to a workspace, splitting the [`ActorRef`] into
    /// its two TEXT columns at the SQL boundary.
    ///
    /// The write is **workspace-scoped through a join to `issue`**: the row is
    /// only inserted when `(issue_id, workspace_id)` resolves to a real issue in
    /// that tenant. A comment aimed at an unknown issue id, or at an issue owned
    /// by another tenant, lands nothing — returning `false` rather than a foreign
    /// write. Returns `true` when exactly one row was inserted.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — for example an
    /// `author_type` `CHECK` violation (impossible through [`ActorRef`], only via
    /// raw SQL).
    pub async fn insert(
        pool: &SqlitePool,
        workspace_id: &str,
        comment: &NewComment,
    ) -> Result<bool, sqlx::Error> {
        Self::insert_with(pool, workspace_id, comment).await
    }

    /// [`insert`](Self::insert) over an arbitrary executor — a pool OR a live
    /// transaction.
    ///
    /// The child-done cascade (parity #3-rest) claims its barrier row and writes
    /// this comment in ONE transaction, so the claim can never be committed
    /// without the comment it promises. Same `WHERE EXISTS` tenant guard.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails.
    pub async fn insert_with<'e, E>(
        exec: E,
        workspace_id: &str,
        comment: &NewComment,
    ) -> Result<bool, sqlx::Error>
    where
        E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
    {
        let author_type = comment.author.kind().as_str();
        let author_id = comment.author.id();
        // The `SELECT ... WHERE EXISTS` form makes the insert a no-op when the
        // target issue does not belong to `workspace_id`: a foreign-tenant (or
        // unknown) issue id matches no row in the sub-select, so zero rows insert.
        let res = sqlx::query(
            "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
             SELECT ?, ?, ?, ?, ?, ? \
             WHERE EXISTS (SELECT 1 FROM issue WHERE id = ? AND workspace_id = ?)",
        )
        .bind(&comment.id)
        .bind(&comment.issue_id)
        .bind(author_type)
        .bind(author_id)
        .bind(&comment.body)
        .bind(comment.created_at)
        .bind(&comment.issue_id)
        .bind(workspace_id)
        .execute(exec)
        .await?;
        Ok(res.rows_affected() == 1)
    }

    /// List the comments of one issue, scoped to a workspace, oldest first.
    ///
    /// Workspace-scoped through the same join to `issue`: a comment whose issue
    /// belongs to another tenant is never returned, and an issue id from a
    /// foreign workspace yields an empty list. Ordered by `created_at` (then `id`
    /// to break ties deterministically) so the transcript interleaves comments in
    /// arrival order.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails or a stored author pair is
    /// malformed (impossible given the `CHECK` constraint plus the non-null
    /// `author_*` columns).
    pub async fn list_by_issue(
        pool: &SqlitePool,
        workspace_id: &str,
        issue_id: &str,
    ) -> Result<Vec<Comment>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT c.id, c.issue_id, c.author_type, c.author_id, c.body, c.created_at \
             FROM comment c \
             JOIN issue i ON i.id = c.issue_id \
             WHERE c.issue_id = ? AND i.workspace_id = ? \
             ORDER BY c.created_at, c.id",
        )
        .bind(issue_id)
        .bind(workspace_id)
        .fetch_all(pool)
        .await?;
        rows.iter().map(comment_from_row).collect()
    }
}

/// Re-assemble an [`ActorRef`] from a non-null `(type, id)` author pair.
///
/// A malformed combination (an unrecognised type token or an empty id) is a
/// row-level corruption surfaced as a decode error — the `author_*` columns are
/// `NOT NULL` with a `CHECK` constraint, so this only fires on external tamper.
fn author_from_columns(kind: &str, id: String) -> Result<ActorRef, sqlx::Error> {
    let kind = kind.parse::<ActorKind>().map_err(|e| decode_err(&e.to_string()))?;
    ActorRef::new(kind, id).map_err(|e| decode_err(&e.to_string()))
}

/// Build a `sqlx` decode error for a malformed author column pair.
fn decode_err(detail: &str) -> sqlx::Error {
    sqlx::Error::ColumnDecode {
        index: "author".to_string(),
        source: format!("malformed comment author: {detail}").into(),
    }
}

/// Map one raw `comment` row into a [`Comment`], re-assembling the polymorphic
/// author columns.
fn comment_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Comment, sqlx::Error> {
    let author_type: String = row.try_get("author_type")?;
    let author = author_from_columns(&author_type, row.try_get("author_id")?)?;
    Ok(Comment {
        id: row.try_get("id")?,
        issue_id: row.try_get("issue_id")?,
        author,
        body: row.try_get("body")?,
        created_at: row.try_get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use ainb_hangar_core::actor::{ActorKind, ActorRef};

    /// Seed a workspace + one issue, returning the `(workspace_id, issue_id)`.
    async fn seed_issue(pool: &SqlitePool, ws: &str, issue: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(ws)
            .bind(ws)
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, title, state, creator_type, creator_id, created_at) \
             VALUES (?, ?, 'T', 'open', 'member', 'u1', 1000)",
        )
        .bind(issue)
        .bind(ws)
        .execute(pool)
        .await
        .unwrap();
    }

    fn author() -> ActorRef {
        ActorRef::new(ActorKind::Member, "u1").unwrap()
    }

    #[tokio::test]
    async fn insert_then_list_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_issue(store.pool(), "ws-a", "issue-a").await;

        let inserted = CommentRepo::insert(
            store.pool(),
            "ws-a",
            &NewComment {
                id: "c1".into(),
                issue_id: "issue-a".into(),
                author: author(),
                body: "first comment".into(),
                created_at: 1234,
            },
        )
        .await
        .unwrap();
        assert!(inserted, "insert into the owning workspace must land a row");

        let comments = CommentRepo::list_by_issue(store.pool(), "ws-a", "issue-a").await.unwrap();
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].id, "c1");
        assert_eq!(comments[0].body, "first comment");
        assert_eq!(comments[0].author, author());
        assert_eq!(comments[0].created_at, 1234);
    }

    #[tokio::test]
    async fn list_is_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_issue(store.pool(), "ws-a", "issue-a").await;
        for (id, body, ts) in [("c2", "second", 2000_i64), ("c1", "first", 1000_i64)] {
            CommentRepo::insert(
                store.pool(),
                "ws-a",
                &NewComment {
                    id: id.into(),
                    issue_id: "issue-a".into(),
                    author: author(),
                    body: body.into(),
                    created_at: ts,
                },
            )
            .await
            .unwrap();
        }
        let comments = CommentRepo::list_by_issue(store.pool(), "ws-a", "issue-a").await.unwrap();
        let bodies: Vec<_> = comments.iter().map(|c| c.body.as_str()).collect();
        assert_eq!(
            bodies,
            ["first", "second"],
            "comments ordered by created_at"
        );
    }

    #[tokio::test]
    async fn insert_into_foreign_workspace_lands_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_issue(store.pool(), "ws-a", "issue-a").await;
        // A second tenant that does NOT own issue-a.
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-b")
            .bind("ws-b")
            .bind("ws-b")
            .bind(1_000_i64)
            .execute(store.pool())
            .await
            .unwrap();

        let inserted = CommentRepo::insert(
            store.pool(),
            "ws-b",
            &NewComment {
                id: "c1".into(),
                issue_id: "issue-a".into(),
                author: author(),
                body: "cross-tenant".into(),
                created_at: 1234,
            },
        )
        .await
        .unwrap();
        assert!(!inserted, "a cross-tenant insert must land no row");
        // issue-a's comment list (in its real workspace) is still empty.
        let comments = CommentRepo::list_by_issue(store.pool(), "ws-a", "issue-a").await.unwrap();
        assert!(
            comments.is_empty(),
            "no comment leaked into the real tenant"
        );
    }

    #[tokio::test]
    async fn list_is_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_issue(store.pool(), "ws-a", "issue-a").await;
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind("ws-b")
            .bind("ws-b")
            .bind("ws-b")
            .bind(1_000_i64)
            .execute(store.pool())
            .await
            .unwrap();
        CommentRepo::insert(
            store.pool(),
            "ws-a",
            &NewComment {
                id: "c1".into(),
                issue_id: "issue-a".into(),
                author: author(),
                body: "owner".into(),
                created_at: 1234,
            },
        )
        .await
        .unwrap();
        // Listing issue-a through the foreign workspace returns nothing.
        let comments = CommentRepo::list_by_issue(store.pool(), "ws-b", "issue-a").await.unwrap();
        assert!(comments.is_empty(), "foreign workspace sees no comments");
    }
}
