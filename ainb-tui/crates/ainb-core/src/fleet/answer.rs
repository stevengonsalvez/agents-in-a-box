// ABOUTME: The `ask` tab's answer machine — pick, compose, send, and say what
// happened.
//
// The chip told the operator a session needed them; this is the part that lets
// them do something about it. Two transports, chosen by the row not by a
// setting:
//
// ```text
//   daemon row  ──▶ attention/answer  ──▶ first-answer-wins + the daemon's own
//                                        verified last-mile send
//   local row   ──▶ fleet-core send   ──▶ the session's own pane, which is what
//                                        keeps this working with no daemon
// ```
//
// The state machine is PURE and lives here; the IO is two blocking calls in
// `control` dispatched on a worker. That split is what lets every rule below —
// which key moves the cursor, what a failed send does to the chip, whether a
// second Enter can double-send — be tested without a socket or a pane.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::attention::{Answerable, SessionAttention};

/// Where the answer is coming from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskFocus {
    /// One of the structured options is selected.
    Options,
    /// The operator is typing a free-text answer.
    FreeText,
}

/// What the last send did, when one has been fired.
#[derive(Debug, Clone)]
pub enum AnswerPhase {
    /// Sent, waiting for the transport to report. The chip reads `SENT`.
    InFlight {
        /// When it was fired, so the pane can show how long it has been going.
        since: Instant,
        /// The text that was sent, kept so a failure can put a TYPED answer
        /// back. `None` when the answer was a picked option: that option is
        /// still highlighted, and writing its label into the composer would
        /// move the operator to a different row carrying an answer they never
        /// typed.
        draft: Option<String>,
    },
    /// The transport reported delivery. The chip clears on the next refresh,
    /// when the producer stops reporting the row.
    Delivered {
        /// How it was delivered, e.g. `tmux (session-name)`.
        via: String,
    },
    /// Nothing was delivered. The chip goes BACK to ASK and this is why.
    Failed {
        /// The reason, verbatim from the transport.
        reason: String,
    },
}

/// The `ask` pane's own state.
///
/// Reset when the operator moves to a different request: an option cursor left
/// over from the previous question would pre-select an answer to a question
/// nobody read.
#[derive(Debug)]
pub struct AskState {
    /// The chip this state belongs to, so a stale one is discarded rather than
    /// applied to whatever is selected now.
    request: Option<String>,
    focus: AskFocus,
    cursor: usize,
    free_text: String,
    phase: Option<AnswerPhase>,
    inbox: Arc<Mutex<Vec<AnswerPhase>>>,
}

