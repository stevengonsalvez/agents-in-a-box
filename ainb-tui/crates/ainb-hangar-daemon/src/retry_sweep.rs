//! The daemon-wide retry sweep: LLM-free auto-`continue` of transient API
//! errors, with no ATC instance anywhere in the picture.
//!
//! ```text
//! [session ERR] --transient pattern--> [count < cap] --> send "continue"
//!                      |                      |
//!                      |                      +- count++ in atc_retry
//!                      |
//!                      +--off ERR roster for the grace window--> [budget back]
//!
//! [count == cap] --> raise_escalation --> attention row --> ERR chip xN
//! ```
//!
//! Three things make this safe to run against the whole fleet unattended:
//!
//! - **Only transient API errors are touched.** `attention_state = 'ERROR'` is
//!   set by generic hook and wire signals, not by an API-pattern match, so the
//!   roster mixes "the provider returned 529" with "your build is broken". The
//!   sweep re-applies [`detect_error_signals`] to the session's own recent
//!   events and leaves anything it cannot recognise for a human. Typing
//!   `continue` at a real failure is how an agent gets talked into repeating a
//!   destructive step nobody watched.
//! - **The budget is durable and capped.** Every continue is stamped in the
//!   `atc_retry` ledger under this sweep's own reserved instance, and a session
//!   that reaches [`DEFAULT_ERR_RETRY_CAP`] escalates to a human exactly once
//!   instead of looping forever, through the same attention pipeline every
//!   other input request uses.
//! - **A live ATC wins.** ATC's own beat drives `continue` into erroring panes
//!   from a fleet-wide roster, so both would type into one pane. Ownership is
//!   re-read every tick rather than cached at boot, because `atc setup` /
//!   `atc disable` land between ticks.

use std::collections::HashSet;
use std::time::Duration;

use ainb_fleet_core::read::detect_error_signals;
use ainb_fleet_core::types::Signal;
use ainb_hangar_core::atc::DEFAULT_ERR_RETRY_CAP;
use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_proto::fleet::{ActionReceiptStatus, ControlAction, FleetActionParams};
use ainb_hangar_store::repo::atc_instance::{AtcInstanceRepo, AtcInstanceRow, RegisterAtc};
use ainb_hangar_store::repo::fleet::{FleetRepo, FleetSessionRow};
use sqlx::SqlitePool;

use crate::atc::{ErrAction, RETRY_RESET_GRACE_MS, err_action, raise_escalation};
use crate::events::EventSink;

/// The reserved `atc_instance` this sweep books its retry ledger against.
///
/// `atc_retry.instance_name` is a foreign key onto `atc_instance(name)`, and
/// SQLite cannot drop a constraint in place: relaxing it means rebuilding a
/// table that holds live escalation history. Registering a synthetic instance
/// instead keeps every ledger call byte-identical to the ATC path AND gives the
/// sweep its own namespace, so its counts can never collide with a real ATC's
/// counts for the same session.
pub const SWEEP_INSTANCE: &str = "__sweep__";

/// How often the sweep re-reads the ERR roster.
///
/// ATC's lite scanner ran at 5s because it was a foreground process with
/// nothing else to do, and the daemon's general sweepers run at 60s, which
/// leaves a rate-limited agent sitting idle for a full minute. 30s matches the
/// daemon's existing presence pass: fast enough that a transient 429 costs half
/// a minute, slow enough to be invisible next to the fleet reconciler.
const SWEEP_INTERVAL: Duration = Duration::from_secs(30);

/// Overrides [`SWEEP_INTERVAL`] with a millisecond value, so a test can drive
/// several ticks in about a second instead of waiting minutes for two.
const SWEEP_INTERVAL_ENV: &str = "AINB_RETRY_SWEEP_INTERVAL_MS";

/// How many of a session's newest durable events the transient-pattern gate
/// reads. The event that drove the row to ERROR is by construction one of the
/// most recent applied events; the slack absorbs the trailing chatter (a
/// `Notification`, a tmux observation) that can land behind it.
const RECENT_EVENT_SCAN: i64 = 20;

