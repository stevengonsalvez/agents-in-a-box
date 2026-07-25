// ABOUTME: Host Fleet subscription, daemon health, and detached control RPCs.
//
// The TUI fleet panel (`components/fleet_panel.rs`) browses the event-sourced
// `current_state` and lets the human ACT: answer an interview (ASK) or
// broadcast a prompt. The send itself is async (`fleet::send::send`, the same
// tmux send-keys path `fleet broadcast` uses) and the TUI key path is sync, so
// this module owns the bridge: a detached worker thread builds a current-thread
// tokio runtime, discovers the live session matching the row, sends the text,
// and publishes a human-readable outcome into a shared cell the panel renders.
//
// Living outside `src/components/` keeps the (legitimately async) send logic
// clear of the render-thread `.await` lint — the worker thread is NOT the
// render path; the component only ever touches the shared feedback cell under a
// microsecond lock.
//
// SAFETY (C1): an ASK answer must reach the EXACT agent that asked. The hook's
// claude session id usually differs from a discovered `Session.id`, so an exact
// id match often fails and we'd fall to a cwd correlation — but two agents can
// share a cwd, so a cwd guess could send the answer to the WRONG agent. We
// therefore REFUSE to answer on an ambiguous cwd (more than one discovered
// session in that cwd, or a merged session that aggregated 2+ sources) rather
// than silently mis-route a safety-critical interview answer. Broadcasts apply
// the same guard (a ping to the wrong agent is lower-harm, but we still refuse —
// the safe call).
//
// CONCURRENCY (C3): each dispatch refuses while another is in flight (an
// `AtomicBool` guard) so key-repeat can't spawn unbounded worker threads or
// double-send an answer to an agent.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ainb_hangar_proto::fleet::FleetSnapshot;

use crate::fleet::bridge::daemon::FleetStreamEvent;

/// Transient feedback published by the async action worker and rendered by the
/// fleet panel's status line. Cloned cheaply; the lock is held only for the
/// microseconds it takes to read/replace the line — never across send I/O.
#[derive(Debug, Clone, Default)]
pub struct ActionFeedback {
    /// Human-readable status, e.g. "answered ask → /work/x: sent via tmux".
    pub message: String,
}

/// Connection health surfaced by the host Fleet panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetDaemonHealth {
    /// Subscription worker is dialing or authenticating.
    Connecting,
    /// Snapshot and live revision stream are active.
    Online,
    /// Stream is unavailable; cached rows remain usable for safe inspection.
    Offline(String),
}

impl FleetDaemonHealth {
    /// Whether authoritative control RPCs may be trusted right now.
    #[must_use]
    pub const fn is_online(&self) -> bool {
        matches!(self, Self::Online)
    }
}

/// Ordered update from the persistent Fleet subscription worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FleetHostUpdate {
    /// Complete authoritative state at one durable revision.
    Snapshot(FleetSnapshot),
    /// Subscription connection health changed.
    Health(FleetDaemonHealth),
}

/// Shared ordered queue drained by the UI thread.
pub type FleetHostUpdateSink = Arc<Mutex<Vec<FleetHostUpdate>>>;

