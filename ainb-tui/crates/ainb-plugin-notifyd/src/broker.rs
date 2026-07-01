//! The approve/deny broker — a synchronous permission round-trip.
//!
//! Unlike the notify socket ([`crate::paths::Paths::socket`]), which is
//! one-way fire-and-forget, the broker listens on a *request/response*
//! socket ([`crate::paths::Paths::approve_socket`]). It exists to close
//! the one gap where AgentPeek's design beats ours: a human synchronously
//! approving or denying a live Claude `PermissionRequest` hook.
//!
//! Three line-JSON operations, one per connection:
//!
//! - **AWAIT** — a waiting hook (the `ainb fleet atc hook`
//!   `PermissionRequest` branch) dials in, registers itself keyed by
//!   `session_id`, and BLOCKS on the connection until a human decides or
//!   the broker times out. The response is the decision.
//! - **DECIDE** — the fleet TUI / CLI dials in, names a `session_id` and
//!   an `approve`/`deny`, the broker hands the decision to the blocked
//!   AWAIT, and both connections resolve.
//! - **LIST** — dump the pending session ids (what is currently waiting
//!   for a human), for the Daemons overlay / CLI status.
//!
//! ## Durability model
//!
//! Pending approvals live ONLY in memory. Waiters re-dial and re-send
//! AWAIT on disconnect, so restarting notifyd is the single resume
//! command: every still-alive waiter re-registers itself. There is no
//! durable pending file. If a waiter's AWAIT times out (no human in the
//! window) the fallback is **deny** — never auto-approve. The
//! correlation key across every surface is `session_id`, because Claude
//! blocks on at most one permission request per session at a time.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tracing::{debug, error, warn};

/// Default AWAIT ceiling. Deliberately longer than the hook's own
/// timeout (the `PermissionRequest` hook is registered with a ~300s
/// budget) so that, in the normal case, Claude's hook timeout fires
/// first and the broker's own timeout is only a backstop against a
/// leaked pending entry. On expiry the fallback is deny.
pub const DEFAULT_AWAIT_TIMEOUT: Duration = Duration::from_secs(600);

/// A human's answer to a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecisionKind {
    /// Allow the tool call to proceed.
    Approve,
    /// Block the tool call.
    Deny,
}

/// The resolved decision handed back to a blocked AWAIT.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Decision {
    /// Approve or deny.
    pub decision: DecisionKind,
    /// Human-readable reason (empty when none was given). For the
    /// timeout/superseded fallbacks this explains why it defaulted to
    /// deny.
    #[serde(default)]
    pub reason: String,
}

impl Decision {
    /// The safe fallback when no human answered in time.
    fn timed_out() -> Self {
        Self {
            decision: DecisionKind::Deny,
            reason: "no human decision before timeout (fallback: deny)".into(),
        }
    }

    /// The fallback when a newer AWAIT for the same session superseded
    /// this one (e.g. the waiter re-dialled after a notifyd restart).
    fn superseded() -> Self {
        Self {
            decision: DecisionKind::Deny,
            reason: "superseded by a newer request for this session".into(),
        }
    }
}

/// One entry in the pending map: a registered AWAIT waiting for a human.
struct Pending {
    /// Monotone id disambiguating two AWAITs racing on the same
    /// `session_id` — only the entry whose id we still hold gets removed
    /// on our own timeout, so a superseding AWAIT is never clobbered.
    id: u64,
    /// Tool the permission request is for (e.g. `Bash`), for display.
    tool: String,
    /// Short human context (e.g. the command), for display.
    context: String,
    /// When this AWAIT registered, ms since epoch — drives `waiting_ms`.
    since_ms: u64,
    /// Channel to deliver the human's decision to the blocked AWAIT.
    tx: oneshot::Sender<Decision>,
}

/// One pending approval as reported by LIST.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingInfo {
    /// The session blocked on a human decision.
    pub session_id: String,
    /// Tool the permission request is for.
    pub tool: String,
    /// Short human context.
    pub context: String,
    /// How long it has been waiting, in milliseconds.
    pub waiting_ms: u64,
}

