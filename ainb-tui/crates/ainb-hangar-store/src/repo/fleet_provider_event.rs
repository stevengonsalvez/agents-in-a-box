//! Durable raw provider-event ledger for Fleet projections, and (since 0079)
//! the ACP transcript store.
//!
//! ## RETENTION: three regimes, split by `source` and by projection state.
//!
//! **Projection sources (`source <> 'acp'`), ALREADY REDUCED
//! (`projection_revision IS NOT NULL`): payload evicted after 7 days**, by the
//! automatic sweep in `ainb-hangar-daemon`'s `fleet_provider_retention`, which
//! calls [`FleetProviderEventRepo::evict_projected_payloads_before`].
//!
//! This REVERSES the never-trim stance this header carried from 0071 until
//! 0094, and the reversal is measurement-driven. That stance rested on a growth
//! model of "roughly 21 rows per 2 days at a ~344 byte mean payload, ~1.3 MB
//! per YEAR", which is falsified: measured on a real profile the table is
//! **372,031 rows / 2,207 MB**, a ~5.9 KB mean payload and three orders of
//! magnitude more bytes than the model allowed for. At that size the table
//! saturated the single `SQLite` writer this crate shares with the Fleet
//! reducer and the ACP transcript writer, and session spawn began failing with
//! `database is locked` — so the cost of keeping every envelope forever is no
//! longer hypothetical disk, it is an unusable daemon.
//!
//! What the old stance was protecting is preserved anyway: a FUTURE reducer can
//! still replay HISTORY, because eviction deletes no row. `ingest_order`,
//! `event_id`, `raw_blake3` and `projection_revision` all survive an eviction —
//! only the bytes of `raw_payload` go, and only on rows a reducer has already
//! consumed. What is genuinely given up is re-deriving NEW facts from an
//! envelope older than 7 days; that is the trade the measurement forces, and
//! 7 days is where it was struck (~450 MB of the measured corpus retained).
//!
//! One accepted consequence, visible rather than silent:
//!
//! 1. [`Self::list_by_session_after`] pages by `session_key` alone, so a
//!    transcript read of a non-ACP session older than the TTL returns rows
//!    whose payload is `""`. That is the retention policy being visible, not a
//!    fault: the row, its order and its type still render, and
//!    `transcript_chunk_wire` already carries a non-JSON payload through as a
//!    string rather than failing the read.
//!
//! **Projection sources, NOT YET REDUCED (`projection_revision IS NULL`):
//! never touched by anything.** On these rows NULL marks pending recovery work
//! which the Codex manager replays on startup, not garbage. Blanking such a
//! payload destroys the replay input itself, so both the sweep predicate and
//! [`Self::delete_acp_before`]'s `source` filter exclude them, and nothing may
//! ever delete such a row.
//!
//! **ACP transcript rows (`source = 'acp'`): operator-invoked
//! export-then-delete only, via [`FleetProviderEventRepo::delete_acp_before`].**
//! These rows carry NO `projection_revision` recovery contract (no reducer
//! ever projects them; `NULL` is their steady state, not pending work), which
//! is exactly what makes deleting them safe. They dominate write volume — one
//! ACP turn emits tens to low hundreds of coalesced chunk rows, so five
//! sessions at twenty turns a day at fifty rows a turn is ~5,000 rows and
//! roughly 1.8 MB per DAY — and they are deliberately left OUT of the automatic
//! sweep, because their reclaim path exports before it destroys and only an
//! operator can say where that export goes.
//!
//! Revisit trigger: `source='acp'` rows exceeding ~1M rows or ~100 MB on a
//! real profile without the export-then-delete command having been run, or ACP
//! write volume visibly contending the single `SQLite` writer (watch the
//! commits-issued counter).
//!
//! Revisit trigger for the PROJECTION-source regime, which the 7-day sweep now
//! bounds: live `raw_payload` for `source <> 'acp'` exceeding ~500 MB on a real
//! profile, or the measured accrual rate exceeding ~150 MB/day (it was ~64
//! MB/day when the sweep was written, and the 7-day window holds roughly seven
//! times the daily rate).
//!
//! This trigger exists because its absence is what let the previous model rot.
//! From 0071 to 0094 this header asserted ~1.3 MB per YEAR from a ~344 byte
//! mean, with nothing to check it against; the real figures were 372k rows /
//! 2207 MB at a ~5.9 KB mean, and the gap only surfaced when the writer
//! saturated and session spawn began failing. An age cutoff alone fixes the
//! WINDOW and lets the arrival rate pick the size, so a rate that doubles
//! doubles the residue with no other signal — the same argument
//! [`crate::repo::fleet_retention`] makes for keeping a byte ceiling as a
//! co-equal control on `fleet_event`. No ceiling is implemented here yet; this
//! trigger is the manual stand-in for one, and reaching it means writing it.
//!
//! The pending-recovery partial index keeps its predicate
//! (`WHERE projection_revision IS NULL`) across 0079; ACP rows are pushed out
//! of the recovery scan's way by the index KEY
//! (`source, provider, projection_revision`), never by a predicate the
//! planner could not prove for a bound `source = ?` parameter.
//!
//! 0094's retention index CAN carry `source <> 'acp'` in its predicate, and
//! that is not a contradiction: the sweep spells all three of its terms as
//! LITERALS, so the implication is provable at plan time, whereas the recovery
//! scan binds `source = ?` and nothing about a parameter is provable.

use blake3::Hasher;
use sqlx::{Row, SqlitePool};

/// One raw provider envelope awaiting or linked to Fleet projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFleetProviderEvent {
    /// Replay-safe identity minted by provider or source adapter.
    pub event_id: String,
    /// Provider token, for example `claude` or `codex`.
    pub provider: String,
    /// Source transport, for example `claude_hook` or `codex_app_server`.
    pub source: String,
    /// Fleet session identity when known.
    pub session_key: Option<String>,
    /// Provider-owned session identity when known.
    pub provider_session_id: Option<String>,
    /// Provider observation time in epoch milliseconds.
    pub observed_at: i64,
    /// Local source receipt time in epoch milliseconds.
    pub received_at: i64,
    /// Provider event discriminator.
    pub event_type: String,
    /// Exact source payload, never a normalized projection body.
    pub raw_payload: String,
}

/// Persisted raw provider envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FleetProviderEventRow {
    /// Monotonic receipt order assigned by SQLite.
    pub ingest_order: i64,
    /// Replay-safe event identity.
    pub event_id: String,
    /// Provider token.
    pub provider: String,
    /// Source transport.
    pub source: String,
    /// Fleet session identity when known.
    pub session_key: Option<String>,
    /// Provider session identity when known.
    pub provider_session_id: Option<String>,
    /// Provider observation time.
    pub observed_at: i64,
    /// Local receipt time.
    pub received_at: i64,
    /// Provider event discriminator.
    pub event_type: String,
    /// Exact source payload, or `""` once retention has evicted it.
    ///
    /// Read it as "the body, if we still hold it". A projected row older than
    /// the retention TTL keeps its identity and loses its bytes; see the module
    /// header. Use [`Self::raw_blake3`] when you need to compare or verify a
    /// payload, since that survives eviction.
    pub raw_payload: String,
    /// Content digest of the payload as originally received.
    ///
    /// Always the digest of the ORIGINAL body, never of an evicted `""`, so it
    /// stays a usable identity for a row whose bytes are gone.
    pub raw_blake3: String,
    /// Fleet projection revision once reduced.
    pub projection_revision: Option<i64>,
}

