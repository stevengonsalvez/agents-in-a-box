//! Auto-standup (D13, Stevie's LOCKED override) — the daemon watcher that WRITES
//! `/standup` into a stagnant, idle-at-prompt session (architecture §4.8, spec P9).
//!
//! The feature is a write-mode override: it types a real `/standup` into another
//! agent's session. That is only safe behind EVERY guardrail, so the gate here is
//! deliberately a pure, exhaustively-tested decision function — the mechanism the
//! reviewer and the tripwires assert against:
//!
//! ```text
//!  global ON? ─no─▶ Skip(GloballyDisabled)
//!    │yes
//!  opted out? ─yes─▶ Skip(OptedOut)
//!    │no
//!  idle-at-prompt? ─no─▶ Skip(NotIdleAtPrompt)   ← a BUSY session NEVER fires
//!    │yes                                            (hook status, never pane,
//!  stagnant≥N? ─no─▶ Skip(NotStagnant)               never mid-turn)
//!    │yes
//!  in flight? ─yes─▶ Skip(InFlight)
//!    │no
//!  in cooldown? ─yes─▶ Skip(Cooldown)             ← fires once, then 60-min quiet
//!    │no
//!  at concurrency cap? ─yes─▶ Skip(ConcurrencyLimit)  ← max 1 concurrent
//!    │no
//!    ▼
//!   FIRE
//! ```
//!
//! # The two invariants that matter most
//!
//! 1. **A busy session is never written to.** The `idle_at_prompt` input is
//!    HOOK-derived (a finished turn), never a pane heuristic and never mid-turn.
//!    The gate short-circuits on it before any stagnation / cooldown math, so no
//!    combination of the other inputs can make a mid-turn session fire.
//! 2. **Fire once, then cool down.** The fire path stamps `last_standup_at` (the
//!    cooldown anchor) AND `in_flight` in one write, so the very next evaluation
//!    of the same session is blocked twice over (InFlight, then Cooldown) until
//!    the window elapses.
//!
//! The watcher loop reuses the autopilot scheduler's shape (a periodic,
//! cancellation-token-exitable tokio task) and is non-fatal: a discovery / send /
//! store fault is warned and degraded, never a panic and never a daemon-down
//! (external-dep-warning rule).

use std::sync::Arc;
use std::time::Duration;

use ainb_fleet_core::send::send;
use ainb_fleet_core::types::{SendOutcome, Session};
use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_proto::events::HangarEvent;
use ainb_hangar_store::repo::attention::{AttentionKind, AttentionRepo, NewAttention};
use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;
use ainb_hangar_store::repo::standup::StandupRepo;
use sqlx::SqlitePool;
use tokio_util::sync::CancellationToken;

use crate::events::EventSink;

/// `daemon_config` key: the global auto-standup toggle (default ON per Stevie).
pub const KEY_ENABLED: &str = "autostandup.enabled";
/// `daemon_config` key: minutes a session must be stagnant before firing.
pub const KEY_STAGNANT_MIN: &str = "autostandup.stagnant_min";
/// `daemon_config` key: per-session cooldown window in minutes.
pub const KEY_COOLDOWN_MIN: &str = "autostandup.cooldown_min";
/// `daemon_config` key: max simultaneous in-flight standups.
pub const KEY_MAX_CONCURRENT: &str = "autostandup.max_concurrent";

/// Coded default: auto-standup is ON (Stevie's override).
pub const DEFAULT_ENABLED: bool = true;
/// Coded default: 15 minutes stagnant.
pub const DEFAULT_STAGNANT_MIN: i64 = 15;
/// Coded default: 60-minute per-session cooldown.
pub const DEFAULT_COOLDOWN_MIN: i64 = 60;
/// Coded default: at most one concurrent standup.
pub const DEFAULT_MAX_CONCURRENT: i64 = 1;

/// How often the watcher scans the fleet for standup candidates.
const TICK: Duration = Duration::from_secs(60);

/// Stale-slot reclaim bound, in cooldown windows. A fired standup whose
/// completion is never observed (a crash between `mark_fired` and the turn, a
/// session that took the keystrokes but never produced a fresh end-turn, or a
/// peer/unhooked session `classify` can no longer return as Idle) would pin the
/// single concurrency slot forever and wedge auto-standup fleet-wide. Once the
/// fire is older than this many cooldown windows we release the slot even with no
/// completion turn, so the fleet always recovers.
const STALE_INFLIGHT_COOLDOWN_MULTIPLE: i64 = 3;

