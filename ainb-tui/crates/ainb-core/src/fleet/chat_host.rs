// ABOUTME: The host side of the chat surface — one conversation, driven off the
// UI thread, with its effects and their outcomes in one place.
//
// `ainb-plugin-hangar`'s `fleet_chat` owns the state machine, the reducer and
// the renderer. It owns no IO: it emits a `ChatIntent` and waits to be told what
// happened. This is the thing that tells it.
//
// It exists as its own module rather than inside a screen because TWO surfaces
// drive the same conversation — the sessions screen's `thread` and `copilot`
// tabs — and a second copy of "spawn a worker, page the daemon, fold the answer
// back" is how two chat surfaces drift apart in what they render and which
// failures they report.

use std::sync::{Arc, Mutex};

use ainb_plugin_hangar::screen::fleet_chat::{
    ChatIntent, ChatSnapshot, ChatState, ChatTopic, chat_tick,
};

/// What one dispatched effect produced.
#[derive(Debug, Clone)]
pub enum ChatOutcome {
    /// A page landed. Replaces what the surface is showing.
    Paged(Box<ChatSnapshot>),
    /// A page failed. The surface keeps what it has and says why.
    PageFailed(String),
    /// A send failed. Reported separately from a page failure because the
    /// surface has to put the operator's text BACK in the composer rather than
    /// leave them retyping it.
    SendFailed(String),
    /// The per-recipient delivery legs of a send.
    Receipts(Vec<ainb_hangar_proto::fleet::FleetMessageDelivery>),
}

/// A conversation the sessions screen is showing, plus its in-flight effects.
#[derive(Debug)]
pub struct ChatHost {
    state: ChatState,
    topic: ChatTopic,
    inbox: Arc<Mutex<Vec<ChatOutcome>>>,
}

impl ChatHost {
    /// Open the copilot conversation.
    #[must_use]
    pub fn copilot() -> Self {
        Self::new(ChatState::opening(), ChatTopic::Copilot)
    }

    /// Open one session's own thread.
    #[must_use]
    pub fn thread(session_key: String) -> Self {
        Self::new(
            ChatState::thread(session_key.clone()),
            ChatTopic::Session { session_key },
        )
    }