/// Start one persistent Fleet stream worker.
pub fn spawn_fleet_subscription(
    updates: FleetHostUpdateSink,
    initial_cursor: i64,
) -> Result<(), String> {
    std::thread::Builder::new()
        .name("ainb-fleet-subscription".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(error) => {
                    publish_host_update(
                        &updates,
                        FleetHostUpdate::Health(FleetDaemonHealth::Offline(format!(
                            "runtime build failed: {error}"
                        ))),
                    );
                    return;
                }
            };
            runtime.block_on(run_fleet_subscription(updates, initial_cursor));
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn run_fleet_subscription(updates: FleetHostUpdateSink, initial_cursor: i64) {
    let mut cursor = initial_cursor.max(0);
    let mut attempt = 0_u32;
    loop {
        publish_host_update(
            &updates,
            FleetHostUpdate::Health(FleetDaemonHealth::Connecting),
        );
        let outcome = run_fleet_connection(&updates, &mut cursor).await;
        let detail = outcome.err().unwrap_or_else(|| "Fleet stream ended".to_string());
        publish_host_update(
            &updates,
            FleetHostUpdate::Health(FleetDaemonHealth::Offline(detail)),
        );
        tokio::time::sleep(subscription_backoff(attempt)).await;
        attempt = attempt.saturating_add(1);
    }
}

async fn run_fleet_connection(
    updates: &FleetHostUpdateSink,
    cursor: &mut i64,
) -> Result<(), String> {
    let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
        .map_err(|error| error.to_string())?;
    let (subscription, mut stream) = client
        .open_fleet_subscription(*cursor)
        .await
        .map_err(|error| error.to_string())?;

    *cursor = subscription.snapshot.head_revision;
    publish_host_update(updates, FleetHostUpdate::Snapshot(subscription.snapshot));

    for event in subscription.replay {
        reconcile_revision(&client, updates, cursor, event.revision).await?;
    }
    publish_host_update(updates, FleetHostUpdate::Health(FleetDaemonHealth::Online));

    loop {
        match stream.next_event().await.map_err(|error| error.to_string())? {
            FleetStreamEvent::Revision(event) => {
                reconcile_revision(&client, updates, cursor, event.revision).await?;
            }
            FleetStreamEvent::ResyncRequired => {
                return Err(format!("Fleet stream lagged after revision {cursor}"));
            }
        }
    }
}

async fn reconcile_revision(
    client: &crate::fleet::bridge::daemon::DaemonClient,
    updates: &FleetHostUpdateSink,
    cursor: &mut i64,
    revision: i64,
) -> Result<(), String> {
    if revision <= *cursor {
        return Ok(());
    }
    let snapshot = client.fleet_snapshot().await.map_err(|error| error.to_string())?;
    if snapshot.head_revision < revision {
        return Err(format!(
            "Fleet snapshot head {} precedes announced revision {revision}",
            snapshot.head_revision
        ));
    }
    *cursor = snapshot.head_revision;
    publish_host_update(updates, FleetHostUpdate::Snapshot(snapshot));
    Ok(())
}

fn publish_host_update(updates: &FleetHostUpdateSink, update: FleetHostUpdate) {
    if let Ok(mut updates) = updates.lock() {
        updates.push(update);
    }
}

fn subscription_backoff(attempt: u32) -> Duration {
    Duration::from_millis(250_u64.saturating_mul(1_u64 << attempt.min(4)))
}

/// Execute one authoritative Fleet broadcast on a worker thread.
pub fn execute_fleet_broadcast_blocking(
    params: ainb_hangar_proto::fleet::FleetBroadcastParams,
) -> Result<ainb_hangar_proto::fleet::FleetBroadcastResult, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client.fleet_broadcast(params).await.map_err(|error| error.to_string())
    })
}

/// Execute one authoritative Fleet action on a worker thread.
pub fn execute_fleet_action_blocking(
    params: ainb_hangar_proto::fleet::FleetActionParams,
) -> Result<ainb_hangar_proto::fleet::FleetActionReceipt, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
            .map_err(|error| error.to_string())?;
        client.fleet_action(params).await.map_err(|error| error.to_string())
    })
}