/// The message written into a stagnant session — a bare `/standup` slash command.
const STANDUP_COMMAND: &str = "/standup";

/// The resolved global auto-standup configuration.
///
/// Loaded from `daemon_config` with coded defaults, so a fresh daemon (or one
/// with no config rows) runs with auto-standup ON at the D13 thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StandupConfig {
    /// The global toggle (default ON).
    pub enabled: bool,
    /// Minutes a session must be stagnant before a standup fires.
    pub stagnant_minutes: i64,
    /// Per-session cooldown window in minutes.
    pub cooldown_minutes: i64,
    /// Maximum simultaneous in-flight standups (the concurrency cap).
    pub max_concurrent: i64,
}

impl Default for StandupConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_ENABLED,
            stagnant_minutes: DEFAULT_STAGNANT_MIN,
            cooldown_minutes: DEFAULT_COOLDOWN_MIN,
            max_concurrent: DEFAULT_MAX_CONCURRENT,
        }
    }
}

impl StandupConfig {
    /// Load the config from `daemon_config`, falling back to the coded defaults
    /// key-by-key (a missing/garbage value never errors — the default holds).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] only if the underlying config reads fail.
    pub async fn load(pool: &SqlitePool) -> Result<Self, sqlx::Error> {
        Ok(Self {
            enabled: DaemonConfigRepo::get_bool(pool, KEY_ENABLED, DEFAULT_ENABLED).await?,
            stagnant_minutes: DaemonConfigRepo::get_i64(
                pool,
                KEY_STAGNANT_MIN,
                DEFAULT_STAGNANT_MIN,
            )
            .await?,
            cooldown_minutes: DaemonConfigRepo::get_i64(
                pool,
                KEY_COOLDOWN_MIN,
                DEFAULT_COOLDOWN_MIN,
            )
            .await?,
            max_concurrent: DaemonConfigRepo::get_i64(
                pool,
                KEY_MAX_CONCURRENT,
                DEFAULT_MAX_CONCURRENT,
            )
            .await?,
        })
    }
}

/// A monitored session's state fed to the pure gate.
#[derive(Debug, Clone)]
pub struct SessionGateInput {
    /// HOOK-derived: the session's last turn ended and it is idle at the prompt.
    /// NEVER a pane heuristic, NEVER mid-turn — the sacred guard.
    pub idle_at_prompt: bool,
    /// How long the session has been stagnant, in minutes.
    pub idle_minutes: i64,
    /// The per-session opt-out (a hard guardrail).
    pub opted_out: bool,
    /// The cooldown anchor: epoch-ms the last standup fired, or `None`.
    pub last_standup_at: Option<i64>,
    /// Whether this session already has a standup turn in flight.
    pub in_flight: bool,
}

/// The gate's decision for one session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandupDecision {
    /// Write `/standup` into the session.
    Fire,
    /// Do not fire, with the guardrail that blocked it.
    Skip(SkipReason),
}

/// Why a standup was NOT fired — one variant per guardrail, so a test (and a log
/// line) names the exact gate that held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The global toggle is off.
    GloballyDisabled,
    /// The human opted this session out.
    OptedOut,
    /// The session is busy / mid-turn — a standup would interrupt it.
    NotIdleAtPrompt,
    /// The session has not been stagnant long enough yet.
    NotStagnant,
    /// A standup for this session is already in flight.
    InFlight,
    /// The session fired a standup within the cooldown window.
    Cooldown,
    /// The daemon is already running its max concurrent standups.
    ConcurrencyLimit,
}

