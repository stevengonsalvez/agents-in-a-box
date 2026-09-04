// ABOUTME: One message to every checked session — the composer, the send, and
// what each recipient's leg actually did.
//
// NOT a `ChatHost`. A thread and the copilot are CONVERSATIONS: they have a
// durable scope, a timeline, and a page that reloads it. A broadcast has none
// of those — `fleet/broadcast` fans one text out to N sessions and answers with
// N receipts, and there is nothing to page afterwards. Modelling it as a
// conversation would mean inventing a scope for it, which is exactly the
// "named channel" the copilot tab already offers for operators who want one.

use std::sync::{Arc, Mutex};

use ainb_hangar_proto::fleet::{ActionReceiptStatus, FleetActionReceipt};

/// Where the broadcast is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum BroadcastPhase {
    /// Typing. The composer holds the text.
    #[default]
    Composing,
    /// `fleet/broadcast` is out on the wire.
    Sending,
    /// It came back. Every recipient's leg, in the daemon's order.
    ///
    /// Kept until the operator clears it: a receipt list that vanished on the
    /// next repaint would make a partial failure unreadable, and a partial
    /// failure is the case this pane exists to show.
    Sent(Vec<FleetActionReceipt>),
    /// The CALL failed, as opposed to a recipient refusing. Nothing was sent.
    Failed(String),
}

/// One landed effect.
#[derive(Debug, Clone)]
enum BroadcastOutcome {
    Sent(Vec<FleetActionReceipt>),
    Failed(String),
}

/// The broadcast composer and its in-flight send.
#[derive(Debug, Default)]
pub struct Broadcast {
    text: String,
    phase: BroadcastPhase,
    inbox: Arc<Mutex<Vec<BroadcastOutcome>>>,
}

impl Broadcast {
    /// What has been typed.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Where the send is.
    #[must_use]
    pub const fn phase(&self) -> &BroadcastPhase {
        &self.phase
    }

    /// Whether the composer is holding the keyboard.
    ///
    /// False while a send is in flight, so the operator cannot type into a
    /// message that has already gone.
    #[must_use]
    pub fn capturing(&self) -> bool {
        !matches!(self.phase, BroadcastPhase::Sending)
    }

    /// Type one character.
    pub fn push(&mut self, ch: char) {
        if self.capturing() {
            self.clear_receipts();
            self.text.push(ch);
        }
    }

    /// Delete the last character.
    pub fn backspace(&mut self) {
        if self.capturing() {
            self.clear_receipts();
            self.text.pop();
        }
    }

    /// A new message clears the last one's receipts: leaving them up while a
    /// different text is being typed reads as though THIS message was sent.
    fn clear_receipts(&mut self) {
        if matches!(
            self.phase,
            BroadcastPhase::Sent(_) | BroadcastPhase::Failed(_)
        ) {
            self.phase = BroadcastPhase::Composing;
        }
    }

    /// Fold in whatever landed. `true` when anything changed.
    pub fn tick(&mut self) -> bool {
        let landed: Vec<BroadcastOutcome> = self
            .inbox
            .lock()
            .map(|mut inbox| inbox.drain(..).collect())
            .unwrap_or_else(|poisoned| poisoned.into_inner().drain(..).collect());
        let changed = !landed.is_empty();
        for outcome in landed {
            self.phase = match outcome {
                BroadcastOutcome::Sent(receipts) => {
                    // The text goes only on a send that actually left. A failed
                    // CALL keeps it, because the operator would otherwise
                    // retype a message the fleet never saw.
                    self.text.clear();
                    BroadcastPhase::Sent(receipts)
                }
                BroadcastOutcome::Failed(detail) => BroadcastPhase::Failed(detail),
            };
        }
        changed
    }

    /// Send to `targets`, if there is anything to send and anyone to send it to.
    ///
    /// Returns whether a send started, so the caller can leave the key alone
    /// when it did not rather than reporting a send that never happened.
    pub fn send(&mut self, targets: Vec<String>) -> bool {
        if !self.capturing() || self.text.trim().is_empty() || targets.is_empty() {
            return false;
        }
        let text = self.text.clone();
        self.phase = BroadcastPhase::Sending;
        let inbox = Arc::clone(&self.inbox);
        // The idempotency key is minted ONCE per send, here, so a retry of the
        // same in-flight call cannot fan a second copy out to the fleet.
        let idempotency_key = format!("tui-broadcast:{}", uuid::Uuid::new_v4().simple());
        let publish_inbox = Arc::clone(&inbox);
        let spawned = std::thread::Builder::new().name("ainb-broadcast".into()).spawn(move || {
            let outcome =
                match crate::fleet::control::broadcast_blocking(targets, text, idempotency_key) {
                    Ok(receipts) => BroadcastOutcome::Sent(receipts),
                    Err(detail) => BroadcastOutcome::Failed(detail),
                };
            if let Ok(mut inbox) = publish_inbox.lock() {
                inbox.push(outcome);
            }
        });
        if let Err(error) = spawned {
            // Published rather than set directly: `tick` owns the phase, and a
            // second writer is how a surface ends up latched on `Sending`.
            if let Ok(mut inbox) = inbox.lock() {
                inbox.push(BroadcastOutcome::Failed(format!(
                    "the broadcast worker did not start: {error}"
                )));
            }
        }
        true
    }
}

