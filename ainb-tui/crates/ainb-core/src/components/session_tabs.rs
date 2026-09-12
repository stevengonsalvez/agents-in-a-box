// ABOUTME: The sessions screen's right-pane tab strip — the switchboard that
// turns one preview pane into the whole attention surface.
//
// Six tabs over one rect: `preview` (the tmux mirror that was always there),
// `ask` (answer what is blocking), `err` (what failed, and why), `thread` (this
// session's chat), `pal` (the fleet's own assistant) and `log` (this session's
// notification history).
//
// The strip is the reason `Enter` stops being ambiguous. `Enter` used to mean
// "attach" everywhere, which is the wrong verb on five of these six panes, so
// it becomes scoped to the ACTIVE TAB and each tab declares its own verb here.
// Attach digits are deliberately NOT scoped: `1`-`9` attach from every tab,
// because "jump to that session" is the one action that means the same thing
// wherever the operator is looking.

use ratatui::{
    prelude::*,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::app::AppState;

/// One pane of the right-hand switchboard.
///
/// Declaration order is STRIP order, left to right, and `cycle` walks it, so
/// the rendered strip and the key that moves through it cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SessionTab {
    /// The read-only tmux mirror. Today's default, and still the default.
    #[default]
    Preview,
    /// Answer the selected row's ASK or APPROVE.
    Ask,
    /// What failed on the selected row, and the reason the producer gave.
    ///
    /// Sits beside `ask` rather than at the end of the strip because it answers
    /// the same question — something on this row wants a human — and the chip
    /// that sends an operator looking is one tab away from the pane that
    /// explains it.
    Err,
    /// This session's own chat thread, scope `session:<key>`.
    Thread,
    /// Pal, the fleet's own assistant, plus its channels.
    Pal,
    /// This session's notification history.
    Log,
}

/// Every tab, in strip order.
pub const ALL_TABS: [SessionTab; 6] = [
    SessionTab::Preview,
    SessionTab::Ask,
    SessionTab::Err,
    SessionTab::Thread,
    SessionTab::Pal,
    SessionTab::Log,
];

/// What `Enter` does while the Pal pane is offering to start the daemon.
///
/// One constant, read by the footer, the offer's own key line and the key
/// handler's test, so the three cannot advertise different things.
pub const START_DAEMON_VERB: &str = "start the hangar daemon";

impl SessionTab {
    /// The strip label as it renders RIGHT NOW.
    ///
    /// Only `thread` is dynamic, and only because the checkboxes change what it
    /// is: with rows checked it stops being one session's conversation and
    /// becomes a broadcast to the checked set. The label has to say so, or the
    /// operator sends a private message to four sessions believing it went to
    /// one.
    #[must_use]
    pub fn label_in(self, state: &AppState) -> std::borrow::Cow<'static, str> {
        match self {
            Self::Thread => {
                let targets = state.broadcast_targets().len();
                if targets == 0 {
                    std::borrow::Cow::Borrowed("thread")
                } else {
                    std::borrow::Cow::Owned(format!("broadcast ({targets})"))
                }
            }
            other => std::borrow::Cow::Borrowed(other.label()),
        }
    }

    /// The strip label. Lower case, because these are panes, not commands.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Ask => "ask",
            Self::Err => "err",
            Self::Thread => "thread",
            Self::Pal => "pal",
            Self::Log => "log",
        }
    }

    /// [`SessionTab::enter_verb`], with the broadcast recipient count spelled
    /// out.
    ///
    /// "send message" is a dangerous thing for a footer to say when the message
    /// is going to four sessions at once. The count is the confirmation step:
    /// this pane has no modal, and a number in the footer is cheaper to read
    /// than a dialog is to dismiss.
    #[must_use]
    pub fn enter_verb_in(self, state: &AppState) -> std::borrow::Cow<'static, str> {
        // The daemon offer OWNS Enter while it is ARMED: the composer beneath it
        // has nothing to send to, so this is the only verb the key has. Armed,
        // not merely shown — focus elsewhere, or a start already out, and the
        // key does nothing here, so neither may this say otherwise.
        if self == Self::Pal && state.pal_daemon_cta_armed() {
            return std::borrow::Cow::Borrowed(START_DAEMON_VERB);
        }
        // A pane that cannot act advertises NO verb. The footer is the last
        // thing an operator reads before pressing the key, so a verb over a
        // pane that will decline it is the advertisement that makes the whole
        // surface a lie — the same defect as a green tick over a failed send.
        if self.enter_refusal(state).is_some() {
            return std::borrow::Cow::Borrowed("");
        }
        let targets = state.broadcast_targets().len();
        if self == Self::Thread && targets > 0 {
            return std::borrow::Cow::Owned(format!("broadcast to {targets}"));
        }
        std::borrow::Cow::Borrowed(self.enter_verb())
    }

    /// Why `Enter` on this tab cannot do its ordinary job right now, in the
    /// refusing surface's OWN words, or `None` when it can.
    ///
    /// ONE place, exhaustive over the tabs, rather than a branch per surface
    /// that discovered the problem for itself. `ask` learned it from a native
    /// picker it cannot answer and `thread`/`pal` from a chat host with
    /// nothing to send to, and those are the same rule wearing two faces: a
    /// footer must never advertise a verb the pane will decline. Two adjacent
    /// special cases invite a third, and the third is the one that gets
    /// forgotten.
    ///
    /// Wildcard-free, so a sixth tab has to answer here rather than inherit
    /// whichever arm happens to be last.
    ///
    /// The REASON, not a bool: both sources already have a sentence, and it is
    /// the sentence the pane itself prints — dropping it at this boundary would
    /// leave the footer and the pane deriving the same fact twice.
    #[must_use]
    pub fn enter_refusal(self, state: &AppState) -> Option<String> {
        match self {
            // Attaching asks nothing of the pane, and neither a history nor a
            // post-mortem has a verb to refuse in the first place: `err` shows
            // what already failed, and there is nothing to send back at it.
            Self::Preview | Self::Err | Self::Log => None,
            // The chip's own refusal. A native picker is answered in the
            // agent's terminal, and nothing typed here ever reaches it.
            Self::Ask => selected_blocking(state)
                .and_then(|chip| chip.answerable.refusal())
                .map(ToString::to_string),
            // The LIVE conversation's answer, not an inference from the tab.
            // `send_block` already yields to a broadcast, whose composer is not
            // the chat host's and is never blocked by it.
            Self::Thread | Self::Pal => state.session_tab_send_block(self),
        }
    }

    /// What `Enter` does on this tab. One sentence, shown in the footer, so the
    /// operator never has to guess which verb they are about to fire.
    #[must_use]
    pub const fn enter_verb(self) -> &'static str {
        match self {
            Self::Preview => "attach",
            Self::Ask => "send answer",
            Self::Thread | Self::Pal => "send message",
            // Neither pane takes an answer: one is a history, the other a
            // post-mortem. An advertised verb that did nothing is the surprise
            // the scoping exists to remove.
            Self::Err | Self::Log => "",
        }
    }

    /// Why this tab is unavailable right now, or `None` when it is available.
    ///
    /// A REASON, not a boolean. The strip dims a disabled tab rather than
    /// hiding it (so it never reflows as state changes), and a dimmed label
    /// with no explanation is a control the operator cannot learn to use.
    #[must_use]
    pub fn disabled_reason(self, state: &AppState) -> Option<&'static str> {
        let has_session = state.get_selected_session().is_some();
        match self {
            // Always available: the mirror needs no selection to say there is
            // none, and the assistant is not about any one session.
            Self::Preview | Self::Pal => None,
            Self::Ask => {
                if !has_session {
                    Some("select a session first")
                } else if selected_blocking(state).is_none() {
                    Some("nothing is waiting on an answer here")
                } else {
                    None
                }
            }
            Self::Log => (!has_session).then_some("select a session first"),
            Self::Err => {
                if !has_session {
                    Some("select a session first")
                } else if selected_errors(state).is_empty() {
                    Some("nothing has failed on this session")
                } else {
                    None
                }
            }
            Self::Thread => {
                // Checked rows win over the cursor, the same rule `Enter` and
                // `r` follow on this screen. A broadcast is about the checked
                // set, so it needs no cursor session at all.
                if !state.broadcast_targets().is_empty() {
                    None
                } else if state.selected_sessions.is_empty() && !has_session {
                    Some("select a session first")
                } else if !state.selected_sessions.is_empty() {
                    // Rows ARE checked, but not one of them has a scope.
                    Some("no checked session has fired a hook yet, so none can be reached")
                } else if state.selected_session_chat_key().is_none() {
                    // Opening it anyway would page a scope the daemon has never
                    // heard of and render an empty timeline forever.
                    Some("this session has not fired a hook yet, so its thread has no scope")
                } else {
                    None
                }
            }
        }
    }

    /// Whether this tab can be opened.
    #[must_use]
    pub fn enabled(self, state: &AppState) -> bool {
        self.disabled_reason(state).is_none()
    }
}

/// The selected session's first BLOCKING chip — what the `ask` tab answers.
///
/// First, not "the one that matches a cursor": chips are already in precedence
/// order, so the first blocking one is the tightest thing waiting on a human.
#[must_use]
pub fn selected_blocking(state: &AppState) -> Option<&crate::fleet::attention::SessionAttention> {
    state
        .get_selected_session()?
        .live_attention
        .iter()
        .find(|chip| chip.kind.blocks())
}