/// The only event types whose payload may decide that an ERROR is transient.
///
/// A `fleet_event` payload is the WHOLE hook envelope, and `UserPromptSubmit`,
/// `PreToolUse` and `PostToolUse` are registered hooks, so an operator's prompt,
/// a tool's arguments and a tool's OUTPUT all land in that column verbatim. The
/// transient patterns are word-boundary substrings (`ECONNRESET`,
/// `rate_limited`, `API Error`, ...), so an agent that merely reads or greps a
/// file containing one of them would otherwise arm its own auto-`continue`:
/// the file's contents come back as a `PostToolUse` payload and match.
///
/// That is not a hypothetical. `ainb-fleet-core`'s own `errors.rs` contains
/// every one of the patterns, so an agent opening the file that defines them
/// would qualify itself.
///
/// These three are the types that can carry a PROVIDER failure rather than
/// content the agent chose to print. `StopFailure` and the `*error*`/`*failed*`
/// wire methods are what set `attention_state = 'ERROR'` in the first place
/// (`fleet.rs`), so bounding the scan to them asks the events that produced the
/// state rather than whatever else the session happened to do.
const ERROR_BEARING_EVENTS: &[&str] = &["StopFailure", "Notification", "acp_error"];

/// How far before the ERROR transition the scan may look, in milliseconds.
///
/// Small on purpose. Without it, a 429 the session already recovered from stays
/// in the window and arms the NEXT, unrelated failure: the pattern search has no
/// idea the incident it matched was resolved two hours ago. One minute is wide
/// enough to catch the events that landed around the transition and narrow
/// enough that a stale incident cannot vote.
const TRANSITION_LOOKBACK_MS: i64 = 60_000;

/// The prompt the sweep types. Lower case and bare, because it is read by the
/// agent as its next user turn, not by a parser.
const CONTINUE_TEXT: &str = "continue";

/// What one pass over the ERR roster did.
///
/// Every ERR row lands in exactly one of the outcome counters, so
/// `scanned` is `transient + skipped_atc_owned + skipped_opaque` plus the
/// already-escalated rows the dedupe drops. `send_failed` is a SUBSET of
/// `continued`: the budget is stamped before the send, so a send that did not
/// land is still a spent unit (see [`sweep_once`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// ERR rows read from the roster this pass.
    pub scanned: usize,
    /// Rows whose recent output matched a transient API-error pattern.
    pub transient: usize,
    /// Rows that spent a unit of continue budget this pass.
    pub continued: usize,
    /// Rows escalated to a human because their budget was exhausted.
    pub escalated: usize,
    /// Rows left alone because a real ATC instance owns them.
    pub skipped_atc_owned: usize,
    /// Rows left alone because their error matched no transient pattern.
    pub skipped_opaque: usize,
    /// Continues whose send did not come back `DELIVERED`.
    pub send_failed: usize,
    /// Ledger rows cleared because the session recovered and stayed recovered.
    pub reset: usize,
}

impl SweepReport {
    /// Whether this pass changed anything, so a quiet fleet logs nothing.
    const fn acted(&self) -> bool {
        self.continued > 0 || self.escalated > 0 || self.reset > 0
    }
}

/// Register the reserved sweep instance, idempotently, at daemon boot.
///
/// `next_tick_at` is `None` on purpose: `AtcInstanceRepo::list_schedulable`
/// selects on `enabled = 1 AND next_tick_at IS NOT NULL`, so a NULL tick keeps
/// this row invisible to the ATC heartbeat cron forever. Its cron expression is
/// never parsed by anything for the same reason, so it carries a description
/// rather than a schedule.
///
/// `err_retry_cap` is seeded from [`DEFAULT_ERR_RETRY_CAP`] so
/// `err_action(count, inst.err_retry_cap)` decides identically for the sweep
/// and for a real ATC, with no special case on either side.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the registration write fails.
pub async fn ensure_instance(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    AtcInstanceRepo::register(
        pool,
        &RegisterAtc {
            name: SWEEP_INSTANCE.to_string(),
            cwd: String::new(),
            tmux_session: None,
            heartbeat_cron: "never (daemon retry sweep)".to_string(),
            err_retry_cap: i64::from(DEFAULT_ERR_RETRY_CAP),
            idle_pause_min: 0,
            next_tick_at: None,
        },
        SystemClock.now_ms(),
    )
    .await
}

