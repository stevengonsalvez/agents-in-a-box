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

use ainb_fleet_core::read::needs::NeedsContext;
use ainb_fleet_core::send::send;
use ainb_fleet_core::types::{SendOutcome, Session};
use ainb_hangar_core::clock::HangarClock;
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::atc_instance::{AtcInstanceRepo, AtcInstanceRow};
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
    use ainb_hangar_core::autopilot::cron::{millis_to_utc, next_tick_after, parse_cron, utc_to_millis};
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
            let next = match AtcInstanceRepo::list_schedulable(&self.pool).await {
                Ok(rows) => rows.into_iter().min_by_key(|r| r.next_tick_at.unwrap_or(i64::MAX)),
                Err(e) => {
                    tracing::error!(error = %e, "ATC heartbeat cron: load failed; re-polling");
                    None
                }
            };
            let (delay, fire_target) = match next {
                Some(inst) => {
                    let tick = inst.next_tick_at.unwrap_or_else(|| self.clock.now_ms());
                    (sleep_delay(tick, self.clock.now_ms()), Some(inst))
                }
                None => (NO_WORK_REPOLL, None),
            };
            tokio::select! {
                () = self.shutdown.cancelled() => break,
                () = tokio::time::sleep(delay) => {}
            }
            if let Some(inst) = fire_target {
                self.fire(&inst).await;
                self.reschedule(&inst).await;
            }
        }
        tracing::info!("ATC heartbeat cron stopped");
    }

    /// Fire one instance's heartbeat: read the fleet needs, enforce the retry cap
    /// against the durable ledger (escalating exhausted ERR sessions through the
    /// attention pipeline), build the compact nudge, send it into the ATC session
    /// via the one verified send path, and stamp `last_heartbeat_at`.
    ///
    /// Non-fatal end to end: an unspawned instance (no tmux target) or a send
    /// fault is warned and skipped, never a panic.
    async fn fire(&self, inst: &AtcInstanceRow) {
        let now = self.clock.now_ms();
        let rows = probe_fleet_needs(now).await;

        // Enforce the retry cap: exhausted ERR sessions escalate; the rest consume
        // one unit of continue budget.
        for r in &rows {
            if let NeedsContext::Err(err) = &r.context {
                let count = AtcInstanceRepo::retry_get(&self.pool, &inst.name, &r.session.id)
                    .await
                    .ok()
                    .flatten()
                    .map_or(0, |row| row.continue_count);
                match err_action(count, inst.err_retry_cap) {
                    ErrAction::Escalate => {
                        let reason = format!("retry cap reached: {}", err.pattern);
                        let _ = raise_escalation(
                            &self.pool,
                            &self.events,
                            &inst.name,
                            &r.session.id,
                            &r.session.cwd,
                            None,
                            &reason,
                            now,
                        )
                        .await;
                    }
                    ErrAction::Continue => {
                        let _ = AtcInstanceRepo::record_continue(
                            &self.pool,
                            &inst.name,
                            &r.session.id,
                            now,
                        )
                        .await;
                    }
                }
            }
        }
        // Deliver the compact nudge into the ATC session (if it is spawned).
        if let Some(tmux) = &inst.tmux_session {
            let body = build_nudge(&rows, now);
            let target = atc_send_target(&inst.name, &inst.cwd, tmux);
            match send(&target, &body).await {
                Ok(SendOutcome::Tmux { .. } | SendOutcome::Broker { .. }) => {}
                Ok(SendOutcome::Failed { reason }) => {
                    tracing::warn!(instance = %inst.name, %reason, "ATC heartbeat send failed");
                }
                Err(e) => {
                    tracing::warn!(instance = %inst.name, error = %e, "ATC heartbeat send failed");
                }
            }
        } else {
            tracing::debug!(instance = %inst.name, "ATC heartbeat: no tmux target; nudge not sent");
        }

        let _ = AtcInstanceRepo::mark_heartbeat(&self.pool, &inst.name, now).await;
    }

    /// Recompute + persist the instance's next heartbeat tick from the fired slot.
    async fn reschedule(&self, inst: &AtcInstanceRow) {
        let next = recompute_next_tick(&inst.heartbeat_cron, inst.next_tick_at, self.clock.now_ms());
        if let Err(e) = AtcInstanceRepo::set_next_tick(&self.pool, &inst.name, next).await {
            tracing::error!(instance = %inst.name, error = %e, "ATC heartbeat reschedule failed");
        }
    }

    /// Spawn the heartbeat cron on a background task with the system clock — boot's
    /// fire-and-forget entry point.
    #[must_use]
    pub fn spawn(pool: SqlitePool, events: EventSink) -> tokio::task::JoinHandle<()> {
        use ainb_hangar_core::clock::SystemClock;
        let sched = Self::new(pool, events, Arc::new(SystemClock), CancellationToken::new());
        tokio::spawn(sched.run())
    }
}

