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

/// How long a session must stay off the ERR roster before its retry ledger row is
/// cleared. Long enough to outlive the ordinary err → continue → working → err
/// cycle (so a session failing repeatedly still accumulates toward its cap),
/// short enough that a genuinely recovered session is not carrying a stale budget
/// hours later.
const RETRY_RESET_GRACE_MS: i64 = 30 * 60 * 1000;

/// Wall-clock ceiling on one delegated beat. The beat shells `fleet needs` and
/// drives tmux, so a wedged tmux server would otherwise park this future forever
/// INSIDE the single ATC cron loop, renewing its claim and starving every other
/// instance.
const BEAT_TIMEOUT: Duration = Duration::from_secs(90);

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
        let Some(report) = run_cli_heartbeat(&ainb_bin(), &inst.name, &exhausted).await else {
            return now;
        };
        self.apply_report(inst, &ledger, &report, now).await;
        now
    }

    /// Fold one beat's report into the durable ledger.
    ///
    /// Three gates decide whether the ledger moves at all, and all three fail
    /// CLOSED — leaving the ledger untouched — because every wrong move here is
    /// destructive: a bogus advance escalates a healthy session, and a bogus reset
    /// hands a permanently-broken one a fresh budget and re-pages the human.
    async fn apply_report(
        &self,
        inst: &AtcInstanceRow,
        ledger: &[AtcRetryRow],
        report: &HeartbeatReport,
        now_ms: i64,
    ) {
        // GATE 1 — is this even our beat? A binary that does not echo the handoff
        // back never saw `--exhausted`, so it is still keeping its own local tally
        // and its roster means something else.
        if report.ledger_owner != "daemon" {
            tracing::warn!(
                instance = %inst.name,
                owner = %report.ledger_owner,
                "ATC beat did not take the daemon ledger handoff; leaving the ledger alone"
            );
            return;
        }
        // GATE 2 — did the scan behind the roster work? A failed `fleet needs`
        // degrades to an empty roster inside the beat, which would read here as
        // "the whole fleet recovered".
        if !report.roster_valid {
            tracing::warn!(
                instance = %inst.name,
                "ATC beat reported an unusable fleet roster; ledger not advanced"
            );
            return;
        }
        // GATE 3 — did the nudge actually land? The beat coalesces rather than
        // stacking pastes, and skips a dead session entirely. Spending continue
        // budget on a nudge ATC never received would escalate a session it was
        // never once asked to continue.
        if !report.delivered {
            tracing::debug!(
                instance = %inst.name,
                "ATC nudge not delivered this beat; ledger left as-is"
            );
            return;
        }

        // Advance: escalation needs the store and the event sink, so it stays here.
        for err in &report.err_sessions {
            self.enforce_err_cap(inst, &err.session_id, &err.cwd, &err.pattern, now_ms)
                .await;
        }

        // Recovery is ABSENCE, but only once it has held. A row is cleared when the
        // session has been off the ERR roster for the whole grace window, measured
        // from the last time the ledger moved for it.
        //
        // Resetting on the FIRST absence would make the cap unreachable: the normal
        // shape is err, continue, working again on the next beat, so the row would
        // be deleted before a second failure could ever accumulate against it, and a
        // session failing every few minutes forever would never escalate. Holding
        // the row keeps that history, while a genuinely recovered session ages out
        // and gets its fresh budget.
        for row in ledger {
            let absent = !report.err_sessions.iter().any(|e| e.session_id == row.session_id);
            let held = now_ms.saturating_sub(row.updated_at) >= RETRY_RESET_GRACE_MS;
            if absent && held {
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
        .filter(|row| {
            row.escalated || matches!(err_action(row.continue_count, cap), ErrAction::Escalate)
        })
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
    /// Whether the fleet scan behind `err_sessions` actually succeeded. A failed
    /// scan degrades to an EMPTY roster inside the beat, which is indistinguishable
    /// from a healthy quiet fleet — and acting on it would reset the ledger for
    /// every session that is still broken. Defaults true so a summary that parsed
    /// at all is trusted; the `ledger_owner` tripwire below catches the version
    /// where the field does not exist yet.
    #[serde(default = "yes")]
    roster_valid: bool,
    /// Whether the nudge actually reached the ATC session. False when the composer
    /// still held an unsubmitted nudge (coalesced) or the session was gone.
    #[serde(default)]
    delivered: bool,
    /// Must read `daemon` — it is the beat echoing back that it saw `--exhausted`
    /// and stood down its own counting. Anything else means the spawned binary is
    /// not the one this daemon shipped with.
    #[serde(default)]
    ledger_owner: String,
}

/// serde default for [`HeartbeatReport::roster_valid`].
const fn yes() -> bool {
    true
}

/// Resolve the `ainb` binary the beat runs as.
///
/// NOT `current_exe()`. The daemon is usually its own binary: `hangar daemon
/// start` prefers a sibling `ainb-hangar-daemon` whenever one exists and is at
/// least as new as `ainb` (`resolve_daemon_launch_for`), which is exactly what a
/// workspace build produces. Self-exec'ing that would spawn a binary whose clap
/// surface is `--once` + `beads`, it would exit non-zero on the heartbeat argv,
/// and ATC would go silently dead — strictly worse than the weak nudge this
/// delegation replaces.
///
/// So: `AINB_BIN` override first, then the sibling `ainb` beside this executable
/// (the layout both the workspace build and the release tarball produce, and the
/// one that keeps a rebuild or a `brew upgrade` moving both together), then
/// `ainb` on `$PATH`.
fn ainb_bin() -> String {
    if let Some(pinned) = std::env::var("AINB_BIN").ok().filter(|s| !s.is_empty()) {
        return pinned;
    }
    if let Some(sibling) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("ainb")))
        .filter(|sibling| sibling.exists())
        .and_then(|sibling| sibling.to_str().map(str::to_string))
    {
        return sibling;
    }
    "ainb".to_string()
}