    fn new(state: ChatState, topic: ChatTopic) -> Self {
        Self {
            state,
            topic,
            inbox: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Which conversation this host is showing.
    #[must_use]
    pub const fn topic(&self) -> &ChatTopic {
        &self.topic
    }

    /// The surface state, for rendering.
    #[must_use]
    pub const fn state(&self) -> &ChatState {
        &self.state
    }

    /// The surface state, for the key reducer.
    pub const fn state_mut(&mut self) -> &mut ChatState {
        &mut self.state
    }

    /// Fold every landed outcome in, then let the surface ask for its next
    /// effect.
    ///
    /// Called once per frame. This is what makes a reply appear without the
    /// operator pressing anything: the tick asks for a refresh on its own
    /// cadence, and the reducer latches an in-flight flag before emitting, so a
    /// per-frame call cannot spawn a worker per repaint.
    ///
    /// Returns `true` when anything changed, so the caller can mark the frame
    /// dirty without diffing the whole surface.
    pub fn tick(&mut self, now_ms: i64) -> bool {
        let landed: Vec<ChatOutcome> = self
            .inbox
            .lock()
            .map(|mut inbox| inbox.drain(..).collect())
            .unwrap_or_else(|poisoned| poisoned.into_inner().drain(..).collect());
        let changed = !landed.is_empty();
        for outcome in landed {
            match outcome {
                ChatOutcome::Paged(snapshot) => self.state.apply_snapshot(*snapshot),
                ChatOutcome::PageFailed(detail) => self.state.apply_failure(detail),
                ChatOutcome::SendFailed(detail) => self.state.apply_send_failure(detail),
                ChatOutcome::Receipts(receipts) => self.state.apply_receipts(receipts),
            }
        }
        if let Some(intent) = chat_tick(&mut self.state, now_ms) {
            self.dispatch(intent);
        }
        changed
    }

    /// Perform one effect on a detached worker.
    ///
    /// Every exit path publishes SOMETHING into the inbox — a page, a page
    /// failure or a send failure. A worker that returned silently would leave
    /// the surface's in-flight latch set forever, which renders as a spinner
    /// that never resolves and is exactly the symptom this screen exists to
    /// stop.
    pub fn dispatch(&self, intent: ChatIntent) {
        let inbox = Arc::clone(&self.inbox);
        let topic = self.topic.clone();
        let spawned = std::thread::Builder::new().name("ainb-chat-host".into()).spawn(move || {
            let publish = |outcome: ChatOutcome| {
                if let Ok(mut inbox) = inbox.lock() {
                    inbox.push(outcome);
                }
            };
            // A WRITE always ends by paging, so the operator sees the durable
            // row the daemon actually stored rather than an optimistic local
            // echo that a failed write would leave behind as a lie.
            let (write_failure, scope_key, receipts) = match intent {
                ChatIntent::Refresh { scope_key, .. } => (None, scope_key, None),
                ChatIntent::Send {
                    scope_key,
                    targets,
                    text,
                    request_id,
                    ..
                } => {
                    let params = ainb_hangar_proto::fleet::FleetMessageSendParams {
                        scope_key: Some(scope_key.clone()),
                        // No actor: an operator send omits the key, which is
                        // exactly what the daemon defaults to. A copilot write
                        // is the daemon's own MCP path and never starts here.
                        actor: None,
                        targets,
                        origin_message_id: None,
                        text,
                        request_id,
                    };
                    match crate::fleet::control::chat_send_blocking(params) {
                        Ok(result) => (None, Some(scope_key), Some(result.deliveries)),
                        Err(detail) => (Some(detail), Some(scope_key), None),
                    }
                }
                ChatIntent::ConfirmAnswer(params) => {
                    match crate::fleet::control::chat_confirm_answer_blocking(params) {
                        Ok(_) => (None, None, None),
                        Err(detail) => (Some(detail), None, None),
                    }
                }
                // Neither belongs to a conversation: a create mints the scope a
                // conversation would be opened ON, and a list is the picker's
                // read. Both are the channel surface's, which arrives with
                // broadcast.
                ChatIntent::CreateChannel { .. } | ChatIntent::ListChannels => {
                    publish(ChatOutcome::SendFailed(
                        "channel management is not part of this pane".to_string(),
                    ));
                    return;
                }
            };
            if let Some(detail) = write_failure {
                publish(ChatOutcome::SendFailed(detail));
            }
            if let Some(receipts) = receipts {
                publish(ChatOutcome::Receipts(receipts));
            }
            // Page LAST, and on every path including a failed write: the page
            // is what clears the surface's in-flight latch, and a failed send
            // still has to show the operator the conversation as it now stands.
            let paged = match &topic {
                ChatTopic::Copilot => crate::fleet::control::chat_page_blocking(scope_key),
                ChatTopic::Session { .. } | ChatTopic::Channel { .. } => {
                    crate::fleet::control::chat_thread_page_blocking(&topic)
                }
            };
            publish(match paged {
                Ok(snapshot) => ChatOutcome::Paged(Box::new(snapshot)),
                Err(detail) => ChatOutcome::PageFailed(detail),
            });
        });
        if let Err(error) = spawned {
            if let Ok(mut inbox) = self.inbox.lock() {
                inbox.push(ChatOutcome::PageFailed(format!(
                    "chat worker did not start: {error}"
                )));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_host_carries_its_sessions_scope() {
        let host = ChatHost::thread("claude:abc".to_string());
        assert_eq!(
            host.topic(),
            &ChatTopic::Session {
                session_key: "claude:abc".to_string()
            }
        );
        assert_eq!(
            host.topic().scope_key().as_deref(),
            Some("session:claude:abc")
        );
    }

    #[test]
    fn a_copilot_host_has_no_scope_until_the_daemon_mints_one() {
        // The `channel:<ulid>` is the daemon's to mint. A client that composed
        // its own would page an empty timeline forever against a real daemon
        // while every unit test stayed green.
        let host = ChatHost::copilot();
        assert_eq!(host.topic(), &ChatTopic::Copilot);
        assert_eq!(host.state().scope_key(), None);
    }

    #[test]
    fn a_landed_page_reaches_the_surface_on_the_next_tick() {
        let mut host = ChatHost::thread("claude:abc".to_string());
        host.inbox.lock().unwrap().push(ChatOutcome::Paged(Box::new(ChatSnapshot {
            scope_key: Some("session:claude:abc".to_string()),
            target_session_key: Some("claude:abc".to_string()),
            messages: Vec::new(),
            confirms: Vec::new(),
            confirms_detail: None,
            session_detail: None,
            activity: Vec::new(),
        })));
        assert!(host.tick(0), "a landed outcome makes the frame dirty");
        assert_eq!(host.state().scope_key(), Some("session:claude:abc"));
        // Deliberately NOT asserting that a second tick reports clean: `tick`
        // also lets the surface dispatch its next refresh, and that worker can
        // publish a failure back into the inbox before the next call. It did,
        // on CI, and not on the machine this was written on. What must hold is
        // that nothing landing leaves the surface where it was.
        host.tick(0);
        assert_eq!(host.state().scope_key(), Some("session:claude:abc"));
    }

    #[test]
    fn a_failed_page_is_reported_and_does_not_blank_the_surface() {
        let mut host = ChatHost::thread("claude:abc".to_string());
        host.inbox.lock().unwrap().push(ChatOutcome::Paged(Box::new(ChatSnapshot {
            scope_key: Some("session:claude:abc".to_string()),
            target_session_key: Some("claude:abc".to_string()),
            messages: Vec::new(),
            confirms: Vec::new(),
            confirms_detail: None,
            session_detail: None,
            activity: Vec::new(),
        })));
        host.tick(0);
        host.inbox
            .lock()
            .unwrap()
            .push(ChatOutcome::PageFailed("socket refused".to_string()));
        host.tick(0);
        assert_eq!(
            host.state().scope_key(),
            Some("session:claude:abc"),
            "a failed page must keep what the surface already had"
        );
        assert!(
            matches!(
                host.state().status(),
                ainb_plugin_hangar::screen::fleet_chat::ChatStatus::Unavailable(detail)
                    if detail.contains("socket refused")
            ),
            "and say why: {:?}",
            host.state().status()
        );
    }
}
