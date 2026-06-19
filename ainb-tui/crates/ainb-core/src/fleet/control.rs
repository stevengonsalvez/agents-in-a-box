// ABOUTME: Fleet control-panel actions — dispatch a real send to a session
// resolved from a `current_state` row, off the UI thread.
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

use std::sync::{Arc, Mutex};

/// Transient feedback published by the async action worker and rendered by the
/// fleet panel's status line. Cloned cheaply; the lock is held only for the
/// microseconds it takes to read/replace the line — never across send I/O.
#[derive(Debug, Clone, Default)]
pub struct ActionFeedback {
    /// Human-readable status, e.g. "answered ask → /work/x: sent via tmux".
    pub message: String,
}

/// Dispatch a send to the session described by `(session_id, cwd)`, off the UI
/// thread. Resolves the row to a live `Session` (for its tmux name) via the
/// same discovery the CLI uses, then sends `text` through `fleet::send::send`.
///
/// Runs on a detached worker thread owning a current-thread tokio runtime so
/// the render loop never blocks on `tmux has-session`/`send-keys`. The outcome
/// is published into the shared [`ActionFeedback`] so the next frame shows it.
///
/// `kind_label` is a short verb for the feedback line ("answered ask",
/// "broadcast").
pub fn dispatch_send(
    feedback: Arc<Mutex<ActionFeedback>>,
    session_id: String,
    cwd: String,
    text: String,
    kind_label: &'static str,
) {
    let target_label = if cwd.is_empty() {
        session_id.clone()
    } else {
        cwd.clone()
    };
    // Keep a handle for the spawn-failure path; the worker thread takes the
    // original.
    let spawn_err_feedback = Arc::clone(&feedback);
    std::thread::Builder::new()
        .name("ainb-fleet-send".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    publish(&feedback, format!("send failed: runtime build error: {e}"));
                    return;
                }
            };
            rt.block_on(async move {
                let outcome = resolve_and_send(&session_id, &cwd, &text).await;
                publish(
                    &feedback,
                    format!("{kind_label} → {target_label}: {outcome}"),
                );
            });
        })
        .map_err(|e| {
            tracing::warn!(error = %e, "fleet panel: send worker thread spawn failed");
            publish(
                &spawn_err_feedback,
                format!("send failed: thread spawn error: {e}"),
            );
        })
        .ok();
}

/// Resolve a `current_state` row to a live `Session` (by exact session id, then
/// by cwd) and send `text` to it. Returns a human-readable outcome string.
async fn resolve_and_send(session_id: &str, cwd: &str, text: &str) -> String {
    use crate::fleet::discover::{discover_from_ainb, discover_from_peers, merge_sessions};
    use crate::fleet::send::send;
    use crate::fleet::types::SendOutcome;

    let (ainb, peers) = tokio::join!(discover_from_ainb(), async { discover_from_peers() });
    let merged = merge_sessions(vec![ainb.unwrap_or_default(), peers.unwrap_or_default()]);

    // Prefer an exact session-id match; fall back to the cwd correlation the
    // rest of the fleet read-side uses (current_state is keyed by session_id +
    // cwd, but a discovered Session's id may differ from the hook session id).
    let target = merged
        .iter()
        .find(|s| s.id == session_id)
        .or_else(|| merged.iter().find(|s| !cwd.is_empty() && s.cwd == cwd));

    let Some(session) = target else {
        return "no live session matched (target may have exited)".to_string();
    };

    match send(session, text).await {
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
