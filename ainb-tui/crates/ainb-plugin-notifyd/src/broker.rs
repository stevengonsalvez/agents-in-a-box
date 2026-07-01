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
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
}