/// Dispatch a send to the session described by `(session_id, cwd)`, off the UI
/// thread. Resolves the row to a live `Session` (for its tmux name) via the
/// same discovery the CLI uses, then sends `text` through `fleet::send::send`.
///
/// `in_flight` is a shared guard: if a previous dispatch is still running this
/// one is REFUSED (feedback "send already in flight…") so rapid key-repeat
/// cannot spawn unbounded worker threads or double-deliver. The flag is set
/// before spawning and cleared once the worker publishes its outcome (every
/// exit path — including spawn failure — clears it).
///
/// `is_answer` marks a safety-critical interview answer: when the exact
/// session-id match fails AND the cwd is ambiguous, the send is REFUSED rather
/// than routed by a cwd guess (C1). Broadcasts pass `false` but get the same
/// refusal — the safe call.
///
/// Runs on a detached worker thread owning a current-thread tokio runtime so
/// the render loop never blocks on `tmux has-session`/`send-keys`. The outcome
/// is published into the shared [`ActionFeedback`] so the next frame shows it.
///
/// `kind_label` is a short verb for the feedback line ("answered ask",
/// "broadcast").
pub fn dispatch_send(
    feedback: Arc<Mutex<ActionFeedback>>,
    in_flight: Arc<AtomicBool>,
    session_id: String,
    cwd: String,
    text: String,
    kind_label: &'static str,
    is_answer: bool,
) {
    // C3: claim the in-flight slot. If another dispatch already holds it, refuse
    // — never spawn a second worker (key-repeat → unbounded threads + duplicate
    // answers to the agent). `compare_exchange` makes the claim atomic.
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        publish(
            &feedback,
            "send already in flight — wait for it to finish".to_string(),
        );
        return;
    }

    let target_label = if cwd.is_empty() {
        session_id.clone()
    } else {
        cwd.clone()
    };
    // Keep handles for the spawn-failure path; the worker thread takes the
    // originals.
    let spawn_err_feedback = Arc::clone(&feedback);
    let spawn_err_flag = Arc::clone(&in_flight);
    let spawn_result =
        std::thread::Builder::new().name("ainb-fleet-send".into()).spawn(move || {
            // Whatever happens below, release the in-flight slot on the way out
            // so the panel can dispatch again.
            let _guard = InFlightGuard(in_flight);
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    publish(&feedback, format!("send failed: runtime build error: {e}"));
                    return;
                }
            };
            rt.block_on(async move {
                let outcome = resolve_and_send(&session_id, &cwd, &text, is_answer).await;
                publish(
                    &feedback,
                    format!("{kind_label} → {target_label}: {outcome}"),
                );
            });
        });
    if let Err(e) = spawn_result {
        tracing::warn!(error = %e, "fleet panel: send worker thread spawn failed");
        // The worker never ran, so release the slot here and surface the error.
        spawn_err_flag.store(false, Ordering::Release);
        publish(
            &spawn_err_feedback,
            format!("send failed: thread spawn error: {e}"),
        );
    }
}

/// Clears the in-flight guard on drop, so EVERY worker exit path (success,
/// runtime-build error, panic-unwind) releases the slot for the next dispatch.
struct InFlightGuard(Arc<AtomicBool>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Dispatch an approve/deny decision for a waiting `PermissionRequest` hook,
/// off the UI thread. Unlike [`dispatch_send`] (which routes text to a live
/// agent via tmux/broker), this delivers a first-class permission decision to
/// the notifyd approve broker: the blocked hook is parked on the approve
/// socket in `client_await`, and `client_decide` hands it the human's answer,
/// which flows back to Claude as its `hookSpecificOutput` permission decision.
///
/// Shares the same `in_flight` guard as sends so rapid key-repeat can't spawn
/// unbounded workers or double-decide. The worker is a plain thread — the
/// broker client is blocking `std::os::unix` I/O, no tokio runtime needed.
///
/// `kind_label` is the short verb for the feedback line ("approved" / "denied").
/// The socket is resolved at dispatch time via [`Paths::from_home`], the same
/// layout the daemon and hook use, so no home has to be threaded through the
/// panel.
pub fn dispatch_decide(
    feedback: Arc<Mutex<ActionFeedback>>,
    in_flight: Arc<AtomicBool>,
    session_id: String,
    kind: ainb_plugin_notifyd::broker::DecisionKind,
    reason: String,
    kind_label: &'static str,
) {
    // Atomically claim the single in-flight slot (shared with sends).
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        publish(
            &feedback,
            "action already in flight — wait for it to finish".to_string(),
        );
        return;
    }

    let spawn_err_feedback = Arc::clone(&feedback);
    let spawn_err_flag = Arc::clone(&in_flight);
    let target_label = session_id.clone();
    let spawn_result =
        std::thread::Builder::new().name("ainb-fleet-decide".into()).spawn(move || {
            let _guard = InFlightGuard(in_flight);
            let sock = match ainb_plugin_notifyd::paths::Paths::from_home() {
                Ok(paths) => paths.approve_socket,
                Err(e) => {
                    publish(&feedback, format!("{kind_label} failed: {e}"));
                    return;
                }
            };
            // `matched` means a waiter WAS parked and the broker handed it the
            // decision — the final write to the hook's socket is not itself
            // acknowledged, so don't claim "delivered".
            let outcome =
                match ainb_plugin_notifyd::broker::client_decide(&sock, &session_id, kind, &reason)
                {
                    Ok(true) => "matched the waiting hook".to_string(),
                    Ok(false) => "no waiter (already resolved or timed out)".to_string(),
                    Err(e) => format!("broker unreachable: {e}"),
                };
            publish(
                &feedback,
                format!("{kind_label} → {target_label}: {outcome}"),
            );
        });
    if let Err(e) = spawn_result {
        tracing::warn!(error = %e, "fleet panel: decide worker thread spawn failed");
        spawn_err_flag.store(false, Ordering::Release);
        publish(
            &spawn_err_feedback,
            format!("{kind_label} failed: thread spawn error: {e}"),
        );
    }
}

