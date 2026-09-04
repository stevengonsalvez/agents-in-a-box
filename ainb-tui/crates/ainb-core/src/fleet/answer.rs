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
    /// What each send did, keyed by the request it was answering.
    ///
    /// Per-REQUEST rather than per-view, because `retarget` runs on every
    /// render of the `ask` tab and a send outlives the operator's cursor.
    /// A single `phase` field got this wrong in both directions: it cleared on
    /// retarget, so answering A and tabbing away dropped the double-send latch
    /// and let the same answer go into the agent's picker twice; and it was
    /// written by whichever outcome landed last, so A's failure painted itself
    /// into B's pane while A, once returned to, showed nothing at all.
    ///
    /// An entry is the latch as well as the view: `InFlight` here IS the
    /// outstanding send, so the two can no longer disagree.
    phases: Vec<(String, AnswerPhase)>,
    /// Shared with every send worker, and deliberately NOT replaced on
    /// retarget: a worker holding an `Arc` to a discarded inbox publishes its
    /// outcome into nothing, so a failed send would report as neither sent nor
    /// failed. Each entry names the request it belongs to, so an outcome
    /// cannot be attributed to whatever question is on screen when it lands.
    inbox: Arc<Mutex<Vec<(String, AnswerPhase)>>>,
}

/// How many requests keep an outcome. Bounds a session that answers questions
/// all day; an in-flight send is never evicted.
const MAX_TRACKED_PHASES: usize = 8;