/// Every error the selected session has, newest first — INCLUDING the ones too
/// old to still light a chip on the row.
///
/// Reads `errors`, not `live_attention`. The row's chip list is windowed
/// (`[ui] attention_err_window_hours`) because "something needs me now" expires;
/// "what went wrong" does not, and a pane that expired with the chip would put
/// the operator back where they started — an ERR they can see and cannot read.
#[must_use]
pub fn selected_errors(state: &AppState) -> &[crate::fleet::attention::SessionAttention] {
    state.get_selected_session().map_or(&[], |session| session.errors.as_slice())
}

/// Whether the selected row is still LIGHTING an ERR chip.
///
/// Asked of the row itself rather than by re-deriving the window here: the two
/// would then be two separate pieces of arithmetic that can disagree, and the
/// pane would tell an operator a chip is showing when it is not. The pane says
/// WHY a failure it lists is no longer on the row, which is what stops retiring
/// the chip from trading one silent surface for another.
#[must_use]
pub fn selected_err_is_on_the_row(state: &AppState) -> bool {
    state.get_selected_session().is_some_and(|session| {
        session
            .live_attention
            .iter()
            .any(|chip| chip.kind == crate::fleet::attention::AttentionKind::Err)
    })
}

/// Move `from` to the next available tab, forward or backward, skipping the
/// disabled ones.
///
/// Skipping rather than stopping on a dimmed tab: `Tab` is a navigation key and
/// a navigation key that lands somewhere it cannot act reads as broken. The
/// dimmed tab stays VISIBLE in the strip regardless, which is what keeps the
/// strip from reflowing every time a session answers a question.
///
/// Returns `from` unchanged when nothing else is available, so the key is a
/// no-op rather than a panic on a screen with one live tab.
#[must_use]
pub fn cycle(state: &AppState, from: SessionTab, forward: bool) -> SessionTab {
    let len = ALL_TABS.len();
    let start = ALL_TABS.iter().position(|tab| *tab == from).unwrap_or(0);
    for step in 1..len {
        let index = if forward {
            (start + step) % len
        } else {
            (start + len - step) % len
        };
        let candidate = ALL_TABS[index];
        if candidate.enabled(state) {
            return candidate;
        }
    }
    from
}

/// The tab that should be active given the current selection.
///
/// Called every frame. A tab can go disabled under the operator — answering the
/// ASK retires it, moving the cursor to a workspace header retires `thread` and
/// `log` — and leaving them on a dead pane would show a stale question they can
/// no longer act on. Falls back to `preview`, which is never disabled.
#[must_use]
pub fn resolve(state: &AppState, active: SessionTab) -> SessionTab {
    if active.enabled(state) {
        active
    } else {
        SessionTab::Preview
    }
}

// Palette shared with the rest of the sessions screen.
const GOLD: Color = Color::Rgb(255, 215, 0);
const SELECTION_GREEN: Color = Color::Rgb(100, 200, 100);
const ALERT_AMBER: Color = Color::Rgb(255, 190, 70);
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const ALERT_RED: Color = Color::Rgb(220, 90, 90);

/// The colour a chip and its age share, mirrored from the session list so the
/// row and the pane never disagree about what an attention state looks like.
const fn chip_color(kind: crate::fleet::attention::AttentionKind) -> Color {
    use crate::fleet::attention::{AttentionTone, tone};
    match tone(kind) {
        AttentionTone::Blocking => ALERT_RED,
        AttentionTone::Error => Color::Rgb(230, 100, 100),
        AttentionTone::Complete => SELECTION_GREEN,
    }
}

/// The tab strip, as the right pane's title line.
///
/// Rendered into the pane's own border title rather than as a row of its own:
/// the right pane is where the content lives, and spending a full row on five
/// words costs the preview a line on every screen.
#[must_use]
pub fn strip(state: &AppState, active: SessionTab) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (index, tab) in ALL_TABS.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(" │ ", Style::default().fg(SUBDUED_BORDER)));
        }
        let style = if *tab == active {
            Style::default()
                .fg(SELECTION_GREEN)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else if tab.enabled(state) {
            Style::default().fg(GOLD)
        } else {
            // Dimmed, never hidden: hiding it reflows the strip every time a
            // session answers a question, and a strip that moves under the
            // cursor is a strip nobody learns.
            Style::default().fg(MUTED_GRAY)
        };
        spans.push(Span::styled(tab.label_in(state).into_owned(), style));
    }
    spans.push(Span::raw(" "));
    Line::from(spans)
}

/// The footer hint for the active tab: what `Enter` does here, and what the
/// other always-live keys do.
///
/// `capturing` is whether a composer on this tab currently owns printable keys.
/// It changes what the footer may honestly promise: the attach digits work on
/// every tab EXCEPT inside a live composer, where a `3` has to be a `3`. A
/// footer that advertised them there would be advertising a key that types a
/// character instead.
#[must_use]
pub fn footer(state: &AppState, active: SessionTab, capturing: bool) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            " \u{21e5}",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" tab ", Style::default().fg(MUTED_GRAY)),
    ];
    let verb = active.enter_verb_in(state);
    if !verb.is_empty() {
        spans.push(Span::styled("│", Style::default().fg(SUBDUED_BORDER)));
        spans.push(Span::styled(
            " Enter",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {verb} "),
            Style::default().fg(MUTED_GRAY),
        ));
    }
    spans.push(Span::styled("│", Style::default().fg(SUBDUED_BORDER)));
    if capturing {
        spans.push(Span::styled(
            " \u{21e7}\u{21e5}",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" focus ", Style::default().fg(MUTED_GRAY)));
        spans.push(Span::styled("│", Style::default().fg(SUBDUED_BORDER)));
        spans.push(Span::styled(
            " Esc",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            " leave (digits type) ",
            Style::default().fg(MUTED_GRAY),
        ));
    } else {
        spans.push(Span::styled(
            " 1-9",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ));
        // Spelled out on every tab because it is the one binding that is NOT
        // scoped: an operator who has learned that Enter changes meaning has
        // every reason to assume the digits do too.
        spans.push(Span::styled(
            " attach (any tab) ",
            Style::default().fg(MUTED_GRAY),
        ));
    }
    if let Some(metadata) = state.selected_fleet_metadata() {
        spans.push(Span::styled("│", Style::default().fg(SUBDUED_BORDER)));
        if let Some(model) = metadata.model.as_deref() {
            spans.push(Span::styled(" model ", Style::default().fg(MUTED_GRAY)));
            spans.push(Span::styled(
                model.to_string(),
                Style::default().fg(SOFT_WHITE),
            ));
        }
        if let Some(effort) = metadata.reasoning_effort.as_deref() {
            spans.push(Span::styled(" effort ", Style::default().fg(MUTED_GRAY)));
            spans.push(Span::styled(
                effort.to_string(),
                Style::default().fg(SOFT_WHITE),
            ));
        }
        if metadata.direct_child_count > 0 {
            let label = if metadata.direct_child_count == 1 {
                " 1 direct child "
            } else {
                " direct children "
            };
            if metadata.direct_child_count == 1 {
                spans.push(Span::styled(label, Style::default().fg(MUTED_GRAY)));
            } else {
                spans.push(Span::styled(
                    format!(" {}{label}", metadata.direct_child_count),
                    Style::default().fg(MUTED_GRAY),
                ));
            }
        }
    }
    Line::from(spans)
}

