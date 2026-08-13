//! ATC on the daemon (D12, spec P9 §4.7) — instance registry, the heartbeat
//! cron, and escalation → attention.
//!
//! P9 pulls ATC off its launchd/systemd side-timers and JSON side-files onto the
//! daemon:
//!
//! - **Registry + heartbeat cron.** `ainb fleet atc setup` registers an instance
//!   in `atc_instance` (via RPC) and the heartbeat becomes a daemon cron job —
//!   the [`AtcHeartbeatScheduler`] reuses the autopilot scheduler's DB-durable
//!   tick loop (earliest `next_tick_at` → sleep → fire → reschedule), so restart
//!   survival lives in the DB, not a launchd plist.
//! - **Retry cap in the store.** The per-session auto-`continue` cap is enforced
//!   against the durable `atc_retry` ledger ([`err_action`]), not an advisory
//!   JSON the model might stop maintaining. `task-log.md` stays as human audit.
//! - **Escalations become attention rows.** An exhausted / stuck session is
//!   raised through the SAME attention pipeline every other input request uses
//!   ([`raise_escalation`], kind=escalation) so it reaches the phone/web push
//!   instead of dead-ending in `task-log.md`.

use std::sync::Arc;
use std::time::Duration;

use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::atc_instance::{
    ATC_SCHEDULER_CLAIM_RENEW_MS, AtcInstanceRepo, AtcInstanceRow, AtcRetryRow,
};
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::events::EventSink;
use crate::scheduler::{recompute_next_tick, sleep_delay};

/// The no-work re-poll interval when no ATC instance is schedulable.
const NO_WORK_REPOLL: Duration = Duration::from_secs(60);

/// What the heartbeat should do with a session stuck on an ERR, given its durable
/// continue-count and the instance's cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrAction {
    /// The session still has continue budget — present the ERR as continue-eligible.
    Continue,
    /// The session has exhausted its budget — escalate to a human instead.
    Escalate,
}

/// Decide the ERR action from a session's current continue-count and the cap.
///
/// `cap` is clamped to at least 1 (a cap of 0 would escalate before any continue,
/// which is never the intent — mirrors the ATC heartbeat builder in `ainb-core`).
/// A session at or over the cap escalates; under it, continues.
#[must_use]
pub fn err_action(continue_count: i64, cap: i64) -> ErrAction {
    if continue_count >= cap.max(1) {
        ErrAction::Escalate
    } else {
        ErrAction::Continue
    }
}

/// Raise an ATC escalation as a durable `attention` row (kind=escalation) and
/// nudge every surface — the D12 escalation path that replaces the old
/// dead-end in `task-log.md`.
///
/// The escalation flows through the SAME attention pipeline as every other input
/// request, so it reaches the TUI control centre, the phone bridge, and web push
/// with no bespoke channel. The instance's retry ledger is marked `escalated`
/// so the heartbeat never re-presents the session as continue-eligible.
///
/// Idempotent on the attention id (`escalation:<instance>:<session>:<now>`): a
/// re-raise at a new instant is a new row (a genuinely new escalation), while the
/// ledger flip is naturally idempotent. Returns the raised attention id.
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the attention insert or the ledger write fails.
pub async fn raise_escalation(
    pool: &SqlitePool,
    events: &EventSink,
    instance_name: &str,
    session_id: &str,
    cwd: &str,
    workspace_id: Option<&str>,
    reason: &str,
    now_ms: i64,
) -> Result<String, sqlx::Error> {
    let id = format!("escalation:{instance_name}:{session_id}:{now_ms}");
    let payload = serde_json::json!({
        "kind": "escalation",
        "instance": instance_name,
        "session_id": session_id,
        "reason": reason,
    })
    .to_string();
    // Resolve the escalation's routing channels ONCE at raise time (tcp T5), for
    // this instance's workspace scope. The seeded default routes escalation to
    // phone+web+os (loudest — a human is being paged).
    let channels =
        crate::notify::resolve_channels(pool, AttentionKind::Escalation, workspace_id).await;
    let new = NewAttention {
        id: id.clone(),
        session_id: session_id.to_string(),
        cwd: cwd.to_string(),
        workspace_id: workspace_id.map(str::to_string),
        kind: AttentionKind::Escalation,
        payload,
        degraded: false,
        created_at: now_ms,
        raise_transcript: None,
        channels,
    };
    AttentionRepo::insert(pool, &new).await?;
    // Mark the ledger so the heartbeat stops presenting this session as
    // continue-eligible (best-effort on top of the raise — the row is the
    // human-facing signal; the ledger flip is the machine one).
    AtcInstanceRepo::mark_escalated(pool, instance_name, session_id, now_ms).await?;
    events.emit_attention(HangarEvent::AttentionRaised {
        attention_id: id.clone(),
        session_id: session_id.to_string(),
        workspace_id: workspace_id.map(str::to_string),
        kind: AttentionKind::Escalation.as_str().to_string(),
        degraded: false,
        created_at: now_ms,
        channels,
    });
    tracing::info!(instance = %instance_name, session = %session_id, %reason, "ATC escalation raised");
    Ok(id)
}