/// Run one delegated beat and parse what it saw.
///
/// Non-fatal end to end, like everything else on this tick: a missing binary, a
/// non-zero exit (an instance never provisioned on this host), or unparseable
/// stdout all yield `None`, which leaves the ledger untouched and reschedules
/// normally. Returning an empty report instead would read as "nothing is
/// erroring" and wrongly clear every session's budget.
async fn run_cli_heartbeat(bin: &str, name: &str, exhausted: &[String]) -> Option<HeartbeatReport> {
    let spawn = tokio::process::Command::new(bin)
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
        .output();
    let out = match tokio::time::timeout(BEAT_TIMEOUT, spawn).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => {
            tracing::warn!(instance = %name, error = %e, "ATC heartbeat spawn failed");
            return None;
        }
        Err(_) => {
            tracing::warn!(
                instance = %name,
                timeout_s = BEAT_TIMEOUT.as_secs(),
                "ATC heartbeat timed out; abandoning this tick so other instances still fire"
            );
            return None;
        }
    };
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
        .map_err(
            |e| tracing::warn!(instance = %name, error = %e, "ATC heartbeat summary unparseable"),
        )
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

    /// Write an executable stub that stands in for the real `ainb` binary: it
    /// appends its argv to `argv_log` and prints `summary` as the beat's JSON.
    /// `run_cli_heartbeat` resolves the binary through `AINB_BIN`, which is the
    /// seam that makes the delegation testable without a real fleet.
    fn ainb_stub(
        dir: &std::path::Path,
        argv_log: &std::path::Path,
        summary: &str,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let stub = dir.join("ainb-stub.sh");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncat <<'ATCJSON'\n{summary}\nATCJSON\n",
                argv_log.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        stub
    }

    /// A healthy beat report, as the CLI emits it.
    fn report(err_sessions: &str) -> String {
        format!(
            r#"{{"action":"heartbeat","ledger_owner":"daemon","roster_valid":true,
                 "delivered":true,"err_sessions":[{err_sessions}]}}"#
        )
    }

    async fn scheduler(store: &Store) -> AtcHeartbeatScheduler {
        let (_b, sink) = broker();
        AtcHeartbeatScheduler::new(
            store.pool().clone(),
            sink,
            Arc::new(ainb_hangar_core::clock::SystemClock),
            CancellationToken::new(),
        )
    }

    /// The spent set goes OUT on the command line and the roster comes BACK, which
    /// is the whole delegation contract. Driven against a stub binary passed
    /// explicitly, so no environment variable is mutated and nothing races the
    /// other tests in this binary.
    #[tokio::test]
    async fn beat_receives_the_spent_set_and_returns_its_roster() {
        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.txt");
        let stub = ainb_stub(
            dir.path(),
            &argv_log,
            &report(r#"{"session_id":"s1","cwd":"/w/s1","pattern":"overloaded"}"#),
        );

        let got = run_cli_heartbeat(stub.to_str().unwrap(), "main", &["spent".to_string()])
            .await
            .expect("stub beat should parse");

        let argv = std::fs::read_to_string(&argv_log).unwrap();
        assert!(
            argv.contains("fleet atc heartbeat main"),
            "wrong verb delegated: {argv}"
        );
        assert!(
            argv.contains("--exhausted spent"),
            "spent set not handed over: {argv}"
        );
        assert_eq!(got.err_sessions.len(), 1);
        assert!(got.delivered && got.roster_valid);
    }

    /// An empty spent set must still pass the flag: its PRESENCE is what tells the
    /// beat to stand down its own counting.
    #[tokio::test]
    async fn beat_is_told_the_daemon_owns_the_ledger_even_with_nothing_spent() {
        let dir = tempfile::tempdir().unwrap();
        let argv_log = dir.path().join("argv.txt");
        let stub = ainb_stub(dir.path(), &argv_log, &report(""));

        run_cli_heartbeat(stub.to_str().unwrap(), "main", &[]).await.expect("parse");

        let argv = std::fs::read_to_string(&argv_log).unwrap();
        assert!(
            argv.contains("--exhausted"),
            "flag dropped when nothing is spent: {argv}"
        );
    }

    /// A beat that never returns must not park the cron loop for every instance.
    #[tokio::test]
    async fn a_wedged_beat_is_abandoned_rather_than_parking_the_loop() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hang.sh");
        std::fs::write(&stub, "#!/bin/sh\nsleep 600\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        tokio::time::pause();
        let beat =
            tokio::spawn(
                async move { run_cli_heartbeat(stub.to_str().unwrap(), "main", &[]).await },
            );
        tokio::time::advance(BEAT_TIMEOUT + Duration::from_secs(1)).await;
        assert!(
            beat.await.unwrap().is_none(),
            "a hung beat must yield no report"
        );
    }

    #[tokio::test]
    async fn report_advances_the_ledger_and_escalates_at_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_instance(&store, "main").await;
        let sched = scheduler(&store).await;
        let inst = AtcInstanceRepo::get(store.pool(), "main").await.unwrap().unwrap();

        for _ in 0..3 {
            AtcInstanceRepo::record_continue(store.pool(), "main", "spent", NOW)
                .await
                .unwrap();
        }
        let parsed: HeartbeatReport = serde_json::from_str(&report(
            r#"{"session_id":"spent","cwd":"/w/s","pattern":"overloaded"},
               {"session_id":"fresh","cwd":"/w/f","pattern":"rate_limited"}"#,
        ))
        .unwrap();
        sched.apply_report(&inst, &[], &parsed, NOW).await;

        let fresh = AtcInstanceRepo::retry_get(store.pool(), "main", "fresh")
            .await
            .unwrap()
            .expect("a newly erroring session gets a row");
        assert_eq!(fresh.continue_count, 1);
        assert!(!fresh.escalated);

        let spent = AtcInstanceRepo::retry_get(store.pool(), "main", "spent")
            .await
            .unwrap()
            .unwrap();
        assert!(
            spent.escalated,
            "a session at the cap escalates rather than continuing"
        );
        assert_eq!(
            spent.continue_count, 3,
            "escalating must not spend more budget"
        );
    }

    /// Recovery clears the budget, but only after the absence has HELD. Resetting
    /// on first absence would make the cap unreachable, because the ordinary cycle
    /// is err → continue → working again on the very next beat.
    #[tokio::test]
    async fn recovery_resets_only_after_the_grace_window() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_instance(&store, "main").await;
        let sched = scheduler(&store).await;
        let inst = AtcInstanceRepo::get(store.pool(), "main").await.unwrap().unwrap();

        AtcInstanceRepo::record_continue(store.pool(), "main", "flaky", NOW)
            .await
            .unwrap();
        let ledger = AtcInstanceRepo::retry_list(store.pool(), "main").await.unwrap();
        let quiet: HeartbeatReport = serde_json::from_str(&report("")).unwrap();

        // One beat later it is off the roster, but the row must survive so a
        // repeat failure still counts against the same budget.
        sched.apply_report(&inst, &ledger, &quiet, NOW + 60_000).await;
        assert!(
            AtcInstanceRepo::retry_get(store.pool(), "main", "flaky")
                .await
                .unwrap()
                .is_some(),
            "budget cleared on first absence — a flapping session would never escalate"
        );

        // Still clear once the absence has outlived the grace window.
        sched.apply_report(&inst, &ledger, &quiet, NOW + RETRY_RESET_GRACE_MS).await;
        assert!(
            AtcInstanceRepo::retry_get(store.pool(), "main", "flaky")
                .await
                .unwrap()
                .is_none(),
            "a genuinely recovered session must get a fresh budget"
        );
    }

    /// All three gates fail closed. Each would otherwise corrupt the ledger in a
    /// way that pages a human or frees a broken session.
    #[tokio::test]
    async fn a_degraded_beat_never_moves_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_instance(&store, "main").await;
        let sched = scheduler(&store).await;
        let inst = AtcInstanceRepo::get(store.pool(), "main").await.unwrap().unwrap();

        AtcInstanceRepo::record_continue(store.pool(), "main", "keep", NOW)
            .await
            .unwrap();
        let ledger = AtcInstanceRepo::retry_list(store.pool(), "main").await.unwrap();
        let erroring = r#"{"session_id":"new","cwd":"/w/n","pattern":"overloaded"}"#;

        for (label, raw) in [
            // The scan failed, so the empty roster is not evidence of recovery.
            (
                "scan failed",
                format!(
                    r#"{{"ledger_owner":"daemon","roster_valid":false,"delivered":true,"err_sessions":[{erroring}]}}"#
                ),
            ),
            // The nudge never landed, so ATC was never asked to continue anything.
            (
                "not delivered",
                format!(
                    r#"{{"ledger_owner":"daemon","roster_valid":true,"delivered":false,"err_sessions":[{erroring}]}}"#
                ),
            ),
            // A binary that never took the handoff is still counting locally.
            (
                "handoff refused",
                format!(
                    r#"{{"ledger_owner":"local","roster_valid":true,"delivered":true,"err_sessions":[{erroring}]}}"#
                ),
            ),
        ] {
            let parsed: HeartbeatReport = serde_json::from_str(&raw).unwrap();
            sched.apply_report(&inst, &ledger, &parsed, NOW + RETRY_RESET_GRACE_MS).await;
            assert!(
                AtcInstanceRepo::retry_get(store.pool(), "main", "new").await.unwrap().is_none(),
                "{label}: budget spent on a beat that should have been ignored"
            );
            assert!(
                AtcInstanceRepo::retry_get(store.pool(), "main", "keep")
                    .await
                    .unwrap()
                    .is_some(),
                "{label}: ledger reset from a beat that should have been ignored"
            );
        }
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