/// Render the `ask` pane: what is waiting, how it would be answered, and what
/// the operator can do about it.
///
/// Read-only in this phase — the composer and the send land with the answering
/// path. What it must already do is never render a blank box: every state here
/// says what it is, including the ones that cannot take an answer.
pub fn render_ask(frame: &mut Frame, area: Rect, state: &AppState) {
    use crate::fleet::answer::{AnswerPhase, AskFocus};
    use ratatui::widgets::{Paragraph, Wrap};

    let Some(chip) = selected_blocking(state) else {
        // Unreachable through the strip (the tab is dimmed), reachable through
        // a race: the ASK is answered between the frame that enabled the tab
        // and this one.
        frame.render_widget(
            Paragraph::new("nothing is waiting on an answer here")
                .style(Style::default().fg(MUTED_GRAY)),
            area,
        );
        return;
    };
    let ask = &state.ask_state;

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            chip.kind.label(),
            Style::default().fg(chip_color(chip.kind)).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            crate::fleet::attention::format_age(
                chrono::Utc::now().timestamp_millis(),
                chip.since_ms,
            ),
            Style::default().fg(MUTED_GRAY),
        ),
    ]));
    lines.push(Line::raw(""));

    // The question, or an honest statement that the producer did not send one.
    // Never a manufactured "waiting for input": that reads as something the
    // agent said.
    match chip.detail.as_deref() {
        Some(question) => lines.push(Line::styled(
            question.to_string(),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        )),
        None => lines.push(Line::styled(
            "the request carried no question text",
            Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
        )),
    }
    lines.push(Line::raw(""));

    // A refused chip renders as a READING surface, not an answering one.
    //
    // The options and the question are still worth showing — a native picker's
    // choices tell the operator what is being asked even when the answer is
    // typed elsewhere. What must go is every affordance that promises a send:
    // the selection caret, the free-text row, the composer and its caret. A
    // pane that paints a cursor under "nothing to send to" is the same lying
    // surface as a green tick over a failed send.
    let refusal = chip.answerable.refusal();
    let can_answer = refusal.is_none();
    let on_free_text = can_answer && ask.focus() == AskFocus::FreeText;
    for (index, option) in chip.options.iter().enumerate() {
        let selected = can_answer && !on_free_text && ask.cursor() == index;
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "\u{25b8}" } else { " " },
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", circled(index)),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                option.label.clone(),
                Style::default().fg(if selected {
                    SELECTION_GREEN
                } else {
                    SOFT_WHITE
                }),
            ),
        ]));
        if !option.description.is_empty() {
            lines.push(Line::styled(
                format!("    {}", option.description),
                Style::default().fg(MUTED_GRAY),
            ));
        }
    }

    // back to the pane. On a REFUSED chip it is omitted entirely: a composer
    // row on a row that cannot send is an affordance that does nothing.
    if can_answer {
        // The free-text row is present on every ANSWERABLE request, even a
        // structured one: an agent's question is not always answerable with one of
        // its own options, and a surface that only offers them forces the operator
        lines.push(Line::from(vec![
            Span::styled(
                if on_free_text { "\u{25b8}" } else { " " },
                Style::default().fg(SELECTION_GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} ", circled(chip.options.len())),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                if chip.options.is_empty() {
                    "answer".to_string()
                } else {
                    "other (type it)".to_string()
                },
                Style::default().fg(if on_free_text {
                    SELECTION_GREEN
                } else {
                    MUTED_GRAY
                }),
            ),
        ]));
        if on_free_text {
            lines.push(Line::from(vec![
                Span::raw("    "),
                Span::styled(ask.free_text().to_string(), Style::default().fg(SOFT_WHITE)),
                // A visible caret, so an empty composer reads as "type here" rather
                // than as a pane that is doing nothing.
                Span::styled("\u{2588}", Style::default().fg(SELECTION_GREEN)),
            ]));
        }
    }

    // What the last send did. Every one of the three states is VISIBLE: an
    // answer that vanished into a worker with no feedback is the failure this
    // pane exists to remove.
    match ask.phase_for(chip) {
        Some(AnswerPhase::InFlight { since, .. }) => {
            // From the matched phase, not from the pane: the two are the same
            // request here, and reading the pane's clock for a chip's spinner
            // is a disagreement waiting for a caller that renders both.
            let secs = since.elapsed().as_secs();
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("\u{283b} sending\u{2026} {secs}s"),
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ));
        }
        Some(AnswerPhase::Delivered { via }) => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("\u{2713} {via}"),
                Style::default().fg(SELECTION_GREEN),
            ));
        }
        Some(AnswerPhase::Failed { reason, .. }) => {
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                format!("\u{2717} not answered: {reason}"),
                Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::styled(
                "the chip is back to ASK; Enter retries",
                Style::default().fg(MUTED_GRAY),
            ));
        }
        None => {}
    }

    // The refusal, when there is one. This is the line that stops a greyed chip
    // being a silent no-op: it names the transport that is missing.
    if let Some(refusal) = chip.answerable.refusal() {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!("\u{26a0} {refusal}"),
            Style::default().fg(ALERT_RED),
        ));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// Render the `err` pane: what failed on this session, and the reason whoever
/// raised it gave.
///
/// The reason is the whole point. `SessionStatus::Error` has always carried a
/// sentence and the chip has always dropped it, so the only place an operator
/// could read WHY a row said ERR was the bottom logs strip — which scrolls, and
/// is not per-session. Both producers land here: a local failure's status text
/// and a daemon `error`/`escalation` row's payload.
pub fn render_err(frame: &mut Frame, area: Rect, state: &AppState) {
    use crate::fleet::attention::AttentionSource;
    use ratatui::widgets::{Paragraph, Wrap};

    let errors = selected_errors(state);
    if errors.is_empty() {
        // Unreachable through the strip (the tab is dimmed), reachable through
        // a race: the session recovers between the frame that enabled the tab
        // and this one.
        frame.render_widget(
            Paragraph::new("nothing has failed on this session")
                .style(Style::default().fg(MUTED_GRAY)),
            area,
        );
        return;
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let on_the_row = selected_err_is_on_the_row(state);
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (index, chip) in errors.iter().enumerate() {
        if index > 0 {
            lines.push(Line::raw(""));
        }
        lines.push(Line::from(vec![
            Span::styled(
                chip.kind.label(),
                Style::default().fg(chip_color(chip.kind)).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                crate::fleet::attention::format_age(now_ms, chip.since_ms),
                Style::default().fg(MUTED_GRAY),
            ),
            Span::raw("  "),
            // Which producer said so. A local failure is ainb's own view of the
            // process; a daemon row is something the agent itself raised, and
            // an operator chasing a failure needs to know which of the two they
            // are reading.
            Span::styled(
                match chip.source {
                    AttentionSource::Local => "local",
                    AttentionSource::Daemon => "daemon",
                },
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            ),
        ]));
        lines.push(Line::raw(""));
        match chip.detail.as_deref() {
            Some(reason) => lines.push(Line::styled(
                reason.to_string(),
                Style::default().fg(SOFT_WHITE),
            )),
            // Honest, not manufactured. A producer that raised an error with no
            // text said nothing, and inventing "the session failed" here would
            // read as something ainb had actually observed.
            None => lines.push(Line::styled(
                "the failure carried no reason text",
                Style::default().fg(MUTED_GRAY).add_modifier(Modifier::ITALIC),
            )),
        }
    }

    // Why the row is quiet about a failure this pane is still showing. Without
    // it, retiring the chip just moves the mystery: the operator now has an
    // error on screen and no idea why nothing is flagged.
    if !on_the_row {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "older than the ERR window, so it no longer lights the row \u{b7} \
             change it at [ui] attention_err_window_hours",
            Style::default().fg(MUTED_GRAY),
        ));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// `①`-style option markers, falling back to a plain number past nine.
fn circled(index: usize) -> String {
    const CIRCLED: [&str; 9] = [
        "\u{2460}", "\u{2461}", "\u{2462}", "\u{2463}", "\u{2464}", "\u{2465}", "\u{2466}",
        "\u{2467}", "\u{2468}",
    ];
    CIRCLED
        .get(index)
        .map_or_else(|| format!("{}.", index + 1), |glyph| (*glyph).to_string())
}

/// Render the `log` pane: this session's own notification history.
///
/// Per-session, not fleet-wide. The cross-session view the host Inbox used to
/// provide lives on in the hangar plugin's `I` tab; recreating it here would
/// rebuild the duplication this screen exists to delete.
///
/// The read happens on [`crate::fleet::session_log`]'s worker, so this takes
/// what the worker last published — including the two states that are NOT
/// "there is no history": the first frame after the cursor moved, and a store
/// that could not be read at all.
pub fn render_log(frame: &mut Frame, area: Rect, log: &crate::fleet::session_log::Log) {
    use crate::fleet::session_log::Log;
    use ratatui::widgets::{List, ListItem, Paragraph};

    let rows = match log {
        Log::Rows(rows) => rows.as_slice(),
        Log::Reading => {
            frame.render_widget(
                Paragraph::new("reading this session's history\u{2026}")
                    .style(Style::default().fg(MUTED_GRAY)),
                area,
            );
            return;
        }
        // Named, not swallowed. A store that cannot be opened rendered as the
        // empty state is how an operator concludes their notifications are
        // gone rather than that a path is wrong.
        Log::Failed(reason) => {
            frame.render_widget(
                Paragraph::new(format!("\u{26a0} {reason}")).style(Style::default().fg(ALERT_RED)),
                area,
            );
            return;
        }
    };

    if rows.is_empty() {
        frame.render_widget(
            ratatui::widgets::Paragraph::new("no notifications recorded for this session yet")
                .style(Style::default().fg(MUTED_GRAY)),
            area,
        );
        return;
    }
    let now_ms = chrono::Utc::now().timestamp_millis();
    let items: Vec<ListItem<'static>> = rows
        .iter()
        .map(|row| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(
                        "{:>5} ",
                        crate::fleet::attention::format_age(now_ms, row.ts)
                    ),
                    Style::default().fg(MUTED_GRAY),
                ),
                Span::styled(
                    row.event.clone(),
                    Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    row.detail.clone(),
                    Style::default().fg(Color::Rgb(220, 220, 230)),
                ),
            ]))
        })
        .collect();
    frame.render_widget(List::new(items), area);
}

/// One row of the `log` tab: a notification this session produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    /// Epoch-ms the hook fired.
    pub ts: i64,
    /// The raw hook event name, as the agent named it.
    pub event: String,
    /// A one-line summary, or empty.
    pub detail: String,
}