/// Shared broker state: the pending map plus the AWAIT timeout. Cloneable
/// (the inner map is behind an `Arc`) so the accept loop can hand a
/// handle to each connection task.
#[derive(Clone)]
pub struct BrokerState {
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    next_id: Arc<AtomicU64>,
    await_timeout: Duration,
}

impl BrokerState {
    /// Fresh state with the default AWAIT timeout.
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_AWAIT_TIMEOUT)
    }

    /// Fresh state with a custom AWAIT timeout (tests use a tiny value).
    pub fn with_timeout(await_timeout: Duration) -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            await_timeout,
        }
    }

    /// Snapshot of every session currently waiting for a human.
    pub fn list(&self) -> Vec<PendingInfo> {
        let now = now_ms();
        let map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
        map.iter()
            .map(|(session_id, p)| PendingInfo {
                session_id: session_id.clone(),
                tool: p.tool.clone(),
                context: p.context.clone(),
                waiting_ms: now.saturating_sub(p.since_ms),
            })
            .collect()
    }

    /// Resolve a pending AWAIT. Returns `true` if a waiter was blocked on
    /// this `session_id` and has now been handed the decision.
    pub fn decide(&self, session_id: &str, decision: Decision) -> bool {
        let taken = {
            let mut map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            map.remove(session_id)
        };
        match taken {
            Some(p) => {
                // Ok(_) is impossible to observe here beyond the send; if the
                // waiter's receiver was already dropped the decision is simply
                // discarded, which is fine (the waiter is gone).
                let _ = p.tx.send(decision);
                true
            }
            None => false,
        }
    }

    /// Register an AWAIT and block until a human decides, the entry is
    /// superseded, or the timeout fires (fallback: deny). Never returns
    /// without a decision — the waiting hook is never left empty-handed.
    async fn await_decision(&self, session_id: String, tool: String, context: String) -> Decision {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        {
            let mut map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            map.insert(
                session_id.clone(),
                Pending {
                    id,
                    tool,
                    context,
                    since_ms: now_ms(),
                    tx,
                },
            );
        }

        match tokio::time::timeout(self.await_timeout, rx).await {
            // A human decided.
            Ok(Ok(decision)) => decision,
            // Sender dropped without sending → a newer AWAIT replaced us.
            Ok(Err(_)) => Decision::superseded(),
            // Timed out. Remove ourselves — but only if the entry is still
            // ours (a superseding AWAIT owns a different id and must survive).
            Err(_) => {
                let mut map = self.pending.lock().unwrap_or_else(|p| p.into_inner());
                if map.get(&session_id).is_some_and(|p| p.id == id) {
                    map.remove(&session_id);
                }
                Decision::timed_out()
            }
        }
    }
}

impl Default for BrokerState {
    fn default() -> Self {
        Self::new()
    }
}

/// A single line-JSON request on the approve socket.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Request {
    /// A waiting hook registers and blocks for a decision.
    Await {
        session_id: String,
        #[serde(default)]
        tool: String,
        #[serde(default)]
        context: String,
    },
    /// A human (TUI/CLI) delivers a decision for a session.
    Decide {
        session_id: String,
        decision: DecisionKind,
        #[serde(default)]
        reason: String,
    },
    /// Dump the pending session ids.
    List,
}

/// Response to a DECIDE.
#[derive(Debug, Serialize)]
struct DecideAck {
    ok: bool,
    /// True when a waiter was actually blocked on that session.
    matched: bool,
}

/// Response to a LIST.
#[derive(Debug, Serialize)]
struct ListResponse {
    pending: Vec<PendingInfo>,
}

/// Accept loop for the approve socket. Runs until the listener errors
/// unrecoverably or the task is aborted (on daemon shutdown). Each
/// connection is handled on its own task so a blocked AWAIT never stalls
/// a concurrent DECIDE/LIST.
pub async fn serve(listener: UnixListener, state: BrokerState) {
    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, state).await {
                        debug!(error = ?e, "approve broker connection ended with error");
                    }
                });
            }
            Err(e) => {
                error!(error = ?e, "approve broker accept failed; continuing");
            }
        }
    }
}

