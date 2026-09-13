// ABOUTME: The host side of the chat surface — one conversation, driven off the
// UI thread, with its effects and their outcomes in one place.
//
// `ainb-plugin-hangar`'s `fleet_chat` owns the state machine, the reducer and
// the renderer. It owns no IO: it emits a `ChatIntent` and waits to be told what
// happened. This is the thing that tells it.
//
// It exists as its own module rather than inside a screen because TWO surfaces
// drive the same conversation — the sessions screen's `thread` and `pal`
// tabs — and a second copy of "spawn a worker, page the daemon, fold the answer
// back" is how two chat surfaces drift apart in what they render and which
// failures they report.

use std::sync::{Arc, Mutex};

use ainb_plugin_hangar::screen::fleet_chat::{
    ChatIntent, ChatOpenStep, ChatSnapshot, ChatState, ChatTopic, chat_tick,
};

/// What one dispatched effect produced.
#[derive(Debug, Clone)]
pub enum ChatOutcome {
    /// The worker reached this step of the open sequence.
    ///
    /// Published from INSIDE the page rather than around it, because the thing
    /// worth reporting is the time spent in a call, not the fact that a worker
    /// started. A fresh install sits in `fleet/channel_create` and
    /// `fleet/acp_session_create` for long enough to matter, and until this
    /// existed the pane drew an ordinary composer throughout.
    Step(ChatOpenStep),
    /// A page landed. Replaces what the surface is showing.
    Paged(Box<ChatSnapshot>),
    /// A page failed. The surface keeps what it has, says why, and names the
    /// call that failed.
    PageFailed(ChatOpenStep, String),
    /// A send failed. Reported separately from a page failure because the
    /// surface has to put the operator's text BACK in the composer rather than
    /// leave them retyping it.
    SendFailed(String),
    /// A write has something to say, and it is not the SEND that went wrong.
    ///
    /// The axis here is not success: it is whether the last send's delivery
    /// legs are still true. [`Self::SendFailed`] exists because a send that
    /// failed has no legs, so it drops them and calls itself "send failed" on
    /// the feedback row. Nothing else on this surface has earned either.
    ///
    /// A cancel is the case that made the difference visible, on BOTH verdicts:
    /// the send whose turn is being cancelled is still the send the pane is
    /// showing, whether the cancel landed or was refused. If anything the
    /// refusal needs those legs more, because the operator has just failed to
    /// stop what they describe.
    ///
    /// The sentence is the CALLER's: this carries it verbatim, so a failed
    /// write says so in its own words rather than borrowing a send's.
    Notice(String),
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
    /// Open the Pal conversation.
    #[must_use]
    pub fn pal() -> Self {
        Self::new(ChatState::opening(), ChatTopic::Pal)
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
                ChatOutcome::Step(step) => self.state.apply_step(step),
                ChatOutcome::Paged(snapshot) => self.state.apply_snapshot(*snapshot),
                ChatOutcome::PageFailed(step, detail) => {
                    self.state.apply_failure(Some(step), detail);
                }
                ChatOutcome::SendFailed(detail) => self.state.apply_send_failure(detail),
                ChatOutcome::Notice(detail) => self.state.apply_notice(detail),
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
        // The scope the surface is CURRENTLY on, for the writes that carry none
        // of their own. A confirm card and a cancel both name a session or a
        // card, never a channel, and handing the page no scope does not mean
        // "page what I am looking at": the Pal page RESOLVES an absent scope
        // newest-wins, so with a second Pal channel in the store (the CLI
        // mints one on demand; `chat_page_blocking` documents a race minting one
        // by accident) the write's page swapped the operator's conversation for
        // a different one. `None` here still means "resolve", which is right
        // before the first page has named a scope.
        let surface_scope = self.state.scope_key().map(ToString::to_string);
        let spawned = std::thread::Builder::new().name("ainb-chat-host".into()).spawn(move || {
            let publish = |outcome: ChatOutcome| {
                if let Ok(mut inbox) = inbox.lock() {
                    inbox.push(outcome);
                }
            };
            // A WRITE always ends by paging, so the operator sees the durable
            // row the daemon actually stored rather than an optimistic local
            // echo that a failed write would leave behind as a lie.
            let (write_report, scope_key, receipts) = match intent {
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
                        // exactly what the daemon defaults to. A Pal write
                        // is the daemon's own MCP path and never starts here.
                        actor: None,
                        targets,
                        origin_message_id: None,
                        text,
                        request_id,
                        mutation: ainb_hangar_proto::mutation::MutationEnvelope::default(),
                    };
                    match crate::fleet::control::chat_send_blocking(params) {
                        Ok(result) => (None, Some(scope_key), Some(result.deliveries)),
                        Err(detail) => {
                            (Some(ChatOutcome::SendFailed(detail)), Some(scope_key), None)
                        }
                    }
                }
                ChatIntent::ConfirmAnswer(params) => {
                    match crate::fleet::control::chat_confirm_answer_blocking(params) {
                        Ok(_) => (None, surface_scope, None),
                        // Named for what it is. This is the same conflation the
                        // cancel below had: answering a card is not a send, so
                        // a card the daemon refused ("already answered") must
                        // not print "send failed" nor drop the legs of a send
                        // that is still running behind the card.
                        Err(detail) => (
                            Some(ChatOutcome::Notice(format!("answer failed: {detail}"))),
                            surface_scope,
                            None,
                        ),
                    }
                }
                // Cancelling is a WRITE like a send, so it ends by paging for
                // the same reason: the legs the pane then renders are the ones
                // the daemon actually holds, not an optimistic local guess that
                // the turn stopped.
                ChatIntent::CancelTurn { session_keys } => {
                    match crate::fleet::control::chat_cancel_turns_blocking(session_keys) {
                        // A cancel that lands silently is as unreadable as a
                        // send that does, so a WORKING cancel still has to put
                        // a sentence on the pane's feedback row. It gets its
                        // own outcome to do that with, on BOTH verdicts:
                        // routing it through the send-failure channel printed
                        // "send failed: cancelled 1 of 1 turn(s)" over a cancel
                        // that worked and "send failed: cancelled 0 of 1" over
                        // one that was refused, and dropped the legs of the
                        // send being cancelled either way. That send is still
                        // running in the refused case, which is precisely when
                        // the operator needs to see what is still in flight.
                        Ok(summary) => (Some(ChatOutcome::Notice(summary)), surface_scope, None),
                        Err(detail) => (
                            Some(ChatOutcome::Notice(format!("cancel failed: {detail}"))),
                            surface_scope,
                            None,
                        ),
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
            if let Some(report) = write_report {
                publish(report);
            }
            if let Some(receipts) = receipts {
                publish(ChatOutcome::Receipts(receipts));
            }
            // Page LAST, and on every path including a failed write: the page
            // is what clears the surface's in-flight latch, and a failed send
            // still has to show the operator the conversation as it now stands.
            let paged = match &topic {
                ChatTopic::Pal => crate::fleet::control::chat_page_blocking(scope_key, &|step| {
                    publish(ChatOutcome::Step(step));
                }),
                ChatTopic::Session { .. } | ChatTopic::Channel { .. } => {
                    crate::fleet::control::chat_thread_page_blocking(&topic)
                }
            };
            publish(match paged {
                Ok(snapshot) => ChatOutcome::Paged(Box::new(snapshot)),
                Err(failure) => ChatOutcome::PageFailed(failure.step, failure.detail),
            });
        });
        if let Err(error) = spawned {
            if let Ok(mut inbox) = self.inbox.lock() {
                // Blamed on the dial step: no RPC was reached, and pinning it on
                // one of the four calls would send an operator to read a daemon
                // log that has nothing in it.
                inbox.push(ChatOutcome::PageFailed(
                    ChatOpenStep::Connecting,
                    format!("chat worker did not start: {error}"),
                ));
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
    fn a_pal_host_has_no_scope_until_the_daemon_mints_one() {
        // The `channel:<ulid>` is the daemon's to mint. A client that composed
        // its own would page an empty timeline forever against a real daemon
        // while every unit test stayed green.
        let host = ChatHost::pal();
        assert_eq!(host.topic(), &ChatTopic::Pal);
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
            turn_deadline_ms: None,
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
            turn_deadline_ms: None,
        })));
        host.tick(0);
        host.inbox.lock().unwrap().push(ChatOutcome::PageFailed(
            ChatOpenStep::LoadingMessages,
            "socket refused".to_string(),
        ));
        host.tick(0);
        assert_eq!(
            host.state().scope_key(),
            Some("session:claude:abc"),
            "a failed page must keep what the surface already had"
        );
        assert!(
            matches!(
                host.state().status(),
                ainb_plugin_hangar::screen::fleet_chat::ChatStatus::Unavailable { detail, .. }
                    if detail.contains("socket refused")
            ),
            "and say why: {:?}",
            host.state().status()
        );
    }

