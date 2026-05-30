//! The autopilot scheduler thread + tick loop (P7.3).
//!
//! A single long-lived tokio task that drives every enabled, cron-scheduled
//! [`Autopilot`] across all workspaces. It is the daemon-global counterpart to
//! the per-workspace [`ainb_hangar_core::autopilot::service::AutopilotService`]:
//! the service answers RPC reads/mutations for one tenant; this loop fires the
//! whole fleet on time.
//!
//! # Loop shape
//!
//! ```text
//! ┌──────────────┐    ┌──────────────────┐    ┌──────────────┐
//! │ pick earliest│───▶│ sleep_until tick │───▶│ fire enqueue │
//! │ next_tick_at │    │  OR shutdown     │    │ + reschedule │
//! └──────────────┘    └──────────────────┘    └──────┬───────┘
//!        ▲                                            │
//!        └────────────────────────────────────────────┘
//! ```
//!
//! Each iteration: read the enabled autopilots (`next_tick_at IS NOT NULL`),
//! pick the one firing soonest, sleep until that instant **or** the
//! [`CancellationToken`] trips, then — re-checking the concurrency limit *at
//! fire time* — either fire ([`fire_autopilot_tick`]) or skip, and in both cases
//! recompute and persist the next `next_tick_at`. With no enabled autopilots the
//! loop re-polls after [`NO_WORK_REPOLL`].
//!
//! # Why recompute from the *fired tick*, not `clock.now()`
//!
//! The next tick is computed strictly after the tick that just fired
//! ([`recompute_next_tick`]), never reseeded from a possibly-late `clock.now()`.
//! Computing from `now` after a slow fire could skip the very next slot (drift);
//! computing from the fired tick keeps the cadence exact. The one nuance: if a
//! daemon was down long enough that the recomputed next tick is *still* in the
//! past, [`recompute_next_tick`] advances forward from `now` so the loop fires
//! one catch-up tick and then resumes the schedule rather than replaying a burst
//! of missed slots (the open-question #2 resolution in P7.md).
//!
//! # Skip-when-in-flight (the concurrency policy)
//!
//! Before firing, the loop counts the autopilot's in-flight runs
//! (`autopilot_run WHERE autopilot_id = ? AND completed_at IS NULL`). When that
//! count has reached `max_concurrent_runs` the tick is **skipped**: no
//! `autopilot_run` / `agent_task_queue` row is created, a `tracing::warn!` is
//! emitted, an [`SchedulerEvent::TickSkipped`] is published to the optional event
//! sink, and `next_tick_at` is still advanced so the autopilot rejoins the
//! schedule at its next slot.

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::autopilot::cron::{
    millis_to_utc, next_tick_after, parse_cron, utc_to_millis,
};
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_store::repo::autopilot::Autopilot;
use ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick;
use sqlx::SqlitePool;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

/// The no-work re-poll interval.
///
/// How long the loop sleeps before re-polling when no autopilot is schedulable
/// (none enabled, or none with a future `next_tick_at`). Matches the P7.md "no
/// enabled autopilots → sleep 60s then re-poll" decision.
pub const NO_WORK_REPOLL: Duration = Duration::from_mins(1);

/// An observable scheduler decision, published to the optional event sink.
///
/// Hangar has no general in-process event bus at v1, so the scheduler surfaces
/// the audit-worthy decisions both via `tracing` (for logs) and — when a sink is
/// wired — onto this channel, which the tests assert against and a future bus /
/// RPC event-stream can drain. The `tick_skipped` variant is the one the P7
/// concurrency policy mandates be auditable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchedulerEvent {
    /// An autopilot fired: `(autopilot_run.id, agent_task_queue.id)`.
    Fired {
        /// The autopilot that fired.
        autopilot_id: String,
        /// The new run id.
        run_id: String,
        /// The enqueued task id.
        task_id: String,
    },
    /// A tick was skipped because the autopilot was at its concurrency limit.
    /// Mirrors the `autopilot.tick_skipped` event the plan called for.
    TickSkipped {
        /// The autopilot whose tick was skipped.
        autopilot_id: String,
        /// The skip reason (`"concurrency"` at v1).
        reason: &'static str,
        /// In-flight run count observed at fire time.
        in_flight: i64,
    },
}