/// Run one pass: continue what can be continued, escalate what is spent, and
/// hand a recovered session its budget back.
///
/// The ledger is stamped BEFORE the send, which is the opposite of the ATC
/// beat's rule and deliberately so. The beat coalesces its nudge to the ATC
/// brain and skips dead sessions, so non-delivery there says nothing about the
/// erroring session and must not cost it budget. Here the send IS the retry: a
/// session we cannot successfully nudge three times running is precisely a
/// session a human should look at, and stamping first also means a crash
/// between the two can only under-retry, never re-fire.
///
/// Every fault degrades to a warn. A sweep that cannot read the roster, cannot
/// prove who owns it, or cannot find its own instance does NOTHING, because
/// each of those failures is indistinguishable from "the fleet is healthy" and
/// acting on that reading is what types into a pane somebody else owns.
pub async fn sweep_once(pool: &SqlitePool, events: &EventSink, now_ms: i64) -> SweepReport {
    let mut report = SweepReport::default();

    let roster = match FleetRepo::list_attention_error(pool).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "retry sweep: ERR roster unreadable; skipping this pass");
            return report;
        }
    };
    report.scanned = roster.len();

    let instances = match AtcInstanceRepo::list(pool).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "retry sweep: ATC registry unreadable; standing down this pass");
            return report;
        }
    };
    let (sweep, atc): (Vec<_>, Vec<_>) =
        instances.into_iter().partition(|inst| inst.name == SWEEP_INSTANCE);

    // A live ATC's `fleet needs` read is fleet-wide, so it can nudge ANY
    // erroring pane, not a subset this sweep could work around. There is no
    // per-session split to make, and both typing into one pane is the failure
    // this gate exists to prevent, so the whole pass stands down.
    if atc.iter().any(|inst| inst.enabled) {
        report.skipped_atc_owned = report.scanned;
        tracing::debug!(
            scanned = report.scanned,
            "retry sweep: a live ATC owns the fleet; standing down"
        );
        return report;
    }

    let Some(inst) = sweep.into_iter().next() else {
        tracing::warn!(
            instance = SWEEP_INSTANCE,
            "retry sweep: reserved instance is not registered; skipping this pass"
        );
        return report;
    };

    for session in &roster {
        if owned_by_atc(&atc, session) {
            report.skipped_atc_owned += 1;
            continue;
        }
        let Some(pattern) = transient_pattern(pool, session).await else {
            report.skipped_opaque += 1;
            tracing::debug!(
                session = %session.session_key,
                "retry sweep: ERR matched no transient API pattern; leaving it for a human"
            );
            continue;
        };
        report.transient += 1;
        enforce_cap(pool, events, &inst, session, &pattern, now_ms, &mut report).await;
    }

    report.reset = reset_recovered(pool, &roster, now_ms).await;
    report
}

/// Spawn the sweep on a background task with the system clock: boot's
/// fire-and-forget entry point. The handle is the caller's to drop; process
/// exit tears the task down, mirroring the other daemon sweepers.
#[must_use]
pub fn spawn(pool: SqlitePool, events: EventSink) -> tokio::task::JoinHandle<()> {
    let interval = sweep_interval();
    tokio::spawn(async move {
        tracing::info!(interval_ms = interval.as_millis(), "retry sweep started");
        let mut ticker = tokio::time::interval(interval);
        loop {
            ticker.tick().await;
            let report = sweep_once(&pool, &events, SystemClock.now_ms()).await;
            if report.acted() {
                tracing::info!(
                    continued = report.continued,
                    escalated = report.escalated,
                    reset = report.reset,
                    send_failed = report.send_failed,
                    skipped_opaque = report.skipped_opaque,
                    "retry sweep pass"
                );
            }
        }
    })
}

/// The tick cadence: [`SWEEP_INTERVAL`], or [`SWEEP_INTERVAL_ENV`] when it
/// names a positive number of milliseconds. Zero is rejected rather than
/// honoured, because `tokio::time::interval` panics on a zero period.
fn sweep_interval() -> Duration {
    std::env::var(SWEEP_INTERVAL_ENV)
        .ok()
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .map_or(SWEEP_INTERVAL, Duration::from_millis)
}

/// Whether a real ATC instance owns this session's pane.
///
/// Matched on `cwd` equality against the instance's own provisioned directory
/// (`<hangar home>/atc/<name>`), which is a whole path and cannot over-match.
/// Deliberately NOT on the tmux target: that carries window and pane suffixes,
/// and tmux name matching is prefix-based, so `atc` would claim `atc-worker`.
///
/// This covers a DISABLED instance too. Its pane still hosts the controller,
/// and `continue` typed at the controller is not a retry, it is the daemon
/// talking to the thing that talks to the fleet.
fn owned_by_atc(atc: &[AtcInstanceRow], session: &FleetSessionRow) -> bool {
    atc.iter().any(|inst| !inst.cwd.is_empty() && inst.cwd == session.cwd)
}

