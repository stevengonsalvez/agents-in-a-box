//! Typed repository over the `standup_session` table (migration 0028) — the
//! per-session state the auto-standup watcher (D13) enforces its guardrails
//! against.
//!
//! Auto-`/standup` WRITES into a stagnant, idle-at-prompt session. Stevie's
//! locked override keeps the feature but demands every guardrail: a per-session
//! opt-out, a per-session cooldown anchored on `last_standup_at`, and an
//! `in_flight` marker whose COUNT across rows enforces the daemon-global
//! max-concurrent cap. This repo is the durable home for that state (the daemon
//! owns all state — INV-1); the pure gate decision lives in the daemon's standup
//! module and reads these rows.
//!
//! One row per monitored session; a session with no row has never been
//! standup-touched, which the watcher treats as "not opted out, no cooldown, not
//! in flight" (the coded default).

use sqlx::{Row, SqlitePool};

/// A materialised `standup_session` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StandupSessionRow {
    /// The monitored session id.
    pub session_id: String,
    /// `true` when the human excluded this session from auto-standup — a hard
    /// guardrail: an opted-out session NEVER fires.
    pub opted_out: bool,
    /// Epoch-ms the last auto-standup fired, or `None` — the cooldown anchor.
    pub last_standup_at: Option<i64>,
    /// `true` while a fired standup's turn is still running; blocks a second fire
    /// and counts toward the max-concurrent cap.
    pub in_flight: bool,
    /// Epoch-ms of the last state change.
    pub updated_at: i64,
}

/// Stateless typed wrapper over the `standup_session` table.
pub struct StandupRepo;

impl StandupRepo {
    /// Fetch one session's standup state, `None` when it has never been touched
    /// (the watcher reads this as the coded default).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn get(
        pool: &SqlitePool,
        session_id: &str,
    ) -> Result<Option<StandupSessionRow>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT session_id, opted_out, last_standup_at, in_flight, updated_at \
             FROM standup_session WHERE session_id = ?",
        )
        .bind(session_id)
        .fetch_optional(pool)
        .await?;
        Ok(row.as_ref().map(row_from_sqlite))
    }

    /// Set (or clear) a session's opt-out, upserting the row.
    ///
    /// The per-session opt-out is a hard guardrail the human controls; this is the
    /// write behind `ainb fleet standup opt-out <session>` (and its `--rejoin`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn set_opt_out(
        pool: &SqlitePool,
        session_id: &str,
        opted_out: bool,
        now_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO standup_session (session_id, opted_out, in_flight, updated_at) \
             VALUES (?, ?, 0, ?) \
             ON CONFLICT(session_id) DO UPDATE SET opted_out = excluded.opted_out, \
                 updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(i64::from(opted_out))
        .bind(now_ms)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Record a fired standup: stamp `last_standup_at` (the cooldown anchor) AND
    /// set `in_flight = 1` in one write. Upserts so a first-time fire creates the
    /// row.
    ///
    /// The cooldown is anchored on FIRE time (not completion) so a session cannot
    /// be re-nudged during its own standup turn even if the watcher never sees the
    /// completion; `in_flight` is the belt to this cooldown's braces.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn mark_fired(
        pool: &SqlitePool,
        session_id: &str,
        now_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO standup_session \
                 (session_id, opted_out, last_standup_at, in_flight, updated_at) \
             VALUES (?, 0, ?, 1, ?) \
             ON CONFLICT(session_id) DO UPDATE SET last_standup_at = excluded.last_standup_at, \
                 in_flight = 1, updated_at = excluded.updated_at",
        )
        .bind(session_id)
        .bind(now_ms)
        .bind(now_ms)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Clear a session's `in_flight` marker — called when the fired standup's turn
    /// completes (the watcher captured its end-turn). Leaves `last_standup_at`
    /// intact so the cooldown keeps counting from the fire.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the write fails.
    pub async fn clear_in_flight(
        pool: &SqlitePool,
        session_id: &str,
        now_ms: i64,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE standup_session SET in_flight = 0, updated_at = ? WHERE session_id = ?",
        )
        .bind(now_ms)
        .bind(session_id)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Count the currently in-flight standups across ALL sessions — the
    /// max-concurrent guardrail's denominator.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn count_in_flight(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM standup_session WHERE in_flight = 1")
            .fetch_one(pool)
            .await
    }

    /// List the session ids with an in-flight standup — the watcher scans these
    /// to detect completion (end-turn after the fire) and clear the marker.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_in_flight(pool: &SqlitePool) -> Result<Vec<StandupSessionRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT session_id, opted_out, last_standup_at, in_flight, updated_at \
             FROM standup_session WHERE in_flight = 1 ORDER BY session_id ASC",
        )
        .fetch_all(pool)
        .await?;
        Ok(rows.iter().map(row_from_sqlite).collect())
    }
}