/// Source-ledger failures that must stop a replay cursor.
#[derive(Debug, thiserror::Error)]
pub enum FleetProviderEventError {
    /// Existing ID points to a different source envelope.
    #[error("provider event id {event_id:?} conflicts with a different envelope")]
    EventIdCollision {
        /// Conflicting event identity.
        event_id: String,
    },
    /// The requested source event is absent from the durable ledger.
    #[error("provider event {event_id:?} was not found")]
    EventNotFound {
        /// Missing event identity.
        event_id: String,
    },
    /// SQLite failed.
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

impl FleetProviderEventError {
    /// Whether this failure can NEVER clear, however often the same rows are
    /// retried.
    ///
    /// A buffering writer needs the distinction to decide whether to keep a
    /// rejected batch: restoring a permanently doomed one pins it at the head
    /// of the buffer forever and nothing behind it ever commits (see
    /// `ainb-acp`'s `StoreWriter::flush`). The taxonomy lives here because the
    /// store owns the error, and because `ainb-acp` is fenced off from `sqlx`
    /// and cannot inspect the underlying database error itself.
    ///
    /// Only failures that are a property of the ROWS qualify. Anything
    /// environmental (lock contention, a closed pool, a full disk, a
    /// read-only file) is retryable, because an operator can clear it and the
    /// rows are still good. Erring in that direction costs a retry; erring the
    /// other way throws away data that would have committed.
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        match self {
            // A committed row already owns this event_id under a different
            // envelope. The ledger is append-only, so no retry changes that.
            Self::EventIdCollision { .. } | Self::EventNotFound { .. } => true,
            // Constraint violations are the batch's own shape: a CHECK the
            // payload fails, a duplicate key, a parent row that is not there.
            Self::Sql(sqlx::Error::Database(db)) => {
                db.is_check_violation() || db.is_unique_violation() || db.is_foreign_key_violation()
            }
            Self::Sql(_) => false,
        }
    }
}

/// Typed access to the raw provider-event ledger.
pub struct FleetProviderEventRepo;

impl FleetProviderEventRepo {
    /// Insert one source envelope. Exact replays return its original row.
    pub async fn append(
        pool: &SqlitePool,
        event: &NewFleetProviderEvent,
    ) -> Result<FleetProviderEventRow, FleetProviderEventError> {
        let digest = digest(&event.raw_payload);
        let result = sqlx::query(
            "INSERT INTO fleet_provider_event (event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT(event_id) DO NOTHING",
        )
        .bind(&event.event_id)
        .bind(&event.provider)
        .bind(&event.source)
        .bind(&event.session_key)
        .bind(&event.provider_session_id)
        .bind(event.observed_at)
        .bind(event.received_at)
        .bind(&event.event_type)
        .bind(&event.raw_payload)
        .bind(&digest)
        .execute(pool)
        .await?;
        let row = Self::get(pool, &event.event_id).await?.ok_or_else(|| {
            FleetProviderEventError::EventNotFound {
                event_id: event.event_id.clone(),
            }
        })?;
        if result.rows_affected() == 0 && !matches_event(&row, event, &digest) {
            return Err(FleetProviderEventError::EventIdCollision {
                event_id: event.event_id.clone(),
            });
        }
        Ok(row)
    }

    /// Insert a whole batch of source envelopes in ONE transaction, returning
    /// the highest `ingest_order` the batch owns.
    ///
    /// This exists for the ACP transcript hot path (plan Phase 4 commit
    /// cadence). Per-row [`Self::append`] means one transaction per chunk, and
    /// every transaction takes the single `SQLite` write lock shared with the
    /// fleet event log, the claim loop and the outbox drain (`store.rs:77-90`,
    /// whose comment warns a contended writer can exhaust its 10 s
    /// `busy_timeout`). Coalescing bounds the ROW count; only a batched commit
    /// bounds the COMMIT count.
    ///
    /// `ON CONFLICT DO UPDATE SET event_id = excluded.event_id` is a deliberate
    /// no-op write: it changes no column value (the conflict key equals the
    /// excluded key) but makes `RETURNING` fire on the replay leg too, so a
    /// batch containing an already-committed `event_id` still reports that
    /// row's true `ingest_order` instead of a stale `last_insert_rowid`.
    ///
    /// The returned row is then compared against the incoming envelope, so
    /// BOTH doors into this ledger enforce one contract: an exact replay is a
    /// no-op, and the same `event_id` carrying a DIFFERENT envelope is
    /// [`FleetProviderEventError::EventIdCollision`], never a silently
    /// discarded payload. The whole batch's transaction rolls back with it.
    pub async fn append_batch(
        pool: &SqlitePool,
        events: &[NewFleetProviderEvent],
    ) -> Result<Option<i64>, FleetProviderEventError> {
        if events.is_empty() {
            return Ok(None);
        }
        let mut tx = pool.begin().await?;
        let mut high_water: Option<i64> = None;
        for event in events {
            let digest = digest(&event.raw_payload);
            let stored = sqlx::query(
                "INSERT INTO fleet_provider_event (event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT(event_id) DO UPDATE SET event_id = excluded.event_id \
                 RETURNING ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision",
            )
            .bind(&event.event_id)
            .bind(&event.provider)
            .bind(&event.source)
            .bind(&event.session_key)
            .bind(&event.provider_session_id)
            .bind(event.observed_at)
            .bind(event.received_at)
            .bind(&event.event_type)
            .bind(&event.raw_payload)
            .bind(&digest)
            .fetch_one(&mut *tx)
            .await?;
            let row = row_from(&stored)?;
            if !matches_event(&row, event, &digest) {
                return Err(FleetProviderEventError::EventIdCollision {
                    event_id: event.event_id.clone(),
                });
            }
            let order = row.ingest_order;
            high_water = Some(high_water.map_or(order, |current: i64| current.max(order)));
        }
        tx.commit().await?;
        Ok(high_water)
    }

    /// Link a source envelope to the revision that reduced it. Replays preserve
    /// the first committed projection, but a conflicting revision is rejected.
    pub async fn mark_projected(
        pool: &SqlitePool,
        event_id: &str,
        revision: i64,
    ) -> Result<(), FleetProviderEventError> {
        let result = sqlx::query(
            "UPDATE fleet_provider_event SET projection_revision = ? WHERE event_id = ? AND (projection_revision IS NULL OR projection_revision = ?)",
        )
        .bind(revision)
        .bind(event_id)
        .bind(revision)
        .execute(pool)
        .await?;
        if result.rows_affected() == 0 {
            if Self::get(pool, event_id).await?.is_none() {
                return Err(FleetProviderEventError::EventNotFound {
                    event_id: event_id.to_string(),
                });
            }
            return Err(FleetProviderEventError::EventIdCollision {
                event_id: event_id.to_string(),
            });
        }
        Ok(())
    }