    /// A failed page names the CALL, not just the error.
    ///
    /// "connection refused" is four different bugs depending on which of the
    /// open sequence's calls raised it, and only one of them is fixed by
    /// changing directory. The step is what tells them apart, and it has to
    /// survive the trip from the worker through the inbox to the surface.
    #[test]
    fn a_failed_page_names_the_call_that_failed_on_the_surface() {
        use ainb_plugin_hangar::screen::fleet_chat::{ChatOpenStep, ChatStatus};

        let mut host = ChatHost::pal();
        host.inbox.lock().unwrap().push(ChatOutcome::PageFailed(
            ChatOpenStep::CreatingSession,
            "scope_key is already held by a session whose cwd is /elsewhere".to_string(),
        ));
        host.tick(0);
        let ChatStatus::Unavailable {
            step: Some(step),
            detail,
        } = host.state().status()
        else {
            panic!("the failure lost its call: {:?}", host.state().status());
        };
        assert_eq!(*step, ChatOpenStep::CreatingSession);
        assert_eq!(step.call(), "fleet/acp_session_create");
        assert!(
            detail.contains("already held by a session"),
            "the daemon's own words were swallowed: {detail}"
        );
    }

    /// A write that WORKED says so without wearing a failure's clothes.
    ///
    /// The cancel this outcome exists for used to be published as a
    /// `SendFailed`, so a cancel that landed printed "send failed: cancelled 1
    /// of 1 turn(s)" and ran the failure reducer over a send that never failed.
    /// That reducer drops the legs of the send whose turn was being cancelled,
    /// which is the one thing the pane is showing while it waits. Both halves
    /// are pinned here: the sentence is the daemon's summary verbatim, and the
    /// legs survive.
    #[test]
    fn a_notice_puts_the_summary_on_the_feedback_row_without_failing_the_send() {
        use ainb_hangar_proto::fleet::{ActionReceiptStatus, FleetMessageDelivery};

        let mut host = ChatHost::pal();
        host.inbox
            .lock()
            .unwrap()
            .push(ChatOutcome::Receipts(vec![FleetMessageDelivery {
                session_key: "claude:one".to_string(),
                state: ActionReceiptStatus::Pending,
                detail: None,
            }]));
        host.tick(0);
        assert_eq!(
            host.state().receipts().len(),
            1,
            "the send's leg never landed"
        );

        host.inbox
            .lock()
            .unwrap()
            .push(ChatOutcome::Notice("cancelled 1 of 1 turn(s)".to_string()));
        assert!(host.tick(0), "a landed notice makes the frame dirty");
        assert_eq!(
            host.state().feedback(),
            Some("cancelled 1 of 1 turn(s)"),
            "a cancel that worked reads as a send that broke"
        );
        assert_eq!(
            host.state().receipts().len(),
            1,
            "the notice dropped the legs of the send whose turn was cancelled"
        );
    }

    /// A step published mid-page reaches the surface on the next frame.
    ///
    /// This is what makes the cold open legible: the worker is still inside
    /// `fleet/channel_create` when the frame that renders "creating the Pal
    /// channel" is drawn. A step that only landed with the finished page would
    /// report progress that had already finished.
    #[test]
    fn a_step_published_mid_page_reaches_the_surface_before_the_page_does() {
        use ainb_plugin_hangar::screen::fleet_chat::{ChatOpenStep, ChatStatus};

        let mut host = ChatHost::pal();
        host.inbox
            .lock()
            .unwrap()
            .push(ChatOutcome::Step(ChatOpenStep::CreatingChannel));
        assert!(host.tick(0), "a landed step makes the frame dirty");
        assert_eq!(
            host.state().status(),
            &ChatStatus::Opening(ChatOpenStep::CreatingChannel)
        );
        assert_eq!(
            host.state().send_block().as_deref(),
            Some("creating the Pal channel (fleet/channel_create)"),
            "the composer does not say the create is why it cannot send yet"
        );
    }
}
