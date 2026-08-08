//! Retention primitives for the `fleet_event` ledger.
//!
//! `fleet_event` is the Fleet revision log. Until migration 0080 it had NO
//! retention at all: measured on a real profile it reached 1.1M rows / 847 MB,
//! of which 724 MB was hook payloads over 87k rows. The corpus is RATE-driven
//! rather than age-driven — 713 of those 714 MB were under seven days old, at
//! roughly 102 MB/day — so an age cutoff generous enough to feel safe would
//! have held the problem steady instead of solving it.
//!
//! Two independent reclaim mechanisms, plus a byte ceiling that tightens the
//! first one:
//!
//! 1. **Payload eviction** ([`FleetRetentionRepo::evict_payloads_before`]).
//!    Blanks the BODY of aged hook events and stamps `payload_evicted_at`. The
//!    row, its revision, and its ordering survive untouched, so no cursor moves.
//! 2. **Row delete** ([`FleetRetentionRepo::delete_events_before`]). Removes
//!    whole rows at a much longer horizon, guarded so it can never destroy the
//!    event backing a session's current request.
//!
//! ## Why deleting rows is safe for replay cursors
//!
//! Not re-derived here: `migrations/0075_prune_tmux_flipflop.sql` documents it
//! in full. In short — `revision` is `INTEGER PRIMARY KEY AUTOINCREMENT`, so a
//! deleted revision is never reused; every replay path is a `revision > ?`
//! range scan that simply skips holes; and state is served by the snapshot, not
//! rebuilt from replay. [`Self::delete_events_before`] additionally refuses to
//! touch the ledger HEAD, so no subscriber can be left parked past the end.

use sqlx::SqlitePool;

/// Aged-out `fleet_event` maintenance.
pub struct FleetRetentionRepo;

/// Hook event types whose payload the eviction sweep may blank.
///
/// STRUCTURAL SAFETY. This set has ZERO overlap with the six types
/// `FleetRepo::subscription_projection` reads to serve a session's open request
/// (`AskUserQuestion`, `PermissionRequest`, `item/tool/requestUserInput`,
/// `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`,
/// `item/permissions/requestApproval`). Disjointness is what makes eviction
/// safe BY CONSTRUCTION: no anti-join is needed because no cutoff, however
/// aggressive, can reach a live ask. `open_request_event_types_are_disjoint`
/// pins that invariant against drift on either side.
pub const EVICTABLE_EVENT_TYPES: [&str; 4] = [
    "PostToolUse",
    "PreToolUse",
    "PostToolBatch",
    "UserPromptSubmit",
];

impl FleetRetentionRepo {
    /// Blank the payloads of evictable hook events observed before `before_ms`.
    ///
    /// Rewrites `payload` to `'{}'` — never NULL, which the column forbids and
    /// which would reach a consumer as a driver error — and stamps
    /// `payload_evicted_at`, the tombstone that tells an evicted body apart
    /// from an event that genuinely carried none.
    ///
    /// Every visited row is stamped, including one whose payload was already
    /// empty. That is required for CONVERGENCE, not an accident: the sweep
    /// selects through the `payload_evicted_at IS NULL` partial index, so a row
    /// left unstamped would be re-selected by every later batch and starve the
    /// LIMIT of forward progress.
    ///
    /// Returns rows evicted. Call in a loop until it returns 0.
    ///
    /// # Errors
    /// Propagates the `SQLite` write failure.
    pub async fn evict_payloads_before(
        pool: &SqlitePool,
        before_ms: i64,
        now_ms: i64,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        let placeholders = vec!["?"; EVICTABLE_EVENT_TYPES.len()].join(", ");
        let sql = format!(
            "UPDATE fleet_event SET payload = '{{}}', payload_evicted_at = ? \
             WHERE revision IN (\
                SELECT revision FROM fleet_event \
                WHERE payload_evicted_at IS NULL AND event_type IN ({placeholders}) \
                  AND observed_at < ? LIMIT ?)"
        );
        let mut query = sqlx::query(&sql).bind(now_ms);
        for event_type in EVICTABLE_EVENT_TYPES {
            query = query.bind(event_type);
        }
        let result = query.bind(before_ms).bind(limit.max(0)).execute(pool).await?;
        Ok(result.rows_affected())
    }