/// The pure auto-standup gate — the exhaustively-tested heart of D13.
///
/// Applies every guardrail in the order that makes each one un-bypassable: the
/// idle-at-prompt guard fires before any stagnation / cooldown math, so a busy
/// session can never be talked into a standup by the other inputs.
///
/// `concurrent_in_flight` is the daemon-global count of currently in-flight
/// standups (across all sessions); `now_ms` is the injected clock instant the
/// cooldown is measured against.
#[must_use]
pub fn decide_standup(
    cfg: &StandupConfig,
    input: &SessionGateInput,
    concurrent_in_flight: i64,
    now_ms: i64,
) -> StandupDecision {
    // 1. Global toggle.
    if !cfg.enabled {
        return StandupDecision::Skip(SkipReason::GloballyDisabled);
    }
    // 2. Per-session opt-out (hard guardrail).
    if input.opted_out {
        return StandupDecision::Skip(SkipReason::OptedOut);
    }
    // 3. THE sacred guard: a busy / mid-turn session is NEVER written to. Checked
    //    before stagnation so no other input can bypass it.
    if !input.idle_at_prompt {
        return StandupDecision::Skip(SkipReason::NotIdleAtPrompt);
    }
    // 4. Stagnation threshold.
    if input.idle_minutes < cfg.stagnant_minutes {
        return StandupDecision::Skip(SkipReason::NotStagnant);
    }
    // 5. Already mid-standup for this session.
    if input.in_flight {
        return StandupDecision::Skip(SkipReason::InFlight);
    }
    // 6. Per-session cooldown: fired once, stay quiet for the window.
    if let Some(last) = input.last_standup_at {
        let cooldown_ms = cfg.cooldown_minutes.saturating_mul(60_000);
        if now_ms.saturating_sub(last) < cooldown_ms {
            return StandupDecision::Skip(SkipReason::Cooldown);
        }
    }
    // 7. Global concurrency cap (max 1).
    if concurrent_in_flight >= cfg.max_concurrent {
        return StandupDecision::Skip(SkipReason::ConcurrencyLimit);
    }
    StandupDecision::Fire
}

/// One idle-at-prompt session observed by the watcher's probe — everything the
/// gate + the fire path need for a single candidate.
#[derive(Debug, Clone)]
pub struct IdleObservation {
    /// The session (the verified-send target).
    pub session: Session,
    /// HOOK-derived idle-at-prompt flag.
    pub idle_at_prompt: bool,
    /// Stagnation in minutes.
    pub idle_minutes: i64,
    /// Epoch-ms of the session's latest end-turn — the completion signal: a turn
    /// that ended AFTER the fire means the standup ran.
    pub last_turn_end_ms: Option<i64>,
    /// The owning workspace, or `None` for a host session.
    pub workspace_id: Option<String>,
}

/// Evaluate one observed session against the store's per-session state + the
/// global config, returning the gate decision.
///
/// Reads the session's `standup_session` row (opt-out / cooldown anchor /
/// in-flight) — an absent row is the coded default (never opted out, no cooldown,
/// not in flight) — and applies [`decide_standup`].
///
/// # Errors
///
/// Returns a [`sqlx::Error`] if the store read fails.
pub async fn evaluate_session(
    pool: &SqlitePool,
    cfg: &StandupConfig,
    obs: &IdleObservation,
    concurrent_in_flight: i64,
    now_ms: i64,
) -> Result<StandupDecision, sqlx::Error> {
    let row = StandupRepo::get(pool, &obs.session.id).await?;
    let input = SessionGateInput {
        idle_at_prompt: obs.idle_at_prompt,
        idle_minutes: obs.idle_minutes,
        opted_out: row.as_ref().is_some_and(|r| r.opted_out),
        last_standup_at: row.as_ref().and_then(|r| r.last_standup_at),
        in_flight: row.as_ref().is_some_and(|r| r.in_flight),
    };
    Ok(decide_standup(cfg, &input, concurrent_in_flight, now_ms))
}

/// The auto-standup watcher: a daemon-global periodic scan that fires `/standup`
/// into stagnant idle-at-prompt sessions, subject to every D13 guardrail.
pub struct StandupWatcher {
    pool: SqlitePool,
    events: EventSink,
    clock: Arc<dyn HangarClock>,
    shutdown: CancellationToken,
}

impl StandupWatcher {
    /// Build a watcher over the store, the event sink, an injected clock, and a
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

    /// Run the watcher until the shutdown token trips. Each tick discovers the
    /// idle-at-prompt fleet, runs the completion pass, then the fire pass. Every
    /// fault degrades to a warn (never downs the daemon).
    pub async fn run(self) {
        tracing::info!("auto-standup watcher started");
        let mut ticker = tokio::time::interval(TICK);
        loop {
            tokio::select! {
                () = self.shutdown.cancelled() => break,
                _ = ticker.tick() => {
                    let now = self.clock.now_ms();
                    let observations = probe_idle_fleet(now).await;
                    self.tick(&observations, now).await;
                }
            }
        }
        tracing::info!("auto-standup watcher stopped");
    }

