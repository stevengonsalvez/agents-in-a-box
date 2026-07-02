//! Typed repository wrapper over the `event_log` table — the durable event
//! outbox (T1 / architecture §4.1–§4.2, migration 0024).
//!
//! [`EventOutboxRepo`] is a thin, stateless sqlx layer over the append-only log
//! of every `HangarEvent` the daemon emits. Where [`super::inbox`] is a
//! read/unread DIGEST of the three notification families, this is the RAW,
//! replayable log keyed by a monotonic [`seq`](StoredEvent::seq): the daemon's
//! outbox drain calls [`EventOutboxRepo::append`] for every emitted event, and a
//! reconnecting or late-joining subscriber calls [`EventOutboxRepo::replay`] to
//! catch up on everything after its last cursor.
//!
//! Every method is **workspace-scoped** by the `workspace_id` column, mirroring
//! the tenant isolation the issue/comment/inbox repos enforce: a foreign
//! tenant's events are never replayed and never counted toward another
//! workspace's head cursor.

use sqlx::{Row, SqlitePool};

/// Parameters for appending one event to the durable outbox.
///
/// `seq` is intentionally absent: it is assigned by the table's AUTOINCREMENT
/// primary key and returned from [`EventOutboxRepo::append`], so the caller
/// never invents a sequence (the database is the single monotonic authority).
#[derive(Debug, Clone)]
pub struct NewEvent {
    /// The owning workspace's resolved row id (never the slug).
    pub workspace_id: String,
    /// The wire event discriminant (the serde tag of the `HangarEvent`, e.g.
    /// `task_finished`).
    pub event_type: String,
    /// The id of the subject the event is about (task / issue / comment / …), or
    /// `None` for the rare variant with no single subject.
    pub entity: Option<String>,
    /// The full serialised `HangarEvent` JSON — re-framed verbatim on replay.
    pub payload: String,
    /// The log timestamp (epoch milliseconds).
    pub ts: i64,
}

/// A fully-materialised `event_log` row read back for replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// The monotonic sequence — the client's resume cursor.
    pub seq: i64,
    /// The log timestamp (epoch milliseconds).
    pub ts: i64,
    /// The owning workspace.
    pub workspace_id: String,
    /// The wire event discriminant.
    pub event_type: String,
    /// The subject id, or `None`.
    pub entity: Option<String>,
    /// The serialised `HangarEvent` JSON (the notification `params`).
    pub payload: String,
}

/// Stateless typed wrapper over the `event_log` table.
pub struct EventOutboxRepo;

impl EventOutboxRepo {
    /// Append one event to the durable outbox, returning its assigned monotonic
    /// [`seq`](StoredEvent::seq).
    ///
    /// The seq comes from the AUTOINCREMENT primary key, so two appends always
    /// return strictly increasing values — even across workspaces (the log is a
    /// single global sequence, workspace-partitioned only for replay scoping).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the insert fails — e.g. a workspace FK
    /// violation (a dangling `workspace_id`).
    pub async fn append(pool: &SqlitePool, event: &NewEvent) -> Result<i64, sqlx::Error> {
        let res = sqlx::query(
            "INSERT INTO event_log (ts, workspace_id, event_type, entity, payload) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(event.ts)
        .bind(&event.workspace_id)
        .bind(&event.event_type)
        .bind(&event.entity)
        .bind(&event.payload)
        .execute(pool)
        .await?;
        Ok(res.last_insert_rowid())
    }

    /// Replay a workspace's events with `seq > after_seq`, in seq order, capped
    /// at `limit`.
    ///
    /// This is the catch-up read: a subscriber resuming from cursor `after_seq`
    /// receives exactly the events it missed, oldest first, so it can apply them
    /// in order and advance its cursor to the last `seq`. Pass `after_seq = 0`
    /// for the full backlog (a late join that has never subscribed).
    ///
    /// Workspace-scoped: a foreign tenant's events are never returned, and an
    /// unknown workspace yields an empty vec.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn replay(
        pool: &SqlitePool,
        workspace_id: &str,
        after_seq: i64,
        limit: i64,
    ) -> Result<Vec<StoredEvent>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT seq, ts, workspace_id, event_type, entity, payload \
             FROM event_log \
             WHERE workspace_id = ? AND seq > ? \
             ORDER BY seq ASC \
             LIMIT ?",
        )
        .bind(workspace_id)
        .bind(after_seq)
        .bind(limit)
        .fetch_all(pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    /// The workspace's current head cursor — the highest `seq` logged for it, or
    /// `0` when the workspace has no events yet.
    ///
    /// A fresh subscriber records this as its starting cursor (nothing before it
    /// is replayed); a reconnecting one compares it against its last-seen cursor.
    /// Workspace-scoped: a sibling tenant's events never raise this figure.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn head_seq(pool: &SqlitePool, workspace_id: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM event_log WHERE workspace_id = ?")
            .bind(workspace_id)
            .fetch_one(pool)
            .await
    }
}