/// Handle one connection: read a single request line, dispatch, respond.
async fn handle_connection(stream: UnixStream, state: BrokerState) -> Result<()> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await.context("reading request")?;
    if n == 0 {
        return Ok(()); // client hung up without sending
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    let request: Request = match serde_json::from_str(trimmed) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, line = %trimmed, "rejecting malformed broker request");
            let body = serde_json::json!({ "ok": false, "error": e.to_string() });
            write_line(reader.get_mut(), &body).await?;
            return Ok(());
        }
    };

    match request {
        Request::Await {
            session_id,
            tool,
            context,
        } => {
            let decision = state.await_decision(session_id, tool, context).await;
            write_line(reader.get_mut(), &decision).await?;
        }
        Request::Decide {
            session_id,
            decision,
            reason,
        } => {
            let matched = state.decide(&session_id, Decision { decision, reason });
            write_line(reader.get_mut(), &DecideAck { ok: true, matched }).await?;
        }
        Request::List => {
            let pending = state.list();
            write_line(reader.get_mut(), &ListResponse { pending }).await?;
        }
    }
    Ok(())
}

/// Write one line-JSON response and flush.
async fn write_line<T: Serialize>(stream: &mut UnixStream, body: &T) -> Result<()> {
    let mut buf = serde_json::to_vec(body).context("serializing response")?;
    buf.push(b'\n');
    stream.write_all(&buf).await.context("writing response")?;
    stream.flush().await.context("flushing response")?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Blocking clients.
//
// The server above is async — it rides notifyd's tokio runtime. Its clients
// are not: the `PermissionRequest` hook is a short-lived synchronous process,
// and the CLI decide/list verbs are one-shot. So the client side is plain
// blocking `std::os::unix::net` — dial in, write one line, read one line.
// No runtime, no async colouring bleeding into the sync hook path.

/// How long a re-dialing waiter keeps trying before giving up and letting the
/// caller fall back to deny. Sits *above* the broker's own AWAIT ceiling
/// ([`DEFAULT_AWAIT_TIMEOUT`], 600s) so a live broker's decision always wins
/// the race, and *below* the Claude `PermissionRequest` hook timeout (660s)
/// so we answer before Claude hard-kills the hook. The re-dial loop only
/// matters across a notifyd restart — the single resume path.
pub const CLIENT_AWAIT_DEADLINE: Duration = Duration::from_secs(640);

/// Pause between re-dials when the broker is unreachable mid-wait.
const REDIAL_INTERVAL: Duration = Duration::from_millis(500);

/// Read timeout for the one-shot decide/list round-trips.
const CLIENT_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Dial the approve socket, register an AWAIT for `session_id`, and block
/// until a human decides. Re-dials and re-sends AWAIT if the connection drops
/// mid-wait (a notifyd restart), so restarting the daemon is the single
/// resume command: every still-alive waiter re-registers itself. Falls back
/// to **deny** if no decision lands within `deadline`; never returns without
/// a decision, so the waiting hook is never lost.
#[must_use]
pub fn client_await(
    sock: &Path,
    session_id: &str,
    tool: &str,
    context: &str,
    deadline: Duration,
) -> Decision {
    let started = Instant::now();
    let req = serde_json::json!({
        "op": "await",
        "session_id": session_id,
        "tool": tool,
        "context": context,
    });
    let line = serde_json::to_string(&req).unwrap_or_default();
    loop {
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Decision::timed_out();
        }
        match try_await_once(sock, &line, remaining) {
            Ok(Some(decision)) => return decision,
            // EOF or connect/read error → broker down or dropped us mid-wait.
            // Re-dial + re-register until the deadline.
            Ok(None) | Err(_) => std::thread::sleep(REDIAL_INTERVAL),
        }
    }
}