/// The transient API-error class in a session's recent output, or `None` when
/// its ERR is something else.
///
/// The gate that makes the sweep safe. It re-derives the class with the SAME
/// [`detect_error_signals`] the fleet reader uses, over the session's own
/// durable event payloads, because the ERROR state itself carries no evidence
/// of WHY. Payloads are hook envelopes and wire notifications (a transcript
/// PATH, never an inlined transcript), so scanning the newest handful is
/// kilobytes of regex work, not megabytes.
async fn transient_pattern(pool: &SqlitePool, session: &FleetSessionRow) -> Option<String> {
    let session_key = session.session_key.as_str();
    // Anchored on the transition, not on "recently". `attention_updated_at` is
    // when this session became ERROR, so the events either side of it are the
    // ones that explain it; anything older belongs to an incident that is over.
    let since = session.attention_updated_at.saturating_sub(TRANSITION_LOOKBACK_MS);
    let payloads = FleetRepo::recent_event_payloads(
        pool,
        session_key,
        ERROR_BEARING_EVENTS,
        since,
        RECENT_EVENT_SCAN,
    )
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(session = %session_key, %error, "retry sweep: event read failed");
        Vec::new()
    });
    payloads.iter().find_map(|payload| {
        detect_error_signals(payload, 0).into_iter().find_map(|signal| match signal {
            Signal::ApiError { pattern, .. } => Some(pattern),
            _ => None,
        })
    })
}

/// Apply the cap decision for ONE transient ERR session.
///
/// Mirrors the ATC heartbeat's `enforce_err_cap`, including its dedupe: a row
/// already flagged `escalated` is neither continued nor re-escalated until it
/// recovers, because [`raise_escalation`] mints a fresh attention id per call,
/// so a session parked at the cap would otherwise raise a brand-new row and a
/// fresh push every 30 seconds for one unchanged failure.
async fn enforce_cap(
    pool: &SqlitePool,
    events: &EventSink,
    inst: &AtcInstanceRow,
    session: &FleetSessionRow,
    pattern: &str,
    now_ms: i64,
    report: &mut SweepReport,
) {
    let ledger = AtcInstanceRepo::retry_get(pool, SWEEP_INSTANCE, &session.session_key)
        .await
        .ok()
        .flatten();
    if ledger.as_ref().is_some_and(|row| row.escalated) {
        return;
    }
    let count = ledger.map_or(0, |row| row.continue_count);
    match err_action(count, inst.err_retry_cap) {
        ErrAction::Escalate => {
            let reason = format!("retry cap reached: {pattern}");
            if raise_escalation(
                pool,
                events,
                SWEEP_INSTANCE,
                &session.session_key,
                &session.cwd,
                None,
                &reason,
                now_ms,
            )
            .await
            .is_ok()
            {
                report.escalated += 1;
            }
        }
        ErrAction::Continue => {
            match AtcInstanceRepo::record_continue(
                pool,
                SWEEP_INSTANCE,
                &session.session_key,
                now_ms,
            )
            .await
            {
                Ok(spent) => {
                    report.continued += 1;
                    if !send_continue(pool, events, session, spent).await {
                        report.send_failed += 1;
                    }
                }
                Err(error) => tracing::warn!(
                    session = %session.session_key,
                    %error,
                    "retry sweep: ledger write failed; not sending a continue it cannot count"
                ),
            }
        }
    }
}

/// Type `continue` into one session through the daemon's one verified send
/// path, and report whether it landed.
///
/// `SendPrompt` rather than `ControlAction::Continue`: the latter needs the
/// `continue_turn` capability and has no tmux arm, so on an ordinary pane it
/// resolves UNKNOWN with "lifecycle transport is not active" and types nothing.
///
/// The request id is derived from the session version and the budget unit being
/// spent, so it is stable for one decision and distinct for the next. A retried
/// tick that finds the session unchanged replays its receipt instead of typing
/// `continue` a second time, while a session that has moved on gets a fresh id.
async fn send_continue(
    pool: &SqlitePool,
    events: &EventSink,
    session: &FleetSessionRow,
    spent: i64,
) -> bool {
    let params = FleetActionParams {
        session_key: session.session_key.clone(),
        expected_version: session.version,
        request_id: format!(
            "retry-sweep:{}:{}:{spent}",
            session.session_key, session.version
        ),
        action: ControlAction::SendPrompt {
            text: CONTINUE_TEXT.to_string(),
        },
    };
    match crate::rpc::execute_fleet_action(pool, params, None, events).await {
        Ok(receipt) if receipt.status == ActionReceiptStatus::Delivered => {
            tracing::info!(session = %session.session_key, spent, "retry sweep sent continue");
            true
        }
        Ok(receipt) => {
            tracing::warn!(
                session = %session.session_key,
                status = ?receipt.status,
                detail = ?receipt.detail,
                "retry sweep: continue did not land; the budget stays spent"
            );
            false
        }
        Err(error) => {
            tracing::warn!(
                session = %session.session_key,
                detail = %error.message,
                "retry sweep: continue was refused; the budget stays spent"
            );
            false
        }
    }
}