    /// One watcher pass over a set of observations: first close out any completed
    /// standups (raising the "standup ready" attention row), then fire on the
    /// candidates the gate approves. Split from [`run`](Self::run) so the loop
    /// body is unit-testable with injected observations + an injected clock.
    ///
    /// Returns the number of standups fired this pass (for tests + logs).
    pub async fn tick(&self, observations: &[IdleObservation], now_ms: i64) -> usize {
        let cfg = match StandupConfig::load(&self.pool).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "auto-standup: config load failed; skipping tick");
                return 0;
            }
        };

        // Completion pass runs regardless of the toggle so an in-flight standup
        // that finishes is always closed out (and its slot released).
        self.close_completed(observations, now_ms).await;

        if !cfg.enabled {
            return 0;
        }

        let mut fired = 0;
        for obs in observations {
            // Re-read the concurrency count each iteration so a fire earlier in
            // this pass counts against the cap for the next candidate.
            let concurrent = match StandupRepo::count_in_flight(&self.pool).await {
                Ok(n) => n,
                Err(e) => {
                    tracing::warn!(error = %e, "auto-standup: in-flight count failed");
                    return fired;
                }
            };
            match evaluate_session(&self.pool, &cfg, obs, concurrent, now_ms).await {
                Ok(StandupDecision::Fire) => {
                    if self.fire(obs, now_ms).await {
                        fired += 1;
                    }
                }
                Ok(StandupDecision::Skip(reason)) => {
                    tracing::debug!(
                        session = %obs.session.id,
                        ?reason,
                        "auto-standup: skipped"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, session = %obs.session.id, "auto-standup: evaluate failed");
                }
            }
        }
        fired
    }

    /// Fire one standup: stamp the store (cooldown anchor + in-flight) FIRST, then
    /// write `/standup` via the one verified send path. A send fault releases the
    /// in-flight slot (so the cap is not leaked) but keeps the cooldown anchor so
    /// the daemon does not hammer a flaky session. Returns whether the send landed.
    async fn fire(&self, obs: &IdleObservation, now_ms: i64) -> bool {
        // Stamp before sending so a crash between the two never re-fires (the
        // cooldown already blocks it on restart).
        if let Err(e) = StandupRepo::mark_fired(&self.pool, &obs.session.id, now_ms).await {
            tracing::warn!(error = %e, session = %obs.session.id, "auto-standup: mark_fired failed");
            return false;
        }
        // Release the concurrency slot on a send fault but keep the cooldown so
        // the daemon does not hammer a flaky session (external-dep-warning: warn
        // + degrade, never hard-fail).
        let reason = match send(&obs.session, STANDUP_COMMAND).await {
            Ok(SendOutcome::Tmux { tmux_session }) => {
                tracing::info!(session = %obs.session.id, tmux = %tmux_session, "auto-standup fired /standup");
                return true;
            }
            Ok(SendOutcome::Broker { peer_id }) => {
                tracing::info!(session = %obs.session.id, peer = %peer_id, "auto-standup fired /standup");
                return true;
            }
            Ok(SendOutcome::Failed { reason }) => reason,
            Err(e) => e.to_string(),
        };
        tracing::warn!(session = %obs.session.id, %reason, "auto-standup: /standup send failed; releasing slot");
        let _ = StandupRepo::clear_in_flight(&self.pool, &obs.session.id, now_ms).await;
        false
    }

    /// Close out standups whose session has finished its turn since the fire:
    /// clear the in-flight marker (releasing the slot) and raise a `waiting`
    /// attention row so the surfaces show "standup ready".
    async fn close_completed(&self, observations: &[IdleObservation], now_ms: i64) {
        let in_flight = match StandupRepo::list_in_flight(&self.pool).await {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, "auto-standup: list in-flight failed");
                return;
            }
        };
        // The stale-slot bound: a few cooldown windows past the fire. Loaded from
        // config so an operator-tuned cooldown scales the reclaim window with it;
        // a config-load fault degrades to the coded default cooldown.
        let cooldown_min = StandupConfig::load(&self.pool)
            .await
            .map_or(DEFAULT_COOLDOWN_MIN, |c| c.cooldown_minutes);
        let stale_ms = cooldown_min
            .saturating_mul(60_000)
            .saturating_mul(STALE_INFLIGHT_COOLDOWN_MULTIPLE);
        for row in in_flight {
            // Completion = the session ended a turn AFTER the standup fired. Anchor
            // on last_turn_end_ms > last_standup_at so we never mistake the pre-fire
            // idle state (or a still-running standup) for a finished one.
            let Some(fire_at) = row.last_standup_at else {
                continue;
            };
            let completed =
                observations.iter().find(|o| o.session.id == row.session_id).is_some_and(|o| {
                    o.idle_at_prompt && o.last_turn_end_ms.is_some_and(|t| t > fire_at)
                });
            // Stale-slot reclaim: if completion is never observed, release the slot
            // once the fire is older than the bound so one missed completion cannot
            // wedge auto-standup fleet-wide (DEFAULT_MAX_CONCURRENT = 1).
            let stale = now_ms.saturating_sub(fire_at) >= stale_ms;
            if !completed && !stale {
                continue;
            }
            if let Err(e) = StandupRepo::clear_in_flight(&self.pool, &row.session_id, now_ms).await
            {
                tracing::warn!(error = %e, session = %row.session_id, "auto-standup: clear in-flight failed");
                continue;
            }
            // Only a genuinely-observed completion surfaces a "standup ready" row;
            // a stale reclaim saw no finished turn, so it silently frees the slot.
            if completed {
                self.raise_standup_ready(&row.session_id, observations, now_ms).await;
            } else {
                tracing::warn!(
                    session = %row.session_id,
                    "auto-standup: reclaimed a stale in-flight slot (completion never observed)"
                );
            }
        }
    }

    /// Raise the "standup ready" attention row (kind=waiting) for a completed
    /// standup and nudge the surfaces. Best-effort — a duplicate id (a re-run
    /// before the surface cleared it) is skipped, and any store fault is warned.
    async fn raise_standup_ready(
        &self,
        session_id: &str,
        observations: &[IdleObservation],
        now_ms: i64,
    ) {
        let obs = observations.iter().find(|o| o.session.id == session_id);
        let cwd = obs.map(|o| o.session.cwd.clone()).unwrap_or_default();
        let workspace_id = obs.and_then(|o| o.workspace_id.clone());
        // Anchor the id on the completion instant so a later standup raises a new
        // row rather than colliding with this one.
        let id = format!("standup:{session_id}:{now_ms}");
        let payload = serde_json::json!({
            "kind": "standup_ready",
            "session_id": session_id,
            "message": "standup ready",
        })
        .to_string();
        // Resolve routing channels once at raise time (tcp T5); a standup-ready is
        // a Waiting kind, which the seeded default routes board-only.
        let channels = crate::notify::resolve_channels(
            &self.pool,
            AttentionKind::Waiting,
            workspace_id.as_deref(),
        )
        .await;
        let new = NewAttention {
            id: id.clone(),
            session_id: session_id.to_string(),
            cwd,
            workspace_id: workspace_id.clone(),
            kind: AttentionKind::Waiting,
            payload,
            degraded: false,
            created_at: now_ms,
            raise_transcript: None,
            channels,
        };
        if let Err(e) = AttentionRepo::insert(&self.pool, &new).await {
            tracing::warn!(error = %e, session = %session_id, "auto-standup: raise standup-ready failed");
            return;
        }
        self.events.emit_attention(HangarEvent::AttentionRaised {
            attention_id: id,
            session_id: session_id.to_string(),
            workspace_id,
            kind: AttentionKind::Waiting.as_str().to_string(),
            degraded: false,
            created_at: now_ms,
            channels,
        });
        tracing::info!(session = %session_id, "auto-standup: standup ready");
    }

    /// Spawn the watcher on a background task with the system clock and a fresh
    /// (never-cancelled here) shutdown token — boot's fire-and-forget entry point.
    #[must_use]
    pub fn spawn(pool: SqlitePool, events: EventSink) -> tokio::task::JoinHandle<()> {
        let watcher = Self::new(
            pool,
            events,
            Arc::new(SystemClock),
            CancellationToken::new(),
        );
        tokio::spawn(watcher.run())
    }
}