/// One AWAIT attempt: connect, send, block for a decision. `Ok(None)` means
/// the broker closed the connection before deciding (re-dial); `Err` means
/// connect/read failed (re-dial).
fn try_await_once(
    sock: &Path,
    line: &str,
    read_timeout: Duration,
) -> std::io::Result<Option<Decision>> {
    let mut stream = StdUnixStream::connect(sock)?;
    stream.set_read_timeout(Some(read_timeout))?;
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut resp = String::new();
    if reader.read_line(&mut resp)? == 0 {
        return Ok(None);
    }
    let decision: Decision = serde_json::from_str(resp.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    Ok(Some(decision))
}

/// Deliver a decision for `session_id` to the broker (the TUI/CLI approve/deny
/// lever). Returns whether a waiter was actually blocked on that session
/// (`matched` false = nothing was waiting, e.g. it already timed out).
pub fn client_decide(
    sock: &Path,
    session_id: &str,
    decision: DecisionKind,
    reason: &str,
) -> std::io::Result<bool> {
    let resp = client_round_trip(
        sock,
        &serde_json::json!({
            "op": "decide",
            "session_id": session_id,
            "decision": decision,
            "reason": reason,
        }),
    )?;
    Ok(resp
        .get("matched")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false))
}

/// List sessions currently blocked on a human decision (Daemons overlay / CLI
/// status).
pub fn client_list(sock: &Path) -> std::io::Result<Vec<PendingInfo>> {
    let resp = client_round_trip(sock, &serde_json::json!({ "op": "list" }))?;
    let pending = resp
        .get("pending")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    Ok(serde_json::from_value(pending).unwrap_or_default())
}