/// Compute an instance's next heartbeat tick (epoch-ms) from its cron, strictly
/// after `now_ms`. Returns `None` when the cron does not parse or has no future
/// match (the caller persists that as a `NULL` `next_tick_at`).
///
/// The registration + reschedule seam, shared with the RPC register handler so a
/// freshly-registered instance is immediately schedulable.
#[must_use]
pub fn next_heartbeat_tick(cron_expr: &str, now_ms: i64) -> Option<i64> {
    use ainb_hangar_core::autopilot::cron::{
        millis_to_utc, next_tick_after, parse_cron, utc_to_millis,
    };
    let schedule = parse_cron(cron_expr).ok()?;
    let after = millis_to_utc(now_ms)?;
    next_tick_after(&schedule, after).map(utc_to_millis)
}

/// The ATC heartbeat cron: a daemon-global tick loop over every enabled ATC
/// instance, firing each heartbeat on its cron (the launchd/systemd timer's
/// daemon-native replacement).
pub struct AtcHeartbeatScheduler {
    pool: SqlitePool,
    events: EventSink,
    clock: Arc<dyn HangarClock>,
    shutdown: CancellationToken,
}

impl AtcHeartbeatScheduler {
    /// Build the heartbeat cron over the store, event sink, injected clock, and a
    /// shutdown token.
    #[must_use]
    pub fn new(
        pool: SqlitePool,
        events: EventSink,
        clock: Arc<dyn HangarClock>,
        shutdown: CancellationToken,
    ) -> Self {
        Self {
            pool,
            events,
            clock,
            shutdown,
        }
    }