/// Create a new ATC-managed session by shelling out to `ainb fleet atc setup
/// <name>`, off the UI thread. This is the panel's "bootstrap a fleet member"
/// action: an ordinary session is gated out of the event log (`is_fleet_member`
/// in `cli/fleet/atc.rs`), so a cold fleet stays empty until an ATC session
/// exists to fire hooks. Running the exact CLI verb keeps one code path for
/// setup (hooks + heartbeat timer + spawn) whether invoked from the shell or
/// the TUI.
///
/// Shares the panel's `in_flight` guard with `dispatch_send` so only one action
/// runs at a time (a create in flight refuses a second create / a send, and vice
/// versa). The detached worker publishes progress + outcome into the same
/// [`ActionFeedback`] cell the status line renders.
pub fn dispatch_new_atc(
    feedback: Arc<Mutex<ActionFeedback>>,
    in_flight: Arc<AtomicBool>,
    name: String,
) {
    if in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        publish(
            &feedback,
            "action already in flight — wait for it to finish".to_string(),
        );
        return;
    }

    let spawn_err_feedback = Arc::clone(&feedback);
    let spawn_err_flag = Arc::clone(&in_flight);
    let spawn_result =
        std::thread::Builder::new().name("ainb-fleet-new-atc".into()).spawn(move || {
            let _guard = InFlightGuard(in_flight);
            publish(&feedback, format!("creating ATC '{name}'…"));
            let bin = crate::cli::fleet::atc::atc_bin();
            let output =
                std::process::Command::new(bin).args(["fleet", "atc", "setup", &name]).output();
            let msg = match output {
                Ok(o) if o.status.success() => {
                    format!("created ATC '{name}' — press r once it fires a hook")
                }
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr);
                    let line =
                        err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("see logs");
                    format!("create failed: {line}")
                }
                Err(e) => format!("create failed: {e}"),
            };
            publish(&feedback, msg);
        });
    if let Err(e) = spawn_result {
        tracing::warn!(error = %e, "fleet panel: new-atc worker thread spawn failed");
        spawn_err_flag.store(false, Ordering::Release);
        publish(
            &spawn_err_feedback,
            format!("create failed: thread spawn error: {e}"),
        );
    }
}