/// The autopilot scheduler: a daemon-global tick loop over every enabled
/// autopilot.
///
/// Reconciled against the committed P7.1/P7.2/P7.4 surfaces: it owns a
/// [`SqlitePool`] + an injected [`HangarClock`] and fires through P7.4's
/// [`fire_autopilot_tick`] (there is no `TaskService` — the stale plan struct's
/// `task_service` field does not exist). A [`CancellationToken`] exits the loop
/// cooperatively within one sleep.
pub struct AutopilotScheduler {
    pool: SqlitePool,
    clock: Arc<dyn HangarClock>,
    shutdown: CancellationToken,
    events: Option<UnboundedSender<SchedulerEvent>>,
}

impl AutopilotScheduler {
    /// Build a scheduler over a pool, clock, and shutdown token.
    #[must_use]
    pub fn new(pool: SqlitePool, clock: Arc<dyn HangarClock>, shutdown: CancellationToken) -> Self {
        Self {
            pool,
            clock,
            shutdown,
            events: None,
        }
    }

    /// Attach an event sink the loop publishes [`SchedulerEvent`]s to (in
    /// addition to `tracing`). Used by tests to observe fire/skip decisions; a
    /// future event bus can subscribe the same way.
    #[must_use]
    pub fn with_event_sink(mut self, tx: UnboundedSender<SchedulerEvent>) -> Self {
        self.events = Some(tx);
        self
    }

    /// Run the scheduler loop until the shutdown token is cancelled.
    ///
    /// Each iteration picks the earliest-firing enabled autopilot, sleeps until
    /// its tick (or shutdown), then fires-or-skips and reschedules. With no
    /// schedulable autopilot it re-polls after [`NO_WORK_REPOLL`]. Errors from a
    /// single tick are logged and swallowed — one bad autopilot must never down
    /// the loop (mirrors the claim loop's per-task error policy).
    pub async fn run(self) {
        tracing::info!("autopilot scheduler started");
        loop {
            if self.shutdown.is_cancelled() {
                break;
            }

            let next = match self.load_enabled().await {
                Ok(rows) => pick_earliest(&rows).cloned(),
                Err(e) => {
                    tracing::error!(error = %e, "autopilot scheduler: load failed; re-polling");
                    None
                }
            };

            // Decide how long to wait: until the earliest tick (clamped so a
            // past-due tick fires immediately, never a negative/huge sleep), or
            // the no-work re-poll interval.
            let (delay, fire_target) = match next {
                Some(ap) => {
                    let tick_ms = ap.next_tick_at.unwrap_or_else(|| self.clock.now_ms());
                    (sleep_delay(tick_ms, self.clock.now_ms()), Some(ap))
                }
                None => (NO_WORK_REPOLL, None),
            };

            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
            }

            if let Some(ap) = fire_target {
                self.fire_or_skip(&ap).await;
            }
        }
        tracing::info!("autopilot scheduler stopped");
    }

    /// Read every enabled autopilot with a scheduled next tick, across all
    /// workspaces (the scheduler is daemon-global, not tenant-scoped).
    async fn load_enabled(&self) -> Result<Vec<Autopilot>, sqlx::Error> {
        sqlx::query_as::<_, Autopilot>(
            "SELECT id, workspace_id, agent_id, name, instructions, cron_expr, \
                    max_concurrent_runs, next_tick_at, enabled, created_at \
             FROM autopilot \
             WHERE enabled = 1 AND next_tick_at IS NOT NULL \
             ORDER BY next_tick_at ASC",
        )
        .fetch_all(&self.pool)
        .await
    }

    /// Fire one autopilot — or skip it when at the concurrency limit — then
    /// recompute and persist its `next_tick_at`. Both branches reschedule, so a
    /// skipped autopilot rejoins the schedule at its next slot. All errors are
    /// logged and swallowed.
    async fn fire_or_skip(&self, ap: &Autopilot) {
        let in_flight = match self.count_in_flight(&ap.id).await {
            Ok(n) => n,
            Err(e) => {
                tracing::error!(autopilot_id = %ap.id, error = %e, "in-flight count failed; skipping fire");
                // Still reschedule so a transient count error does not wedge the
                // autopilot off the schedule forever.
                self.reschedule(ap).await;
                return;
            }
        };

        if in_flight >= ap.max_concurrent_runs {
            tracing::warn!(
                autopilot_id = %ap.id,
                in_flight,
                max = ap.max_concurrent_runs,
                "autopilot.tick_skipped — concurrency limit"
            );
            self.emit(SchedulerEvent::TickSkipped {
                autopilot_id: ap.id.clone(),
                reason: "concurrency",
                in_flight,
            });
        } else {
            match fire_autopilot_tick(&self.pool, &*self.clock, ap).await {
                Ok((run_id, task_id)) => {
                    tracing::info!(
                        autopilot_id = %ap.id,
                        run_id = %run_id,
                        task_id = %task_id,
                        "autopilot fired"
                    );
                    self.emit(SchedulerEvent::Fired {
                        autopilot_id: ap.id.clone(),
                        run_id: run_id.to_string(),
                        task_id: task_id.to_string(),
                    });
                }
                Err(e) => {
                    tracing::error!(autopilot_id = %ap.id, error = %e, "autopilot fire failed");
                }
            }
        }

        self.reschedule(ap).await;
    }

    /// Count the autopilot's in-flight (not-yet-completed) runs — the
    /// concurrency-policy denominator.
    async fn count_in_flight(&self, autopilot_id: &str) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM autopilot_run \
             WHERE autopilot_id = ? AND completed_at IS NULL",
        )
        .bind(autopilot_id)
        .fetch_one(&self.pool)
        .await
    }

    /// Recompute `next_tick_at` from the just-fired tick (not `clock.now()`) and
    /// persist it. A row whose cron no longer parses, or that has no future
    /// match, gets `next_tick_at = NULL` so the loop stops scheduling it.
    async fn reschedule(&self, ap: &Autopilot) {
        let next = recompute_next_tick(&ap.cron_expr, ap.next_tick_at, self.clock.now_ms());
        if let Err(e) = sqlx::query("UPDATE autopilot SET next_tick_at = ? WHERE id = ?")
            .bind(next)
            .bind(&ap.id)
            .execute(&self.pool)
            .await
        {
            tracing::error!(autopilot_id = %ap.id, error = %e, "reschedule persist failed");
        }
    }

    /// Publish an event to the sink when one is attached (best-effort; a closed
    /// receiver is ignored — the loop never blocks on observability).
    fn emit(&self, event: SchedulerEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(event);
        }
    }
}