/// Build the verified-send target for an ATC instance's tmux session.
fn atc_send_target(name: &str, cwd: &str, tmux_session: &str) -> Session {
    Session {
        id: format!("atc:{name}"),
        cwd: cwd.to_string(),
        pid: None,
        git_root: None,
        tmux_session: Some(tmux_session.to_string()),
        workspace_name: None,
        worktree_path: None,
        peer_id: None,
        bg_job_id: None,
        transcript_path: None,
        sources: vec![ainb_fleet_core::types::SessionSource::Ainb],
        summary: None,
        last_seen_ms: None,
    }
}

/// Build the compact `[HEARTBEAT]` nudge from the current fleet-needs rows. A
/// daemon-local, single-line builder (the daemon cannot depend on `ainb-core`'s
/// richer builder without a crate cycle); the retry-cap accounting now lives in
/// the store, so this body only summarises the live state.
fn build_nudge(rows: &[NeedsContextRow], now_ms: i64) -> String {
    if rows.is_empty() {
        return format!(
            "[HEARTBEAT {now_ms}] fleet quiet — 0 sessions need attention. Stand by."
        );
    }
    let (mut ask, mut err, mut idle, mut wait) = (0u32, 0u32, 0u32, 0u32);
    for r in rows {
        match r.context {
            NeedsContext::Ask(_) => ask += 1,
            NeedsContext::Err(_) => err += 1,
            NeedsContext::Idle(_) => idle += 1,
            NeedsContext::Wait(_) => wait += 1,
        }
    }
    format!(
        "[HEARTBEAT {now_ms}] {} session(s) need attention — ERR {err} · ASK {ask} · \
IDLE {idle} · WAIT {wait}. Apply the ATC playbook (CLAUDE.md).",
        rows.len()
    )
}

/// The subset of a `NeedsRow` the daemon heartbeat cares about (id + cwd +
/// classified context). Alias kept local so the nudge builder + fire path share
/// one shape.
type NeedsContextRow = ainb_fleet_core::read::needs::NeedsRow;

/// Discover + classify the whole fleet for the heartbeat. Best-effort: any fault
/// yields an empty roster (a quiet heartbeat), never a panic.
async fn probe_fleet_needs(now_ms: i64) -> Vec<NeedsContextRow> {
    use ainb_fleet_core::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
    use ainb_fleet_core::read::needs::{ClassifyInput, classify};

    let ainb: Vec<Session> = discover_from_ainb().await.unwrap_or_default();
    let peers: Vec<Session> = tokio::task::spawn_blocking(discover_from_peers)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let merged = merge_sessions(vec![ainb, peers]);

    let mut out = Vec::new();
    for session in merged {
        let s = session.clone();
        if let Some(row) = tokio::task::spawn_blocking(move || {
            classify(ClassifyInput::from_env(s, None, now_ms))
        })
        .await
        .ok()
        .flatten()
        {
            out.push(row);
        }
    }
    out
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

    #[test]
    fn nudge_summarises_counts_and_is_quiet_when_empty() {
        assert!(build_nudge(&[], NOW).contains("fleet quiet"));
    }
}