/// Resolve a `current_state` row to a live `Session` and send `text` to it,
/// returning a human-readable outcome string.
///
/// Resolution priority + the C1 safety guard:
///   1. EXACT session-id match → always safe to send (it is unambiguously the
///      agent the row names).
///   2. No exact match → correlate by cwd, but ONLY when that cwd is
///      UNAMBIGUOUS. Ambiguous = more than one discovered session shares the
///      cwd, OR the merged session for that cwd aggregated 2+ sources (so we
///      can't tell which underlying agent it is). On ambiguity we REFUSE rather
///      than guess — sending an interview answer (or even a ping) to the wrong
///      agent is the bug we're closing.
async fn resolve_and_send(session_id: &str, cwd: &str, text: &str, is_answer: bool) -> String {
    use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
    use crate::fleet::send::send;
    use crate::fleet::types::Session;

    // C4: real concurrency. `discover_from_ainb` is async; `discover_from_peers`
    // is a SYNC blocking SQLite read, so run it on a blocking thread instead of
    // a bare `async {}` block (which would run it inline on this thread — the
    // previous illusory-concurrency bug). Both then make progress in parallel.
    let ainb_fut = discover_from_ainb();
    let peers_fut = tokio::task::spawn_blocking(discover_from_peers);
    let (ainb, peers_join) = tokio::join!(ainb_fut, peers_fut);
    let ainb: Vec<Session> = ainb.unwrap_or_default();
    let peers: Vec<Session> = peers_join.ok().and_then(Result::ok).unwrap_or_default();

    // Count raw (pre-merge) sessions sharing the target cwd across BOTH sources,
    // so two distinct claude sessions in the same dir register as ambiguous even
    // though `merge_sessions` would coalesce them onto one cwd-keyed row.
    let raw_cwd_count = if cwd.is_empty() {
        0
    } else {
        ainb.iter().chain(peers.iter()).filter(|s| s.cwd == cwd).count()
    };

    let merged = merge_sessions(vec![ainb, peers]);

    // 1. Exact session-id match is always unambiguous — send it.
    if let Some(session) = merged.iter().find(|s| s.id == session_id) {
        return outcome_string(send(session, text).await);
    }

    // 2. No exact match → cwd correlation, guarded by ambiguity.
    let Some(by_cwd) = merged.iter().find(|s| !cwd.is_empty() && s.cwd == cwd) else {
        return "no live session matched (target may have exited)".to_string();
    };

    // Ambiguous when >1 raw session shared the cwd, OR the merged session for
    // the cwd aggregated 2+ sources (we can't tell which underlying agent it is).
    let ambiguous = raw_cwd_count > 1 || by_cwd.sources.len() > 1;
    if ambiguous {
        let label = if is_answer {
            "cannot safely answer"
        } else {
            "refusing to send"
        };
        return format!(
            "ambiguous target — {label} ({} sessions in this cwd)",
            raw_cwd_count.max(by_cwd.sources.len())
        );
    }

    outcome_string(send(by_cwd, text).await)
}

/// Map a `send()` result to the human-readable feedback string.
fn outcome_string(result: anyhow::Result<crate::fleet::types::SendOutcome>) -> String {
    use crate::fleet::types::SendOutcome;
    match result {
        Ok(SendOutcome::Tmux { tmux_session }) => format!("sent via tmux ({tmux_session})"),
        Ok(SendOutcome::Broker { peer_id }) => format!("sent via broker ({peer_id})"),
        Ok(SendOutcome::Failed { reason }) => format!("not delivered: {reason}"),
        Err(e) => format!("send error: {e}"),
    }
}