/// Discover the idle-at-prompt fleet: every live session whose HOOK-derived state
/// says its last turn ended and which is stagnant. Pane-classified (unhooked)
/// sessions are deliberately EXCLUDED — D13 fires only on hook status, never a
/// pane heuristic.
///
/// Best-effort: any discovery / classify fault yields an empty set (a quiet tick),
/// never a panic.
async fn probe_idle_fleet(now_ms: i64) -> Vec<IdleObservation> {
    use ainb_fleet_core::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
    use ainb_fleet_core::read::needs::{ClassifyInput, NeedsContext, classify};

    let ainb: Vec<Session> = discover_from_ainb().await.unwrap_or_default();
    let peers: Vec<Session> = tokio::task::spawn_blocking(discover_from_peers)
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    let merged = merge_sessions(vec![ainb, peers]);

    let mut out = Vec::new();
    for session in merged {
        // Classify off the runtime — reads the JSONL transcript (blocking fs I/O).
        let s = session.clone();
        let Some(row) =
            tokio::task::spawn_blocking(move || classify(ClassifyInput::from_env(s, None, now_ms)))
                .await
                .ok()
                .flatten()
        else {
            continue;
        };
        // Only a HOOK/JSONL-derived idle-at-prompt (an ended turn) qualifies. ASK
        // / ERR / WAIT are attention items, not stagnation; a pane-only classify
        // never yields Idle here, so pane heuristics can't drive a standup.
        if let NeedsContext::Idle(idle) = &row.context {
            out.push(IdleObservation {
                idle_at_prompt: true,
                idle_minutes: idle.idle_minutes,
                last_turn_end_ms: Some(
                    now_ms.saturating_sub(idle.idle_minutes.saturating_mul(60_000)),
                ),
                workspace_id: None,
                session,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_fleet_core::types::SessionSource;
    use ainb_hangar_store::Store;

    const NOW: i64 = 1_767_225_600_000; // 2026-01-01T00:00:00Z
    const MIN_MS: i64 = 60_000;

    fn cfg() -> StandupConfig {
        StandupConfig::default()
    }

    fn idle_input() -> SessionGateInput {
        SessionGateInput {
            idle_at_prompt: true,
            idle_minutes: 20, // past the 15-min default
            opted_out: false,
            last_standup_at: None,
            in_flight: false,
        }
    }

    // --- the pure gate: every guardrail, in isolation --------------------------

    #[test]
    fn idle_stagnant_session_fires() {
        assert_eq!(
            decide_standup(&cfg(), &idle_input(), 0, NOW),
            StandupDecision::Fire
        );
    }

    #[test]
    fn busy_session_never_fires_regardless_of_everything_else() {
        // The sacred guard: even a wildly-stagnant, never-cooled-down, in-budget
        // session that is MID-TURN must be skipped as NotIdleAtPrompt.
        let mut input = idle_input();
        input.idle_at_prompt = false;
        input.idle_minutes = 10_000; // extremely stagnant
        assert_eq!(
            decide_standup(&cfg(), &input, 0, NOW),
            StandupDecision::Skip(SkipReason::NotIdleAtPrompt),
        );
    }

    #[test]
    fn globally_disabled_short_circuits_before_anything() {
        let mut c = cfg();
        c.enabled = false;
        assert_eq!(
            decide_standup(&c, &idle_input(), 0, NOW),
            StandupDecision::Skip(SkipReason::GloballyDisabled),
        );
    }

    #[test]
    fn opted_out_session_is_skipped() {
        let mut input = idle_input();
        input.opted_out = true;
        assert_eq!(
            decide_standup(&cfg(), &input, 0, NOW),
            StandupDecision::Skip(SkipReason::OptedOut),
        );
    }

    #[test]
    fn not_stagnant_enough_is_skipped() {
        let mut input = idle_input();
        input.idle_minutes = 5; // under the 15-min default
        assert_eq!(
            decide_standup(&cfg(), &input, 0, NOW),
            StandupDecision::Skip(SkipReason::NotStagnant),
        );
    }

    #[test]
    fn in_flight_session_is_skipped() {
        let mut input = idle_input();
        input.in_flight = true;
        assert_eq!(
            decide_standup(&cfg(), &input, 0, NOW),
            StandupDecision::Skip(SkipReason::InFlight),
        );
    }

    #[test]
    fn within_cooldown_is_skipped_but_after_the_window_fires_again() {
        let mut input = idle_input();
        // Fired 30 min ago; 60-min cooldown → still cooling.
        input.last_standup_at = Some(NOW - 30 * MIN_MS);
        assert_eq!(
            decide_standup(&cfg(), &input, 0, NOW),
            StandupDecision::Skip(SkipReason::Cooldown),
        );
        // 61 min ago → window elapsed → fires.
        input.last_standup_at = Some(NOW - 61 * MIN_MS);
        assert_eq!(
            decide_standup(&cfg(), &input, 0, NOW),
            StandupDecision::Fire
        );
    }

    #[test]
    fn concurrency_cap_blocks_a_second_concurrent_standup() {
        // Default max_concurrent = 1: one already in flight elsewhere → skip.
        assert_eq!(
            decide_standup(&cfg(), &idle_input(), 1, NOW),
            StandupDecision::Skip(SkipReason::ConcurrencyLimit),
        );
        // Zero in flight → fires.
        assert_eq!(
            decide_standup(&cfg(), &idle_input(), 0, NOW),
            StandupDecision::Fire
        );
    }

    #[test]
    fn opt_out_beats_idle_and_cooldown_ordering() {
        // Opt-out is checked before the idle guard, so an opted-out busy session
        // reports OptedOut (the human's exclusion is the top-most session guard).
        let mut input = idle_input();
        input.opted_out = true;
        input.idle_at_prompt = false;
        assert_eq!(
            decide_standup(&cfg(), &input, 0, NOW),
            StandupDecision::Skip(SkipReason::OptedOut),
        );
    }

    // --- store-backed evaluate: the fire-once-then-cooldown lifecycle ----------

    fn session(id: &str) -> Session {
        Session {
            id: id.to_string(),
            cwd: format!("/work/{id}"),
            pid: None,
            git_root: None,
            tmux_session: Some(format!("tmux-{id}")),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![SessionSource::Ainb],
            summary: None,
            last_seen_ms: None,
        }
    }

    fn obs(id: &str, idle_at_prompt: bool, idle_minutes: i64) -> IdleObservation {
        IdleObservation {
            session: session(id),
            idle_at_prompt,
            idle_minutes,
            last_turn_end_ms: Some(NOW - idle_minutes * MIN_MS),
            workspace_id: None,
        }
    }

    #[tokio::test]
    async fn evaluate_fires_on_fresh_idle_then_cooldown_blocks_refire() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let c = cfg();
        let o = obs("s1", true, 20);

        // Fresh idle session with no prior standup → Fire.
        assert_eq!(
            evaluate_session(store.pool(), &c, &o, 0, NOW).await.unwrap(),
            StandupDecision::Fire,
        );

        // Simulate the fire: stamp the store. Now the SAME session is doubly
        // blocked — in-flight, and inside the cooldown window.
        StandupRepo::mark_fired(store.pool(), "s1", NOW).await.unwrap();
        assert_eq!(
            evaluate_session(store.pool(), &c, &o, 1, NOW + MIN_MS).await.unwrap(),
            StandupDecision::Skip(SkipReason::InFlight),
        );

        // Complete the standup (clear in-flight). Still inside cooldown → Cooldown.
        StandupRepo::clear_in_flight(store.pool(), "s1", NOW + 2 * MIN_MS)
            .await
            .unwrap();
        assert_eq!(
            evaluate_session(store.pool(), &c, &o, 0, NOW + 10 * MIN_MS).await.unwrap(),
            StandupDecision::Skip(SkipReason::Cooldown),
        );

        // After the 60-min window → fires again.
        assert_eq!(
            evaluate_session(store.pool(), &c, &o, 0, NOW + 61 * MIN_MS).await.unwrap(),
            StandupDecision::Fire,
        );
    }

    #[tokio::test]
    async fn evaluate_respects_stored_opt_out() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        StandupRepo::set_opt_out(store.pool(), "s1", true, NOW).await.unwrap();
        assert_eq!(
            evaluate_session(store.pool(), &cfg(), &obs("s1", true, 30), 0, NOW)
                .await
                .unwrap(),
            StandupDecision::Skip(SkipReason::OptedOut),
        );
    }

    #[tokio::test]
    async fn config_load_honours_stored_disable_and_defaults_on() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        // Fresh store: auto-standup defaults ON per Stevie.
        assert!(StandupConfig::load(store.pool()).await.unwrap().enabled);
        // Explicit disable is honoured.
        DaemonConfigRepo::set(store.pool(), KEY_ENABLED, "false").await.unwrap();
        assert!(!StandupConfig::load(store.pool()).await.unwrap().enabled);
    }

    #[tokio::test]
    async fn tick_disabled_fires_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        DaemonConfigRepo::set(store.pool(), KEY_ENABLED, "false").await.unwrap();
        let broker = crate::events::EventBroker::new();
        let watcher = StandupWatcher::new(
            store.pool().clone(),
            broker.sink(),
            Arc::new(SystemClock),
            CancellationToken::new(),
        );
        let fired = watcher.tick(&[obs("s1", true, 30)], NOW).await;
        assert_eq!(fired, 0, "a disabled watcher fires nothing");
        // And nothing was stamped.
        assert!(StandupRepo::get(store.pool(), "s1").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn close_completed_raises_standup_ready_attention() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let broker = crate::events::EventBroker::new();
        let watcher = StandupWatcher::new(
            store.pool().clone(),
            broker.sink(),
            Arc::new(SystemClock),
            CancellationToken::new(),
        );
        // A session with a standup in flight (fired at NOW).
        StandupRepo::mark_fired(store.pool(), "s1", NOW).await.unwrap();

        // A later observation: the session ended a NEW turn (after the fire) and is
        // idle again → completion.
        let mut o = obs("s1", true, 2);
        o.last_turn_end_ms = Some(NOW + 5 * MIN_MS);
        watcher.close_completed(&[o], NOW + 6 * MIN_MS).await;

        // The in-flight slot is released and a waiting "standup ready" row exists.
        assert!(!StandupRepo::get(store.pool(), "s1").await.unwrap().unwrap().in_flight);
        let rows = AttentionRepo::list_fleet(store.pool()).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].kind, AttentionKind::Waiting);
        assert!(rows[0].payload.contains("standup_ready"));
        assert_eq!(rows[0].session_id, "s1");
    }

    #[tokio::test]
    async fn close_completed_reclaims_a_stale_in_flight_slot() {
        // A fired standup whose completion is NEVER observed must not pin the single
        // concurrency slot forever: a later tick past the stale bound reclaims it.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let broker = crate::events::EventBroker::new();
        let watcher = StandupWatcher::new(
            store.pool().clone(),
            broker.sink(),
            Arc::new(SystemClock),
            CancellationToken::new(),
        );
        // Fire at NOW, then the session vanishes — no observation ever reports a
        // fresh end-turn (peer/unhooked session, or a crash before the turn).
        StandupRepo::mark_fired(store.pool(), "s1", NOW).await.unwrap();
        // A tick just past 3× the 60-min cooldown, with NO observation of s1.
        let stale_after =
            NOW + (STALE_INFLIGHT_COOLDOWN_MULTIPLE * DEFAULT_COOLDOWN_MIN + 1) * MIN_MS;
        watcher.close_completed(&[], stale_after).await;

        // The slot is reclaimed even though completion was never seen…
        assert!(!StandupRepo::get(store.pool(), "s1").await.unwrap().unwrap().in_flight);
        // …and NO "standup ready" row is raised (we never observed it finish).
        assert!(AttentionRepo::list_fleet(store.pool()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn close_completed_keeps_a_fresh_in_flight_slot_before_the_stale_bound() {
        // Before the stale bound an un-completed in-flight standup stays in flight
        // (the reclaim must not fire early and step on a still-running standup).
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let broker = crate::events::EventBroker::new();
        let watcher = StandupWatcher::new(
            store.pool().clone(),
            broker.sink(),
            Arc::new(SystemClock),
            CancellationToken::new(),
        );
        StandupRepo::mark_fired(store.pool(), "s1", NOW).await.unwrap();
        // One cooldown window in — well short of the 3× stale bound — no observation.
        watcher.close_completed(&[], NOW + DEFAULT_COOLDOWN_MIN * MIN_MS).await;
        assert!(StandupRepo::get(store.pool(), "s1").await.unwrap().unwrap().in_flight);
    }

    #[tokio::test]
    async fn close_completed_does_not_fire_before_the_standup_turn_ends() {
        // Right after firing, the session's newest turn is the PRE-fire one — its
        // last_turn_end_ms is <= the fire instant, so completion must NOT trigger.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let broker = crate::events::EventBroker::new();
        let watcher = StandupWatcher::new(
            store.pool().clone(),
            broker.sink(),
            Arc::new(SystemClock),
            CancellationToken::new(),
        );
        StandupRepo::mark_fired(store.pool(), "s1", NOW).await.unwrap();
        let mut o = obs("s1", true, 1);
        o.last_turn_end_ms = Some(NOW - MIN_MS); // pre-fire turn
        watcher.close_completed(&[o], NOW + MIN_MS).await;
        // Still in flight, no premature standup-ready row.
        assert!(StandupRepo::get(store.pool(), "s1").await.unwrap().unwrap().in_flight);
        assert!(AttentionRepo::list_fleet(store.pool()).await.unwrap().is_empty());
    }
}