/// Map one raw `event_log` row into a [`StoredEvent`].
fn event_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<StoredEvent, sqlx::Error> {
    Ok(StoredEvent {
        seq: row.try_get("seq")?,
        ts: row.try_get("ts")?,
        workspace_id: row.try_get("workspace_id")?,
        event_type: row.try_get("event_type")?,
        entity: row.try_get("entity")?,
        payload: row.try_get("payload")?,
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

    fn event(ws: &str, ty: &str, entity: &str, ts: i64) -> NewEvent {
        NewEvent {
            workspace_id: ws.into(),
            event_type: ty.into(),
            entity: Some(entity.into()),
            payload: format!("{{\"event\":\"{ty}\",\"id\":\"{entity}\"}}"),
            ts,
        }
    }

    #[tokio::test]
    async fn append_returns_strictly_monotonic_seq() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;

        let s1 = EventOutboxRepo::append(store.pool(), &event("ws-a", "task_started", "t1", 100))
            .await
            .unwrap();
        let s2 = EventOutboxRepo::append(store.pool(), &event("ws-a", "task_finished", "t1", 200))
            .await
            .unwrap();
        assert!(s2 > s1, "seq must strictly increase ({s1} -> {s2})");
    }

    #[tokio::test]
    async fn replay_returns_only_events_after_the_cursor_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;

        let s1 = EventOutboxRepo::append(store.pool(), &event("ws-a", "issue_created", "i1", 1))
            .await
            .unwrap();
        let s2 = EventOutboxRepo::append(store.pool(), &event("ws-a", "issue_updated", "i1", 2))
            .await
            .unwrap();
        let s3 = EventOutboxRepo::append(store.pool(), &event("ws-a", "task_finished", "t1", 3))
            .await
            .unwrap();

        // From the very start: the full backlog, oldest first.
        let all = EventOutboxRepo::replay(store.pool(), "ws-a", 0, 100).await.unwrap();
        assert_eq!(
            all.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [s1, s2, s3],
            "replay(0) is the whole backlog in seq order"
        );
        assert_eq!(all[0].event_type, "issue_created");
        assert_eq!(all[2].entity.as_deref(), Some("t1"));
        assert!(
            all[0].payload.contains("issue_created"),
            "payload is round-tripped"
        );

        // From a mid cursor: only the later events.
        let tail = EventOutboxRepo::replay(store.pool(), "ws-a", s1, 100).await.unwrap();
        assert_eq!(
            tail.iter().map(|e| e.seq).collect::<Vec<_>>(),
            [s2, s3],
            "replay(after=s1) drops everything at or before s1"
        );

        // From the head cursor: nothing.
        assert!(
            EventOutboxRepo::replay(store.pool(), "ws-a", s3, 100).await.unwrap().is_empty(),
            "replay from the head returns no events"
        );

        // The `limit` caps the batch.
        let capped = EventOutboxRepo::replay(store.pool(), "ws-a", 0, 2).await.unwrap();
        assert_eq!(capped.len(), 2, "limit caps the replay batch");
        assert_eq!(capped[0].seq, s1, "the cap keeps the oldest first");
    }

    #[tokio::test]
    async fn replay_and_head_seq_are_workspace_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_workspace(store.pool(), "ws-a").await;
        seed_workspace(store.pool(), "ws-b").await;

        EventOutboxRepo::append(store.pool(), &event("ws-a", "task_finished", "ta", 1))
            .await
            .unwrap();
        let b_seq = EventOutboxRepo::append(store.pool(), &event("ws-b", "task_finished", "tb", 2))
            .await
            .unwrap();

        // ws-a's replay never carries ws-b's event, even though ws-b's seq is
        // globally later.
        let a = EventOutboxRepo::replay(store.pool(), "ws-a", 0, 100).await.unwrap();
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].entity.as_deref(), Some("ta"));

        // A cursor from ws-b's seq must not hide ws-a's earlier event: scoping is
        // by workspace, not by the shared global sequence.
        let a_from_b_cursor =
            EventOutboxRepo::replay(store.pool(), "ws-a", b_seq, 100).await.unwrap();
        assert!(
            a_from_b_cursor.is_empty(),
            "ws-a has no event with seq greater than ws-b's later seq"
        );

        // Head cursor is per-workspace.
        assert_eq!(
            EventOutboxRepo::head_seq(store.pool(), "ws-b").await.unwrap(),
            b_seq
        );
        assert_eq!(
            EventOutboxRepo::head_seq(store.pool(), "ws-nope").await.unwrap(),
            0,
            "an unknown workspace has a zero head cursor"
        );
    }
}
