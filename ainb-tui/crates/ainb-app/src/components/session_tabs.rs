// ABOUTME: Renderer-agnostic half of the `session_tabs` component: its
// state types and the logic that does not draw. The renderer lives in
// `ainb-core::components::session_tabs`, which re-exports this module.

use crate::app::AppState;

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
                } else if state.sessions.selected_sessions.is_empty() && !has_session {
                    Some("select a session first")
                } else if !state.sessions.selected_sessions.is_empty() {
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