/// Keep one session's rows out of a batch the store already returned.
///
/// PURE, and deliberately not a store read: the read runs on
/// [`crate::fleet::session_log`]'s worker thread, because doing it here — which
/// is inside `terminal.draw` — is what made the `log` tab cost a second a
/// frame. This is the half that has to happen for whatever the worker fetched.
#[must_use]
pub fn log_rows(
    records: &[ainb_plugin_notifyd::NotificationRecord],
    cwd: &str,
    agent: Option<&str>,
    limit: usize,
) -> Vec<LogRow> {
    let cwd = cwd.trim_end_matches('/');
    records
        .iter()
        .filter(|row| {
            row.cwd.trim_end_matches('/') == cwd && agent.is_none_or(|agent| row.agent == agent)
        })
        .take(limit)
        .map(|row| LogRow {
            ts: row.ts,
            event: row.raw_event.clone(),
            detail: log_detail(row),
        })
        .collect()
}

/// The one-line summary for a log row: the hook's own message when it sent one,
/// else the project it fired in.
fn log_detail(row: &ainb_plugin_notifyd::NotificationRecord) -> String {
    serde_json::from_str::<serde_json::Value>(&row.payload_json)
        .ok()
        .and_then(|payload| {
            payload
                .get("message")
                .and_then(serde_json::Value::as_str)
                .map(|message| message.trim().to_string())
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| row.project.clone())
}

/// Paint a plugin `WireBuffer` into a ratatui frame at `area`.
///
/// `fleet_chat` renders into the plugin SDK's buffer, not a ratatui one, and
/// the sessions screen has to show it. Lifted out of the Fleet panel, which is
/// about to be deleted, so the two surfaces never render the same conversation
/// through two different blits.
pub fn blit_wire(
    frame: &mut Frame,
    area: Rect,
    wire: ainb_plugin_protocol::wire_buffer::WireBuffer,
) {
    let buffer = frame.buffer_mut();
    for (coord, cell) in wire.cells {
        if coord.x >= area.width || coord.y >= area.height {
            continue;
        }
        let Some(target) = buffer.cell_mut((area.x + coord.x, area.y + coord.y)) else {
            continue;
        };
        target.set_symbol(&cell.symbol);
        let mut style = Style::default().fg(wire_color(cell.fg)).bg(wire_color(cell.bg));
        if cell.modifier & 1 != 0 {
            style = style.add_modifier(Modifier::BOLD);
        }
        target.set_style(style);
    }
}

fn wire_color(color: Option<ainb_plugin_protocol::wire_buffer::Color>) -> Color {
    color.map_or(Color::Reset, |color| Color::Rgb(color.r, color.g, color.b))
}

/// Render the Pal pane: the engine / model / mode header, then the
/// conversation under it.
///
/// The header is drawn even when the conversation cannot be, and a Pal with no
/// live session still has an engine to pick and a registry to read, and the
/// engine picker is how an operator RECOVERS from an adapter that will not
/// spawn. Hiding it behind a working chat would put the fix behind the failure.
pub fn render_pal(
    frame: &mut Frame,
    area: Rect,
    header: Vec<Line<'static>>,
    offer: Option<&crate::fleet::daemon_cta::DaemonStartCta>,
    host: Option<&crate::fleet::chat_host::ChatHost>,
) {
    let height = u16::try_from(header.len()).unwrap_or(u16::MAX).min(area.height);
    let [head, rest] = Layout::vertical([
        ratatui::layout::Constraint::Length(height),
        ratatui::layout::Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(ratatui::widgets::Paragraph::new(header), head);
    if rest.height == 0 {
        return;
    }
    // The offer is INSERTED, never a replacement. The dials above it are how an
    // operator recovers from an adapter that will not spawn, and the
    // conversation below still has its own failure to report — a pane that
    // swapped both for one sentence would take away two working surfaces to
    // add one.
    let rest = match offer {
        Some(offer) => {
            // Yield to the conversation where there is room, because below its
            // floor the chat renderer draws nothing at all and an offer that
            // blanked it would hide the daemon's own words. But never to
            // NOTHING: one row is always taken while any row exists, and the
            // clamp below guarantees that row is the key. A pane too short even
            // for that already says "widen the pane" for the chat.
            let budget = rest.height.saturating_sub(MIN_CHAT_ROWS).max(1).min(rest.height);
            // Clamped by what is PASSED, not only by the rect. A `Paragraph`
            // handed more rows than it has drops the ones at the BOTTOM in
            // silence, and the bottom of this block is the action line — so the
            // pane would keep the diagnosis and lose the remedy while the
            // footer went on advertising it.
            let lines = daemon_offer_lines(offer, rest.width, budget);
            let height = offer_block_height(&lines, rest.width).min(budget);
            if height == 0 {
                rest
            } else {
                let [block, below] = Layout::vertical([
                    ratatui::layout::Constraint::Length(height),
                    ratatui::layout::Constraint::Min(0),
                ])
                .areas(rest);
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(lines)
                        .wrap(ratatui::widgets::Wrap { trim: false }),
                    block,
                );
                below
            }
        }
        None => rest,
    };
    if rest.height == 0 {
        return;
    }
    match host {
        Some(host) => render_chat(frame, rest, host),
        None => frame.render_widget(
            ratatui::widgets::Paragraph::new("opening the Pal channel\u{2026}")
                .style(Style::default().fg(MUTED_GRAY)),
            rest,
        ),
    }
}

/// How many ROWS `lines` occupy at `width` once wrapped.
///
/// The block is sized by what will be painted, not by how many `Line`s were
/// built: a headline that wraps to two rows pushes the key off a block measured
/// in lines, which is the silent cut this exists to stop.
fn offer_block_height(lines: &[Line<'static>], width: u16) -> u16 {
    lines.iter().map(|line| wrapped_rows(line, width)).sum::<u16>()
}

/// The rows one line occupies at `width`, greedily word-wrapped like the
/// `Paragraph` that paints it. Always at least one, so a blank still costs its
/// row.
fn wrapped_rows(line: &Line<'static>, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
    let mut rows: u16 = 1;
    let mut used = 0usize;
    for word in text.split_inclusive(' ') {
        let len = word.chars().count();
        if used > 0 && used + len > width {
            rows = rows.saturating_add(1);
            used = len;
        } else {
            used += len;
        }
        // A single word longer than the pane wraps on its own.
        while used > width {
            rows = rows.saturating_add(1);
            used -= width;
        }
    }
    rows
}

/// The Pal pane's offer to start the hangar daemon, as the lines that will
/// FIT in `max_rows` at `width`.
///
/// Built rather than painted so the caller can size the block against what is
/// left of the pane, and CLAMPED here rather than left to the paragraph: a
/// paragraph given more rows than it has drops the ones at the bottom without
/// saying so, and the bottom of this block is the key.
///
/// Dropped in priority order, from the least load-bearing end. The action line
/// is priority zero and is never a casualty — a pane that kept the diagnosis
/// and lost the remedy is the dead end this whole surface removes. Next most
/// load-bearing is the last start's own words, then the headline, then the
/// padding.
///
/// Every state says what it is. A start that failed keeps the key, because the
/// remedy for a port that was busy is to try again; a start that is out shows
/// no key at all, so the offer cannot be fired twice into one home.
fn daemon_offer_lines(
    cta: &crate::fleet::daemon_cta::DaemonStartCta,
    width: u16,
    max_rows: u16,
) -> Vec<Line<'static>> {
    use crate::fleet::daemon_cta::CtaStatus;

    // (priority, line) in SCREEN order. 0 is never dropped.
    let mut rows: Vec<(u8, Line<'static>)> = vec![
        (3, Line::raw("")),
        (
            2,
            Line::styled(
                " Pal needs the hangar daemon, which is not running.",
                Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
            ),
        ),
    ];
    match cta.status() {
        CtaStatus::Offered => rows.push((0, offer_line())),
        // The spinner IS the action line's stand-in while a start is out: it is
        // what tells the operator their key press went somewhere.
        CtaStatus::Starting => rows.push((
            0,
            Line::styled(
                "   \u{25cf} starting the hangar daemon\u{2026}",
                Style::default().fg(ALERT_AMBER),
            ),
        )),
        // The command's own closing line, never a paraphrase: `start` reports
        // "already running" as a SUCCESS, and an operator still staring at this
        // offer afterwards needs to read that rather than a tick.
        CtaStatus::Reported { ok, detail } => {
            rows.push((
                1,
                Line::styled(
                    format!("   {} {detail}", if *ok { "\u{2713}" } else { "\u{2717}" }),
                    Style::default().fg(if *ok { SELECTION_GREEN } else { ALERT_RED }),
                ),
            ));
            // The offer stands on BOTH outcomes. A start that exited zero and
            // left this pane still asking for a daemon has not produced one,
            // and withdrawing the key there would leave the operator with a
            // green tick and nothing to press.
            rows.push((0, offer_line()));
        }
    }
    rows.push((3, Line::raw("")));

    // Drop the lowest priority first, and the LAST of that priority, so the
    // padding above the headline outlives the padding below the key only if it
    // has to.
    while offer_block_height(
        &rows.iter().map(|(_, line)| line.clone()).collect::<Vec<_>>(),
        width,
    ) > max_rows
    {
        let Some(index) = rows
            .iter()
            .enumerate()
            .filter(|(_, (priority, _))| *priority > 0)
            .max_by_key(|(index, (priority, _))| (*priority, *index))
            .map(|(index, _)| index)
        else {
            break;
        };
        rows.remove(index);
    }
    rows.into_iter().map(|(_, line)| line).collect()
}

/// The key line, worded once so the two states that offer it cannot differ.
fn offer_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "   \u{23ce}  ",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{START_DAEMON_VERB} now"),
            Style::default().fg(SOFT_WHITE),
        ),
    ])
}