impl Default for AskState {
    fn default() -> Self {
        Self {
            request: None,
            focus: AskFocus::Options,
            cursor: 0,
            free_text: String::new(),
            phases: Vec::new(),
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
        // would pre-load a reply to a question nobody has read, so the per-view
        // fields reset.
        self.request = Some(id);
        // A request with no structured options has only one place an answer can
        // come from, so start there. Defaulting to the option list leaves the
        // operator on an empty list, typing into a composer that is not focused
        // and shows neither their text nor a caret.
        self.focus = if chip.options.is_empty() {
            AskFocus::FreeText
        } else {
            AskFocus::Options
        };
        self.cursor = 0;
        self.free_text.clear();
        // `phases` is deliberately untouched: it is keyed by request, so the
        // question being shown selects its own outcome. This method runs on
        // every render, and clearing here is what let the operator navigate
        // away from an in-flight answer and send it a second time.
    }

    /// The outcome recorded for `request`, if one is.
    fn phase_of(&self, request: &str) -> Option<&AnswerPhase> {
        self.phases.iter().find(|(id, _)| id == request).map(|(_, phase)| phase)
    }

    /// Record `phase` against `request`, returning what it replaced.
    fn set_phase(&mut self, request: &str, phase: AnswerPhase) -> Option<AnswerPhase> {
        if let Some(slot) = self.phases.iter_mut().find(|(id, _)| id == request) {
            return Some(std::mem::replace(&mut slot.1, phase));
        }
        // Evict settled outcomes only. Dropping an in-flight entry would drop
        // the latch with it and re-open the double-send.
        while self.phases.len() >= MAX_TRACKED_PHASES {
            let Some(oldest) = self
                .phases
                .iter()
                .position(|(_, phase)| !matches!(phase, AnswerPhase::InFlight { .. }))
            else {
                break;
            };
            self.phases.remove(oldest);
        }
        self.phases.push((request.to_string(), phase));
        None
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

    /// What the last send did to the request on screen.
    #[must_use]
    pub fn phase(&self) -> Option<&AnswerPhase> {
        self.phase_of(self.request.as_deref()?)
    }

    /// What the last send did TO THIS CHIP, or `None` when the phase belongs to
    /// a different request.
    ///
    /// The chip strip paints every row; without this identity check an
    /// in-flight answer on one session would render every other session's chip
    /// as `SENT`.
    #[must_use]
    pub fn phase_for(&self, chip: &SessionAttention) -> Option<&AnswerPhase> {
        self.phase_of(&request_id(chip))
    }

    /// Whether THIS chip's answer is still on the wire.
    ///
    /// Answers the question the chip strip actually asks, on every row at once,
    /// so the `SENT` marker stays on the session it belongs to after the
    /// operator has navigated somewhere else.
    #[must_use]
    pub fn is_sending(&self, chip: &SessionAttention) -> bool {
        matches!(self.phase_for(chip), Some(AnswerPhase::InFlight { .. }))
    }

    /// Whether a send is still outstanding for the request now on screen.
    ///
    /// Asked of the LATCH rather than the displayed phase: the phase is reset
    /// every time the pane retargets, and a question the operator navigated
    /// away from and back to still has its answer in flight.
    #[must_use]
    pub fn in_flight(&self) -> bool {
        matches!(self.phase(), Some(AnswerPhase::InFlight { .. }))
    }

    /// Whether ANY send is outstanding, whichever question it belongs to.
    #[must_use]
    pub fn any_in_flight(&self) -> bool {
        self.phases
            .iter()
            .any(|(_, phase)| matches!(phase, AnswerPhase::InFlight { .. }))
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
        let landed: Vec<(String, AnswerPhase)> = self
            .inbox
            .lock()
            .map(|mut inbox| inbox.drain(..).collect())
            .unwrap_or_else(|poisoned| poisoned.into_inner().drain(..).collect());
        let changed = !landed.is_empty();
        for (request, phase) in landed {
            // Recorded against the request it answers, never against whatever
            // the pane is showing. Overwriting the shown phase is how one
            // question's failure came to be painted under another question.
            let failed = matches!(phase, AnswerPhase::Failed { .. });
            let previous = self.set_phase(&request, phase);
            // A failure puts a TYPED answer back, so nobody retypes what the
            // transport lost. A picked option is deliberately not restored: it
            // is still highlighted where the operator left it, and its label in
            // the composer would read as an answer they wrote. Only for the
            // request on screen: text belonging to another question would
            // appear in this one's composer as something the operator typed.
            if !failed || self.request.as_deref() != Some(request.as_str()) {
                continue;
            }
            if let Some(AnswerPhase::InFlight {
                draft: Some(text), ..
            }) = previous
            {
                self.free_text = text;
                self.focus = AskFocus::FreeText;
            }
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
        // The identity of what is being answered, captured before the worker
        // runs so its outcome lands against this question no matter where the
        // operator has navigated by the time it does.
        let request = request_id(chip);
        let reply_to = request.clone();
        let route = chip.answerable.clone();
        let session_id = session_id.to_string();
        let cwd = cwd.to_string();
        let sent = text.clone();
        let spawned = std::thread::Builder::new().name("ainb-ask-send".into()).spawn(move || {
            let outcome = match route {
                Answerable::Daemon { attention_id } => {
                    crate::fleet::control::answer_via_daemon_blocking(attention_id, sent)
                }
                Answerable::Tmux => {
                    crate::fleet::control::answer_via_tmux_blocking(&session_id, &cwd, &sent)
                }
                Answerable::Broker {
                    session_id: waiter, ..
                } => {
                    // The label the operator picked IS the decision. Matched
                    // against the same constants the options were built from,
                    // so the word on screen and the verdict the hook receives
                    // cannot drift; anything else is refused rather than
                    // guessed, because guessing here approves a tool call.
                    match sent.as_str() {
                        crate::fleet::attention::APPROVE_LABEL => {
                            crate::fleet::control::answer_via_broker_blocking(&waiter, true, "")
                        }
                        crate::fleet::attention::DENY_LABEL => {
                            crate::fleet::control::answer_via_broker_blocking(&waiter, false, &sent)
                        }
                        other => Err(format!(
                            "a permission request takes `{}` or `{}`, not `{other}`",
                            crate::fleet::attention::APPROVE_LABEL,
                            crate::fleet::attention::DENY_LABEL,
                        )),
                    }
                }
                // Refused above; unreachable, and a panic here would take the
                // whole TUI down for a state that simply cannot arrive.
                Answerable::No(why) => Err(why.reason().to_string()),
            };
            if let Ok(mut inbox) = inbox.lock() {
                inbox.push((
                    reply_to,
                    match outcome {
                        Ok(via) => AnswerPhase::Delivered { via },
                        Err(reason) => AnswerPhase::Failed { reason },
                    },
                ));
            }
        });
        match spawned {
            Ok(_) => {
                // Filed under the request, so it still refuses a second send
                // after the operator navigates away and back, and so the chip
                // for THIS question keeps reading `SENT` from wherever they go.
                self.set_phase(
                    &request,
                    AnswerPhase::InFlight {
                        since: Instant::now(),
                        draft,
                    },
                );
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
        match self.phase() {
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

    /// Put a send in flight against `chip`, as `send` does on success, without
    /// spawning a worker at a daemon the test does not have.
    fn latch(state: &mut AskState, chip: &SessionAttention, draft: Option<&str>) {
        state.set_phase(
            &request_id(chip),
            AnswerPhase::InFlight {
                since: Instant::now(),
                draft: draft.map(str::to_string),
            },
        );
    }

    /// Publish a worker's outcome for `chip`, as the send thread does.
    fn publish(state: &AskState, chip: &SessionAttention, phase: AnswerPhase) {
        state.inbox.lock().unwrap().push((request_id(chip), phase));
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
        assert!(
            state.answer_text(&chip).is_err(),
            "whitespace is not an answer"
        );
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
        assert_eq!(
            state.cursor(),
            0,
            "a cursor from the previous question would pre-load a reply"
        );
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
        let chip =
            SessionAttention::local(AttentionKind::Ask, 0).unanswerable(Unanswerable::DaemonGone);
        let mut state = AskState::default();
        state.push_char('y');
        state.focus = AskFocus::FreeText;
        let refused = state.send(&chip, "s", "/w").expect_err("must refuse");
        assert!(
            refused.contains("attention/answer"),
            "and name the call that is unavailable: {refused}"
        );
        assert!(
            !state.in_flight(),
            "nothing may be latched for a refused send"
        );
    }

    #[test]
    fn a_second_enter_cannot_double_send() {
        // Key-repeat would otherwise deliver the same answer N times into an
        // agent's open picker, and the picker consumes each as a keystroke.
        let chip = ask_with_options(&["Focused"]);
        let mut state = AskState::default();
        state.retarget(&chip);
        // The in-flight entry IS the latch, and what refuses the next send. It
        // is filed under the question, not under whatever the pane shows.
        latch(&mut state, &chip, None);
        assert_eq!(
            state.send(&chip, "s", "/w"),
            Err("an answer is already in flight".to_string())
        );
    }

    #[test]
    fn a_failed_send_puts_the_operators_text_back() {
        let chip = ask_with_options(&[]);
        let mut state = AskState::default();
        state.retarget(&chip);
        latch(&mut state, &chip, Some("the long answer they typed"));
        publish(
            &state,
            &chip,
            AnswerPhase::Failed {
                reason: "target_not_running".to_string(),
            },
        );
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
        latch(&mut state, &chip, None);
        publish(
            &state,
            &chip,
            AnswerPhase::Failed {
                reason: "no live target".to_string(),
            },
        );
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
        let chip = ask_with_options(&["a"]);
        let mut state = AskState::default();
        state.retarget(&chip);
        latch(&mut state, &chip, None);
        publish(
            &state,
            &chip,
            AnswerPhase::Delivered {
                via: "tmux (tmux_proj)".to_string(),
            },
        );
        state.tick();
        assert_eq!(state.free_text(), "", "a delivered answer is not a draft");
        assert!(!state.in_flight());
    }

    #[test]
    fn an_in_flight_send_reports_how_long_it_has_been_going() {
        let chip = ask_with_options(&["a"]);
        let mut state = AskState::default();
        state.retarget(&chip);
        assert!(state.elapsed().is_none());
        latch(&mut state, &chip, None);
        assert!(
            state.elapsed().is_some(),
            "a spinner with no elapsed time says nothing"
        );
    }

    #[test]
    fn an_in_flight_answer_marks_only_its_own_chip() {
        // The strip paints every row. Without the identity check an answer in
        // flight on one session would render every other session as SENT.
        let mine = ask_with_options(&["a"]);
        let theirs = SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-other".into());
        let mut state = AskState::default();
        state.retarget(&mine);
        latch(&mut state, &mine, None);
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
    /// THE double-send. `retarget` runs on every render of the `ask` tab, so it
    /// used to reset the in-flight latch each time the operator navigated away
    /// from a question and back, and the second Enter typed the same answer
    /// into the agent's picker again. The daemon route survives that on
    /// first-answer-wins; the tmux route has no dedupe at all.
    #[test]
    fn navigating_away_and_back_does_not_release_the_double_send_guard() {
        let a = ask_with_options(&["yes", "no"]);
        let b = SessionAttention::daemon(AttentionKind::Ask, 2_000, "att-2".into());
        let mut state = AskState::default();

        state.retarget(&a);
        // Stand in for a landed send: this is what `send` stamps on success,
        // without spawning a worker at a daemon the test does not have.
        latch(&mut state, &a, None);
        assert!(state.in_flight(), "the answer to A is out");

        // Away to another question, then back. Both are ordinary renders.
        state.retarget(&b);
        assert!(
            !state.in_flight(),
            "B has no answer out, so B may be answered"
        );
        assert!(
            state.any_in_flight(),
            "but A's send is still outstanding somewhere"
        );

        state.retarget(&a);
        assert!(
            state.in_flight(),
            "returning to A must still refuse a second send"
        );
        assert_eq!(
            state.send(&a, "sid", "/work").unwrap_err(),
            "an answer is already in flight"
        );
    }

    /// The worker's outcome must reach the pane even if the operator navigated
    /// away while it was in flight. A retarget that minted a fresh inbox left
    /// the worker publishing into a discarded one, so a failed send reported as
    /// neither sent nor failed.
    #[test]
    fn an_outcome_still_lands_after_the_pane_moved_and_came_back() {
        let a = ask_with_options(&["yes"]);
        let b = SessionAttention::daemon(AttentionKind::Ask, 3_000, "att-3".into());
        let mut state = AskState::default();
        state.retarget(&a);
        latch(&mut state, &a, None);
        // The worker's handle, taken before the operator navigates.
        let worker_inbox = Arc::clone(&state.inbox);

        state.retarget(&b);
        state.retarget(&a);

        worker_inbox.lock().unwrap().push((
            request_id(&a),
            AnswerPhase::Failed {
                reason: "target_not_running".to_string(),
            },
        ));
        assert!(state.tick(), "the outcome must be observed");
        assert!(
            matches!(state.phase_for(&a), Some(AnswerPhase::Failed { reason }) if reason == "target_not_running"),
            "the failure must be shown against the question it belongs to"
        );
        assert!(!state.any_in_flight(), "and the latch is released");
    }

    /// An outcome belongs to the QUESTION it answers, never to whatever the
    /// pane happens to be showing when it lands. A single shared `phase` field
    /// was written by whichever worker finished last, so a failure on A was
    /// painted under B's question, over B's own text, as if B had failed.
    /// A surface that reports the wrong question is worse than one that is
    /// silent about it.
    #[test]
    fn a_failure_on_one_question_is_never_shown_under_another() {
        let a = ask_with_options(&["yes"]);
        let b = SessionAttention::daemon(AttentionKind::Ask, 4_000, "att-4".into());
        let mut state = AskState::default();
        state.retarget(&a);
        latch(&mut state, &a, Some("A's typed answer"));

        // The operator moves to B while A is still on the wire, and starts
        // typing an answer to B.
        state.retarget(&b);
        state.push_char('n');

        publish(
            &state,
            &a,
            AnswerPhase::Failed {
                reason: "target_not_running".to_string(),
            },
        );
        assert!(state.tick(), "the outcome must be observed");

        assert!(
            state.phase_for(&b).is_none(),
            "B never sent anything, so B's pane must report nothing"
        );
        assert_eq!(
            state.free_text(),
            "n",
            "and A's draft must not be restored over what they are typing at B"
        );
        assert!(
            state.phase().is_none(),
            "the pane is on B, and B has no outcome"
        );

        // Back to A: the failure is still there, against the question it
        // belongs to. Clearing on retarget lost it entirely.
        state.retarget(&a);
        assert!(
            matches!(state.phase_for(&a), Some(AnswerPhase::Failed { reason }) if reason == "target_not_running"),
            "A's failure survives the round trip and is shown on A"
        );
    }

    /// The chip strip paints every row, so `SENT` has to follow the question
    /// rather than the cursor: an answer still on the wire for A stayed marked
    /// only while the pane happened to be pointed at A.
    #[test]
    fn a_chip_keeps_reading_sent_after_the_operator_navigates_away() {
        let a = ask_with_options(&["yes"]);
        let b = SessionAttention::daemon(AttentionKind::Ask, 5_000, "att-5".into());
        let mut state = AskState::default();
        state.retarget(&a);
        latch(&mut state, &a, None);

        state.retarget(&b);
        assert!(state.is_sending(&a), "A's answer is still out");
        assert!(!state.is_sending(&b), "B has sent nothing");
    }

    /// The outcome store is bounded, and an in-flight send is never the entry
    /// that gets dropped: evicting one would release the double-send latch.
    #[test]
    fn tracked_outcomes_are_bounded_and_never_evict_a_live_send() {
        let live = ask_with_options(&["yes"]);
        let mut state = AskState::default();
        state.retarget(&live);
        latch(&mut state, &live, None);

        for since in 0..(MAX_TRACKED_PHASES as i64 * 3) {
            let settled =
                SessionAttention::daemon(AttentionKind::Ask, since, format!("settled-{since}"));
            state.retarget(&settled);
            state.set_phase(
                &request_id(&settled),
                AnswerPhase::Delivered { via: "tmux".into() },
            );
        }

        assert!(
            state.phases.len() <= MAX_TRACKED_PHASES,
            "the store is bounded"
        );
        assert!(
            state.is_sending(&live),
            "and the outstanding send is still latched"
        );
    }
}