    /// Run the heartbeat cron until shutdown. Each iteration picks the
    /// earliest-firing enabled instance, sleeps until its tick (or shutdown),
    /// fires the heartbeat, and reschedules. With no schedulable instance it
    /// re-polls after [`NO_WORK_REPOLL`]. Every fault is logged + swallowed (one
    /// bad instance never downs the loop — mirrors the autopilot scheduler).
    pub async fn run(self) {
        tracing::info!("ATC heartbeat cron started");
        loop {
            if self.shutdown.is_cancelled() {
                break;
            }
            let now_ms = self.clock.now_ms();
            let next = match AtcInstanceRepo::list_schedulable(&self.pool, now_ms).await {
                Ok(rows) => rows.into_iter().min_by_key(|r| r.next_tick_at.unwrap_or(i64::MAX)),
                Err(e) => {
                    tracing::error!(error = %e, "ATC heartbeat cron: load failed; re-polling");
                    None
                }
            };
            let (delay, fire_target) = match next {
                Some(inst) => {
                    let tick = inst.next_tick_at.unwrap_or_else(|| self.clock.now_ms());
                    let until_tick = sleep_delay(tick, self.clock.now_ms());
                    if until_tick > NO_WORK_REPOLL {
                        (NO_WORK_REPOLL, None)
                    } else {
                        (until_tick, Some(inst))
                    }
                }
                None => (NO_WORK_REPOLL, None),
            };
            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
            }
            if let Some(inst) = fire_target {
                let due_tick_at = inst.next_tick_at.expect("schedulable rows have a due tick");
                match AtcInstanceRepo::claim_due(
                    &self.pool,
                    &inst.name,
                    inst.config_generation,
                    due_tick_at,
                    self.clock.now_ms(),
                )
                .await
                {
                    Ok(Some(claim_token)) => {
                        if let Some(fired_at) = self.fire_while_claimed(&inst, &claim_token).await {
                            self.reschedule(&inst, &claim_token, fired_at).await;
                        }
                    }
                    Ok(None) => tracing::debug!(
                        instance = %inst.name,
                        generation = inst.config_generation,
                        "ATC heartbeat claim unavailable or invalidated"
                    ),
                    Err(error) => tracing::error!(
                        instance = %inst.name,
                        error = %error,
                        "ATC heartbeat claim failed"
                    ),
                }
            }
        }
        tracing::info!("ATC heartbeat cron stopped");
    }

    /// Keep the exact-token lease alive for the full asynchronous heartbeat.
    /// Losing or failing to renew the claim cancels the in-flight future before
    /// another scheduler may deliver the same tick.
    async fn fire_while_claimed(&self, inst: &AtcInstanceRow, claim_token: &str) -> Option<i64> {
        let renew_every = Duration::from_millis(
            u64::try_from(ATC_SCHEDULER_CLAIM_RENEW_MS)
                .expect("ATC claim renewal interval is positive"),
        );
        let mut renewals =
            tokio::time::interval_at(tokio::time::Instant::now() + renew_every, renew_every);
        let fire = self.fire(inst);
        tokio::pin!(fire);

        loop {
            tokio::select! {
                fired_at = &mut fire => return Some(fired_at),
                () = self.shutdown.cancelled() => {
                    self.release_claim(inst, claim_token, "shutdown").await;
                    return None;
                },
                _ = renewals.tick() => {
                    match AtcInstanceRepo::renew_claim(
                        &self.pool,
                        &inst.name,
                        inst.config_generation,
                        claim_token,
                        self.clock.now_ms(),
                    )
                    .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::warn!(
                                instance = %inst.name,
                                generation = inst.config_generation,
                                "ATC heartbeat claim lost during delivery"
                            );
                            return None;
                        }
                        Err(error) => {
                            tracing::warn!(
                                instance = %inst.name,
                                error = %error,
                                "ATC heartbeat claim renewal failed"
                            );
                            self.release_claim(inst, claim_token, "renewal failure").await;
                            return None;
                        }
                    }
                }
            }
        }
    }

    async fn release_claim(&self, inst: &AtcInstanceRow, claim_token: &str, reason: &str) {
        match AtcInstanceRepo::release_claim(
            &self.pool,
            &inst.name,
            inst.config_generation,
            claim_token,
        )
        .await
        {
            Ok(true) => tracing::debug!(
                instance = %inst.name,
                generation = inst.config_generation,
                %reason,
                "ATC heartbeat claim released"
            ),
            Ok(false) => tracing::debug!(
                instance = %inst.name,
                generation = inst.config_generation,
                %reason,
                "ATC heartbeat claim already lost"
            ),
            Err(error) => tracing::warn!(
                instance = %inst.name,
                %reason,
                error = %error,
                "ATC heartbeat claim release failed"
            ),
        }
    }

    /// Fire one instance's heartbeat: read the fleet needs, enforce the retry cap
    /// against the durable ledger (escalating exhausted ERR sessions through the
    /// attention pipeline), build the compact nudge, send it into the ATC session
    /// via the one verified send path, and stamp `last_heartbeat_at`.
    ///
    /// Non-fatal end to end: an unspawned instance (no tmux target) or a send
    /// fault is warned and skipped, never a panic.
    async fn fire(&self, inst: &AtcInstanceRow) -> i64 {
        let now = self.clock.now_ms();

        // 1. Read the durable ledger BEFORE the scan and derive the spent set. The
        //    beat needs it to render `ESCALATE-ONLY`, and only the ledger knows.
        let ledger = AtcInstanceRepo::retry_list(&self.pool, &inst.name).await.unwrap_or_else(|e| {
            tracing::warn!(instance = %inst.name, error = %e, "ATC retry ledger unreadable; beating without a cap set");
            Vec::new()
        });
        let exhausted = exhausted_sessions(&ledger, inst.err_retry_cap);

        // 2. Delegate the whole beat to `ainb fleet atc heartbeat`. That verb owns
        //    the ONE nudge body: hooks-primary needs read, idle-pause, the durable
        //    completion inbox, untrusted fencing, composer coalescing, and the
        //    `heartbeat-state.json` stamp the Daemons view reads. Building a second
        //    body here is what made the daemon-scheduled beat strictly weaker than
        //    the timer-scheduled one.
        let Some(report) = run_cli_heartbeat(&inst.name, &exhausted).await else {
            return now;
        };

        // 3. Advance the ledger from what the beat actually saw. Escalation stays
        //    here because it needs the store and the event sink.
        for err in &report.err_sessions {
            self.enforce_err_cap(inst, &err.session_id, &err.cwd, &err.pattern, now).await;
        }

        // 4. Recovery is ABSENCE: a ledger row with no matching ERR row this beat
        //    means the session stopped erroring, so its budget is cleared and a
        //    later failure starts fresh. Without this an escalated session stays
        //    escalated forever — `enforce_err_cap` short-circuits on the flag, and
        //    nothing else ever cleared it.
        for row in &ledger {
            if !report.err_sessions.iter().any(|e| e.session_id == row.session_id) {
                if let Err(e) =
                    AtcInstanceRepo::reset_retry(&self.pool, &inst.name, &row.session_id).await
                {
                    tracing::warn!(
                        instance = %inst.name,
                        session = %row.session_id,
                        error = %e,
                        "ATC retry reset failed; session keeps its spent budget"
                    );
                }
            }
        }

        now
    }

    /// Apply the retry-cap decision for ONE ERR session of an instance.
    ///
    /// Reads the durable ledger row ONCE and short-circuits when it is already
    /// `escalated`: an escalated session is neither continued nor re-escalated
    /// until it recovers (recovery clears the ledger via `reset_retry`). Without
    /// this guard a session stuck at/over the cap would re-enter the Escalate
    /// branch every heartbeat, and [`raise_escalation`] would mint a fresh
    /// `escalation:{inst}:{sess}:{now_ms}` id each tick — a brand-new attention row
    /// + `AttentionRaised` push (every 2 min by default) for the same unchanged
    /// failure. Under the cap and not escalated, one unit of continue budget is
    /// consumed.
    async fn enforce_err_cap(
        &self,
        inst: &AtcInstanceRow,
        session_id: &str,
        session_cwd: &str,
        pattern: &str,
        now_ms: i64,
    ) {
        let ledger = AtcInstanceRepo::retry_get(&self.pool, &inst.name, session_id)
            .await
            .ok()
            .flatten();
        // Dedupe: a row already flagged escalated is done — no continue, no re-raise.
        if ledger.as_ref().is_some_and(|row| row.escalated) {
            return;
        }
        let count = ledger.map_or(0, |row| row.continue_count);
        match err_action(count, inst.err_retry_cap) {
            ErrAction::Escalate => {
                let reason = format!("retry cap reached: {pattern}");
                let _ = raise_escalation(
                    &self.pool,
                    &self.events,
                    &inst.name,
                    session_id,
                    session_cwd,
                    None,
                    &reason,
                    now_ms,
                )
                .await;
            }
            ErrAction::Continue => {
                let _ =
                    AtcInstanceRepo::record_continue(&self.pool, &inst.name, session_id, now_ms)
                        .await;
            }
        }
    }

    /// Recompute + persist the instance's next heartbeat tick from the fired slot.
    async fn reschedule(&self, inst: &AtcInstanceRow, claim_token: &str, fired_at: i64) {
        let next =
            recompute_next_tick(&inst.heartbeat_cron, inst.next_tick_at, self.clock.now_ms());
        match AtcInstanceRepo::complete_claim(
            &self.pool,
            &inst.name,
            inst.config_generation,
            claim_token,
            next,
            fired_at,
        )
        .await
        {
            Ok(true) => {}
            Ok(false) => tracing::debug!(
                instance = %inst.name,
                generation = inst.config_generation,
                "ATC heartbeat completion invalidated by mutation or claim takeover"
            ),
            Err(error) => tracing::error!(
                instance = %inst.name,
                error = %error,
                "ATC heartbeat reschedule failed"
            ),
        }
    }

    /// Spawn the heartbeat cron on a background task with the system clock — boot's
    /// fire-and-forget entry point.
    #[must_use]
    pub fn spawn(pool: SqlitePool, events: EventSink) -> tokio::task::JoinHandle<()> {
        use ainb_hangar_core::clock::SystemClock;
        let sched = Self::new(
            pool,
            events,
            Arc::new(SystemClock),
            CancellationToken::new(),
        );
        tokio::spawn(sched.run())
    }
}