/// Clear the ledger rows of sessions that recovered and STAYED recovered,
/// returning how many were cleared.
///
/// Recovery is absence from the ERR roster, but only once it has held for
/// [`RETRY_RESET_GRACE_MS`]. Clearing on first absence would make the cap
/// unreachable: the ordinary shape is err, continue, working again by the next
/// tick, so the row would be deleted before a second failure could accumulate
/// against it and a session failing every minute forever would never escalate.
///
/// Absence is measured against the WHOLE ERR roster, not the subset this pass
/// acted on. A session skipped as opaque or as ATC-owned is still erroring, and
/// handing it a fresh budget for being skipped is the one direction that costs
/// a human an extra page.
async fn reset_recovered(pool: &SqlitePool, roster: &[FleetSessionRow], now_ms: i64) -> usize {
    let erroring: HashSet<&str> =
        roster.iter().map(|session| session.session_key.as_str()).collect();
    let ledger = match AtcInstanceRepo::retry_list(pool, SWEEP_INSTANCE).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(%error, "retry sweep: ledger unreadable; nothing reset this pass");
            return 0;
        }
    };
    let mut reset = 0;
    for row in ledger {
        let absent = !erroring.contains(row.session_id.as_str());
        let held = now_ms.saturating_sub(row.updated_at) >= RETRY_RESET_GRACE_MS;
        if !(absent && held) {
            continue;
        }
        match AtcInstanceRepo::reset_retry(pool, SWEEP_INSTANCE, &row.session_id).await {
            Ok(()) => reset += 1,
            Err(error) => tracing::warn!(
                session = %row.session_id,
                %error,
                "retry sweep: reset failed; session keeps its spent budget"
            ),
        }
    }
    reset
}

#[cfg(test)]
mod tests {
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::attention::AttentionRepo;
    use ainb_hangar_store::repo::fleet::{FleetSessionPatch, NewFleetEvent, ObservationAuthority};

    use super::*;

    const NOW: i64 = 1_767_225_600_000;
    /// A payload the fleet reader's own patterns recognise as transient.
    const OVERLOADED: &str = r#"{"error_type":"overloaded_error"}"#;