    /// Fetch one persisted source envelope.
    pub async fn get(
        pool: &SqlitePool,
        event_id: &str,
    ) -> Result<Option<FleetProviderEventRow>, sqlx::Error> {
        sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision FROM fleet_provider_event WHERE event_id = ?",
        )
        .bind(event_id)
        .fetch_optional(pool)
        .await?
        .as_ref()
        .map(row_from)
        .transpose()
    }

    /// List source envelopes that still require projection, oldest receipt first.
    pub async fn unprojected(
        pool: &SqlitePool,
        provider: &str,
        source: &str,
        limit: i64,
    ) -> Result<Vec<FleetProviderEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event WHERE provider = ? AND source = ? AND projection_revision IS NULL \
             ORDER BY ingest_order ASC LIMIT ?",
        )
        .bind(provider)
        .bind(source)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from).collect()
    }

    /// Newest committed `ingest_order` for one session, or `None` on an empty
    /// transcript. The subscribe acknowledgement's head cursor.
    pub async fn head_order_for_session(
        pool: &SqlitePool,
        session_key: &str,
    ) -> Result<Option<i64>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT MAX(ingest_order) FROM fleet_provider_event WHERE session_key = ?",
        )
        .bind(session_key)
        .fetch_one(pool)
        .await
    }

    /// The NEWEST row of one session carrying `event_type`, or `None` when the
    /// session wrote none.
    ///
    /// Deliberately NOT a filter over [`Self::list_by_session_tail`]: a run's
    /// accounting row (`acp.usage`) is written whenever the agent reports one,
    /// and an ordinary turn buries it under an unbounded number of later text
    /// and tool rows. A tail window would therefore answer "no usage" on
    /// exactly the chatty runs whose usage matters most, and would look like it
    /// worked on the short ones. This walks
    /// `idx_fleet_provider_event_session_order` backwards and stops at the first
    /// match instead. `the_event_type_read_seeks_on_the_session_index` asserts
    /// the plan, because a scan here would only ever show up as a slow finalize.
    pub async fn last_by_session_event_type(
        pool: &SqlitePool,
        session_key: &str,
        event_type: &str,
    ) -> Result<Option<FleetProviderEventRow>, sqlx::Error> {
        let row = sqlx::query(LAST_OF_TYPE_SELECT)
            .bind(session_key)
            .bind(event_type)
            .fetch_optional(pool)
            .await?;
        row.as_ref().map(row_from).transpose()
    }

    /// Page one session's rows after an `ingest_order` cursor, oldest first
    /// (the transcript read model; rides `idx_fleet_provider_event_session_order`).
    pub async fn list_by_session_after(
        pool: &SqlitePool,
        session_key: &str,
        after_order: i64,
        limit: i64,
    ) -> Result<Vec<FleetProviderEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event WHERE session_key = ? AND ingest_order > ? \
             ORDER BY ingest_order ASC LIMIT ?",
        )
        .bind(session_key)
        .bind(after_order)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from).collect()
    }

    /// The NEWEST rows of one session's transcript, returned oldest first, and
    /// whether older rows were left behind.
    ///
    /// The tail read behind `hangar/board_card_timeline`: an execution view
    /// shows the END of a run, and a whole transcript is unbounded in both rows
    /// and bytes (a coalesced text chunk is a few KiB, a tool call's verbatim
    /// update has no ceiling at all). So it is bounded TWICE — `max_rows` in
    /// SQL, then `max_payload_bytes` walking back from the newest row.
    ///
    /// **Neither cap dominates the other, and which one binds depends entirely
    /// on the run.** A transcript of short structural rows hits `max_rows`
    /// first; one of coalesced 4 KiB text chunks hits the byte budget first.
    /// That is why the truncation flag is RETURNED rather than inferable: a
    /// caller cannot tell from `rows.len()` alone whether it is holding the
    /// whole transcript, and one that assumed it was would render a partial run
    /// as a complete one. Returned as a tuple the caller must destructure, so
    /// ignoring it is a deliberate `_` and not an oversight.
    ///
    /// At least one row always survives the byte budget: a single payload
    /// larger than the whole budget must still be shown (its own renderer caps
    /// it), never silently swallowed into an empty transcript.
    pub async fn list_by_session_tail(
        pool: &SqlitePool,
        session_key: &str,
        max_rows: i64,
        max_payload_bytes: usize,
    ) -> Result<(Vec<FleetProviderEventRow>, bool), sqlx::Error> {
        let max_rows = max_rows.max(1);
        // One row MORE than the cap, so "was there anything older" is answered by
        // this query rather than by a second one. A session holding exactly
        // `max_rows` rows is complete, not truncated, and the extra row is the
        // only thing that distinguishes the two.
        let rows = sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event WHERE session_key = ? \
             ORDER BY ingest_order DESC LIMIT ?",
        )
        .bind(session_key)
        .bind(max_rows.saturating_add(1))
        .fetch_all(pool)
        .await?;
        let mut truncated = i64::try_from(rows.len()).is_ok_and(|got| got > max_rows);
        let rows = &rows[..rows.len().min(usize::try_from(max_rows).unwrap_or(usize::MAX))];
        let mut budget = max_payload_bytes;
        let mut tail: Vec<FleetProviderEventRow> = Vec::new();
        for row in rows {
            // A row that cannot be decoded is SKIPPED, not fatal: the classifier
            // reading these same rows is documented total, and aborting here
            // would lose a whole run's view to one bad row.
            let row = match row_from(row) {
                Ok(row) => row,
                Err(error) => {
                    tracing::warn!(%session_key, %error, "skipping an undecodable transcript row");
                    truncated = true;
                    continue;
                }
            };
            let cost = row.raw_payload.len();
            if !tail.is_empty() && cost > budget {
                truncated = true;
                break;
            }
            budget = budget.saturating_sub(cost);
            tail.push(row);
        }
        tail.reverse();
        Ok((tail, truncated))
    }

    /// One PAGE of the rows [`Self::delete_acp_before`] could remove, oldest
    /// first, starting strictly above `after_ingest_order`.
    ///
    /// The export half of export-then-delete. Paged rather than fetched whole
    /// because the prune's ceiling is a ROW cap and a row's payload has no byte
    /// cap: an export that materialised every row plus its serialisation would
    /// size its peak memory off operator input.
    ///
    /// The caller must delete on the watermark it actually EXPORTED
    /// (`last exported ingest_order + 1`), never on `before_ingest_order`: a
    /// turn that commits between the last page and the delete lands below the
    /// requested watermark and would otherwise be destroyed without ever having
    /// been exported.
    pub async fn list_acp_before(
        pool: &SqlitePool,
        session_key: &str,
        after_ingest_order: i64,
        before_ingest_order: i64,
        limit: i64,
    ) -> Result<Vec<FleetProviderEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event \
             WHERE session_key = ? AND ingest_order > ? AND ingest_order < ? AND source = 'acp' \
             ORDER BY ingest_order ASC LIMIT ?",
        )
        .bind(session_key)
        .bind(after_ingest_order)
        .bind(before_ingest_order)
        .bind(limit.max(0))
        .fetch_all(pool)
        .await?;
        rows.iter().map(row_from).collect()
    }

    /// The operator export-then-delete leg: remove one session's ACP
    /// transcript rows below `before_ingest_order`, returning the count.
    ///
    /// `before_ingest_order` is the EXPORTED watermark
    /// (`last exported ingest_order + 1`), never the watermark the operator
    /// asked for. Re-evaluating the operator's predicate here would delete
    /// every row committed since the export read its last page, and after this
    /// call the export is the only copy those rows never made it into.
    ///
    /// Two refusal rules, both enforced by the statement itself:
    /// - a row with `source <> 'acp'` is never touched, regardless of the
    ///   range asked for;
    /// - therefore no row carrying the `projection_revision IS NULL` pending
    ///   recovery contract is ever touched, because that contract exists only
    ///   on the non-ACP projection sources (see the header: on ACP rows NULL
    ///   is the steady state, which is what makes them deletable at all).
    pub async fn delete_acp_before(
        pool: &SqlitePool,
        session_key: &str,
        before_ingest_order: i64,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            "DELETE FROM fleet_provider_event \
             WHERE session_key = ? AND ingest_order < ? AND source = 'acp'",
        )
        .bind(session_key)
        .bind(before_ingest_order)
        .execute(pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Blank the payloads of ALREADY-REDUCED projection-source rows observed
    /// before `before_ms`, keeping the rows themselves. Returns rows evicted.
    ///
    /// The automatic half of this table's retention (0094), driven by
    /// `ainb-hangar-daemon`'s `fleet_provider_retention` sweeper. The header
    /// carries the measurement that overturned the old never-trim stance and
    /// the trade it makes; this is what it does mechanically.
    ///
    /// Rewrites `raw_payload` to `''` and NOTHING else. No row is deleted and no
    /// other column is written, so `ingest_order`, `event_id`, `raw_blake3` and
    /// `projection_revision` all read back unchanged after an eviction.
    ///
    /// Three refusals, all enforced by the statement itself:
    ///
    /// - `projection_revision IS NOT NULL` — a NULL marks pending recovery work
    ///   the Codex manager replays at startup, and its payload IS the replay
    ///   input. This is the one that must never regress.
    /// - `source <> 'acp'` — ACP transcript rows reclaim through the operator's
    ///   export-then-delete ([`Self::delete_acp_before`]), which exports before
    ///   it destroys. Nothing sweeps them on a timer.
    /// - `raw_payload <> ''` — already evicted, and, more importantly, this is
    ///   what makes the sweep CONVERGE. Unlike `fleet_event`, which needs a
    ///   `payload_evicted_at` tombstone because its `'{}'` sentinel is
    ///   ambiguous, a blanked row here simply stops matching, so a later batch
    ///   cannot re-select it and starve the LIMIT of forward progress.
    ///
    /// `ORDER BY observed_at ASC` is load-bearing, not cosmetic: it is the key
    /// of `idx_fleet_provider_event_retention` (0094), whose predicate is these
    /// three refusals verbatim so `SQLite` can prove the partial index usable.
    /// Ordering by `ingest_order` instead silently degrades the plan from
    /// `SEARCH ... USING INDEX idx_fleet_provider_event_retention (observed_at<?)`
    /// to a full `SCAN` of a multi-gigabyte table, with identical results and no
    /// other symptom. `the_sweep_seeks_on_the_retention_index` asserts the plan.
    ///
    /// Call in a loop until it returns 0.
    ///
    /// # Errors
    /// Propagates the `SQLite` write failure.
    pub async fn evict_projected_payloads_before(
        pool: &SqlitePool,
        before_ms: i64,
        limit: i64,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(&format!(
            "UPDATE fleet_provider_event SET raw_payload = '' \
             WHERE ingest_order IN ({EVICTION_SELECT})"
        ))
        .bind(before_ms)
        .bind(limit.max(0))
        .execute(pool)
        .await?;
        Ok(result.rows_affected())
    }
}