/// The sessions whose auto-`continue` budget is spent, as the beat's renderer
/// needs them.
///
/// A row counts as spent when it is already flagged `escalated` OR its count has
/// reached the cap. The flag alone is not enough: the row is written at the
/// moment of escalation, so a crash between `record_continue` and
/// `mark_escalated` would otherwise hand back a session the beat then invites
/// another `continue` for.
#[must_use]
fn exhausted_sessions(ledger: &[AtcRetryRow], cap: i64) -> Vec<String> {
    ledger
        .iter()
        .filter(|row| row.escalated || matches!(err_action(row.continue_count, cap), ErrAction::Escalate))
        .map(|row| row.session_id.clone())
        .collect()
}

/// One ERR session the beat observed, as reported back by the CLI.
#[derive(Debug, Clone, serde::Deserialize)]
struct ErrSessionReport {
    session_id: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    pattern: String,
}

/// The slice of the CLI heartbeat's JSON summary the daemon acts on. Every other
/// field is ignored, so the CLI can grow its summary without breaking the daemon.
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct HeartbeatReport {
    #[serde(default)]
    err_sessions: Vec<ErrSessionReport>,
}

/// Resolve the `ainb` binary the beat runs as.
///
/// The daemon IS `ainb hangar daemon start`, so `current_exe()` is already the
/// right binary and a rebuild or a `brew upgrade` moves both together. `AINB_BIN`
/// overrides for tests and for a deliberately pinned install, matching the CLI's
/// own `atc_bin`.
fn ainb_bin() -> String {
    std::env::var("AINB_BIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(str::to_string))
            .unwrap_or_else(|| "ainb".to_string())
    })
}