fn publish(feedback: &Mutex<ActionFeedback>, message: String) {
    if let Ok(mut g) = feedback.lock() {
        g.message = message;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::types::{Session, SessionSource};

    fn session(id: &str, cwd: &str, src: SessionSource) -> Session {
        Session {
            id: id.to_string(),
            cwd: cwd.to_string(),
            pid: None,
            git_root: None,
            tmux_session: Some("tmux".to_string()),
            workspace_name: None,
            worktree_path: None,
            peer_id: None,
            bg_job_id: None,
            transcript_path: None,
            sources: vec![src],
            summary: None,
            last_seen_ms: None,
        }
    }

    /// The C1 resolution decision, isolated from the actual `send()` I/O so it is
    /// deterministically unit-testable: given the discovered roster, what target
    /// does an answer resolve to — or does it refuse?
    #[derive(Debug, PartialEq)]
    enum Target {
        Exact(String),
        Cwd(String),
        Refuse,
        NoMatch,
    }

    /// Mirror of `resolve_and_send`'s resolution logic (steps 1+2 + the C1
    /// ambiguity guard) WITHOUT the send. `raw` is the pre-merge roster (both
    /// sources concatenated); `merged` is `merge_sessions` applied to it.
    fn resolve_target(raw: &[Session], session_id: &str, cwd: &str) -> Target {
        let merged = crate::fleet::discover::merge_sessions(vec![raw.to_vec()]);
        if let Some(s) = merged.iter().find(|s| s.id == session_id) {
            return Target::Exact(s.id.clone());
        }
        let raw_cwd_count = if cwd.is_empty() {
            0
        } else {
            raw.iter().filter(|s| s.cwd == cwd).count()
        };
        let Some(by_cwd) = merged.iter().find(|s| !cwd.is_empty() && s.cwd == cwd) else {
            return Target::NoMatch;
        };
        if raw_cwd_count > 1 || by_cwd.sources.len() > 1 {
            return Target::Refuse;
        }
        Target::Cwd(by_cwd.id.clone())
    }

    #[test]
    fn exact_session_id_match_sends() {
        // The hook session id exactly matches a discovered session → send to it,
        // even if another session shares the cwd.
        let raw = vec![
            session("hook-sid", "/work/x", SessionSource::Ainb),
            session("other", "/work/x", SessionSource::Peers),
        ];
        // (These two share /work/x, so merge would coalesce — but the exact id
        // wins before any cwd logic.)
        assert_eq!(
            resolve_target(&raw, "hook-sid", "/work/x"),
            Target::Exact("hook-sid".to_string())
        );
    }

    #[test]
    fn unambiguous_cwd_match_sends() {
        // No exact id match, exactly one session in the cwd, single source → safe
        // to correlate by cwd.
        let raw = vec![session("discovered-id", "/work/x", SessionSource::Ainb)];
        assert_eq!(
            resolve_target(&raw, "hook-session-differs", "/work/x"),
            Target::Cwd("discovered-id".to_string())
        );
    }

    #[test]
    fn ambiguous_cwd_two_raw_sessions_refuses() {
        // Two DISTINCT sessions share the cwd (different ids, both pre-merge).
        // `merge_sessions` coalesces them onto one cwd row, but the raw count is
        // 2 → ambiguous → refuse rather than answer the wrong agent.
        let raw = vec![
            session("a", "/work/x", SessionSource::Ainb),
            session("b", "/work/x", SessionSource::Ainb),
        ];
        assert_eq!(resolve_target(&raw, "hook-sid", "/work/x"), Target::Refuse);
    }

    #[test]
    fn ambiguous_cwd_multi_source_merge_refuses() {
        // One cwd, but the merged session aggregated 2+ sources (ainb + peers) —
        // we can't tell which underlying agent it is → refuse.
        let raw = vec![
            session("a", "/work/x", SessionSource::Ainb),
            session("a", "/work/x", SessionSource::Peers),
        ];
        // raw count is 2 here too, but the multi-source guard is the independent
        // signal; assert refuse.
        assert_eq!(resolve_target(&raw, "hook-sid", "/work/x"), Target::Refuse);
    }

    #[test]
    fn no_session_in_cwd_is_no_match_not_refuse() {
        let raw = vec![session("a", "/other", SessionSource::Ainb)];
        assert_eq!(resolve_target(&raw, "hook-sid", "/work/x"), Target::NoMatch);
    }

    #[test]
    fn empty_cwd_without_exact_match_is_no_match() {
        // An empty cwd never correlates (it would collapse distinct sessions).
        let raw = vec![session("a", "", SessionSource::Ainb)];
        assert_eq!(resolve_target(&raw, "hook-sid", ""), Target::NoMatch);
    }

    #[test]
    fn in_flight_guard_refuses_a_second_dispatch() {
        let feedback = Arc::new(Mutex::new(ActionFeedback::default()));
        let in_flight = Arc::new(AtomicBool::new(false));
        // Simulate a dispatch already in flight.
        in_flight.store(true, Ordering::Release);
        dispatch_send(
            Arc::clone(&feedback),
            Arc::clone(&in_flight),
            "sid".to_string(),
            "/work/x".to_string(),
            "answer".to_string(),
            "answered ask",
            true,
        );
        // The dispatch must have been refused with the in-flight message and the
        // flag left set (we never started a second worker).
        assert!(feedback.lock().unwrap().message.contains("already in flight"));
        assert!(in_flight.load(Ordering::Acquire));
    }

    #[test]
    fn in_flight_guard_clears_after_a_completed_worker() {
        // A fresh dispatch claims the slot, runs the worker (which fails to find
        // a session — no tmux in tests — but still completes), and the guard
        // releases the slot. We poll briefly for the worker thread to finish.
        let feedback = Arc::new(Mutex::new(ActionFeedback::default()));
        let in_flight = Arc::new(AtomicBool::new(false));
        dispatch_send(
            Arc::clone(&feedback),
            Arc::clone(&in_flight),
            "sid".to_string(),
            "/nonexistent-cwd-xyz".to_string(),
            "answer".to_string(),
            "answered ask",
            true,
        );
        // Wait for the detached worker to publish + drop its guard.
        for _ in 0..200 {
            if !in_flight.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            !in_flight.load(Ordering::Acquire),
            "the in-flight slot must be released after the worker completes"
        );
    }
}