/// One setting row: label, value, and the key that cycles it.
fn dial_row(label: &str, value: String, key: char, dim: bool) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {label:<7}"), Style::default().fg(MUTED_GRAY)),
        Span::styled(
            value,
            Style::default().fg(if dim { MUTED_GRAY } else { SOFT_WHITE }),
        ),
        Span::styled("  \u{25c0} ", Style::default().fg(SUBDUED_BORDER)),
        // The key sits NEXT TO the control it turns, not in a footer legend: a
        // three-dial header with its bindings elsewhere is three things to
        // remember instead of three things to read.
        //
        // ALT-modified, and shown that way. A bare letter is a letter to the
        // composer below, which holds focus as soon as the conversation opens,
        // so a bare binding would be advertised here and do nothing in the
        // state an operator is usually in.
        Span::styled(
            format!("\u{2325}{key}"),
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
    ])
}

#[must_use]
pub fn pal_header(dial: &crate::fleet::pal_dial::PalDial) -> Vec<Line<'static>> {
    use crate::fleet::pal_dial::DialStatus;

    let mut lines = vec![
        dial_row(
            "engine",
            dial.engine().unwrap_or("\u{2026}").to_string(),
            'e',
            dial.engine().is_none(),
        ),
        dial_row(
            "model",
            // An adapter with no declared models runs its own default, and
            // saying so beats an empty value that reads as a failed read.
            dial.model().map_or_else(|| "adapter default".to_string(), ToString::to_string),
            'o',
            dial.model().is_none(),
        ),
        dial_row("mode", dial.mode().as_str().to_string(), 'g', false),
    ];
    // The named broadcast channels: durable conversations an operator can come
    // back to, which is exactly what the checkbox broadcast is not. Listed
    // rather than counted, because the name IS the way back to one.
    let (channels, dim) = if !dial.channels().is_empty() {
        (dial.channels().join(" \u{b7} "), false)
    } else if dial.channels_listed() {
        ("none yet".to_string(), true)
    } else {
        // Not "none": the read has not come back, and rendering an empty list
        // as a fact is how an operator concludes their channels are gone.
        ("\u{2026}".to_string(), true)
    };
    lines.push(Line::from(vec![
        Span::styled(" channels", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            format!("  {channels}"),
            Style::default().fg(if dim { MUTED_GRAY } else { SOFT_WHITE }),
        ),
    ]));
    // `yolo` fires destructive fleet tools with no card. It gets a banner
    // because the whole point of the mode is that nothing else will stop and
    // ask, so the pane itself has to be the reminder.
    if dial.mode() == ainb_hangar_proto::fleet::FleetPalMode::Yolo {
        lines.push(Line::from(Span::styled(
            " yolo: writes fire with no confirm card (kill still asks)",
            Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
        )));
    }
    if dial.session_replaced() {
        lines.push(Line::from(Span::styled(
            " engine swapped; this channel is on a new session",
            Style::default().fg(SELECTION_GREEN),
        )));
    }
    match dial.status() {
        DialStatus::Idle => {}
        DialStatus::Working(verb) => lines.push(Line::from(Span::styled(
            format!(" \u{25cf} {verb}\u{2026}"),
            Style::default().fg(ALERT_AMBER),
        ))),
        // The METHOD, then the detail, then the retry key: which call failed is
        // the actionable half, and a failure with no way forward is the dead
        // end this pane replaces.
        DialStatus::Failed { call, detail } => lines.push(Line::from(vec![
            Span::styled(
                format!(" {call} failed: "),
                Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
            ),
            Span::styled(detail.clone(), Style::default().fg(SOFT_WHITE)),
            Span::styled("  \u{25c0} ", Style::default().fg(SUBDUED_BORDER)),
            Span::styled(
                "\u{2325}r",
                Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" retry", Style::default().fg(MUTED_GRAY)),
        ])),
    }
    lines.push(Line::from(Span::styled(
        "\u{2500}".repeat(4),
        Style::default().fg(SUBDUED_BORDER),
    )));
    lines
}