fn digest(payload: &str) -> String {
    let mut hasher = Hasher::new();
    hasher.update(payload.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// The rows one eviction batch claims, as ONE definition.
///
/// Shared by `evict_projected_payloads_before` and by the plan test rather than
/// written out twice: a test that re-types the statement it is meant to guard
/// cannot see the statement drift away from it. Binds `observed_at` cutoff then
/// row limit.
///
/// Every term of the WHERE is a literal, not a bound parameter, so SQLite can
/// prove it implies `idx_fleet_provider_event_retention`'s predicate and use the
/// partial index. Binding `source = ?` instead would silently lose the index.
/// `ORDER BY observed_at` must stay paired with that index's key; ordering by
/// `ingest_order` degrades the plan to a full scan with no other symptom.
const EVICTION_SELECT: &str = "SELECT ingest_order FROM fleet_provider_event \
     WHERE raw_payload <> '' AND projection_revision IS NOT NULL \
       AND source <> 'acp' AND observed_at < ? \
     ORDER BY observed_at ASC LIMIT ?";

/// The statement [`FleetProviderEventRepo::last_by_session_event_type`] runs, as
/// ONE definition, shared with the plan test for the same reason
/// [`EVICTION_SELECT`] is. Binds `session_key` then `event_type`.
///
/// `ORDER BY ingest_order DESC` must stay paired with
/// `idx_fleet_provider_event_session_order`'s second key column: that is what
/// makes `LIMIT 1` a backwards seek along the index rather than a sort of every
/// row of the type. `event_type` is deliberately NOT in the index: it filters
/// rows the walk visits, and the walk stops at the first match.
const LAST_OF_TYPE_SELECT: &str = "SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
     FROM fleet_provider_event WHERE session_key = ? AND event_type = ? \
     ORDER BY ingest_order DESC LIMIT 1";

fn matches_event(
    row: &FleetProviderEventRow,
    event: &NewFleetProviderEvent,
    raw_blake3: &str,
) -> bool {
    row.provider == event.provider
        && row.source == event.source
        && row.session_key == event.session_key
        && row.provider_session_id == event.provider_session_id
        && row.observed_at == event.observed_at
        && row.event_type == event.event_type
        // An EVICTED row has no payload to compare, so comparing one would call
        // every replay of it a collision. `raw_blake3` survives eviction and is
        // the stronger check anyway -- it is the digest OF that payload, so
        // equal digests mean equal bodies without needing the body. Skipping
        // the byte comparison for an empty stored payload therefore loses no
        // identity, and keeping it would break the caller below.
        //
        // Not cosmetic: `attention_ingest` replays `events.jsonl` from the
        // cursor, and its cursor resets to 0 whenever the file is missing,
        // corrupt or truncated (`read_cursor`; `write_cursor` is best-effort).
        // A replayed line older than the retention TTL would rebuild the
        // original payload, mismatch the blanked row, and come back as
        // `EventIdCollision` -- which `process_line` maps to `LineOutcome::Retry`
        // and `ingest_once` turns into a `break` that never advances
        // `committed_end`. The pipeline would then stall on that one line
        // forever, every pass, behind a single `warn!`.
        && (row.raw_payload.is_empty() || row.raw_payload == event.raw_payload)
        && row.raw_blake3 == raw_blake3
}

fn row_from(row: &sqlx::sqlite::SqliteRow) -> Result<FleetProviderEventRow, sqlx::Error> {
    Ok(FleetProviderEventRow {
        ingest_order: row.try_get("ingest_order")?,
        event_id: row.try_get("event_id")?,
        provider: row.try_get("provider")?,
        source: row.try_get("source")?,
        session_key: row.try_get("session_key")?,
        provider_session_id: row.try_get("provider_session_id")?,
        observed_at: row.try_get("observed_at")?,
        received_at: row.try_get("received_at")?,
        event_type: row.try_get("event_type")?,
        raw_payload: row.try_get("raw_payload")?,
        raw_blake3: row.try_get("raw_blake3")?,
        projection_revision: row.try_get("projection_revision")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    fn event(id: &str, payload: &str) -> NewFleetProviderEvent {
        NewFleetProviderEvent {
            event_id: id.to_string(),
            provider: "claude".to_string(),
            source: "claude_hook".to_string(),
            session_key: Some("claude:session-1".to_string()),
            provider_session_id: Some("session-1".to_string()),
            observed_at: 100,
            received_at: 101,
            event_type: "PostToolUse".to_string(),
            raw_payload: payload.to_string(),
        }
    }

    #[tokio::test]
    async fn source_event_replay_preserves_exact_raw_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let input = event("source-1", r#"{\"large\":\"payload\"}"#);
        let first = FleetProviderEventRepo::append(store.pool(), &input).await.unwrap();
        let replay = FleetProviderEventRepo::append(store.pool(), &input).await.unwrap();

        assert_eq!(first, replay);
        assert_eq!(first.raw_payload, input.raw_payload);
        assert_eq!(first.ingest_order, 1);
    }

    #[tokio::test]
    async fn append_batch_commits_once_and_reports_the_high_water_mark() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let batch: Vec<NewFleetProviderEvent> =
            (0..5).map(|index| event(&format!("acp-{index}"), "{}")).collect();

        let high_water = FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();

        assert_eq!(high_water, Some(5));
        let rows =
            FleetProviderEventRepo::list_by_session_after(store.pool(), "claude:session-1", 0, 100)
                .await
                .unwrap();
        assert_eq!(rows.len(), 5);
        assert!(rows.iter().all(|row| row.raw_blake3.len() == 64));
    }

    #[tokio::test]
    async fn append_batch_replay_is_a_no_op_that_still_reports_the_original_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let batch = vec![event("acp-1", "first")];

        let first = FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();
        // The `DO UPDATE` is a no-op assignment, present only so RETURNING
        // fires on the replay leg.
        let replay = FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();

        assert_eq!(first, replay);
        let stored = FleetProviderEventRepo::get(store.pool(), "acp-1").await.unwrap().unwrap();
        assert_eq!(stored.raw_payload, "first");
    }

    /// Both doors enforce one contract: the batch leg rejects a reused
    /// `event_id` carrying a different envelope exactly as
    /// `source_event_id_rejects_different_raw_payload` pins for `append`,
    /// and the whole batch rolls back with it.
    #[tokio::test]
    async fn append_batch_rejects_a_reused_event_id_with_a_different_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        FleetProviderEventRepo::append_batch(store.pool(), &[event("acp-1", "first")])
            .await
            .unwrap();

        let collision = FleetProviderEventRepo::append_batch(
            store.pool(),
            &[event("acp-2", "fresh"), event("acp-1", "second")],
        )
        .await;

        assert!(matches!(
            collision,
            Err(FleetProviderEventError::EventIdCollision { .. })
        ));
        let stored = FleetProviderEventRepo::get(store.pool(), "acp-1").await.unwrap().unwrap();
        assert_eq!(stored.raw_payload, "first", "the stored payload is intact");
        assert!(
            FleetProviderEventRepo::get(store.pool(), "acp-2").await.unwrap().is_none(),
            "the batch's other row rolled back with the collision"
        );
    }

    /// The newest row OF ITS TYPE, however deeply the run buried it.
    ///
    /// Both halves matter and neither is hypothetical. The last `acp.usage` row
    /// is not the last row (an agent reports usage and then keeps talking), and
    /// the tail read beside it is windowed, so the assertion that the window
    /// MISSES this row is what says why the query exists at all rather than
    /// being a filter over `list_by_session_tail`.
    #[tokio::test]
    async fn the_newest_row_of_a_type_is_found_under_a_tail_of_other_types() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let typed = |id: &str, event_type: &str, payload: &str| NewFleetProviderEvent {
            event_type: event_type.to_string(),
            ..event(id, payload)
        };

        let mut batch = vec![
            typed("u-0", "acp.usage", r#"{"used":1}"#),
            typed("u-1", "acp.usage", r#"{"used":2}"#),
        ];
        // The chatter after the last report. 200 rows is well past the 64-row
        // window the PR capture scans with.
        batch.extend((0..200).map(|i| typed(&format!("m-{i}"), "acp.message", r#"{"text":"hi"}"#)));
        FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();

        let found = FleetProviderEventRepo::last_by_session_event_type(
            store.pool(),
            "claude:session-1",
            "acp.usage",
        )
        .await
        .unwrap()
        .expect("the session's usage row");
        assert_eq!(
            found.raw_payload, r#"{"used":2}"#,
            "the LAST usage row wins, not the first"
        );

        let (window, _truncated) = FleetProviderEventRepo::list_by_session_tail(
            store.pool(),
            "claude:session-1",
            64,
            64 * 1024,
        )
        .await
        .unwrap();
        // The `all` below is TRUE of an empty window, and it carries the whole
        // justification for the query above, so the window has to be shown full
        // first: 64 rows read, none of them the accounting row.
        assert_eq!(
            window.len(),
            64,
            "the tail must be a FULL window for the miss below to mean anything"
        );
        assert!(
            window.iter().all(|row| row.event_type != "acp.usage"),
            "a windowed tail read cannot see this row, which is why the query above exists"
        );

        assert_eq!(
            FleetProviderEventRepo::last_by_session_event_type(
                store.pool(),
                "claude:session-1",
                "acp.plan",
            )
            .await
            .unwrap(),
            None,
            "a type the session never wrote is absent, not an error"
        );
    }

    /// The plan for the exact statement `last_by_session_event_type` runs, with
    /// only its ORDER BY varied. Derived from the production constant, never
    /// re-typed, so the test and the query cannot drift.
    async fn last_of_type_plan(pool: &SqlitePool, order_by: &str) -> String {
        let select = super::LAST_OF_TYPE_SELECT.replace(
            "ORDER BY ingest_order DESC",
            &format!("ORDER BY {order_by} DESC"),
        );
        assert!(
            select.contains(&format!("ORDER BY {order_by} DESC")),
            "LAST_OF_TYPE_SELECT no longer contains the ORDER BY this test rewrites"
        );
        let rows = sqlx::query(&format!("EXPLAIN QUERY PLAN {select}"))
            .bind("acp:s-1")
            .bind("acp.usage")
            .fetch_all(pool)
            .await
            .unwrap();
        rows.iter()
            .map(|row| row.try_get::<String, _>("detail").unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The usage read must SEEK `idx_fleet_provider_event_session_order` and
    /// take its ordering from the index, never sort.
    ///
    /// Asserted on the PLAN for the same reason
    /// `the_sweep_seeks_on_the_retention_index` is: the row returned is correct
    /// either way and only the cost differs, so a degradation here has no
    /// symptom except a finalize that got slower on exactly the longest
    /// transcripts.
    #[tokio::test]
    async fn the_event_type_read_seeks_on_the_session_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        // TWO sessions, because one is not a table: with every row sharing a
        // session_key, ANALYZE would record the leading column as useless and a
        // planner could reasonably scan, which would make this test agree with
        // production only by accident.
        for session in ["acp:s-1", "acp:s-2"] {
            let batch = (0..200)
                .map(|i| acp_event(&format!("{session}-{i}"), session))
                .collect::<Vec<_>>();
            FleetProviderEventRepo::append_batch(store.pool(), &batch).await.unwrap();
        }
        sqlx::query("ANALYZE").execute(store.pool()).await.unwrap();

        let detail = last_of_type_plan(store.pool(), "ingest_order").await;
        // SEARCH, the index NAME, and no sort. All three: a full index scan
        // (`SCAN ... USING INDEX ...`) also contains the name, and a plan that
        // seeks the session and then sorts its whole transcript to find the
        // newest row is the cost this query exists to avoid.
        assert!(
            detail.contains("SEARCH")
                && detail.contains("idx_fleet_provider_event_session_order")
                && !detail.contains("TEMP B-TREE"),
            "the usage read must seek the session index and take its order from \
             it, plan was:\n{detail}"
        );
        let by_observed_at = last_of_type_plan(store.pool(), "observed_at").await;
        assert!(
            by_observed_at.contains("TEMP B-TREE"),
            "ORDER BY and the index's second key column must stay paired; if \
             ordering by observed_at is also free, the pairing comment on \
             LAST_OF_TYPE_SELECT is now misleading, plan was:\n{by_observed_at}"
        );
    }

    #[tokio::test]
    async fn append_batch_of_nothing_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert_eq!(
            FleetProviderEventRepo::append_batch(store.pool(), &[]).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn source_event_replay_ignores_local_receipt_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let first = FleetProviderEventRepo::append(store.pool(), &event("source-1", "{}"))
            .await
            .unwrap();
        let mut replay = event("source-1", "{}");
        replay.received_at = 9_999;

        assert_eq!(
            FleetProviderEventRepo::append(store.pool(), &replay).await.unwrap(),
            first,
        );
    }

    #[tokio::test]
    async fn source_event_id_rejects_different_raw_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        FleetProviderEventRepo::append(store.pool(), &event("source-1", "first"))
            .await
            .unwrap();

        assert!(matches!(
            FleetProviderEventRepo::append(store.pool(), &event("source-1", "second")).await,
            Err(FleetProviderEventError::EventIdCollision { .. })
        ));
    }

    #[tokio::test]
    async fn unknown_projection_event_is_not_a_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        assert!(matches!(
            FleetProviderEventRepo::mark_projected(store.pool(), "missing", 1).await,
            Err(FleetProviderEventError::EventNotFound { .. })
        ));
    }

    fn acp_event(id: &str, session_key: &str) -> NewFleetProviderEvent {
        NewFleetProviderEvent {
            event_id: id.to_string(),
            provider: "claude-agent-acp".to_string(),
            source: "acp".to_string(),
            session_key: Some(session_key.to_string()),
            provider_session_id: Some("adapter-1".to_string()),
            observed_at: 100,
            received_at: 101,
            event_type: "acp.message".to_string(),
            raw_payload: format!("{{\"chunk\":\"{id}\"}}"),
        }
    }

    /// Seed both ACP transcript rows and the classic projection-source rows,
    /// so the plans and deletions below are asserted against mixed content.
    async fn seed_mixed(store: &Store) {
        for i in 0..5 {
            FleetProviderEventRepo::append(
                store.pool(),
                &acp_event(&format!("acp-{i}"), "acp:s-1"),
            )
            .await
            .unwrap();
        }
        FleetProviderEventRepo::append(store.pool(), &event("codex-1", "{}"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_appended_row_carries_a_valid_raw_blake3_digest() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let row = FleetProviderEventRepo::append(store.pool(), &acp_event("acp-1", "acp:s-1"))
            .await
            .unwrap();
        assert_eq!(row.raw_blake3.len(), 64);
        assert_eq!(row.raw_blake3, digest(&row.raw_payload));
    }

    /// Replaying an EVICTED event is a no-op, not a collision.
    ///
    /// The regression this guards is a permanent pipeline stall, not a lost
    /// row. `attention_ingest` replays `events.jsonl` from a cursor that resets
    /// to 0 whenever the file is missing, corrupt or truncated. A replayed line
    /// older than the retention TTL rebuilds the original payload; if that were
    /// compared against the blanked row it would raise `EventIdCollision`,
    /// which `process_line` maps to `LineOutcome::Retry` and `ingest_once`
    /// turns into a `break` that never advances the cursor. Hook ingest would
    /// then stall on that one line forever, behind a single `warn!`.
    #[tokio::test]
    async fn replaying_an_evicted_event_is_absorbed_not_a_collision() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let original = seed_projected(&store, "proj-replay", 100).await;

        FleetProviderEventRepo::evict_projected_payloads_before(store.pool(), 1_000, 100)
            .await
            .unwrap();

        // Exactly what the hook replay reconstructs: same id, same body.
        let replay = NewFleetProviderEvent {
            event_id: original.event_id.clone(),
            provider: original.provider.clone(),
            source: original.source.clone(),
            session_key: original.session_key.clone(),
            provider_session_id: original.provider_session_id.clone(),
            observed_at: original.observed_at,
            received_at: original.received_at + 5_000,
            event_type: original.event_type.clone(),
            raw_payload: original.raw_payload.clone(),
        };
        let row = FleetProviderEventRepo::append(store.pool(), &replay)
            .await
            .expect("an evicted row must absorb its own replay, not collide");
        assert_eq!(row.ingest_order, original.ingest_order, "no new row");
        assert_eq!(
            row.raw_payload, "",
            "the replay must not resurrect the body"
        );

        // A genuinely DIFFERENT body under the same id is still a collision:
        // relaxing the payload check must not relax identity.
        let conflicting = NewFleetProviderEvent {
            raw_payload: "{\"different\":true}".to_string(),
            ..replay
        };
        assert!(
            FleetProviderEventRepo::append(store.pool(), &conflicting).await.is_err(),
            "a different payload under the same event_id must still collide"
        );
    }

    /// The digest is the identity that OUTLIVES the payload.
    ///
    /// Renamed from "every row": since 0094 that is no longer a whole-table
    /// invariant, and a test asserting it under the old name would have gone on
    /// passing purely because it seeds one fresh row. This pins what actually
    /// holds after eviction, which is what consumers now depend on.
    #[tokio::test]
    async fn eviction_keeps_the_digest_of_the_original_payload() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let original = seed_projected(&store, "proj-digest", 100).await;
        let original_digest = original.raw_blake3.clone();
        assert_eq!(original_digest, digest(&original.raw_payload));

        FleetProviderEventRepo::evict_projected_payloads_before(store.pool(), 1_000, 100)
            .await
            .unwrap();

        let evicted = FleetProviderEventRepo::get(store.pool(), "proj-digest")
            .await
            .unwrap()
            .expect("eviction never deletes the row");
        assert_eq!(evicted.raw_payload, "", "payload is evicted");
        assert_eq!(
            evicted.raw_blake3, original_digest,
            "the digest must still describe the ORIGINAL body, not the blank"
        );
        assert_ne!(
            evicted.raw_blake3,
            digest(&evicted.raw_payload),
            "digest of the blank would make the row unidentifiable on replay"
        );
    }

    /// I10: the REAL consumer query still plans onto the recreated partial
    /// index with ACP rows present. Asserting the index merely EXISTS is not
    /// enough; only the plan proves the recovery scan did not degrade to a
    /// full table scan over the table ACP transcripts inflate.
    #[tokio::test]
    async fn recovery_scan_plans_onto_the_projection_index_with_acp_rows_present() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_mixed(&store).await;

        // The exact SQL `unprojected` runs, prefixed with EXPLAIN QUERY PLAN.
        let rows = sqlx::query(
            "EXPLAIN QUERY PLAN \
             SELECT ingest_order, event_id, provider, source, session_key, provider_session_id, observed_at, received_at, event_type, raw_payload, raw_blake3, projection_revision \
             FROM fleet_provider_event WHERE provider = ? AND source = ? AND projection_revision IS NULL \
             ORDER BY ingest_order ASC LIMIT ?",
        )
        .bind("codex")
        .bind("codex_app_server")
        .bind(10_i64)
        .fetch_all(store.pool())
        .await
        .unwrap();
        let detail = rows
            .iter()
            .map(|row| row.try_get::<String, _>("detail").unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            detail.contains("idx_fleet_provider_event_projection"),
            "the consumer query must use the partial index, plan was:\n{detail}"
        );
    }

    #[tokio::test]
    async fn list_by_session_after_pages_one_session_by_ingest_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_mixed(&store).await;
        FleetProviderEventRepo::append(store.pool(), &acp_event("acp-other", "acp:s-2"))
            .await
            .unwrap();

        let page = FleetProviderEventRepo::list_by_session_after(store.pool(), "acp:s-1", 2, 2)
            .await
            .unwrap();
        assert_eq!(
            page.iter().map(|r| r.event_id.as_str()).collect::<Vec<_>>(),
            vec!["acp-2", "acp-3"],
            "cursor-exclusive, limit-bounded, single-session"
        );
    }

    /// The export-then-delete leg removes ONLY `source='acp'` rows below the
    /// watermark; every row carrying the pending-recovery contract survives.
    #[tokio::test]
    async fn delete_acp_before_removes_only_acp_rows_below_the_watermark() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_mixed(&store).await;
        let watermark = FleetProviderEventRepo::get(store.pool(), "acp-3")
            .await
            .unwrap()
            .unwrap()
            .ingest_order;

        let deleted = FleetProviderEventRepo::delete_acp_before(store.pool(), "acp:s-1", watermark)
            .await
            .unwrap();
        assert_eq!(deleted, 3, "acp-0..2 fall below the watermark");
        assert!(FleetProviderEventRepo::get(store.pool(), "acp-0").await.unwrap().is_none());
        assert!(FleetProviderEventRepo::get(store.pool(), "acp-3").await.unwrap().is_some());
        assert!(FleetProviderEventRepo::get(store.pool(), "acp-4").await.unwrap().is_some());
    }

    /// The data-loss case the export watermark exists for: a turn commits
    /// BELOW the operator's `--before` while the export is still being written.
    /// Deleting on the operator's watermark would destroy that row unexported,
    /// and after the delete the export is the only copy.
    #[tokio::test]
    async fn delete_acp_before_spares_a_row_committed_after_the_export_page() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_mixed(&store).await;
        // What the export actually captured, asked for with a watermark far
        // above the log (exactly what `--before 1000000` does).
        let exported =
            FleetProviderEventRepo::list_acp_before(store.pool(), "acp:s-1", 0, 1_000_000, 512)
                .await
                .unwrap();
        assert_eq!(exported.len(), 5, "acp-0..4 were exported");

        // A live turn commits while the operator's file is being fsynced.
        let late = FleetProviderEventRepo::append(store.pool(), &acp_event("acp-late", "acp:s-1"))
            .await
            .unwrap();
        assert!(
            late.ingest_order < 1_000_000,
            "the late row is inside the range the operator asked for"
        );

        let cut = exported.last().unwrap().ingest_order + 1;
        let deleted = FleetProviderEventRepo::delete_acp_before(store.pool(), "acp:s-1", cut)
            .await
            .unwrap();
        assert_eq!(
            deleted as usize,
            exported.len(),
            "deleted can never exceed exported"
        );
        assert!(
            FleetProviderEventRepo::get(store.pool(), "acp-late").await.unwrap().is_some(),
            "a row committed after the export page survives the prune"
        );
    }

    /// Refusal rule one: a non-ACP row is never touched, even when the caller
    /// hands a range that covers it.
    #[tokio::test]
    async fn delete_acp_before_refuses_a_non_acp_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let codex = FleetProviderEventRepo::append(store.pool(), &event("codex-1", "{}"))
            .await
            .unwrap();

        let deleted = FleetProviderEventRepo::delete_acp_before(
            store.pool(),
            codex.session_key.as_deref().unwrap(),
            codex.ingest_order + 1,
        )
        .await
        .unwrap();
        assert_eq!(deleted, 0);
        assert!(
            FleetProviderEventRepo::get(store.pool(), "codex-1").await.unwrap().is_some(),
            "a non-acp row survives an in-range delete"
        );
    }

    /// Refusal rule two: a row marking pending recovery work
    /// (`projection_revision IS NULL` on a projection source) is never
    /// touched. On ACP rows NULL is the steady state, not pending work, so the
    /// guard is the `source` filter itself; this pins the recovery-contract
    /// half of it.
    #[tokio::test]
    async fn delete_acp_before_refuses_a_pending_recovery_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pending = FleetProviderEventRepo::append(store.pool(), &event("codex-pending", "{}"))
            .await
            .unwrap();
        assert_eq!(pending.projection_revision, None, "seeded as pending work");

        let deleted = FleetProviderEventRepo::delete_acp_before(
            store.pool(),
            pending.session_key.as_deref().unwrap(),
            i64::MAX,
        )
        .await
        .unwrap();
        assert_eq!(deleted, 0);
        assert!(
            FleetProviderEventRepo::get(store.pool(), "codex-pending")
                .await
                .unwrap()
                .is_some(),
            "pending recovery work survives any delete range"
        );
    }

    /// A projection-source envelope observed at `observed_at`, carrying a body
    /// big enough that losing it is the point.
    fn aged_event(id: &str, observed_at: i64) -> NewFleetProviderEvent {
        NewFleetProviderEvent {
            observed_at,
            received_at: observed_at,
            ..event(id, r#"{"envelope":"verbatim provider bytes"}"#)
        }
    }

    /// Mint one real `fleet_event` revision. `projection_revision` is a foreign
    /// key and `PRAGMA foreign_keys` is ON, so a projected row needs a target
    /// that actually exists.
    async fn seed_revision(pool: &SqlitePool, event_id: &str) -> i64 {
        sqlx::query(
            "INSERT OR IGNORE INTO fleet_session \
             (session_key, cwd, discovered_at, last_observed_at) \
             VALUES ('claude:session-1', '/tmp', 0, 0)",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO fleet_event \
             (event_id, session_key, observed_at, authority, event_type, payload, \
              session_version, applied) \
             VALUES (?, 'claude:session-1', 0, 'authoritative', 'PostToolUse', '{}', 1, 1)",
        )
        .bind(event_id)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    /// Append one projection-source envelope and mark it reduced, which is the
    /// only state the automatic sweep may touch.
    async fn seed_projected(store: &Store, id: &str, observed_at: i64) -> FleetProviderEventRow {
        FleetProviderEventRepo::append(store.pool(), &aged_event(id, observed_at))
            .await
            .unwrap();
        let revision = seed_revision(store.pool(), &format!("fe-{id}")).await;
        FleetProviderEventRepo::mark_projected(store.pool(), id, revision)
            .await
            .unwrap();
        FleetProviderEventRepo::get(store.pool(), id).await.unwrap().unwrap()
    }

    /// An envelope a reducer has already consumed loses its bytes at the
    /// cutoff, and loses NOTHING else: the row, its ingest order, its content
    /// digest and its projection link all read back untouched. That is what
    /// makes this eviction rather than a delete.
    #[tokio::test]
    async fn eviction_blanks_an_aged_projected_envelope_and_keeps_its_identity() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let before = seed_projected(&store, "codex-aged", 100).await;

        let evicted =
            FleetProviderEventRepo::evict_projected_payloads_before(store.pool(), 1_000, 100)
                .await
                .unwrap();

        assert_eq!(evicted, 1);
        let after = FleetProviderEventRepo::get(store.pool(), "codex-aged").await.unwrap().unwrap();
        assert_eq!(after.raw_payload, "", "the bytes are gone");
        assert_eq!(
            (
                after.ingest_order,
                after.raw_blake3,
                after.projection_revision
            ),
            (
                before.ingest_order,
                before.raw_blake3,
                before.projection_revision
            ),
            "identity, digest and projection link survive eviction"
        );
    }

    /// THE regression that matters: `projection_revision IS NULL` is pending
    /// recovery work the Codex manager replays at startup, and its payload is
    /// the replay input. No cutoff, however old, may reach it.
    #[tokio::test]
    async fn eviction_never_touches_an_unprojected_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pending =
            FleetProviderEventRepo::append(store.pool(), &aged_event("codex-pending", 100))
                .await
                .unwrap();
        assert_eq!(pending.projection_revision, None, "seeded as pending work");

        let evicted =
            FleetProviderEventRepo::evict_projected_payloads_before(store.pool(), i64::MAX, 100)
                .await
                .unwrap();

        assert_eq!(evicted, 0);
        assert_eq!(
            FleetProviderEventRepo::get(store.pool(), "codex-pending")
                .await
                .unwrap()
                .unwrap()
                .raw_payload,
            pending.raw_payload,
            "pending recovery work keeps its payload at any cutoff"
        );
    }

    /// ACP transcripts reclaim through the operator's export-then-delete, which
    /// exports before it destroys. The timer-driven sweep must not race it.
    #[tokio::test]
    async fn eviction_never_touches_an_acp_transcript_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let acp = FleetProviderEventRepo::append(store.pool(), &acp_event("acp-1", "acp:s-1"))
            .await
            .unwrap();
        let revision = seed_revision(store.pool(), "fe-acp-1").await;
        // Even the state that WOULD make a projection-source row evictable.
        FleetProviderEventRepo::mark_projected(store.pool(), "acp-1", revision)
            .await
            .unwrap();

        let evicted =
            FleetProviderEventRepo::evict_projected_payloads_before(store.pool(), i64::MAX, 100)
                .await
                .unwrap();

        assert_eq!(evicted, 0);
        assert_eq!(
            FleetProviderEventRepo::get(store.pool(), "acp-1")
                .await
                .unwrap()
                .unwrap()
                .raw_payload,
            acp.raw_payload,
            "an ACP transcript row is never swept automatically"
        );
    }

    /// Inside the window nothing is touched, so a reducer upgrade still has the
    /// recent history to replay from.
    #[tokio::test]
    async fn eviction_spares_an_envelope_inside_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let fresh = seed_projected(&store, "codex-fresh", 5_000).await;

        let evicted =
            FleetProviderEventRepo::evict_projected_payloads_before(store.pool(), 1_000, 100)
                .await
                .unwrap();

        assert_eq!(evicted, 0);
        assert_eq!(
            FleetProviderEventRepo::get(store.pool(), "codex-fresh")
                .await
                .unwrap()
                .unwrap()
                .raw_payload,
            fresh.raw_payload
        );
    }

    /// Convergence with NO tombstone column: a blanked row stops matching
    /// `raw_payload <> ''`, so batched calls walk FORWARD instead of
    /// re-selecting what they just wrote, and a settled ledger costs nothing.
    #[tokio::test]
    async fn eviction_converges_on_the_blanked_payload_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        for index in 0..3 {
            seed_projected(&store, &format!("proj-{index}"), 100 + index).await;
        }

        // One row per call: without forward progress the LIMIT would keep
        // handing back the same already-evicted row forever.
        let mut per_call = Vec::new();
        for _ in 0..4 {
            per_call.push(
                FleetProviderEventRepo::evict_projected_payloads_before(store.pool(), 1_000, 1)
                    .await
                    .unwrap(),
            );
        }

        assert_eq!(per_call, vec![1, 1, 1, 0], "three rows, then converged");
        for index in 0..3 {
            assert_eq!(
                FleetProviderEventRepo::get(store.pool(), &format!("proj-{index}"))
                    .await
                    .unwrap()
                    .unwrap()
                    .raw_payload,
                "",
                "every row was reached, not just the first"
            );
        }
    }

    /// The sweep must SEEK on `idx_fleet_provider_event_retention`, never scan.
    ///
    /// Asserted on the PLAN because there is no other symptom: the results are
    /// identical either way and only the cost differs. Measured on this fixture,
    /// re-ordering the same query by `ingest_order` instead of `observed_at`
    /// turns the seek into a bare `SCAN` — the trap 0081 documents for
    /// `fleet_event`, which is why the second assertion pins the failure mode
    /// rather than trusting the first to catch it.
    /// The plan for the exact subquery `evict_projected_payloads_before` runs,
    /// with only its ORDER BY varied.
    async fn retention_plan(pool: &SqlitePool, order_by: &str) -> String {
        // Derived from the production constant, never re-typed: the negative
        // case varies ONLY the ORDER BY, so a drift in the predicate cannot
        // leave this test green while the sweep starts scanning.
        let select = super::EVICTION_SELECT.replace(
            "ORDER BY observed_at ASC",
            &format!("ORDER BY {order_by} ASC"),
        );
        assert!(
            select.contains(&format!("ORDER BY {order_by} ASC")),
            "EVICTION_SELECT no longer contains the ORDER BY this test rewrites"
        );
        let rows = sqlx::query(&format!("EXPLAIN QUERY PLAN {select}"))
            .bind(1_000_i64)
            .bind(500_i64)
            .fetch_all(pool)
            .await
            .unwrap();
        rows.iter()
            .map(|row| row.try_get::<String, _>("detail").unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn the_sweep_seeks_on_the_retention_index() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        for index in 0..64 {
            seed_projected(&store, &format!("proj-{index}"), 100 + index).await;
        }
        // ACP transcript rows present, because they are what inflates the table
        // the sweep must not degrade to scanning.
        seed_mixed(&store).await;
        sqlx::query("ANALYZE").execute(store.pool()).await.unwrap();

        let detail = retention_plan(store.pool(), "observed_at").await;
        // SEARCH and the range term, not merely the index NAME: a full index
        // scan (`SCAN ... USING INDEX idx_fleet_provider_event_retention`) also
        // contains the name, and is exactly the degradation this test exists to
        // catch.
        assert!(
            detail.contains("SEARCH")
                && detail.contains("idx_fleet_provider_event_retention")
                && detail.contains("observed_at<?"),
            "the retention sweep must SEEK its partial index on the \
             observed_at range, not scan it, plan was:\n{detail}"
        );
        let by_ingest_order = retention_plan(store.pool(), "ingest_order").await;
        assert!(
            !by_ingest_order.contains("idx_fleet_provider_event_retention"),
            "ORDER BY and the index key must stay paired; if ordering by \
             ingest_order also uses the index, the pairing comment in \
             evict_projected_payloads_before is now misleading, plan was:\n{by_ingest_order}"
        );
    }
}