    async fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        ensure_instance(store.pool()).await.unwrap();
        (dir, store)
    }

    fn broker() -> (crate::events::EventBroker, EventSink) {
        let broker = crate::events::EventBroker::new();
        let sink = broker.sink();
        (broker, sink)
    }

    /// Seed one session the roster will hand back as erroring, carrying
    /// `payload` as the durable event the transient gate reads.
    ///
    /// Capabilities are left at `{}` on purpose: the send then stops at
    /// `execute_fleet_action`'s capability gate with a durable REJECTED receipt
    /// instead of reaching tmux, so the decision path is exercised end to end
    /// without a subprocess.
    async fn seed_err(store: &Store, key: &str, cwd: &str, payload: &str) {
        FleetRepo::apply_event(
            store.pool(),
            &NewFleetEvent {
                event_id: format!("seed:{key}"),
                session_key: key.to_string(),
                observed_at: NOW,
                authority: ObservationAuthority::Authoritative,
                event_type: "StopFailure".to_string(),
                payload: payload.to_string(),
                patch: FleetSessionPatch {
                    provider: Some("claude".to_string()),
                    cwd: Some(cwd.to_string()),
                    lifecycle_state: Some("TURN_COMPLETE".to_string()),
                    attention_state: Some("ERROR".to_string()),
                    ..FleetSessionPatch::default()
                },
            },
        )
        .await
        .unwrap();
    }

    /// Seed one extra event on an existing session, with an arbitrary type and
    /// observation time. The transient gate reads only some of these.
    async fn seed_event(
        store: &Store,
        key: &str,
        event_id: &str,
        event_type: &str,
        payload: &str,
        observed_at: i64,
    ) {
        FleetRepo::apply_event(
            store.pool(),
            &NewFleetEvent {
                event_id: event_id.to_string(),
                session_key: key.to_string(),
                observed_at,
                authority: ObservationAuthority::Authoritative,
                event_type: event_type.to_string(),
                payload: payload.to_string(),
                patch: FleetSessionPatch::default(),
            },
        )
        .await
        .unwrap();
    }

    /// Register a REAL ATC instance (not the reserved one), enabled or not.
    async fn seed_atc(store: &Store, name: &str, cwd: &str, enabled: bool) {
        AtcInstanceRepo::register(
            store.pool(),
            &RegisterAtc {
                name: name.to_string(),
                cwd: cwd.to_string(),
                tmux_session: Some(format!("atc-{name}")),
                heartbeat_cron: "*/2 * * * *".to_string(),
                err_retry_cap: 3,
                idle_pause_min: 60,
                next_tick_at: Some(NOW + 120_000),
            },
            NOW,
        )
        .await
        .unwrap();
        if !enabled {
            AtcInstanceRepo::set_enabled(store.pool(), name, false, None).await.unwrap();
        }
    }

    async fn ledger(store: &Store, key: &str) -> Option<i64> {
        AtcInstanceRepo::retry_get(store.pool(), SWEEP_INSTANCE, key)
            .await
            .unwrap()
            .map(|row| row.continue_count)
    }

    /// The reserved instance is registered so the ledger's foreign key holds,
    /// and is invisible to the heartbeat cron that would otherwise try to beat
    /// an ATC session that does not exist.
    #[tokio::test]
    async fn the_reserved_instance_is_never_schedulable() {
        let (_dir, store) = store().await;
        seed_atc(&store, "tower", "/work/atc/tower", true).await;

        let schedulable =
            AtcInstanceRepo::list_schedulable(store.pool(), NOW + 600_000).await.unwrap();

        assert!(
            schedulable.iter().all(|inst| inst.name != SWEEP_INSTANCE),
            "the sweep's reserved instance must never reach the ATC heartbeat cron"
        );
        assert_eq!(
            schedulable.len(),
            1,
            "a real ATC instance is still schedulable, so the assertion above is not vacuous"
        );
    }

    /// A transient ERR under the cap spends one unit of budget and dispatches
    /// `continue` through the one verified send path.
    #[tokio::test]
    async fn a_transient_err_under_the_cap_spends_budget_and_dispatches_a_continue() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        seed_err(&store, "claude:s-1", "/work/one", OVERLOADED).await;

        let report = sweep_once(store.pool(), &sink, NOW).await;

        assert_eq!(report.scanned, 1);
        assert_eq!(report.transient, 1);
        assert_eq!(report.continued, 1);
        assert_eq!(ledger(&store, "claude:s-1").await, Some(1));

        // The action reached `execute_fleet_action` and was resolved by ITS
        // gates, which is as far as a hermetic test can follow a tmux send.
        let session = FleetRepo::get_session(store.pool(), "claude:s-1").await.unwrap().unwrap();
        let receipt = FleetRepo::get_action_receipt(
            store.pool(),
            &format!("retry-sweep:claude:s-1:{}:1", session.version),
        )
        .await
        .unwrap()
        .expect("the sweep's continue leaves a durable receipt");
        assert_eq!(receipt.action_kind, "send_prompt");
        assert_eq!(
            receipt.status, "REJECTED",
            "capability-less session: {receipt:?}"
        );
        assert_eq!(report.send_failed, 1);
    }

    /// An ERR whose recent output matches no transient API pattern is a real
    /// failure a human must see, so the sweep must not touch it at all. This is
    /// the safety property: `attention_state = 'ERROR'` is set by generic hook
    /// signals, so the roster is full of errors that are not retryable.
    #[tokio::test]
    async fn an_opaque_err_is_left_entirely_alone() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        seed_err(
            &store,
            "claude:s-opaque",
            "/work/opaque",
            r#"{"error_type":"the project's own test suite is red"}"#,
        )
        .await;

        let report = sweep_once(store.pool(), &sink, NOW).await;

        assert_eq!(report.skipped_opaque, 1);
        assert_eq!(report.transient, 0);
        assert_eq!(report.continued, 0);
        assert_eq!(
            ledger(&store, "claude:s-opaque").await,
            None,
            "an opaque ERR must not even open a ledger row"
        );
        assert!(
            FleetRepo::list_action_receipts(store.pool(), 10).await.unwrap().is_empty(),
            "an opaque ERR must produce no send at all"
        );
    }

    /// At the cap the sweep escalates exactly once. Without the escalated-flag
    /// dedupe a session parked at the cap would mint a new attention row and a
    /// new push every 30 seconds for one unchanged failure.
    #[tokio::test]
    async fn at_the_cap_it_escalates_once_and_never_re_raises() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        seed_err(&store, "claude:s-spent", "/work/spent", OVERLOADED).await;
        for _ in 0..i64::from(DEFAULT_ERR_RETRY_CAP) {
            AtcInstanceRepo::record_continue(store.pool(), SWEEP_INSTANCE, "claude:s-spent", NOW)
                .await
                .unwrap();
        }

        let first = sweep_once(store.pool(), &sink, NOW).await;
        assert_eq!(first.escalated, 1);
        assert_eq!(first.continued, 0, "escalating must not spend more budget");
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1
        );

        let second = sweep_once(store.pool(), &sink, NOW + 30_000).await;
        assert_eq!(second.escalated, 0);
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1,
            "an already-escalated session raises no second row on the next tick"
        );
        assert_eq!(
            ledger(&store, "claude:s-spent").await,
            Some(i64::from(DEFAULT_ERR_RETRY_CAP))
        );
    }

    /// A live ATC drives `continue` into erroring panes from a fleet-wide
    /// roster, so the sweep stands down entirely rather than typing into the
    /// same pane from the other side.
    #[tokio::test]
    async fn a_live_atc_instance_stands_the_whole_sweep_down() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        seed_err(&store, "claude:s-1", "/work/one", OVERLOADED).await;
        seed_atc(&store, "tower", "/work/atc/tower", true).await;

        let report = sweep_once(store.pool(), &sink, NOW).await;

        assert_eq!(report.scanned, 1);
        assert_eq!(report.skipped_atc_owned, 1);
        assert_eq!(report.continued, 0);
        assert_eq!(ledger(&store, "claude:s-1").await, None);
    }

    /// An ATC's OWN pane is never swept, even when that instance is disabled
    /// and the rest of the fleet is fair game: `continue` typed at the
    /// controller is the daemon nudging the thing that nudges the fleet.
    #[tokio::test]
    async fn an_atc_instances_own_pane_is_never_swept() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        seed_atc(&store, "tower", "/work/atc/tower", false).await;
        seed_err(&store, "claude:s-atc", "/work/atc/tower", OVERLOADED).await;
        seed_err(&store, "claude:s-worker", "/work/worker", OVERLOADED).await;

        let report = sweep_once(store.pool(), &sink, NOW).await;

        assert_eq!(report.skipped_atc_owned, 1);
        assert_eq!(ledger(&store, "claude:s-atc").await, None);
        assert_eq!(
            ledger(&store, "claude:s-worker").await,
            Some(1),
            "a disabled ATC must not shield the sessions it is not driving"
        );
    }

    /// Recovery gives the budget back, but only once the absence has HELD.
    /// Clearing on first absence would make the cap unreachable, because the
    /// ordinary shape is err, continue, working again by the next tick.
    #[tokio::test]
    async fn a_recovered_session_gets_its_budget_back_only_after_the_grace_window() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        AtcInstanceRepo::record_continue(store.pool(), SWEEP_INSTANCE, "claude:s-flaky", NOW)
            .await
            .unwrap();

        let early = sweep_once(store.pool(), &sink, NOW + 60_000).await;
        assert_eq!(early.reset, 0);
        assert_eq!(
            ledger(&store, "claude:s-flaky").await,
            Some(1),
            "budget cleared on first absence: a flapping session would never escalate"
        );

        let held = sweep_once(store.pool(), &sink, NOW + RETRY_RESET_GRACE_MS).await;
        assert_eq!(held.reset, 1);
        assert_eq!(
            ledger(&store, "claude:s-flaky").await,
            None,
            "a genuinely recovered session must get a fresh budget"
        );
    }

    /// A session that is STILL erroring keeps its spent budget however long the
    /// error has run, so a permanently-broken session reaches the cap instead of
    /// being handed a fresh budget every grace window.
    #[tokio::test]
    async fn a_session_still_on_the_err_roster_is_never_reset() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        seed_err(&store, "claude:s-stuck", "/work/stuck", OVERLOADED).await;
        AtcInstanceRepo::record_continue(store.pool(), SWEEP_INSTANCE, "claude:s-stuck", NOW)
            .await
            .unwrap();

        let report = sweep_once(store.pool(), &sink, NOW + RETRY_RESET_GRACE_MS).await;

        assert_eq!(report.reset, 0);
        assert_eq!(
            ledger(&store, "claude:s-stuck").await,
            Some(2),
            "a still-erroring session accumulates toward the cap"
        );
    }

    /// An exited session's error is history: its pane is gone, so a continue
    /// cannot land and an escalation would name a session nobody can open.
    #[tokio::test]
    async fn an_exited_session_is_off_the_roster_entirely() {
        let (_dir, store) = store().await;
        let (_broker, sink) = broker();
        seed_err(&store, "claude:s-dead", "/work/dead", OVERLOADED).await;
        FleetRepo::apply_event(
            store.pool(),
            &NewFleetEvent {
                event_id: "seed:exit".to_string(),
                session_key: "claude:s-dead".to_string(),
                observed_at: NOW + 1,
                authority: ObservationAuthority::Authoritative,
                event_type: "SessionEnd".to_string(),
                payload: "{}".to_string(),
                patch: FleetSessionPatch {
                    lifecycle_state: Some("EXITED".to_string()),
                    ..FleetSessionPatch::default()
                },
            },
        )
        .await
        .unwrap();

        let report = sweep_once(store.pool(), &sink, NOW).await;

        assert_eq!(report.scanned, 0);
        assert_eq!(ledger(&store, "claude:s-dead").await, None);
    }

    /// The cadence is 30s in production and overridable so a test or a smoke
    /// run can drive several ticks in about a second.
    #[test]
    fn the_interval_override_only_accepts_a_positive_millisecond_value() {
        // Parsed, not read from the environment: the env read itself is a
        // process-global other tests in this binary would race.
        let parse = |raw: &str| {
            raw.trim()
                .parse::<u64>()
                .ok()
                .filter(|ms| *ms > 0)
                .map_or(SWEEP_INTERVAL, Duration::from_millis)
        };
        assert_eq!(parse(" 250 "), Duration::from_millis(250));
        assert_eq!(
            parse("0"),
            SWEEP_INTERVAL,
            "a zero period panics the ticker"
        );
        assert_eq!(parse("soon"), SWEEP_INTERVAL);
    }
    /// THE injection case. A `fleet_event` payload is the whole hook envelope,
    /// and `PostToolUse` carries a tool's OUTPUT, so an agent that merely reads
    /// or greps a file containing a transient signature would otherwise arm its
    /// own auto-`continue`. `ainb-fleet-core`'s `errors.rs` contains every one
    /// of the patterns, so opening the file that DEFINES them would qualify.
    ///
    /// The session's own ERROR here is opaque; only the tool output looks
    /// transient. Nothing may be sent.
    #[tokio::test]
    async fn a_transient_string_in_a_tool_result_never_arms_the_sweep() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        ensure_instance(store.pool()).await.unwrap();
        let (_broker, events) = broker();

        seed_err(
            &store,
            "claude:reader",
            "/work/reader",
            r#"{"error":"the build is broken"}"#,
        )
        .await;
        // What an agent gets back from `grep ECONNRESET src/`.
        seed_event(
            &store,
            "claude:reader",
            "tool-output",
            "PostToolUse",
            r#"{"tool_response":"errors.rs:44: ECONNRESET|connection reset"}"#,
            NOW,
        )
        .await;

        let report = sweep_once(store.pool(), &events, NOW).await;
        assert_eq!(report.scanned, 1);
        assert_eq!(
            report.skipped_opaque, 1,
            "a transient signature in a TOOL RESULT must not qualify the session"
        );
        assert_eq!(report.transient, 0);
        assert_eq!(report.continued, 0);
        assert!(
            AtcInstanceRepo::retry_get(store.pool(), SWEEP_INSTANCE, "claude:reader")
                .await
                .unwrap()
                .is_none(),
            "no budget may be spent on a session the gate did not qualify"
        );
    }

    /// An incident the session already recovered from must not arm the NEXT,
    /// unrelated failure. Without a bound on how far back the scan may look, a
    /// 429 from hours ago still matches, and the pattern search has no idea it
    /// was resolved.
    ///
    /// Asserts on the GATE rather than through `sweep_once`: the whole sweep
    /// has several other reasons to skip a session, so a pass there would not
    /// prove the time bound is what did it.
    #[tokio::test]
    async fn the_gate_ignores_an_incident_older_than_the_lookback() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();

        // The OLD, resolved 429, outside the lookback.
        seed_event(
            &store,
            "claude:stale",
            "old-429",
            "StopFailure",
            r#"{"error":"rate_limited"}"#,
            NOW - TRANSITION_LOOKBACK_MS - 60_000,
        )
        .await;
        // Today's failure, which is not transient at all.
        seed_err(
            &store,
            "claude:stale",
            "/work/stale",
            r#"{"error":"tests failed"}"#,
        )
        .await;

        let session = FleetRepo::list_attention_error(store.pool())
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.session_key == "claude:stale")
            .expect("the seeded session must be on the ERR roster");
        assert_eq!(
            session.attention_updated_at, NOW,
            "the transition time is what the lookback is measured from"
        );
        assert_eq!(
            transient_pattern(store.pool(), &session).await,
            None,
            "a 429 older than the lookback must not qualify today's unrelated failure"
        );

        // ...and the same 429 INSIDE the window does qualify, so the assertion
        // above is the bound talking and not a gate that never matches.
        seed_event(
            &store,
            "claude:fresh",
            "fresh-429",
            "StopFailure",
            r#"{"error":"rate_limited"}"#,
            NOW - 1_000,
        )
        .await;
        seed_err(
            &store,
            "claude:fresh",
            "/work/fresh",
            r#"{"error":"tests failed"}"#,
        )
        .await;
        let fresh = FleetRepo::list_attention_error(store.pool())
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.session_key == "claude:fresh")
            .expect("fresh session on the roster");
        assert_eq!(
            transient_pattern(store.pool(), &fresh).await,
            Some("rate_limited".to_string())
        );
    }
}