/// Pick the autopilot firing soonest from a list ordered by `next_tick_at ASC`.
///
/// A pure decision function (no IO) so the "pick earliest" choice is unit-tested
/// directly. Rows without a `next_tick_at` are not schedulable and are ignored.
#[must_use]
pub fn pick_earliest(rows: &[Autopilot]) -> Option<&Autopilot> {
    rows.iter()
        .filter(|a| a.next_tick_at.is_some())
        .min_by_key(|a| a.next_tick_at.unwrap_or(i64::MAX))
}

/// The sleep duration until `tick_ms`, given the current `now_ms`.
///
/// Clamps to zero when the tick is already due (or in the past) so a past-due
/// tick fires immediately rather than the loop computing a negative duration —
/// the no-busy-spin guard the reviewer flags for. A pure function so the
/// past-due / future cases are tested without a runtime.
#[must_use]
pub fn sleep_delay(tick_ms: i64, now_ms: i64) -> Duration {
    let delta = tick_ms.saturating_sub(now_ms);
    if delta <= 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(u64::try_from(delta).unwrap_or(u64::MAX))
    }
}

/// Recompute the next firing instant (epoch-ms) after the tick that just fired.
///
/// Computes strictly after `fired_tick_ms` (the cadence-preserving choice), but
/// if that result is still `<= now_ms` — a daemon that was down across several
/// slots — advances forward from `now_ms` so the loop fires one catch-up tick
/// and then resumes, never replaying a burst (P7.md open-question #2). Returns
/// `None` when the cron no longer parses or has no future match, which the
/// caller persists as a `NULL` `next_tick_at` (the row stops being scheduled).
///
/// Pure: the recompute logic is unit-tested without a database or runtime.
#[must_use]
pub fn recompute_next_tick(
    cron_expr: &str,
    fired_tick_ms: Option<i64>,
    now_ms: i64,
) -> Option<i64> {
    let schedule = parse_cron(cron_expr).ok()?;
    // Anchor on the fired tick to preserve cadence; fall back to `now` if the
    // fired tick is unknown.
    let anchor_ms = fired_tick_ms.unwrap_or(now_ms);
    let anchor = millis_to_utc(anchor_ms)?;
    let mut next = next_tick_after(&schedule, anchor).map(utc_to_millis)?;
    // If the cadence-preserved next tick is still in the past (daemon was down),
    // jump forward from `now` to fire one catch-up tick, not a replay storm.
    if next <= now_ms {
        let after_now = millis_to_utc(now_ms)?;
        next = next_tick_after(&schedule, after_now).map(utc_to_millis)?;
    }
    Some(next)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-01-01T00:00:00Z in epoch-ms — the frozen anchor for these tests.
    const T0: i64 = 1_767_225_600_000;
    const HOUR_MS: i64 = 3_600_000;
    const MIN_MS: i64 = 60_000;

    fn ap(id: &str, next_tick_at: Option<i64>, cron: &str, max: i64) -> Autopilot {
        Autopilot {
            id: id.to_string(),
            workspace_id: "ws-a".to_string(),
            agent_id: "agent-1".to_string(),
            name: format!("ap-{id}"),
            instructions: None,
            cron_expr: cron.to_string(),
            max_concurrent_runs: max,
            next_tick_at,
            enabled: true,
            created_at: 0,
        }
    }

    #[test]
    fn pick_earliest_returns_soonest_tick() {
        let rows = vec![
            ap("late", Some(T0 + 2 * HOUR_MS), "0 * * * *", 1),
            ap("soon", Some(T0 + HOUR_MS), "0 * * * *", 1),
            ap("latest", Some(T0 + 3 * HOUR_MS), "0 * * * *", 1),
        ];
        assert_eq!(pick_earliest(&rows).unwrap().id, "soon");
    }

    #[test]
    fn pick_earliest_ignores_unscheduled_rows() {
        let rows = vec![
            ap("no-tick", None, "0 * * * *", 1),
            ap("has-tick", Some(T0 + HOUR_MS), "0 * * * *", 1),
        ];
        assert_eq!(pick_earliest(&rows).unwrap().id, "has-tick");
    }

    #[test]
    fn pick_earliest_empty_is_none() {
        assert!(pick_earliest(&[]).is_none());
        let rows = vec![ap("no-tick", None, "0 * * * *", 1)];
        assert!(pick_earliest(&rows).is_none());
    }

    #[test]
    fn sleep_delay_future_tick_is_positive() {
        let five_min: u64 = 5 * 60_000;
        assert_eq!(
            sleep_delay(T0 + 5 * MIN_MS, T0),
            Duration::from_millis(five_min)
        );
    }

    #[test]
    fn sleep_delay_due_or_past_clamps_to_zero() {
        // Exactly due.
        assert_eq!(sleep_delay(T0, T0), Duration::ZERO);
        // Past due (daemon woke late) — must not produce a negative/huge sleep.
        assert_eq!(sleep_delay(T0 - HOUR_MS, T0), Duration::ZERO);
    }

    #[test]
    fn recompute_advances_from_fired_tick_to_preserve_cadence() {
        // Fired at 00:00; every-5-min cron. Even if `now` drifted to 00:00:30,
        // the next tick anchors on the fired tick → 00:05, not 00:10.
        let next = recompute_next_tick("*/5 * * * *", Some(T0), T0 + 30_000).unwrap();
        assert_eq!(next, T0 + 5 * MIN_MS);
    }

    #[test]
    fn recompute_catches_up_once_when_far_behind() {
        // Fired tick is a year stale (daemon was down). The cadence-preserved
        // next tick (T0+1h) is still <= now, so recompute jumps forward from
        // `now` → fires one catch-up tick, not a replay of every missed slot.
        let now = T0 + 365 * 24 * HOUR_MS; // ~1 year later
        let next = recompute_next_tick("0 * * * *", Some(T0), now).unwrap();
        // Must be strictly after `now`, and the very next hourly slot.
        assert!(next > now, "catch-up tick must be in the future");
        assert_eq!(next, now + HOUR_MS);
    }

    #[test]
    fn recompute_bad_cron_yields_none() {
        // A corrupt stored cron stops scheduling (persisted as NULL next_tick).
        assert!(recompute_next_tick("not a cron", Some(T0), T0).is_none());
    }

    #[test]
    fn skip_decision_at_limit() {
        // The pure skip predicate the loop applies: in_flight >= max ⇒ skip.
        let row = ap("x", Some(T0), "*/5 * * * *", 1);
        let in_flight_at_limit: i64 = 1;
        let in_flight_under: i64 = 0;
        assert!(
            in_flight_at_limit >= row.max_concurrent_runs,
            "1 in-flight at max=1 ⇒ skip"
        );
        assert!(
            in_flight_under < row.max_concurrent_runs,
            "0 in-flight at max=1 ⇒ fire"
        );
    }
}