/// Map one raw `standup_session` row into a [`StandupSessionRow`].
fn row_from_sqlite(row: &sqlx::sqlite::SqliteRow) -> StandupSessionRow {
    let opted_out: i64 = row.get("opted_out");
    let in_flight: i64 = row.get("in_flight");
    StandupSessionRow {
        session_id: row.get("session_id"),
        opted_out: opted_out != 0,
        last_standup_at: row.get("last_standup_at"),
        in_flight: in_flight != 0,
        updated_at: row.get("updated_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[tokio::test]
    async fn untouched_session_reads_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert!(StandupRepo::get(store.pool(), "sess").await.unwrap().is_none());
        assert_eq!(StandupRepo::count_in_flight(store.pool()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn opt_out_roundtrips_and_toggles() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        StandupRepo::set_opt_out(store.pool(), "sess", true, 1000).await.unwrap();
        let row = StandupRepo::get(store.pool(), "sess").await.unwrap().unwrap();
        assert!(row.opted_out);
        assert!(row.last_standup_at.is_none());
        // Rejoin flips it back without disturbing the rest of the row.
        StandupRepo::set_opt_out(store.pool(), "sess", false, 2000).await.unwrap();
        assert!(!StandupRepo::get(store.pool(), "sess").await.unwrap().unwrap().opted_out);
    }

    #[tokio::test]
    async fn mark_fired_sets_cooldown_anchor_and_in_flight() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        StandupRepo::mark_fired(store.pool(), "sess", 5000).await.unwrap();
        let row = StandupRepo::get(store.pool(), "sess").await.unwrap().unwrap();
        assert_eq!(row.last_standup_at, Some(5000));
        assert!(row.in_flight);
        assert_eq!(StandupRepo::count_in_flight(store.pool()).await.unwrap(), 1);

        // Completion clears in_flight but preserves the cooldown anchor.
        StandupRepo::clear_in_flight(store.pool(), "sess", 6000).await.unwrap();
        let row = StandupRepo::get(store.pool(), "sess").await.unwrap().unwrap();
        assert!(!row.in_flight);
        assert_eq!(
            row.last_standup_at,
            Some(5000),
            "cooldown anchor survives completion"
        );
        assert_eq!(StandupRepo::count_in_flight(store.pool()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn mark_fired_preserves_opt_out_history() {
        // A previously opted-out-then-rejoined session that later fires must keep
        // its opt-out state accurate (mark_fired only touches the standup fields).
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        StandupRepo::set_opt_out(store.pool(), "sess", false, 1000).await.unwrap();
        StandupRepo::mark_fired(store.pool(), "sess", 5000).await.unwrap();
        let row = StandupRepo::get(store.pool(), "sess").await.unwrap().unwrap();
        assert!(
            !row.opted_out,
            "a fire must not silently re-opt-out the session"
        );
        assert!(row.in_flight);
    }

    #[tokio::test]
    async fn count_and_list_in_flight_span_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        StandupRepo::mark_fired(store.pool(), "s1", 1000).await.unwrap();
        StandupRepo::mark_fired(store.pool(), "s2", 2000).await.unwrap();
        assert_eq!(StandupRepo::count_in_flight(store.pool()).await.unwrap(), 2);
        let ids: Vec<_> = StandupRepo::list_in_flight(store.pool())
            .await
            .unwrap()
            .into_iter()
            .map(|r| r.session_id)
            .collect();
        assert_eq!(ids, ["s1", "s2"]);
        // Clearing one leaves the other in flight.
        StandupRepo::clear_in_flight(store.pool(), "s1", 3000).await.unwrap();
        assert_eq!(StandupRepo::count_in_flight(store.pool()).await.unwrap(), 1);
    }
}