    /// Delete whole `fleet_event` rows observed before `before_ms`.
    ///
    /// Uniform across event types, with two refusals:
    ///
    /// - a row whose `request_fingerprint` is some session's
    ///   `current_request_fingerprint` is the body of a LIVE ask, and deleting
    ///   it would silently blank that session's prompt;
    /// - the ledger head is never deleted, so `MAX(revision)` cannot move
    ///   backwards under a subscriber's cursor (same guard as 0075).
    ///
    /// `ORDER BY observed_at, revision` is load-bearing, not cosmetic: it is
    /// the key of `idx_fleet_event_observed_at` (migration 0081), and ordering
    /// by `revision` alone makes `SQLite` prefer a full `INTEGER PRIMARY KEY`
    /// scan to satisfy the sort and ignore that index — turning the steady
    /// state, where nothing is old enough to delete, back into an hourly walk
    /// of the whole ledger. Confirmed with `EXPLAIN QUERY PLAN`; there is no
    /// other symptom if the two drift apart.
    ///
    /// Returns rows deleted. Call in a loop until it returns 0.
    ///
    /// # Errors
    /// Propagates the `SQLite` write failure.
    pub async fn delete_events_before(
        pool: &SqlitePool,
        before_ms: i64,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM fleet_event WHERE revision IN (\
                SELECT e.revision FROM fleet_event e \
                WHERE e.observed_at < ? \
                  AND e.revision < (SELECT MAX(revision) FROM fleet_event) \
                  AND NOT EXISTS (\
                     SELECT 1 FROM fleet_session s \
                     WHERE s.current_request_fingerprint = e.request_fingerprint) \
                ORDER BY e.observed_at ASC, e.revision ASC LIMIT ?)",
        )
        .bind(before_ms)
        .bind(limit.max(0))
        .execute(pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Fold the write-ahead log back into the database file.
    ///
    /// Required between retention batches, not hygiene. Blanking ~700 MB of
    /// payload writes ~700 MB of WAL frames, and a WAL only shrinks at a
    /// checkpoint. A continuous backlog sweep starves the automatic checkpointer
    /// (it runs on a committing writer, and this writer never pauses long
    /// enough), so the WAL and the pages pinned behind it grow for the whole
    /// sweep — measured at 2,599 MB RSS against a 204 MB steady state on the
    /// first production backlog run.
    ///
    /// `PASSIVE` on purpose: it checkpoints as much as it can and returns
    /// immediately if a reader is mid-snapshot. `TRUNCATE`/`RESTART` would BLOCK
    /// on that reader, which is the one thing a background janitor must never do
    /// to the Fleet read path.
    ///
    /// # Errors
    /// Propagates the `SQLite` failure.
    pub async fn checkpoint_wal(pool: &SqlitePool) -> Result<(), sqlx::Error> {
        sqlx::query("PRAGMA wal_checkpoint(PASSIVE)").execute(pool).await?;
        Ok(())
    }

    /// Total bytes of `fleet_event` payload still holding content.
    ///
    /// The input to the byte ceiling.
    ///
    /// `LENGTH(CAST(payload AS BLOB))`, never a bare `LENGTH(payload)`. The
    /// content-skipping optimisation that lets `SQLite` answer a length from a
    /// row header applies to BLOB only: on TEXT, `length()` counts UTF-8
    /// CHARACTERS, so it must fault in every overflow page it measures.
    /// Measured on a 164 MB fixture — `SUM(LENGTH(text_col))` 0.178s versus
    /// `SUM(LENGTH(CAST(text_col AS BLOB)))` 0.064s, the latter matching a bare
    /// `COUNT(*)` at 0.030s. Reading 700 MB of payload through the page cache
    /// once an hour to answer one number is exactly the pressure this module
    /// exists to remove.
    ///
    /// Bytes are also the honest unit here: the ceiling is about disk, and a
    /// character count is not a byte count for any payload with non-ASCII in it.
    ///
    /// # Errors
    /// Propagates the `SQLite` read failure.
    pub async fn live_payload_bytes(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(LENGTH(CAST(payload AS BLOB))), 0) FROM fleet_event \
             WHERE payload_evicted_at IS NULL",
        )
        .fetch_one(pool)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;
    use sqlx::Row as _;

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        (dir, store)
    }

    async fn seed_session(pool: &SqlitePool, key: &str, fingerprint: Option<&str>) {
        sqlx::query(
            "INSERT INTO fleet_session \
             (session_key, cwd, discovered_at, last_observed_at, current_request_fingerprint) \
             VALUES (?, '/tmp', 0, 0, ?)",
        )
        .bind(key)
        .bind(fingerprint)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed_event(
        pool: &SqlitePool,
        event_id: &str,
        session_key: &str,
        observed_at: i64,
        event_type: &str,
        payload: &str,
        fingerprint: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO fleet_event \
             (event_id, session_key, observed_at, authority, event_type, payload, \
              request_fingerprint, session_version, applied) \
             VALUES (?, ?, ?, 'authoritative', ?, ?, ?, 1, 1)",
        )
        .bind(event_id)
        .bind(session_key)
        .bind(observed_at)
        .bind(event_type)
        .bind(payload)
        .bind(fingerprint)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn payload_of(pool: &SqlitePool, event_id: &str) -> Option<(String, Option<i64>)> {
        sqlx::query_as("SELECT payload, payload_evicted_at FROM fleet_event WHERE event_id = ?")
            .bind(event_id)
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    /// The invariant the whole eviction stage rests on: the types it blanks and
    /// the types an open request is read from share no member. If either list
    /// drifts this fails BEFORE eviction can eat a live ask.
    #[test]
    fn open_request_event_types_are_disjoint() {
        // Mirrors the IN list in `FleetRepo::subscription_projection`.
        const OPEN_REQUEST_EVENT_TYPES: [&str; 6] = [
            "AskUserQuestion",
            "PermissionRequest",
            "item/tool/requestUserInput",
            "item/commandExecution/requestApproval",
            "item/fileChange/requestApproval",
            "item/permissions/requestApproval",
        ];
        let source = include_str!("fleet.rs");
        for event_type in OPEN_REQUEST_EVENT_TYPES {
            assert!(
                source.contains(&format!("'{event_type}'")),
                "{event_type} is no longer read by the open-request projection — \
                 re-derive the disjointness argument before trusting eviction"
            );
            assert!(
                !EVICTABLE_EVENT_TYPES.contains(&event_type),
                "{event_type} became evictable — eviction would blank a live ask"
            );
        }
    }

    /// Aged hook payloads are blanked and tombstoned; a live `AskUserQuestion`
    /// of the same age is untouched, because its type is not evictable.
    #[tokio::test]
    async fn eviction_blanks_aged_hooks_and_spares_an_open_ask() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        seed_session(pool, "claude:s-1", Some("fp-live")).await;
        seed_event(
            pool,
            "e-hook",
            "claude:s-1",
            100,
            "PostToolUse",
            r#"{"big":1}"#,
            None,
        )
        .await;
        seed_event(
            pool,
            "e-ask",
            "claude:s-1",
            100,
            "AskUserQuestion",
            r#"{"question":"ship it?"}"#,
            Some("fp-live"),
        )
        .await;

        let evicted = FleetRetentionRepo::evict_payloads_before(pool, 1_000, 5_000, 100)
            .await
            .unwrap();

        assert_eq!(evicted, 1, "only the hook row is evictable");
        assert_eq!(
            payload_of(pool, "e-hook").await.unwrap(),
            ("{}".to_string(), Some(5_000)),
            "hook payload blanked and tombstoned"
        );
        assert_eq!(
            payload_of(pool, "e-ask").await.unwrap(),
            (r#"{"question":"ship it?"}"#.to_string(), None),
            "an open ask must survive eviction untouched"
        );
    }

    /// A second pass over already-evicted rows finds nothing: the tombstone,
    /// not the payload value, is what makes the sweep converge.
    #[tokio::test]
    async fn eviction_converges_and_never_revisits() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        seed_session(pool, "claude:s-1", None).await;
        seed_event(pool, "e-empty", "claude:s-1", 100, "PreToolUse", "{}", None).await;
        seed_event(
            pool,
            "e-full",
            "claude:s-1",
            100,
            "PreToolUse",
            r#"{"a":1}"#,
            None,
        )
        .await;

        assert_eq!(
            FleetRetentionRepo::evict_payloads_before(pool, 1_000, 5_000, 100)
                .await
                .unwrap(),
            2,
            "an already-empty row is still stamped, or it clogs every later batch"
        );
        assert_eq!(
            FleetRetentionRepo::evict_payloads_before(pool, 1_000, 6_000, 100)
                .await
                .unwrap(),
            0,
            "a converged sweep must do no work"
        );
    }

    /// The row delete drops aged rows but refuses the one backing a session's
    /// current request, and refuses the ledger head.
    #[tokio::test]
    async fn delete_spares_the_live_request_and_the_head() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        seed_session(pool, "claude:s-1", Some("fp-live")).await;
        seed_event(pool, "e-old", "claude:s-1", 100, "PostToolUse", "{}", None).await;
        seed_event(
            pool,
            "e-live-ask",
            "claude:s-1",
            100,
            "AskUserQuestion",
            r#"{"q":1}"#,
            Some("fp-live"),
        )
        .await;
        seed_event(
            pool,
            "e-stale-fp",
            "claude:s-1",
            100,
            "PostToolUse",
            "{}",
            Some("fp-gone"),
        )
        .await;
        seed_event(pool, "e-head", "claude:s-1", 100, "PostToolUse", "{}", None).await;

        let deleted = FleetRetentionRepo::delete_events_before(pool, 1_000, 100).await.unwrap();

        assert_eq!(
            deleted, 2,
            "the unreferenced old row and the stale fingerprint"
        );
        let surviving: Vec<String> =
            sqlx::query_scalar("SELECT event_id FROM fleet_event ORDER BY revision")
                .fetch_all(pool)
                .await
                .unwrap();
        assert_eq!(
            surviving,
            vec!["e-live-ask".to_string(), "e-head".to_string()],
            "a live request body and the ledger head must both survive"
        );
    }

    /// The byte ceiling counts BYTES, and only bodies still holding content.
    ///
    /// The payloads are deliberately non-ASCII. A bare `LENGTH()` on a TEXT
    /// column counts UTF-8 CHARACTERS, so on any payload carrying an em dash or
    /// a non-Latin path it under-reports the disk it is supposed to be
    /// bounding — and it is the same bare `LENGTH()` that forces `SQLite` to
    /// fault in every overflow page to do it. One assertion pins both the unit
    /// and, by consequence, the cheap plan.
    #[tokio::test]
    async fn live_payload_bytes_counts_bytes_not_characters_and_drops_evicted_rows() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        seed_session(pool, "claude:s-1", None).await;
        // 10 characters but 13 bytes: 'é' is 2 and '—' is 3.
        let body = r#"{"a":"é—"}"#;
        assert_eq!(body.chars().count(), 10);
        assert_eq!(body.len(), 13);
        seed_event(pool, "e-1", "claude:s-1", 100, "PostToolUse", body, None).await;
        seed_event(pool, "e-2", "claude:s-1", 9_000, "PostToolUse", body, None).await;

        let before = FleetRetentionRepo::live_payload_bytes(pool).await.unwrap();
        FleetRetentionRepo::evict_payloads_before(pool, 1_000, 5_000, 100)
            .await
            .unwrap();
        let after = FleetRetentionRepo::live_payload_bytes(pool).await.unwrap();

        assert_eq!(
            before, 26,
            "two 13-BYTE payloads — a character count would say 20 and under-report the disk"
        );
        assert_eq!(after, 13, "the evicted row leaves the accounting entirely");
    }

    /// The row delete must SEEK on `idx_fleet_event_observed_at`, never scan.
    ///
    /// Its steady state is the common one — nothing is 30 days old, so the
    /// query matches nothing — and without the index that costs a full walk of
    /// the whole ledger every hour to delete zero rows. Asserted on the PLAN
    /// because there is no other symptom: the results are identical either way,
    /// only the cost differs, and reordering the query by `revision` alone is
    /// enough to silently lose the index (measured: `SQLite` then prefers the
    /// `INTEGER PRIMARY KEY` scan to satisfy the sort).
    ///
    /// Mirrors `fleet_provider_event`'s own plan guard on its partial index.
    #[tokio::test]
    async fn the_row_delete_seeks_on_the_observed_at_index() {
        let (_dir, store) = store().await;
        let pool = store.pool();
        seed_session(pool, "claude:s-1", None).await;
        for n in 0..64 {
            seed_event(
                pool,
                &format!("e-{n}"),
                "claude:s-1",
                n,
                "PostToolUse",
                "{}",
                None,
            )
            .await;
        }
        sqlx::query("ANALYZE").execute(pool).await.unwrap();

        // The exact subquery `delete_events_before` runs, prefixed.
        let rows = sqlx::query(
            "EXPLAIN QUERY PLAN \
             SELECT e.revision FROM fleet_event e \
             WHERE e.observed_at < ? \
               AND e.revision < (SELECT MAX(revision) FROM fleet_event) \
               AND NOT EXISTS (\
                  SELECT 1 FROM fleet_session s \
                  WHERE s.current_request_fingerprint = e.request_fingerprint) \
             ORDER BY e.observed_at ASC, e.revision ASC LIMIT ?",
        )
        .bind(10_i64)
        .bind(500_i64)
        .fetch_all(pool)
        .await
        .unwrap();
        let detail = rows
            .iter()
            .map(|row| row.try_get::<String, _>("detail").unwrap())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            detail.contains("idx_fleet_event_observed_at"),
            "the retention delete must seek the observed_at index, plan was:\n{detail}"
        );
        assert!(
            detail.contains("idx_fleet_session_current_request"),
            "and the live-request guard must ride its own index rather than \
             rebuilding an AUTOMATIC one per batch, plan was:\n{detail}"
        );
    }
}