/// Shared one-shot request/response for decide + list.
fn client_round_trip(
    sock: &Path,
    req: &serde_json::Value,
) -> std::io::Result<serde_json::Value> {
    let mut stream = StdUnixStream::connect(sock)?;
    stream.set_read_timeout(Some(CLIENT_RPC_TIMEOUT))?;
    let mut line = serde_json::to_string(req).unwrap_or_default();
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()?;
    let mut reader = std::io::BufReader::new(stream);
    let mut resp = String::new();
    reader.read_line(&mut resp)?;
    serde_json::from_str(resp.trim())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Bind a broker on a short-path socket (AF_UNIX has a ~104-char
    /// limit, so tempdirs under long worktree paths overflow — use /tmp).
    async fn spawn_broker(timeout: Duration) -> (std::path::PathBuf, BrokerState, tokio::task::JoinHandle<()>) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let sock = std::env::temp_dir().join(format!(
            "ainb-broker-test-{}-{}.sock",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let state = BrokerState::with_timeout(timeout);
        let state2 = state.clone();
        let handle = tokio::spawn(async move { serve(listener, state2).await });
        (sock, state, handle)
    }

    /// Poll `client_list` until an await for `session_id` is registered,
    /// or panic after ~4s. Runs the blocking client call off-thread so it
    /// never starves the server task that must answer it.
    async fn wait_for_pending(sock: &Path, session_id: &str) {
        for _ in 0..200 {
            let s = sock.to_path_buf();
            let pending = tokio::task::spawn_blocking(move || client_list(&s).unwrap_or_default())
                .await
                .unwrap();
            if pending.iter().any(|p| p.session_id == session_id) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("await for {session_id} never registered");
    }

    /// Send one request line, read one response line.
    async fn round_trip(sock: &Path, request: &str) -> String {
        let mut stream = UnixStream::connect(sock).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        stream.write_all(b"\n").await.unwrap();
        stream.flush().await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        line
    }

    #[tokio::test]
    async fn await_then_decide_approve_round_trips() {
        let (sock, _state, handle) = spawn_broker(Duration::from_secs(30)).await;

        // A waiter blocks on AWAIT.
        let sock_w = sock.clone();
        let waiter = tokio::spawn(async move {
            round_trip(
                &sock_w,
                r#"{"op":"await","session_id":"s1","tool":"Bash","context":"rm -rf /tmp/x"}"#,
            )
            .await
        });

        // Give the AWAIT a beat to register, then a human approves.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let ack = round_trip(
            &sock,
            r#"{"op":"decide","session_id":"s1","decision":"approve","reason":"ok"}"#,
        )
        .await;
        assert!(ack.contains("\"matched\":true"), "decide matched: {ack}");

        let decision = waiter.await.unwrap();
        let parsed: Decision = serde_json::from_str(decision.trim()).unwrap();
        assert_eq!(parsed.decision, DecisionKind::Approve);
        assert_eq!(parsed.reason, "ok");

        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn await_times_out_to_deny() {
        // Tiny timeout: the waiter gets a deny fallback with no human.
        let (sock, _state, handle) = spawn_broker(Duration::from_millis(80)).await;
        let decision = round_trip(&sock, r#"{"op":"await","session_id":"s2"}"#).await;
        let parsed: Decision = serde_json::from_str(decision.trim()).unwrap();
        assert_eq!(
            parsed.decision,
            DecisionKind::Deny,
            "timeout must fall back to deny, never approve"
        );
        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn list_reports_pending_await() {
        let (sock, state, handle) = spawn_broker(Duration::from_secs(30)).await;
        let sock_w = sock.clone();
        let waiter = tokio::spawn(async move {
            round_trip(
                &sock_w,
                r#"{"op":"await","session_id":"s3","tool":"Write","context":"foo.rs"}"#,
            )
            .await
        });
        // Wait for registration.
        for _ in 0..50 {
            if !state.list().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let listing = round_trip(&sock, r#"{"op":"list"}"#).await;
        assert!(listing.contains("\"session_id\":\"s3\""), "list: {listing}");
        assert!(listing.contains("\"tool\":\"Write\""), "list: {listing}");

        // Clean up the waiter with a decision.
        round_trip(
            &sock,
            r#"{"op":"decide","session_id":"s3","decision":"deny"}"#,
        )
        .await;
        let _ = waiter.await;
        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn decide_with_no_waiter_reports_unmatched() {
        let (sock, _state, handle) = spawn_broker(Duration::from_secs(30)).await;
        let ack = round_trip(
            &sock,
            r#"{"op":"decide","session_id":"ghost","decision":"approve"}"#,
        )
        .await;
        assert!(ack.contains("\"matched\":false"), "no waiter: {ack}");
        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn re_await_supersedes_and_new_await_wins() {
        // Models a waiter re-dialling (e.g. after a notifyd restart): a second
        // AWAIT on the same session replaces the first. The stale AWAIT resolves
        // to a superseded-deny; the fresh one receives the real decision.
        let (sock, state, handle) = spawn_broker(Duration::from_secs(30)).await;

        let sock1 = sock.clone();
        let stale = tokio::spawn(async move {
            round_trip(&sock1, r#"{"op":"await","session_id":"s4","tool":"Bash"}"#).await
        });
        // Ensure the stale AWAIT registered first.
        for _ in 0..50 {
            if !state.list().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let sock2 = sock.clone();
        let fresh = tokio::spawn(async move {
            round_trip(&sock2, r#"{"op":"await","session_id":"s4","tool":"Bash"}"#).await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // The stale waiter should already be resolved (superseded → deny).
        let stale_decision: Decision = serde_json::from_str(stale.await.unwrap().trim()).unwrap();
        assert_eq!(stale_decision.decision, DecisionKind::Deny);
        assert!(stale_decision.reason.contains("superseded"));

        // Decide now hits the fresh waiter.
        round_trip(
            &sock,
            r#"{"op":"decide","session_id":"s4","decision":"approve"}"#,
        )
        .await;
        let fresh_decision: Decision = serde_json::from_str(fresh.await.unwrap().trim()).unwrap();
        assert_eq!(fresh_decision.decision, DecisionKind::Approve);

        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn blocking_clients_round_trip_with_server() {
        // Proves the sync client (hook/CLI side) and the async server agree on
        // the wire: client_await blocks, client_list sees it, client_decide
        // approves, client_await returns approve.
        let (sock, _state, handle) = spawn_broker(Duration::from_secs(30)).await;

        let sock_w = sock.clone();
        let waiter = tokio::task::spawn_blocking(move || {
            client_await(
                &sock_w,
                "cli-A",
                "Bash",
                "rm -rf /tmp/x",
                Duration::from_secs(10),
            )
        });

        // Wait for the AWAIT to register. The client calls are blocking sync
        // IO; run them via spawn_blocking so they never occupy a worker thread
        // — otherwise a blocked `client_list` starves the server task that must
        // answer it, each call hits CLIENT_RPC_TIMEOUT, and the poll loop stalls
        // for minutes under whole-suite CPU pressure.
        let mut pending = Vec::new();
        for _ in 0..100 {
            let s = sock.clone();
            pending = tokio::task::spawn_blocking(move || client_list(&s).unwrap_or_default())
                .await
                .unwrap();
            if !pending.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(pending.len(), 1, "list should report one pending await");
        assert_eq!(pending[0].session_id, "cli-A");
        assert_eq!(pending[0].tool, "Bash");

        let s = sock.clone();
        let matched =
            tokio::task::spawn_blocking(move || client_decide(&s, "cli-A", DecisionKind::Approve, "ok"))
                .await
                .unwrap()
                .unwrap();
        assert!(matched, "decide should match the waiting session");

        let decision = waiter.await.unwrap();
        assert_eq!(decision.decision, DecisionKind::Approve);
        assert_eq!(decision.reason, "ok");

        handle.abort();
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn client_await_denies_when_socket_dead() {
        // Fault tolerance: no broker listening at all → the waiter never hangs
        // forever and never auto-approves; it re-dials until the deadline and
        // falls back to deny. The waiting hook is never lost.
        let sock = std::env::temp_dir().join(format!(
            "ainb-broker-dead-{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&sock);
        let sock_w = sock.clone();
        let decision = tokio::task::spawn_blocking(move || {
            client_await(&sock_w, "orphan", "Bash", "x", Duration::from_secs(1))
        })
        .await
        .unwrap();
        assert_eq!(
            decision.decision,
            DecisionKind::Deny,
            "dead socket must fall back to deny, never approve"
        );
    }

    /// The single-resume guarantee, proven end-to-end: a permission waiter
    /// parked on the broker survives a full daemon restart. We kill broker
    /// one outright (drop its runtime → every connection closes, exactly like
    /// the process exiting), rebind a *fresh* broker on the same socket path
    /// with new state that has no memory of the waiter, and the waiter's own
    /// re-dial ladder re-registers itself — then a human approve round-trips
    /// back to it. Restarting notifyd is all it takes to resume a pending
    /// prompt; no waiting hook is lost. Sync test so dropping the runtimes is
    /// safe (dropping a Runtime inside async panics).
    #[test]
    fn waiter_resumes_after_socket_restart() {
        let sock = std::env::temp_dir()
            .join(format!("ainb-broker-restart-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);

        // Poll client_list on this thread until `session_id` is registered on
        // whatever broker currently owns the socket, or panic after ~6s.
        let wait_registered = |session_id: &str| {
            for _ in 0..300 {
                if client_list(&sock)
                    .unwrap_or_default()
                    .iter()
                    .any(|p| p.session_id == session_id)
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            panic!("await for {session_id} never registered");
        };

        let spawn_broker_rt = |sock: std::path::PathBuf| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.spawn(async move {
                let listener = UnixListener::bind(&sock).unwrap();
                serve(listener, BrokerState::with_timeout(Duration::from_secs(30))).await;
            });
            rt
        };

        // Broker one comes up; a hook parks on AWAIT and registers.
        let rt1 = spawn_broker_rt(sock.clone());
        let sock_w = sock.clone();
        let waiter = std::thread::spawn(move || {
            client_await(&sock_w, "resume-A", "Bash", "rm -rf /tmp/x", Duration::from_secs(20))
        });
        wait_registered("resume-A");

        // Daemon dies: dropping the runtime aborts the accept loop AND every
        // detached connection task, closing the waiter's socket — the true
        // "process exited" signal, not a soft abort. Unlink the stale path.
        rt1.shutdown_background();
        let _ = std::fs::remove_file(&sock);

        // Restart: a fresh daemon binds the same path with brand-new state.
        let rt2 = spawn_broker_rt(sock.clone());

        // The waiter's re-dial ladder must find the new broker and re-register
        // there, all on its own — this is the resume with no lost hook.
        wait_registered("resume-A");

        // Human approves against the fresh daemon; the resumed waiter answers.
        let matched = client_decide(&sock, "resume-A", DecisionKind::Approve, "ok").unwrap();
        assert!(matched, "fresh broker should match the re-registered waiter");

        let decision = waiter.join().unwrap();
        assert_eq!(
            decision.decision,
            DecisionKind::Approve,
            "waiter must round-trip approve after the restart"
        );
        assert_eq!(decision.reason, "ok");

        rt2.shutdown_background();
        let _ = std::fs::remove_file(&sock);
    }
}