/// Render the broadcast pane: who it goes to, what has been typed, and what
/// each recipient's leg did.
///
/// The recipient list is spelled out rather than counted. A count is enough for
/// the tab label, where the operator is choosing a pane; it is not enough at
/// the moment of sending, because the checkboxes are on the OTHER pane and may
/// be scrolled off screen.
pub fn render_broadcast(
    frame: &mut Frame,
    area: Rect,
    broadcast: &crate::fleet::broadcast::Broadcast,
    targets: &[String],
    unreachable: usize,
) {
    use crate::fleet::broadcast::BroadcastPhase;
    use ratatui::widgets::{Paragraph, Wrap};

    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![
        Span::styled(" to  ", Style::default().fg(MUTED_GRAY)),
        Span::styled(
            targets.join(", "),
            Style::default().fg(SOFT_WHITE).add_modifier(Modifier::BOLD),
        ),
    ])];
    if unreachable > 0 {
        // Named, not silently dropped: the operator ticked these rows, and a
        // send that quietly reached fewer sessions than were checked is the
        // failure this line exists to prevent.
        lines.push(Line::from(Span::styled(
            format!(
                " {unreachable} checked session{} cannot be reached yet (no hook fired)",
                if unreachable == 1 { "" } else { "s" }
            ),
            Style::default().fg(ALERT_AMBER),
        )));
    }
    lines.push(Line::from(Span::styled(
        "\u{2500}".repeat(4),
        Style::default().fg(SUBDUED_BORDER),
    )));

    match broadcast.phase() {
        BroadcastPhase::Composing => lines.push(Line::from(vec![
            Span::styled(" > ", Style::default().fg(GOLD)),
            Span::styled(
                broadcast.text().to_string(),
                Style::default().fg(SOFT_WHITE),
            ),
            Span::styled("\u{2588}", Style::default().fg(SELECTION_GREEN)),
        ])),
        BroadcastPhase::Sending => lines.push(Line::from(Span::styled(
            format!(" \u{25cf} sending to {}\u{2026}", targets.len()),
            Style::default().fg(ALERT_AMBER),
        ))),
        BroadcastPhase::Failed(detail) => lines.push(Line::from(vec![
            Span::styled(
                " fleet/broadcast failed: ",
                Style::default().fg(ALERT_RED).add_modifier(Modifier::BOLD),
            ),
            Span::styled(detail.clone(), Style::default().fg(SOFT_WHITE)),
            // The text is still in the composer, and saying so is what stops
            // an operator retyping a message the fleet never saw.
            Span::styled(
                " (your message is still here)",
                Style::default().fg(MUTED_GRAY),
            ),
        ])),
        BroadcastPhase::Sent(receipts) => {
            let (delivered, total) = crate::fleet::broadcast::tally(receipts);
            let all = delivered == total;
            lines.push(Line::from(Span::styled(
                format!(" delivered to {delivered} of {total}"),
                Style::default()
                    .fg(if all { SELECTION_GREEN } else { ALERT_AMBER })
                    .add_modifier(Modifier::BOLD),
            )));
            // Every leg, always — including the ones that worked. A list of
            // only the failures cannot be told apart from a list that failed
            // to render.
            for receipt in receipts {
                let failed =
                    receipt.status != ainb_hangar_proto::fleet::ActionReceiptStatus::Delivered;
                let mut row = vec![
                    Span::styled(
                        format!("  {:<28}", receipt.session_key),
                        Style::default().fg(SOFT_WHITE),
                    ),
                    Span::styled(
                        ainb_hangar_proto::fleet::receipt_status_token(receipt.status).to_string(),
                        Style::default().fg(if failed { ALERT_RED } else { SELECTION_GREEN }),
                    ),
                ];
                if let Some(detail) = &receipt.detail {
                    row.push(Span::styled(
                        format!("  {detail}"),
                        Style::default().fg(MUTED_GRAY),
                    ));
                }
                lines.push(Line::from(row));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

/// The fewest rows the chat renderer will paint a conversation into.
///
/// Named because the daemon offer above it has to respect the same floor: an
/// offer that ate the conversation's last rows would hide the daemon's own
/// words to make room for a key that explains them.
pub const MIN_CHAT_ROWS: u16 = 4;

/// Render one chat conversation into the right pane.
pub fn render_chat(frame: &mut Frame, area: Rect, host: &crate::fleet::chat_host::ChatHost) {
    // Below this the chat renderer draws nothing at all rather than something
    // illegible, so say so instead of leaving a blank pane — a blank box with
    // no explanation is the symptom this screen exists to remove.
    if area.width < 24 || area.height < MIN_CHAT_ROWS {
        frame.render_widget(
            ratatui::widgets::Paragraph::new("widen the pane to show this conversation")
                .style(Style::default().fg(MUTED_GRAY)),
            area,
        );
        return;
    }
    let mut wire = ainb_plugin_protocol::wire_buffer::WireBuffer::new(area.width, area.height);
    ainb_plugin_hangar::screen::fleet_chat::render_chat(
        &mut wire,
        area.width,
        0,
        area.height,
        host.state(),
    );
    blit_wire(frame, area, wire);
}

#[cfg(test)]
mod pal_header_tests {
    use ainb_hangar_proto::fleet::{FleetAdapter, FleetPalMode};

    use super::*;
    use crate::fleet::pal_dial::{DialOutcome, PalDial};

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn dial(outcomes: Vec<DialOutcome>) -> PalDial {
        let mut dial = PalDial::new();
        dial.seed_for_test(outcomes);
        dial
    }

    fn adapter(name: &str, models: &[&str]) -> FleetAdapter {
        FleetAdapter {
            name: name.to_string(),
            command: name.to_string(),
            permission_mode: "default".to_string(),
            built_in: true,
            models: models.iter().map(ToString::to_string).collect(),
        }
    }

    /// Every dial names the key that turns it, ON the row it turns. A header
    /// whose bindings live in a footer legend is three things to remember.
    #[test]
    fn each_setting_carries_its_own_key() {
        let rendered = text(&pal_header(&dial(vec![DialOutcome::Adapters(vec![
            adapter("claude-agent-acp", &["sonnet-5"]),
        ])])));
        for (label, key) in [("engine", "e"), ("model", "o"), ("mode", "g")] {
            // Anchored on the padded label, not `contains`: "model" contains
            // "mode", so a loose match hands the mode assertion the model row.
            let row = rendered
                .lines()
                .find(|line| line.starts_with(&format!(" {label:<7}")))
                .unwrap_or_else(|| panic!("no {label} row in:\n{rendered}"));
            assert!(
                row.contains(key),
                "the {label} row does not name `{key}`: {row}"
            );
        }
        assert!(rendered.contains("claude-agent-acp"));
        assert!(rendered.contains("guarded"), "the dial defaults to guarded");
    }

    /// An adapter with no declared models runs its own default. Saying so beats
    /// a blank value, which reads as a failed read.
    #[test]
    fn a_modelless_adapter_says_it_runs_its_own_default() {
        let rendered = text(&pal_header(&dial(vec![DialOutcome::Adapters(vec![
            adapter("codex-acp", &[]),
        ])])));
        assert!(rendered.contains("adapter default"), "{rendered}");
    }

    /// `yolo` is the mode where nothing else stops to ask, so the pane itself
    /// has to be the reminder — and it must still say `kill` is exempt.
    #[test]
    fn yolo_carries_its_banner_and_names_the_exemption() {
        let rendered = text(&pal_header(&dial(vec![
            DialOutcome::Adapters(vec![adapter("claude-agent-acp", &[])]),
            DialOutcome::Applied {
                provider: "claude-agent-acp".to_string(),
                mode: FleetPalMode::Yolo,
                model: None,
                replaced: false,
            },
        ])));
        assert!(rendered.contains("yolo"), "{rendered}");
        assert!(rendered.contains("no confirm card"), "{rendered}");
        assert!(rendered.contains("kill still asks"), "{rendered}");

        let guarded = text(&pal_header(&dial(vec![DialOutcome::Adapters(vec![
            adapter("claude-agent-acp", &[]),
        ])])));
        assert!(
            !guarded.contains("no confirm card"),
            "the banner must be yolo-only: {guarded}"
        );
    }

    /// A swap changes which session the channel talks to. An operator who is
    /// mid-conversation has to be told, or the empty timeline reads as a bug.
    #[test]
    fn a_replaced_session_is_announced() {
        let rendered = text(&pal_header(&dial(vec![
            DialOutcome::Adapters(vec![
                adapter("claude-agent-acp", &[]),
                adapter("codex-acp", &[]),
            ]),
            DialOutcome::Applied {
                provider: "codex-acp".to_string(),
                mode: FleetPalMode::Guarded,
                model: None,
                replaced: true,
            },
        ])));
        assert!(rendered.contains("engine swapped"), "{rendered}");
        assert!(rendered.contains("new session"), "{rendered}");
    }

    /// The failure names the CALL and offers the way out. A dead end with no
    /// retry is the symptom this pane replaces.
    #[test]
    fn a_failure_names_the_call_and_the_retry_key() {
        let rendered = text(&pal_header(&dial(vec![DialOutcome::Failed {
            call: "fleet/adapter_list".to_string(),
            detail: "daemon is not running".to_string(),
        }])));
        assert!(rendered.contains("fleet/adapter_list failed"), "{rendered}");
        assert!(rendered.contains("daemon is not running"), "{rendered}");
        assert!(rendered.contains("retry"), "{rendered}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::attention::{AttentionKind, SessionAttention};
    use crate::models::{Session, SessionStatus, Workspace};

    fn state_with(chips: Vec<SessionAttention>, select: bool) -> AppState {
        state_with_errors(chips, Vec::new(), select)
    }

    /// `chips` is what the ROW lights; `errors` is what the `err` pane keeps,
    /// which is deliberately a superset — a failure past the window leaves the
    /// first and stays in the second.
    fn state_with_errors(
        chips: Vec<SessionAttention>,
        errors: Vec<SessionAttention>,
        select: bool,
    ) -> AppState {
        let mut state = AppState::new();
        state.workspaces.clear();
        let mut workspace = Workspace::new("proj".to_string(), "/work/proj".into());
        let mut session = Session::new("proj".to_string(), "/work/proj".to_string());
        session.status = SessionStatus::Idle;
        session.live_attention = chips;
        session.errors = errors;
        workspace.add_session(session);
        state.workspaces.push(workspace);
        state.selected_workspace_index = Some(0);
        state.selected_session_index = select.then_some(0);
        state
    }

    /// The rendered text of a pane, so a test asserts what an operator reads.
    fn rendered<F: FnOnce(&mut Frame, Rect)>(width: u16, height: u16, draw: F) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                let area = frame.area();
                draw(frame, area);
            })
            .expect("draw");
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer.cell((x, y)).map_or(" ", |c| c.symbol()).to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn err_chip(since_ms: i64, detail: Option<&str>) -> SessionAttention {
        let chip = SessionAttention::local(AttentionKind::Err, since_ms);
        match detail {
            Some(detail) => chip.with_detail(detail),
            None => chip,
        }
    }

    #[test]
    fn err_needs_something_to_have_failed() {
        let none = state_with(Vec::new(), false);
        assert_eq!(
            SessionTab::Err.disabled_reason(&none),
            Some("select a session first")
        );
        let quiet = state_with(Vec::new(), true);
        assert_eq!(
            SessionTab::Err.disabled_reason(&quiet),
            Some("nothing has failed on this session")
        );
        // A RETIRED error still opens the pane. That is the whole point: the
        // chip is gone from the row and the reason must not go with it.
        let retired = state_with_errors(Vec::new(), vec![err_chip(0, Some("boom"))], true);
        assert!(SessionTab::Err.enabled(&retired));
    }

    /// The reason the pane exists. `SessionStatus::Error` has always carried a
    /// sentence; before this it reached no per-session surface at all.
    #[test]
    fn the_err_pane_shows_the_reason_the_producer_gave() {
        let state = state_with_errors(
            vec![err_chip(0, Some("adapter exited 1: no such model"))],
            vec![err_chip(0, Some("adapter exited 1: no such model"))],
            true,
        );
        let text = rendered(70, 8, |frame, area| render_err(frame, area, &state));
        assert!(
            text.contains("adapter exited 1"),
            "the err pane must show the reason:\n{text}"
        );
        assert!(text.contains("ERR"), "and name the state:\n{text}");
    }

    /// A producer that raised an error with no text said nothing, and the pane
    /// says so rather than inventing a sentence that reads like the agent's.
    #[test]
    fn an_err_with_no_text_says_so_rather_than_inventing_one() {
        let state = state_with_errors(vec![err_chip(0, None)], vec![err_chip(0, None)], true);
        let text = rendered(70, 8, |frame, area| render_err(frame, area, &state));
        assert!(text.contains("carried no reason text"), "{text}");
    }

    /// Retiring the chip must not just move the mystery: the pane says WHY the
    /// row is quiet about a failure it is still showing, and where to change it.
    #[test]
    fn a_retired_err_says_why_the_row_no_longer_lights() {
        let live = state_with_errors(
            vec![err_chip(0, Some("boom"))],
            vec![err_chip(0, Some("boom"))],
            true,
        );
        let live_text = rendered(70, 10, |frame, area| render_err(frame, area, &live));
        assert!(
            !live_text.contains("no longer lights"),
            "a chip that IS on the row must not claim it has retired:\n{live_text}"
        );

        let retired = state_with_errors(Vec::new(), vec![err_chip(0, Some("boom"))], true);
        let retired_text = rendered(70, 10, |frame, area| render_err(frame, area, &retired));
        assert!(
            retired_text.contains("no longer lights"),
            "a retired failure must say why nothing is flagged:\n{retired_text}"
        );
        assert!(
            retired_text.contains("attention_err_window_hours"),
            "and name the knob that decides it:\n{retired_text}"
        );
    }

    /// `Tab` skips a dimmed pane rather than stopping on it, and the new one is
    /// no exception.
    #[test]
    fn tab_skips_the_err_pane_when_nothing_has_failed() {
        let state = state_with(Vec::new(), true);
        assert!(!SessionTab::Err.enabled(&state));
        assert_ne!(
            cycle(&state, SessionTab::Ask, true),
            SessionTab::Err,
            "Tab must not land on a pane that cannot say anything"
        );
    }

    /// Everything the footer actually paints, as one line of text.
    fn footer_text(state: &AppState, tab: SessionTab, capturing: bool) -> String {
        footer(state, tab, capturing)
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    /// Render the Pal pane into a rect of exactly `w`x`h` and return its
    /// rows, so a test can ask what a SHORT pane actually painted.
    fn pal_rows(
        offer: Option<&crate::fleet::daemon_cta::DaemonStartCta>,
        w: u16,
        h: u16,
    ) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut terminal = ratatui::Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                render_pal(
                    frame,
                    frame.area(),
                    vec![Line::raw(" engine   claude-agent-acp  \u{25c0} \u{2325}e")],
                    offer,
                    None,
                );
            })
            .expect("draw the Pal pane");
        let buffer = terminal.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buffer.cell((x, y)).map_or(" ", ratatui::buffer::Cell::symbol))
                    .collect::<String>()
            })
            .collect()
    }

    /// Put the attention poller's cell in the state it reaches when the socket
    /// is dialled and nothing accepts.
    fn with_daemon(state: &mut AppState, reachable: bool, not_running: bool) {
        *state.daemon_attention.lock().unwrap() = crate::fleet::attention::DaemonAttention {
            by_session_id: std::collections::HashMap::new(),
            by_cwd: std::collections::HashMap::new(),
            by_cwd_without_session_id: std::collections::HashMap::new(),
            all: std::collections::HashMap::new(),
            reachable,
            error: (!reachable).then(|| "connect /x/hangar.sock: refused".to_string()),
            not_running,
        };
    }

    /// The footer's verb is the one the key fires. With no daemon that is the
    /// offer, and `send message` — advertised over a pane that says "nothing to
    /// send to" in the same breath — must be gone.
    #[test]
    fn a_pane_offering_a_daemon_advertises_that_and_not_a_send() {
        let mut state = state_with(Vec::new(), true);
        state.session_tab = SessionTab::Pal;
        state.focused_pane = crate::app::state::FocusedPane::LiveLogs;
        with_daemon(&mut state, false, true);

        assert!(state.pal_daemon_cta_open());
        assert!(state.pal_daemon_cta_armed());
        assert_eq!(SessionTab::Pal.enter_verb_in(&state), START_DAEMON_VERB);
        let footer = footer_text(&state, SessionTab::Pal, false);
        assert!(
            footer.contains(START_DAEMON_VERB),
            "the footer must name the verb Enter fires: {footer}"
        );
        assert!(
            !footer.contains("send message"),
            "and must not still promise a send: {footer}"
        );
    }

    /// The offer appears ONLY for a daemon that is not there. A daemon that
    /// answered the dial and then wedged is equally unreachable and must not be
    /// offered a start: that would be a fresh lie on the surface the offer was
    /// added to fix.
    #[test]
    fn an_unreachable_daemon_that_is_still_running_is_not_offered_a_start() {
        let mut state = state_with(Vec::new(), true);
        state.session_tab = SessionTab::Pal;
        state.focused_pane = crate::app::state::FocusedPane::LiveLogs;
        with_daemon(&mut state, false, false);

        assert!(!state.hangar_daemon_not_running());
        assert!(!state.pal_daemon_cta_open());
        assert_ne!(SessionTab::Pal.enter_verb_in(&state), START_DAEMON_VERB);

        // And with the daemon up, nothing about this offer is on screen.
        with_daemon(&mut state, true, false);
        assert!(!state.pal_daemon_cta_open());
    }

    /// The footer asks the LIVE pane whether it can send. A Pal that opened
    /// against a daemon which never minted its channel says it cannot, and the
    /// footer must not promise otherwise — that is the exact pairing an
    /// operator saw: `⊘ nothing to send to` under `Enter send message`.
    #[test]
    fn a_pane_that_cannot_send_advertises_no_verb_at_all() {
        let mut state = state_with(Vec::new(), true);
        state.session_tab = SessionTab::Pal;
        state.focused_pane = crate::app::state::FocusedPane::LiveLogs;
        // Daemon UP: this is not the offer's case, it is the one where the
        // conversation opened and its scope never resolved.
        with_daemon(&mut state, true, false);
        state.pal_chat = Some(crate::fleet::chat_host::ChatHost::pal());

        assert!(
            state.session_tab_send_block(SessionTab::Pal).is_some(),
            "a Pal with no minted scope has nothing to send to"
        );
        assert_eq!(SessionTab::Pal.enter_verb_in(&state), "");
        let footer = footer_text(&state, SessionTab::Pal, true);
        assert!(
            !footer.contains("Enter"),
            "a footer with no verb must not print a bare Enter either: {footer}"
        );
    }

    /// A pane too short for the whole offer keeps the KEY, not the top of the
    /// block.
    ///
    /// A `Paragraph` drops the rows it has no room for from the BOTTOM, and the
    /// bottom of this block is the remedy — so a short pane silently kept the
    /// diagnosis and lost the way out, while the footer went on advertising it.
    /// Driven through a real render into a deliberately short rect, because
    /// only the painted buffer settles what survived.
    #[test]
    fn a_short_pane_keeps_the_offers_key_and_drops_its_padding() {
        let cta = crate::fleet::daemon_cta::DaemonStartCta::default();
        // 6 rows: 1 header + 5 left, of which the chat floor claims 4. One row
        // for the offer, and it has to be the one that says what to press.
        for height in [6, 7, 8, 12] {
            let rows = pal_rows(Some(&cta), 90, height);
            let painted = rows.join("\n");
            assert!(
                painted.contains(START_DAEMON_VERB) && painted.contains('\u{23ce}'),
                "a {height}-row pane lost the offer's key:\n{painted}"
            );
        }

        // And at a WIDTH that wraps the headline onto a second row, so the
        // block is sized by rows painted rather than by lines built.
        let narrow = pal_rows(Some(&cta), 30, 8).join("\n");
        assert!(
            narrow.contains("start the hangar") && narrow.contains('\u{23ce}'),
            "a wrapped headline pushed the key off the block:\n{narrow}"
        );
    }

    /// The offer never takes the conversation's floor while there is room to
    /// leave it: the daemon's own words below are what an operator reads when
    /// the start does not help.
    #[test]
    fn a_tall_pane_gives_the_offer_its_padding_and_the_chat_its_floor() {
        let cta = crate::fleet::daemon_cta::DaemonStartCta::default();
        let rows = pal_rows(Some(&cta), 90, 20);
        let painted = rows.join("\n");
        assert!(painted.contains(START_DAEMON_VERB));
        assert!(
            painted.contains("Pal needs the hangar daemon"),
            "a pane with room must still lead with the reason:\n{painted}"
        );
        assert!(
            painted.contains("opening the Pal channel"),
            "the conversation below must keep its rows:\n{painted}"
        );
    }

    /// With focus on the session list, `Enter` is the LIST's. The offer stays
    /// on screen, and the footer stops promising a key it will not get.
    #[test]
    fn the_offer_neither_claims_enter_nor_advertises_it_from_the_session_list() {
        let mut state = state_with(Vec::new(), true);
        state.session_tab = SessionTab::Pal;
        state.focused_pane = crate::app::state::FocusedPane::Sessions;
        with_daemon(&mut state, false, true);

        assert!(
            state.pal_daemon_cta_open(),
            "the offer is still on screen beside the list"
        );
        assert!(
            !state.pal_daemon_cta_armed(),
            "but Enter belongs to the list, so starting a daemon is not on it"
        );
        let footer = footer_text(&state, SessionTab::Pal, false);
        assert!(
            !footer.contains(START_DAEMON_VERB),
            "and the footer must not promise a key that goes elsewhere: {footer}"
        );
    }

    /// A start already out disarms the key, so the footer stops advertising a
    /// second press that the offer declines.
    #[test]
    fn a_start_in_flight_disarms_the_key_and_the_verb() {
        let mut state = state_with(Vec::new(), true);
        state.session_tab = SessionTab::Pal;
        state.focused_pane = crate::app::state::FocusedPane::LiveLogs;
        with_daemon(&mut state, false, true);
        assert!(state.pal_daemon_cta_armed());

        state.daemon_start_cta.start();
        assert!(!state.pal_daemon_cta_armed());
        assert!(!footer_text(&state, SessionTab::Pal, true).contains(START_DAEMON_VERB));
    }

    /// The tab the pane TICKS and the tab it PAINTS resolve to the same host.
    ///
    /// The render path ticks through `chat_host_for` (which needs `&mut`) and
    /// then paints through `chat_host`, because the two borrows cannot be held
    /// at once. That is only safe while the two resolve identically, so it is
    /// pinned rather than assumed — reaching for `pal_chat` at the render
    /// site was the same fact written twice.
    #[test]
    fn ticking_a_tabs_host_and_painting_it_resolve_to_the_same_conversation() {
        let mut state = state_with(Vec::new(), true);
        state.workspaces[0].sessions[0].provider_session_id = Some("hook-sess-1".to_string());

        for tab in ALL_TABS {
            let ticked = state.chat_host_for(tab).map(std::ptr::from_ref);
            let painted = state.chat_host(tab).map(std::ptr::from_ref);
            assert_eq!(
                ticked, painted,
                "{tab:?} ticks one conversation and paints another"
            );
        }
    }

    /// Both refusals go through the ONE predicate, in the refusing surface's
    /// own words.
    ///
    /// They arrived as two adjacent special cases — an `ask` whose picker is
    /// native, and a chat host with nothing to send to — and a third tab
    /// needing this must extend the match rather than add a fourth branch to
    /// the footer. Asserted on the REASON, so a predicate that answered `true`
    /// for the wrong surface would not pass.
    #[test]
    fn one_predicate_carries_every_reason_a_tab_refuses_enter() {
        use crate::fleet::attention::{Answerable, Unanswerable};

        // The `ask` case, which landed on main: a native picker is answered in
        // the agent's own terminal.
        let mut refused = SessionAttention::local(AttentionKind::Ask, 0);
        refused.answerable = Answerable::No(Unanswerable::NativePicker);
        let mut state = state_with(vec![refused], true);
        assert_eq!(
            SessionTab::Ask.enter_refusal(&state).as_deref(),
            Some(Unanswerable::NativePicker.reason()),
            "the ask tab must carry the chip's own refusal, not a paraphrase"
        );
        assert_eq!(SessionTab::Ask.enter_verb_in(&state), "");

        // The chat case: a Pal whose scope the daemon never minted.
        state.session_tab = SessionTab::Pal;
        state.pal_chat = Some(crate::fleet::chat_host::ChatHost::pal());
        with_daemon(&mut state, true, false);
        assert!(
            SessionTab::Pal.enter_refusal(&state).is_some(),
            "a Pal with no scope has nothing to send to"
        );
        assert_eq!(SessionTab::Pal.enter_verb_in(&state), "");

        // And the tabs with nothing to refuse say so rather than defaulting.
        // `err` is one of them: it shows what already failed, so there is no
        // send for it to decline.
        for tab in [SessionTab::Preview, SessionTab::Err, SessionTab::Log] {
            assert_eq!(tab.enter_refusal(&state), None, "{tab:?}");
        }
    }

    #[test]
    fn preview_and_pal_are_never_disabled() {
        let state = state_with(Vec::new(), false);
        assert!(SessionTab::Preview.enabled(&state));
        assert!(SessionTab::Pal.enabled(&state));
    }

    #[test]
    fn ask_needs_something_actually_waiting() {
        // A selected session with no blocking chip: the tab is there, dimmed,
        // and says why.
        let quiet = state_with(Vec::new(), true);
        assert_eq!(
            SessionTab::Ask.disabled_reason(&quiet),
            Some("nothing is waiting on an answer here")
        );
        // A DONE chip is not a question either.
        let done = state_with(vec![SessionAttention::local(AttentionKind::Done, 0)], true);
        assert!(!SessionTab::Ask.enabled(&done));
        // An ASK opens it.
        let asking = state_with(vec![SessionAttention::local(AttentionKind::Ask, 0)], true);
        assert!(SessionTab::Ask.enabled(&asking));
    }

    #[test]
    fn thread_and_log_need_a_session_row() {
        let none = state_with(Vec::new(), false);
        for tab in [SessionTab::Thread, SessionTab::Log] {
            assert_eq!(tab.disabled_reason(&none), Some("select a session first"));
        }
        let mut selected = state_with(Vec::new(), true);
        assert!(SessionTab::Log.enabled(&selected));
        // The thread needs one thing more: the agent's own session id, which is
        // what its scope is addressed by.
        assert_eq!(
            SessionTab::Thread.disabled_reason(&selected),
            Some("this session has not fired a hook yet, so its thread has no scope"),
        );
        selected.workspaces[0].sessions[0].provider_session_id = Some("abc".to_string());
        assert!(SessionTab::Thread.enabled(&selected));
    }

    #[test]
    fn the_thread_scope_is_the_agents_session_id_never_the_tmux_name() {
        // A scope composed from the tmux name addresses something the daemon
        // has never heard of: an empty timeline forever against a real daemon,
        // with every unit test still green.
        let mut state = state_with(Vec::new(), true);
        state.workspaces[0].sessions[0].tmux_session_name = Some("tmux_proj".to_string());
        assert_eq!(state.selected_session_chat_key(), None);
        state.workspaces[0].sessions[0].provider_session_id = Some("hook-sess-1".to_string());
        assert_eq!(
            state.selected_session_chat_key().as_deref(),
            Some("claude:hook-sess-1")
        );
    }

    #[test]
    fn cycling_skips_the_dimmed_tabs() {
        // Nothing selected: only preview and pal are live.
        let state = state_with(Vec::new(), false);
        assert_eq!(cycle(&state, SessionTab::Preview, true), SessionTab::Pal);
        assert_eq!(cycle(&state, SessionTab::Pal, true), SessionTab::Preview);
        assert_eq!(cycle(&state, SessionTab::Preview, false), SessionTab::Pal);
    }

    #[test]
    fn cycling_visits_every_tab_when_everything_is_available() {
        let mut state = state_with_errors(
            vec![SessionAttention::local(AttentionKind::Ask, 0)],
            // `err` needs a failure to show before it is reachable, the same
            // way `ask` needs a question.
            vec![SessionAttention::local(AttentionKind::Err, 0).with_detail("boom")],
            true,
        );
        // The thread needs a scope before it is reachable.
        state.workspaces[0].sessions[0].provider_session_id = Some("hook-sess-1".to_string());
        let state = state;
        let mut seen = vec![SessionTab::Preview];
        let mut at = SessionTab::Preview;
        for _ in 1..ALL_TABS.len() {
            at = cycle(&state, at, true);
            seen.push(at);
        }
        assert_eq!(seen, ALL_TABS.to_vec());
        assert_eq!(cycle(&state, at, true), SessionTab::Preview, "and it wraps");
    }

    #[test]
    fn a_tab_that_goes_dead_under_the_operator_falls_back_to_preview() {
        // The ASK is answered while the operator is on the `ask` tab. Leaving
        // them there shows a question they can no longer act on.
        let answered = state_with(Vec::new(), true);
        assert_eq!(resolve(&answered, SessionTab::Ask), SessionTab::Preview);
        // A live tab is left exactly where it was.
        let asking = state_with(vec![SessionAttention::local(AttentionKind::Ask, 0)], true);
        assert_eq!(resolve(&asking, SessionTab::Ask), SessionTab::Ask);
    }

    #[test]
    fn the_strip_always_renders_every_label_in_strip_order() {
        // Declaration order IS strip order and `Tab` order, so pinning it here
        // is what stops the rendered strip and the key that walks it drifting.
        assert_eq!(
            ALL_TABS.iter().map(|tab| tab.label()).collect::<Vec<_>>(),
            vec!["preview", "ask", "err", "thread", "pal", "log"],
        );
        // Dimmed, never hidden — otherwise the strip reflows under the cursor
        // every time a session answers a question.
        for select in [true, false] {
            let state = state_with(Vec::new(), select);
            let rendered: String = strip(&state, SessionTab::Preview)
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            for tab in ALL_TABS {
                assert!(
                    rendered.contains(tab.label()),
                    "{} missing at select={select}: {rendered}",
                    tab.label()
                );
            }
        }
    }

    #[test]
    fn every_tab_that_takes_enter_names_its_verb() {
        for tab in ALL_TABS {
            let verb = tab.enter_verb();
            assert_eq!(
                verb.is_empty(),
                matches!(tab, SessionTab::Err | SessionTab::Log),
                "{tab:?} must declare a verb unless Enter is a no-op there"
            );
        }
    }

    #[test]
    fn the_footer_says_attach_digits_work_on_every_tab() {
        let state = state_with(Vec::new(), true);
        for tab in ALL_TABS {
            let rendered: String =
                footer(&state, tab, false).spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                rendered.contains("1-9") && rendered.contains("any tab"),
                "an operator who learned Enter is scoped will assume the digits \
                 are too: {rendered}"
            );
        }
    }

    #[test]
    fn selected_footer_shows_observed_model_effort_and_direct_children() {
        let mut state = state_with(Vec::new(), true);
        let id = state.workspaces[0].sessions[0].id;
        state.fleet_metadata.insert(
            id,
            crate::app::state::SessionFleetMetadata {
                model: Some("gpt-5.6".to_string()),
                reasoning_effort: Some("high".to_string()),
                direct_child_count: 2,
                provider_session_id: None,
                lifecycle: None,
                lifecycle_updated_at: 0,
            },
        );
        let rendered = footer_text(&state, SessionTab::Preview, false);
        assert!(rendered.contains("model gpt-5.6"), "{rendered}");
        assert!(rendered.contains("effort high"), "{rendered}");
        assert!(rendered.contains("2 direct children"), "{rendered}");
    }

    #[test]
    fn the_footer_stops_promising_attach_digits_inside_a_composer() {
        // A `3` typed into a message has to be a `3`. Advertising the attach
        // digits there would advertise a key that types a character instead.
        let state = state_with(Vec::new(), true);
        let rendered: String = footer(&state, SessionTab::Thread, true)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(!rendered.contains("1-9"), "{rendered}");
        assert!(rendered.contains("digits type"), "{rendered}");
        assert!(rendered.contains("Esc"), "and name the way out: {rendered}");
        assert!(
            rendered.contains("focus"),
            "and name the key that moves between the pane's two halves, since \
             Tab now belongs to the strip: {rendered}"
        );
    }
}