impl Default for AskState {
    fn default() -> Self {
        Self {
            request: None,
            focus: AskFocus::Options,
            cursor: 0,
            free_text: String::new(),
            phase: None,
            inbox: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

/// A stable identity for one open request, so the pane can tell "the same
/// question, re-reported" from "a different question".
///
/// The daemon's attention id when there is one; otherwise the kind and the
/// instant it was raised, which is the most a local row can offer.
#[must_use]
pub fn request_id(chip: &SessionAttention) -> String {
    match &chip.answerable {
        Answerable::Daemon { attention_id } => attention_id.clone(),
        _ => format!("{}:{}", chip.kind.label(), chip.since_ms),
    }
}

impl AskState {
    /// Point this state at `chip`, resetting it if the request changed.
    pub fn retarget(&mut self, chip: &SessionAttention) {
        let id = request_id(chip);
        if self.request.as_deref() == Some(id.as_str()) {
            return;
        }
        // A cursor or a half-typed answer left over from the previous question
        // would pre-load a reply to a question nobody has read.
        *self = Self {
            request: Some(id),
            // A request with no structured options has only one place an answer
            // can come from, so start there. Defaulting to the option list
            // leaves the operator on an empty list, typing into a composer that
            // is not focused and shows neither their text nor a caret.
            focus: if chip.options.is_empty() {
                AskFocus::FreeText
            } else {
                AskFocus::Options
            },
            ..Self::default()
        };
    }

    /// Which half of the pane the keyboard is driving.
    #[must_use]
    pub const fn focus(&self) -> AskFocus {
        self.focus
    }

    /// The selected option index.
    #[must_use]
    pub const fn cursor(&self) -> usize {
        self.cursor
    }

    /// The free-text buffer.
    #[must_use]
    pub fn free_text(&self) -> &str {
        &self.free_text
    }

    /// What the last send did.
    #[must_use]
    pub const fn phase(&self) -> Option<&AnswerPhase> {
        self.phase.as_ref()
    }

    /// What the last send did TO THIS CHIP, or `None` when the phase belongs to
    /// a different request.
    ///
    /// The chip strip paints every row; without this identity check an
    /// in-flight answer on one session would render every other session's chip
    /// as `SENT`.
    #[must_use]
    pub fn phase_for(&self, chip: &SessionAttention) -> Option<&AnswerPhase> {
        (self.request.as_deref() == Some(request_id(chip).as_str())).then_some(())?;
        self.phase.as_ref()
    }

    /// Whether a send is still outstanding.
    #[must_use]
    pub const fn in_flight(&self) -> bool {
        matches!(self.phase, Some(AnswerPhase::InFlight { .. }))
    }

    /// Move the option cursor, wrapping. A free-text row sits after the last
    /// option, which is how the operator reaches the composer with the arrows
    /// alone.
    pub fn move_cursor(&mut self, chip: &SessionAttention, delta: isize) {
        let rows = chip.options.len() + 1;
        let next = (self.cursor as isize + delta).rem_euclid(rows as isize) as usize;
        self.cursor = next;
        self.focus = if next >= chip.options.len() {
            AskFocus::FreeText
        } else {
            AskFocus::Options
        };
    }

    /// Type one character into the free-text answer.
    pub fn push_char(&mut self, c: char) {
        self.free_text.push(c);
    }

    /// Delete the last character of the free-text answer.
    pub fn backspace(&mut self) {
        self.free_text.pop();
    }

    /// The text this pane would send right now, or why it would not.
    ///
    /// # Errors
    ///
    /// Returns the reason there is nothing to send.
    pub fn answer_text(&self, chip: &SessionAttention) -> Result<String, String> {
        match self.focus {
            AskFocus::Options => chip
                .options
                .get(self.cursor)
                .map(|option| option.label.clone())
                .ok_or_else(|| "no option selected".to_string()),
            AskFocus::FreeText => {
                let text = self.free_text.trim();
                if text.is_empty() {
                    // An empty answer delivered into an agent's open picker is
                    // a bare Enter, which picks whatever the picker had
                    // highlighted. Refusing is the only safe reading.
                    Err("type an answer first".to_string())
                } else {
                    Ok(text.to_string())
                }
            }
        }
    }

    /// Fold in whatever the worker has reported. Returns `true` when something
    /// landed, so the caller can mark the frame dirty.
    pub fn tick(&mut self) -> bool {
        let landed: Vec<AnswerPhase> = self
            .inbox
            .lock()
            .map(|mut inbox| inbox.drain(..).collect())
            .unwrap_or_else(|poisoned| poisoned.into_inner().drain(..).collect());
        let changed = !landed.is_empty();
        for phase in landed {
            // A failure puts a TYPED answer back, so nobody retypes what the
            // transport lost. A picked option is deliberately not restored: it
            // is still highlighted where the operator left it, and its label in
            // the composer would read as an answer they wrote.
            if let (
                AnswerPhase::Failed { .. },
                Some(AnswerPhase::InFlight {
                    draft: Some(text), ..
                }),
            ) = (&phase, &self.phase)
            {
                self.free_text.clone_from(text);
                self.focus = AskFocus::FreeText;
            }
            self.phase = Some(phase);
        }
        changed
    }

    /// Send the current answer for `chip`.
    ///
    /// # Errors
    ///
    /// Returns the reason nothing was sent — no answer chosen, a send already
    /// outstanding, or no transport at all.
    pub fn send(
        &mut self,
        chip: &SessionAttention,
        session_id: &str,
        cwd: &str,
    ) -> Result<(), String> {
        // One outstanding send at a time. Key-repeat on Enter would otherwise
        // deliver the same answer N times into an agent's open picker, and the
        // picker would consume each one as a separate keystroke.
        if self.in_flight() {
            return Err("an answer is already in flight".to_string());
        }
        if let Some(refusal) = chip.answerable.refusal() {
            return Err(refusal.to_string());
        }
        let text = self.answer_text(chip)?;
        // Only a typed answer is a draft worth restoring.
        let draft = (self.focus == AskFocus::FreeText).then(|| self.free_text.clone());
        let inbox = Arc::clone(&self.inbox);
        let route = chip.answerable.clone();
        let session_id = session_id.to_string();
        let cwd = cwd.to_string();
        let sent = text.clone();
        let spawned = std::thread::Builder::new().name("ainb-ask-send".into()).spawn(move || {
            let outcome = match route {
                Answerable::Daemon { attention_id } => {
                    crate::fleet::control::answer_via_daemon_blocking(attention_id, sent)
                }
                Answerable::Tmux { .. } => {
                    crate::fleet::control::answer_via_tmux_blocking(&session_id, &cwd, &sent)
                }
                // Refused above; unreachable, and a panic here would take the
                // whole TUI down for a state that simply cannot arrive.
                Answerable::No(why) => Err(why.reason().to_string()),
            };
            if let Ok(mut inbox) = inbox.lock() {
                inbox.push(match outcome {
                    Ok(via) => AnswerPhase::Delivered { via },
                    Err(reason) => AnswerPhase::Failed { reason },
                });
            }
        });
        match spawned {
            Ok(_) => {
                self.phase = Some(AnswerPhase::InFlight {
                    since: Instant::now(),
                    draft,
                });
                Ok(())
            }
            // The worker never ran, so there is nothing in flight to wait for.
            // Reported rather than latched, or the pane would show a spinner
            // for a send that was never attempted.
            Err(error) => Err(format!("send worker did not start: {error}")),
        }
    }

    /// How long the outstanding send has been going, for the pane to show.
    #[must_use]
    pub fn elapsed(&self) -> Option<Duration> {
        match &self.phase {
            Some(AnswerPhase::InFlight { since, .. }) => Some(since.elapsed()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::attention::{AttentionKind, AttentionOption, Unanswerable};

    fn ask_with_options(labels: &[&str]) -> SessionAttention {
        SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-1".into()).with_options(
            labels
                .iter()
                .map(|label| AttentionOption {
                    label: (*label).to_string(),
                    description: String::new(),
                })
                .collect(),
        )
    }

    #[test]
    fn the_cursor_wraps_through_the_options_and_the_free_text_row() {
        let chip = ask_with_options(&["a", "b"]);
        let mut state = AskState::default();
        assert_eq!(state.focus(), AskFocus::Options);
        state.move_cursor(&chip, 1);
        assert_eq!(state.cursor(), 1);
        // One past the last option is the composer, which is how the arrows
        // alone reach free text.
        state.move_cursor(&chip, 1);
        assert_eq!(state.focus(), AskFocus::FreeText);
        state.move_cursor(&chip, 1);
        assert_eq!(state.cursor(), 0);
        assert_eq!(state.focus(), AskFocus::Options);
        // And backwards from the top lands on the composer.
        state.move_cursor(&chip, -1);
        assert_eq!(state.focus(), AskFocus::FreeText);
    }

    #[test]
    fn a_request_with_no_options_starts_on_the_composer() {
        // Not after a key press — on ARRIVAL. There is only one place an answer
        // can come from, and starting on an empty option list leaves the
        // operator typing into a composer that shows neither their text nor a
        // caret.
        let chip = ask_with_options(&[]);
        let mut state = AskState::default();
        state.retarget(&chip);
        assert_eq!(state.focus(), AskFocus::FreeText);
    }

    #[test]
    fn a_structured_request_starts_on_its_options() {
        let chip = ask_with_options(&["a", "b"]);
        let mut state = AskState::default();
        state.retarget(&chip);
        assert_eq!(state.focus(), AskFocus::Options);
        assert_eq!(state.cursor(), 0);
    }

    #[test]
    fn an_empty_free_text_answer_is_refused() {
        // Delivered into an agent's open picker an empty answer is a bare
        // Enter, which picks whatever the picker had highlighted.
        let chip = ask_with_options(&[]);
        let mut state = AskState::default();
        state.move_cursor(&chip, 0);
        assert_eq!(
            state.answer_text(&chip),
            Err("type an answer first".to_string())
        );
        state.push_char(' ');
        assert!(state.answer_text(&chip).is_err(), "whitespace is not an answer");
        state.push_char('y');
        assert_eq!(state.answer_text(&chip), Ok("y".to_string()));
    }

    #[test]
    fn the_selected_option_label_is_the_answer() {
        let chip = ask_with_options(&["Focused", "Broad"]);
        let mut state = AskState::default();
        state.move_cursor(&chip, 1);
        assert_eq!(state.answer_text(&chip), Ok("Broad".to_string()));
    }

    #[test]
    fn moving_to_a_different_request_clears_the_cursor_and_the_draft() {
        let first = ask_with_options(&["a", "b"]);
        let mut state = AskState::default();
        state.retarget(&first);
        state.move_cursor(&first, 1);
        state.push_char('x');

        let second = SessionAttention::daemon(AttentionKind::Ask, 2_000, "att-2".into())
            .with_options(first.options.clone());
        state.retarget(&second);
        assert_eq!(state.cursor(), 0, "a cursor from the previous question would pre-load a reply");
        assert_eq!(state.free_text(), "");
    }

    #[test]
    fn the_same_request_re_reported_keeps_what_the_operator_typed() {
        // The producers re-report an open row every refresh. Resetting on each
        // one would wipe a half-typed answer several times a minute.
        let chip = ask_with_options(&[]);
        let mut state = AskState::default();
        state.retarget(&chip);
        state.push_char('h');
        state.retarget(&chip);
        assert_eq!(state.free_text(), "h");
    }

    #[test]
    fn a_row_with_no_transport_refuses_with_its_own_reason() {
        let chip = SessionAttention::local(AttentionKind::Ask, 0)
            .unanswerable(Unanswerable::DaemonGone);
        let mut state = AskState::default();
        state.push_char('y');
        state.focus = AskFocus::FreeText;
        let refused = state.send(&chip, "s", "/w").expect_err("must refuse");
        assert!(
            refused.contains("attention/answer"),
            "and name the call that is unavailable: {refused}"
        );
        assert!(!state.in_flight(), "nothing may be latched for a refused send");
    }

    #[test]
    fn a_second_enter_cannot_double_send() {
        // Key-repeat would otherwise deliver the same answer N times into an
        // agent's open picker, and the picker consumes each as a keystroke.
        let mut state = AskState::default();
        state.phase = Some(AnswerPhase::InFlight {
            since: Instant::now(),
            draft: None,
        });
        let chip = ask_with_options(&["Focused"]);
        assert_eq!(
            state.send(&chip, "s", "/w"),
            Err("an answer is already in flight".to_string())
        );
    }

    #[test]
    fn a_failed_send_puts_the_operators_text_back() {
        let mut state = AskState::default();
        state.phase = Some(AnswerPhase::InFlight {
            since: Instant::now(),
            draft: Some("the long answer they typed".to_string()),
        });
        state.inbox.lock().unwrap().push(AnswerPhase::Failed {
            reason: "target_not_running".to_string(),
        });
        assert!(state.tick());
        assert_eq!(
            state.free_text(),
            "the long answer they typed",
            "a failure must not make the operator retype it"
        );
        assert_eq!(state.focus(), AskFocus::FreeText);
        assert!(!state.in_flight(), "and the chip goes back to ASK");
        assert!(matches!(state.phase(), Some(AnswerPhase::Failed { .. })));
    }

    #[test]
    fn a_failed_option_pick_does_not_write_its_label_into_the_composer() {
        // The option is still highlighted where the operator left it. Its label
        // in the composer would move them to a different row carrying an answer
        // they never typed — and a retry would then send that text instead of
        // the option.
        let chip = ask_with_options(&["data/box.db", "api/src/db.sqlite"]);
        let mut state = AskState::default();
        state.retarget(&chip);
        state.move_cursor(&chip, 1);
        state.phase = Some(AnswerPhase::InFlight {
            since: Instant::now(),
            draft: None,
        });
        state.inbox.lock().unwrap().push(AnswerPhase::Failed {
            reason: "no live target".to_string(),
        });
        state.tick();
        assert_eq!(state.free_text(), "");
        assert_eq!(state.focus(), AskFocus::Options);
        assert_eq!(
            state.answer_text(&chip),
            Ok("api/src/db.sqlite".to_string()),
            "a retry must send the option that is still highlighted"
        );
    }

    #[test]
    fn a_delivered_send_leaves_the_draft_alone() {
        let mut state = AskState::default();
        state.phase = Some(AnswerPhase::InFlight {
            since: Instant::now(),
            draft: None,
        });
        state.inbox.lock().unwrap().push(AnswerPhase::Delivered {
            via: "tmux (tmux_proj)".to_string(),
        });
        state.tick();
        assert_eq!(state.free_text(), "", "a delivered answer is not a draft");
        assert!(!state.in_flight());
    }

    #[test]
    fn an_in_flight_send_reports_how_long_it_has_been_going() {
        let mut state = AskState::default();
        assert!(state.elapsed().is_none());
        state.phase = Some(AnswerPhase::InFlight {
            since: Instant::now(),
            draft: None,
        });
        assert!(state.elapsed().is_some(), "a spinner with no elapsed time says nothing");
    }

    #[test]
    fn an_in_flight_answer_marks_only_its_own_chip() {
        // The strip paints every row. Without the identity check an answer in
        // flight on one session would render every other session as SENT.
        let mine = ask_with_options(&["a"]);
        let theirs = SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-other".into());
        let mut state = AskState::default();
        state.retarget(&mine);
        state.phase = Some(AnswerPhase::InFlight {
            since: Instant::now(),
            draft: None,
        });
        assert!(state.phase_for(&mine).is_some());
        assert!(state.phase_for(&theirs).is_none());
    }

    #[test]
    fn a_daemon_row_is_identified_by_its_attention_id() {
        let daemon = SessionAttention::daemon(AttentionKind::Ask, 1, "att-9".into());
        assert_eq!(request_id(&daemon), "att-9");
        // A local row has no id, so its kind and instant have to serve — enough
        // to tell one question from the next.
        let local = SessionAttention::local(AttentionKind::Approve, 42);
        assert_eq!(request_id(&local), "APPROVE:42");
    }
}