/// Run one delegated beat and parse what it saw.
///
/// Non-fatal end to end, like everything else on this tick: a missing binary, a
/// non-zero exit (an instance never provisioned on this host), or unparseable
/// stdout all yield `None`, which leaves the ledger untouched and reschedules
/// normally. Returning an empty report instead would read as "nothing is
/// erroring" and wrongly clear every session's budget.
async fn run_cli_heartbeat(name: &str, exhausted: &[String]) -> Option<HeartbeatReport> {
    let out = tokio::process::Command::new(ainb_bin())
        .args([
            "--format",
            "json",
            "fleet",
            "atc",
            "heartbeat",
            name,
            // ALWAYS passed, empty set included: its presence is what tells the
            // beat the daemon owns the ledger and it must not count locally.
            "--exhausted",
        ])
        .arg(exhausted.join(","))
        .output()
        .await
        .map_err(|e| tracing::warn!(instance = %name, error = %e, "ATC heartbeat spawn failed"))
        .ok()?;
    if !out.status.success() {
        tracing::warn!(
            instance = %name,
            status = %out.status,
            stderr = %String::from_utf8_lossy(&out.stderr).trim(),
            "ATC heartbeat exited non-zero"
        );
        return None;
    }
    serde_json::from_slice::<HeartbeatReport>(&out.stdout)
        .map_err(|e| tracing::warn!(instance = %name, error = %e, "ATC heartbeat summary unparseable"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::atc_instance::RegisterAtc;

    const NOW: i64 = 1_767_225_600_000;

    async fn seed_instance(store: &Store, name: &str) {
        AtcInstanceRepo::register(
            store.pool(),
            &RegisterAtc {
                name: name.to_string(),
                cwd: "/work/atc".to_string(),
                tmux_session: Some("atc-main".to_string()),
                heartbeat_cron: "*/2 * * * *".to_string(),
                err_retry_cap: 3,
                idle_pause_min: 60,
                next_tick_at: Some(NOW + 120_000),
            },
            NOW,
        )
        .await
        .unwrap();
    }

    fn broker() -> (crate::events::EventBroker, EventSink) {
        let b = crate::events::EventBroker::new();
        let s = b.sink();
        (b, s)
    }

    #[test]
    fn err_action_escalates_at_or_over_cap() {
        assert_eq!(err_action(0, 3), ErrAction::Continue);
        assert_eq!(err_action(2, 3), ErrAction::Continue);
        assert_eq!(err_action(3, 3), ErrAction::Escalate);
        assert_eq!(err_action(9, 3), ErrAction::Escalate);
        // Cap of 0 is clamped to 1: the first ERR is continue-eligible.
        assert_eq!(err_action(0, 0), ErrAction::Continue);
        assert_eq!(err_action(1, 0), ErrAction::Escalate);
    }

    #[test]
    fn next_heartbeat_tick_is_after_now() {
        // Every-2-min cron: the next tick is strictly after now, at most 2 min out.
        let next = next_heartbeat_tick("*/2 * * * *", NOW).unwrap();
        assert!(next > NOW && next <= NOW + 120_000);
        assert!(next_heartbeat_tick("not a cron", NOW).is_none());
    }

    #[tokio::test]
    async fn escalation_becomes_an_attention_row_and_marks_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_instance(&store, "main").await;
        let (_b, sink) = broker();

        let id = raise_escalation(
            store.pool(),
            &sink,
            "main",
            "sess-1",
            "/work/x",
            None,
            "retry cap reached: overloaded_error",
            NOW,
        )
        .await
        .unwrap();

        // A durable escalation attention row exists on the fleet feed.
        let rows = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].kind, AttentionKind::Escalation);
        assert_eq!(rows[0].session_id, "sess-1");
        assert!(rows[0].payload.contains("retry cap reached"));

        // The instance ledger is marked escalated so the heartbeat won't re-continue.
        let ledger = AtcInstanceRepo::retry_get(store.pool(), "main", "sess-1")
            .await
            .unwrap()
            .unwrap();
        assert!(ledger.escalated);
    }

    fn retry_row(session: &str, count: i64, escalated: bool) -> AtcRetryRow {
        AtcRetryRow {
            instance_name: "main".into(),
            session_id: session.into(),
            continue_count: count,
            escalated,
            note: None,
            updated_at: NOW,
        }
    }

    #[test]
    fn exhausted_set_covers_at_cap_and_already_escalated() {
        let ledger = vec![
            retry_row("under", 1, false),
            retry_row("at-cap", 3, false),
            retry_row("over-cap", 9, false),
            // Escalated but the count says otherwise: the crash window between
            // record_continue and mark_escalated. The flag must still win.
            retry_row("flagged", 0, true),
        ];
        let spent = exhausted_sessions(&ledger, 3);
        assert_eq!(spent, ["at-cap", "over-cap", "flagged"]);
    }

    #[test]
    fn exhausted_set_clamps_a_zero_cap_like_the_beat_does() {
        // cap 0 would escalate before any continue was ever sent.
        let ledger = vec![retry_row("fresh", 0, false)];
        assert!(exhausted_sessions(&ledger, 0).is_empty());
    }

    #[test]
    fn report_parses_err_rows_and_tolerates_new_summary_fields() {
        // The daemon reads a SLICE of the CLI summary, so the CLI can grow fields
        // without breaking this seam.
        let raw = br#"{"action":"heartbeat","name":"tower","needs_count":2,
            "err_sessions":[{"session_id":"s1","cwd":"/w/s1","pattern":"overloaded"}],
            "delivered":true,"something_new":42}"#;
        let report: HeartbeatReport = serde_json::from_slice(raw).expect("parse");
        assert_eq!(report.err_sessions.len(), 1);
        assert_eq!(report.err_sessions[0].session_id, "s1");
        assert_eq!(report.err_sessions[0].pattern, "overloaded");
    }

    #[test]
    fn report_with_no_err_sessions_is_an_empty_roster_not_a_parse_error() {
        let report: HeartbeatReport =
            serde_json::from_slice(br#"{"action":"heartbeat"}"#).expect("parse");
        assert!(report.err_sessions.is_empty());
    }

    #[tokio::test]
    async fn escalation_is_raised_once_and_not_re_raised_while_escalated() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_instance(&store, "main").await;
        let (_b, sink) = broker();
        let sched = AtcHeartbeatScheduler::new(
            store.pool().clone(),
            sink,
            Arc::new(ainb_hangar_core::clock::SystemClock),
            CancellationToken::new(),
        );
        let inst = AtcInstanceRepo::get(store.pool(), "main").await.unwrap().unwrap();

        // Drive the session to its cap (3) so the next decision escalates.
        for _ in 0..3 {
            AtcInstanceRepo::record_continue(store.pool(), "main", "sess-1", NOW)
                .await
                .unwrap();
        }

        // First heartbeat at cap → escalates exactly once and flags the ledger.
        sched.enforce_err_cap(&inst, "sess-1", "/work/x", "overloaded_error", NOW).await;
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1,
            "a cap-reached ERR escalates once"
        );
        assert!(
            AtcInstanceRepo::retry_get(store.pool(), "main", "sess-1")
                .await
                .unwrap()
                .unwrap()
                .escalated
        );

        // Second heartbeat two minutes later, session still stuck ERR at cap → the
        // escalated flag short-circuits: NO brand-new attention row is minted.
        sched
            .enforce_err_cap(
                &inst,
                "sess-1",
                "/work/x",
                "overloaded_error",
                NOW + 120_000,
            )
            .await;
        assert_eq!(
            AttentionRepo::list_fleet(store.pool()).await.unwrap().len(),
            1,
            "an already-escalated session raises no second row on the next heartbeat"
        );
    }
}