/// How many recipients refused or failed, out of how many were asked.
///
/// A broadcast's headline number: "sent to 4" is a lie when one of them was
/// rejected, and the operator has to be able to see that at a glance rather
/// than reading four rows to find the one that did not land.
#[must_use]
pub fn tally(receipts: &[FleetActionReceipt]) -> (usize, usize) {
    let delivered = receipts
        .iter()
        .filter(|receipt| receipt.status == ActionReceiptStatus::Delivered)
        .count();
    (delivered, receipts.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt(status: ActionReceiptStatus) -> FleetActionReceipt {
        FleetActionReceipt {
            request_id: "r-1".to_string(),
            session_key: "claude:one".to_string(),
            action_kind: "send_prompt".to_string(),
            action_fingerprint: "fp".to_string(),
            expected_version: 1,
            idempotency_key: None,
            status,
            detail: None,
            session_version: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn an_empty_message_or_no_targets_sends_nothing() {
        let mut broadcast = Broadcast::default();
        assert!(
            !broadcast.send(vec!["claude:one".to_string()]),
            "empty text"
        );
        broadcast.push('h');
        assert!(!broadcast.send(vec![]), "no recipients");
        // Whitespace is not a message.
        let mut blank = Broadcast::default();
        blank.push(' ');
        assert!(!blank.send(vec!["claude:one".to_string()]));
    }

    /// A failed CALL keeps the text. The fleet never saw it, and retyping a
    /// message the daemon rejected is the worst possible answer to a daemon
    /// that was briefly down.
    #[test]
    fn a_failed_call_keeps_the_message_and_a_send_clears_it() {
        let mut broadcast = Broadcast::default();
        for ch in "ship it".chars() {
            broadcast.push(ch);
        }
        broadcast
            .inbox
            .lock()
            .unwrap()
            .push(BroadcastOutcome::Failed("daemon down".into()));
        assert!(broadcast.tick());
        assert_eq!(broadcast.text(), "ship it");
        assert_eq!(
            broadcast.phase(),
            &BroadcastPhase::Failed("daemon down".to_string())
        );

        broadcast.inbox.lock().unwrap().push(BroadcastOutcome::Sent(vec![receipt(
            ActionReceiptStatus::Delivered,
        )]));
        broadcast.tick();
        assert_eq!(broadcast.text(), "", "a landed send clears the composer");
    }

    /// Typing after a send clears the previous receipts: leaving them up while
    /// a different message is being written reads as though THIS one was sent.
    #[test]
    fn typing_after_a_send_drops_the_previous_receipts() {
        let mut broadcast = Broadcast::default();
        broadcast.inbox.lock().unwrap().push(BroadcastOutcome::Sent(vec![receipt(
            ActionReceiptStatus::Delivered,
        )]));
        broadcast.tick();
        assert!(matches!(broadcast.phase(), BroadcastPhase::Sent(_)));
        broadcast.push('n');
        assert_eq!(broadcast.phase(), &BroadcastPhase::Composing);
    }

    /// The composer stops taking keys while the send is out, so an operator
    /// cannot type into a message that has already gone.
    #[test]
    fn the_composer_is_shut_while_a_send_is_in_flight() {
        let mut broadcast = Broadcast::default();
        broadcast.phase = BroadcastPhase::Sending;
        broadcast.push('x');
        broadcast.backspace();
        assert_eq!(broadcast.text(), "");
        assert!(!broadcast.capturing());
    }

    /// A partial failure must be visible as a number, not only as one row in a
    /// list the operator has to read to the end.
    #[test]
    fn the_tally_counts_only_delivered_legs() {
        assert_eq!(tally(&[]), (0, 0));
        assert_eq!(
            tally(&[
                receipt(ActionReceiptStatus::Delivered),
                receipt(ActionReceiptStatus::Rejected),
                receipt(ActionReceiptStatus::Delivered),
            ]),
            (2, 3)
        );
    }
}
