// ABOUTME: The sessions screen's right-pane tab strip — the switchboard that
// turns one preview pane into the whole attention surface.
//
// Five tabs over one rect: `preview` (the tmux mirror that was always there),
// `ask` (answer what is blocking), `thread` (this session's chat), `copilot`
// (the ainb assistant) and `log` (this session's notification history).
//
// The strip is the reason `Enter` stops being ambiguous. `Enter` used to mean
// "attach" everywhere, which is the wrong verb on four of these five panes, so
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
    /// This session's own chat thread, scope `session:<key>`.
    Thread,
    /// The general ainb assistant, plus its channels.
    Copilot,
    /// This session's notification history.
    Log,
}

/// Every tab, in strip order.
pub const ALL_TABS: [SessionTab; 5] = [
    SessionTab::Preview,
    SessionTab::Ask,
    SessionTab::Thread,
    SessionTab::Copilot,
    SessionTab::Log,
];

impl SessionTab {
    /// The strip label. Lower case, because these are panes, not commands.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Ask => "ask",
            Self::Thread => "thread",
            Self::Copilot => "copilot",
            Self::Log => "log",
        }
    }

    /// What `Enter` does on this tab. One sentence, shown in the footer, so the
    /// operator never has to guess which verb they are about to fire.
    #[must_use]
    pub const fn enter_verb(self) -> &'static str {
        match self {
            Self::Preview => "attach",
            Self::Ask => "send answer",
            Self::Thread | Self::Copilot => "send message",
            Self::Log => "",
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
            Self::Preview | Self::Copilot => None,
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
            Self::Thread => {
                if !has_session {
                    Some("select a session first")
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
const MUTED_GRAY: Color = Color::Rgb(120, 120, 140);
const SUBDUED_BORDER: Color = Color::Rgb(60, 60, 80);
const SOFT_WHITE: Color = Color::Rgb(220, 220, 230);
const ALERT_RED: Color = Color::Rgb(220, 90, 90);
const ALERT_AMBER: Color = Color::Rgb(230, 180, 80);

/// The colour a chip and its age share, mirrored from the session list so the
/// row and the pane never disagree about what an ASK looks like.
const fn chip_color(kind: crate::fleet::attention::AttentionKind) -> Color {
    use crate::fleet::attention::AttentionKind;
    match kind {
        AttentionKind::Ask => ALERT_AMBER,
        AttentionKind::Approve => ALERT_RED,
        AttentionKind::Err => Color::Rgb(230, 100, 100),
        AttentionKind::Done => SELECTION_GREEN,
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
        spans.push(Span::styled(tab.label(), style));
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
    let _ = state;
    let mut spans = vec![
        Span::styled(
            " \u{21e5}",
            Style::default().fg(GOLD).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" tab ", Style::default().fg(MUTED_GRAY)),
    ];
    let verb = active.enter_verb();
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

    let on_free_text = ask.focus() == AskFocus::FreeText;
    for (index, option) in chip.options.iter().enumerate() {
        let selected = !on_free_text && ask.cursor() == index;
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

    // The free-text row is always present, even on a structured request: an
    // agent's question is not always answerable with one of its own options,
    // and a surface that only offers them forces the operator back to the pane.
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

    // What the last send did. Every one of the three states is VISIBLE: an
    // answer that vanished into a worker with no feedback is the failure this
    // pane exists to remove.
    match ask.phase_for(chip) {
        Some(AnswerPhase::InFlight { .. }) => {
            let secs = ask.elapsed().map_or(0, |elapsed| elapsed.as_secs());
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
        Some(AnswerPhase::Failed { reason }) => {
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
pub fn render_log(frame: &mut Frame, area: Rect, rows: &[LogRow]) {
    use ratatui::widgets::{List, ListItem};

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

/// Read one session's notification history out of the notifyd store.
///
/// Opens the store per call rather than holding a handle, matching how the chip
/// producer reads it: the query is microseconds, it only runs while the `log`
/// tab is actually open, and it keeps the daemon as the database's sole
/// long-lived owner.
#[must_use]
pub fn read_log(cwd: &str, agent: Option<&str>, limit: u32) -> Vec<LogRow> {
    let Ok(paths) = ainb_plugin_notifyd::Paths::from_home() else {
        return Vec::new();
    };
    if !paths.db.exists() {
        return Vec::new();
    }
    let Ok(store) = ainb_plugin_notifyd::Store::open(&paths.db) else {
        return Vec::new();
    };
    let cwd = cwd.trim_end_matches('/');
    // Window and limit deliberately generous: this is a history pane an
    // operator opens on purpose, not a per-frame read.
    let Ok(rows) = store.recent_since(0, limit.saturating_mul(20)) else {
        return Vec::new();
    };
    rows.into_iter()
        .filter(|row| {
            row.cwd.trim_end_matches('/') == cwd && agent.is_none_or(|agent| row.agent == agent)
        })
        .take(limit as usize)
        .map(|row| LogRow {
            ts: row.ts,
            event: row.raw_event.clone(),
            detail: log_detail(&row),
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

/// Render one chat conversation into the right pane.
pub fn render_chat(frame: &mut Frame, area: Rect, host: &crate::fleet::chat_host::ChatHost) {
    // Below this the chat renderer draws nothing at all rather than something
    // illegible, so say so instead of leaving a blank pane — a blank box with
    // no explanation is the symptom this screen exists to remove.
    if area.width < 24 || area.height < 4 {
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
mod tests {
    use super::*;
    use crate::fleet::attention::{AttentionKind, SessionAttention};
    use crate::models::{Session, SessionStatus, Workspace};

    fn state_with(chips: Vec<SessionAttention>, select: bool) -> AppState {
        let mut state = AppState::new();
        state.workspaces.clear();
        let mut workspace = Workspace::new("proj".to_string(), "/work/proj".into());
        let mut session = Session::new("proj".to_string(), "/work/proj".to_string());
        session.status = SessionStatus::Idle;
        session.live_attention = chips;
        workspace.add_session(session);
        state.workspaces.push(workspace);
        state.selected_workspace_index = Some(0);
        state.selected_session_index = select.then_some(0);
        state
    }

    #[test]
    fn preview_and_copilot_are_never_disabled() {
        let state = state_with(Vec::new(), false);
        assert!(SessionTab::Preview.enabled(&state));
        assert!(SessionTab::Copilot.enabled(&state));
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
        // Nothing selected: only preview and copilot are live.
        let state = state_with(Vec::new(), false);
        assert_eq!(
            cycle(&state, SessionTab::Preview, true),
            SessionTab::Copilot
        );
        assert_eq!(
            cycle(&state, SessionTab::Copilot, true),
            SessionTab::Preview
        );
        assert_eq!(
            cycle(&state, SessionTab::Preview, false),
            SessionTab::Copilot
        );
    }

    #[test]
    fn cycling_visits_every_tab_when_everything_is_available() {
        let mut state = state_with(vec![SessionAttention::local(AttentionKind::Ask, 0)], true);
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
    fn the_strip_always_renders_all_five_labels() {
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
                tab == SessionTab::Log,
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
