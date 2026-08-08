//! The Fleet chat surface: the operator's conversation with the fleet copilot.
//!
//! Pure, exactly like the rest of [`super::fleet`]. This module owns no socket:
//! it folds host-supplied wire rows into state, emits typed intents the host
//! executes, and paints into a [`WireBuffer`]. Every RPC named in a doc comment
//! here is issued by `ainb-core`, never from this crate.
//!
//! Three things in here are load-bearing, and all three are the same class of
//! bug the ACP provider label shipped with (a fact mapped TWICE, with only one
//! half updated):
//!
//! 1. **Attribution.** `fleet/message_send` carries an `actor` precisely so a
//!    copilot-authored write is distinguishable from a human's. That guarantee
//!    dies at the last inch if the panel paints both rows the same, so
//!    [`ChatActor`] is an enum with an exhaustive label mapping, and a blank
//!    sender renders as [`ChatActor::Unattributed`] rather than degrading into
//!    the operator.
//! 2. **Answerability.** A confirm card is answerable ONLY in
//!    [`FleetConfirmState::Open`], decided by an exhaustive match so a new
//!    lifecycle state fails to compile here instead of silently rendering an
//!    approve key. A card whose frame this build cannot decode at all becomes
//!    [`ChatConfirmCard::Unrecognised`], which is never answerable — the mirror
//!    of the macOS client's `isAnswerable` rule, and the reason the confirm
//!    list is decoded card-by-card from raw JSON rather than as one typed
//!    `Vec<FleetConfirm>` that a single unknown token would reject wholesale.
//! 3. **Display tokens.** Every wire enum this screen renders (message kind,
//!    confirm state, activity class, activity outcome) is mapped with a
//!    wildcard-free match, so adding a variant upstream is a compile error in
//!    this file rather than an `UNKNOWN` on the one screen an operator reads.

use ainb_hangar_proto::fleet::{
    FleetActivityClass, FleetActivityOutcome, FleetActivityRow, FleetConfirm, FleetConfirmAnswer,
    FleetConfirmAnswerParams, FleetConfirmState, FleetMessage, FleetMessageKind,
};
use ainb_plugin_sdk::{Color, WireBuffer};

use super::fleet::{
    ALERT, BLUE, FG, GOLD, GREEN, MUTED, SURFACE, VIOLET, fill_background, put_str, put_str_styled,
    truncate_ellipsis,
};

/// The channel kind the copilot conversation lives on.
///
/// NOT a scope. `fleet/channel_create` MINTS `channel:<ulid>`, so the copilot
/// channel's scope is a value only the daemon knows; this surface learns it
/// from `fleet/channel_list` and carries it in [`ChatSnapshot::scope_key`].
///
/// A literal `channel:copilot` was the first version of this and it was the
/// same bug as the ACP provider label: two halves of one fact written in two
/// places, agreeing in every unit test and disagreeing against a real daemon,
/// where the timeline silently reads an empty scope forever.
pub const COPILOT_CHANNEL_KIND: &str = "copilot";

/// How many timeline rows the surface keeps in memory.
///
/// ponytail: a flat cap, not a scrollback. The channel timeline is the copilot
/// conversation, not a transcript; `fleet/message_list` pages by cursor when a
/// real scrollback is wanted.
pub const CHAT_TIMELINE_MAX: usize = 200;

/// Milliseconds between poll refreshes while the chat surface is open.
///
/// ponytail: polling, not `fleet/message_subscribe`. One timer beats a second
/// long-lived socket in the render path, and the subscribe stream is the
/// upgrade when a poll's latency is actually felt.
pub const CHAT_POLL_INTERVAL_MS: i64 = 1_000;

/// Who wrote one chat row.
///
/// Derived from [`FleetMessage::sender`], which the daemon records from the
/// send's `actor` and never from the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatActor {
    /// A human at a client: `sender == "operator"`.
    Operator,
    /// The fleet copilot writing through its MCP tools: `sender == "copilot"`.
    Copilot,
    /// An agent session replying in its own name: `sender` is a session key.
    Session(String),
    /// A row whose sender is blank.
    ///
    /// The daemon refuses a blank actor, so this should be unreachable over a
    /// current wire. It exists because the alternative default is "operator",
    /// and a row that cannot say who wrote it must never claim a human did.
    Unattributed,
}

impl ChatActor {
    /// Map a wire `sender` token onto the actor the panel paints.
    #[must_use]
    pub fn from_wire(sender: &str) -> Self {
        match sender.trim() {
            "" => Self::Unattributed,
            "operator" => Self::Operator,
            "copilot" => Self::Copilot,
            other => Self::Session(other.to_string()),
        }
    }

    /// The on-screen attribution label.
    ///
    /// Exhaustive on purpose: a new actor kind must fail to compile here rather
    /// than fall through to a label that reads like somebody else.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Operator => "YOU".to_string(),
            Self::Copilot => "COPILOT".to_string(),
            Self::Session(key) => truncate_ellipsis(key, 12),
            Self::Unattributed => "UNATTRIBUTED".to_string(),
        }
    }

    /// The colour the attribution column is painted in.
    #[must_use]
    pub const fn color(&self) -> Color {
        match self {
            Self::Operator => GOLD,
            Self::Copilot => VIOLET,
            Self::Session(_) => BLUE,
            Self::Unattributed => ALERT,
        }
    }
}

/// The on-screen label for a message kind.
///
/// Wildcard-free: a new [`FleetMessageKind`] is a compile error here.
#[must_use]
pub const fn message_kind_label(kind: FleetMessageKind) -> &'static str {
    match kind {
        FleetMessageKind::User => "",
        FleetMessageKind::Agent => "",
        FleetMessageKind::Marker => "· ",
    }
}

/// The on-screen label for a confirm card's lifecycle state.
///
/// Wildcard-free: a new [`FleetConfirmState`] is a compile error here.
#[must_use]
pub const fn confirm_state_label(state: FleetConfirmState) -> &'static str {
    match state {
        FleetConfirmState::Open => "OPEN",
        FleetConfirmState::Approved => "APPROVED",
        FleetConfirmState::Denied => "DENIED",
        FleetConfirmState::Expired => "EXPIRED",
    }
}

/// Whether a card in `state` may be answered.
///
/// The mirror of the macOS client's `isAnswerable`. Wildcard-free so a new
/// lifecycle state has to be classified deliberately: the failure this guards
/// is a state this build does not understand rendering an approve key, which
/// is exactly what a tolerant decode is supposed to prevent.
#[must_use]
pub const fn confirm_state_is_answerable(state: FleetConfirmState) -> bool {
    match state {
        FleetConfirmState::Open => true,
        FleetConfirmState::Approved | FleetConfirmState::Denied | FleetConfirmState::Expired => {
            false
        }
    }
}

/// The on-screen label for a copilot action's guardrail class.
#[must_use]
pub const fn activity_class_label(class: FleetActivityClass) -> &'static str {
    match class {
        FleetActivityClass::Read => "READ",
        FleetActivityClass::Write => "WRITE",
        FleetActivityClass::Destructive => "DESTRUCTIVE",
    }
}

/// The colour a guardrail class is painted in.
#[must_use]
pub const fn activity_class_color(class: FleetActivityClass) -> Color {
    match class {
        FleetActivityClass::Read => MUTED,
        FleetActivityClass::Write => BLUE,
        FleetActivityClass::Destructive => ALERT,
    }
}

/// The on-screen label for how a copilot action ended.
#[must_use]
pub const fn activity_outcome_label(outcome: FleetActivityOutcome) -> &'static str {
    match outcome {
        FleetActivityOutcome::Ok => "ok",
        FleetActivityOutcome::Denied => "denied",
        FleetActivityOutcome::Expired => "expired",
        FleetActivityOutcome::Error => "error",
    }
}

/// One confirm card as this build understands it.
///
/// The list is decoded ROW BY ROW from raw JSON. A typed `Vec<FleetConfirm>`
/// would fail the whole frame on one unknown enum token, and the operator would
/// lose the cards this build does understand along with the one it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatConfirmCard {
    /// A card this build decoded completely.
    Known(FleetConfirm),
    /// A card carrying something this build cannot decode.
    ///
    /// Rendered, never answerable: an unknown state that silently offers an
    /// approve key is the precise failure the tolerant-decode rule exists for.
    Unrecognised {
        /// Card identity, when the frame carried a readable one.
        confirm_id: String,
        /// Tool name, when the frame carried a readable one.
        tool: String,
        /// Why the decode failed, for the operator and the bug report.
        detail: String,
    },
}

impl ChatConfirmCard {
    /// Decode one confirm frame, tolerantly.
    #[must_use]
    pub fn decode(value: &serde_json::Value) -> Self {
        match serde_json::from_value::<FleetConfirm>(value.clone()) {
            Ok(confirm) => Self::Known(confirm),
            Err(error) => Self::Unrecognised {
                confirm_id: string_field(value, "confirm_id"),
                tool: string_field(value, "tool"),
                detail: error.to_string(),
            },
        }
    }

    /// The card's stable identity.
    #[must_use]
    pub fn confirm_id(&self) -> &str {
        match self {
            Self::Known(confirm) => &confirm.confirm_id,
            Self::Unrecognised { confirm_id, .. } => confirm_id,
        }
    }

    /// The tool the copilot asked to run.
    #[must_use]
    pub fn tool(&self) -> &str {
        match self {
            Self::Known(confirm) => &confirm.tool,
            Self::Unrecognised { tool, .. } => tool,
        }
    }

    /// The state label the row shows.
    #[must_use]
    pub fn state_label(&self) -> &'static str {
        match self {
            Self::Known(confirm) => confirm_state_label(confirm.state),
            Self::Unrecognised { .. } => "UNRECOGNISED",
        }
    }

    /// Whether this row may be approved, denied, or answered.
    #[must_use]
    pub fn is_answerable(&self) -> bool {
        match self {
            Self::Known(confirm) => confirm_state_is_answerable(confirm.state),
            Self::Unrecognised { .. } => false,
        }
    }

    /// Why the row is not answerable, for the feedback line.
    #[must_use]
    pub fn refusal(&self) -> String {
        match self {
            Self::Known(confirm) => format!(
                "{} is {}, not answerable",
                confirm.confirm_id,
                confirm_state_label(confirm.state)
            ),
            Self::Unrecognised { detail, .. } => {
                format!("card not understood by this build, not answerable: {detail}")
            }
        }
    }

    /// The tool arguments row, already projected by the daemon.
    ///
    /// The daemon projects a card's arguments down to its tool's declared
    /// schema keys (`ainb_fleet_tools::server::project_arguments`) BEFORE
    /// persisting, so model-authored prose never reaches this line. The panel
    /// renders what it is given and adds nothing.
    #[must_use]
    pub fn arguments_line(&self) -> String {
        match self {
            Self::Known(confirm) => confirm.arguments.to_string(),
            Self::Unrecognised { .. } => String::new(),
        }
    }
}

fn string_field(value: &serde_json::Value, key: &str) -> String {
    value
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// One timeline row, already attributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessageRow {
    /// Stable message identity.
    pub id: String,
    /// Who wrote it.
    pub actor: ChatActor,
    /// Message kind.
    pub kind: FleetMessageKind,
    /// Message body.
    pub body: String,
}

impl From<FleetMessage> for ChatMessageRow {
    fn from(message: FleetMessage) -> Self {
        Self {
            id: message.id,
            actor: ChatActor::from_wire(&message.sender),
            kind: message.kind,
            body: message.body,
        }
    }
}

/// What the surface is currently able to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatStatus {
    /// The first load has not answered yet.
    Loading,
    /// The daemon answered; the timeline is live.
    Live,
    /// The daemon could not answer; the detail is the operator's explanation.
    Unavailable(String),
}

/// Which half of the surface the keyboard is driving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatFocus {
    /// Typing a message.
    Composer,
    /// Selecting and answering confirm cards.
    Cards,
}

/// How many confirm cards fit in the rendered block.
///
/// The cursor is windowed to this, never clamped past it: a selection walking
/// off the painted region is a `y` press against a destructive tool call the
/// operator never saw.
pub const CARDS_VISIBLE: usize = 4;

/// The argument editor, ANCHORED to the card it was opened on.
///
/// The anchor is the `confirm_id`, never the cursor position. A poll can drop,
/// answer or expire the card under the operator mid-edit, and an index-anchored
/// editor then submits the operator's typed arguments against whichever card
/// slid into that slot — a different tool, approved without being read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CardEdit {
    confirm_id: String,
    buffer: String,
}

/// A page of chat state fetched by the host.
///
/// `confirms` stays raw so the tolerant decode happens in ONE place
/// ([`ChatConfirmCard::decode`]) rather than at every call site.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatSnapshot {
    /// The copilot channel's minted scope, as the daemon reported it.
    ///
    /// The host resolves this through `fleet/channel_list`; the surface never
    /// guesses it. `None` means the daemon has no copilot channel yet.
    pub scope_key: Option<String>,
    /// The copilot session messages are addressed to.
    pub target_session_key: Option<String>,
    /// Timeline rows in ascending commit order.
    pub messages: Vec<FleetMessage>,
    /// Confirm card frames, undecoded.
    pub confirms: Vec<serde_json::Value>,
    /// Why the confirm feed is empty, when it is empty for a reason.
    pub confirms_detail: Option<String>,
    /// Why there is no copilot session, in the daemon's own words.
    ///
    /// The daemon's refusals here are specific and actionable (`scope_key` ...
    /// is already held by a session whose cwd is X"), so swallowing them leaves
    /// the operator staring at a generic "not started" forever.
    pub session_detail: Option<String>,
    /// Copilot activity rows in ascending commit order.
    pub activity: Vec<FleetActivityRow>,
}

/// The Fleet chat surface's whole state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatState {
    scope_key: Option<String>,
    target_session_key: Option<String>,
    messages: Vec<ChatMessageRow>,
    confirms: Vec<ChatConfirmCard>,
    confirms_detail: Option<String>,
    session_detail: Option<String>,
    activity: Vec<FleetActivityRow>,
    composer: String,
    edit: Option<CardEdit>,
    focus: ChatFocus,
    /// The selected card's IDENTITY, never its index.
    ///
    /// The list is replaced wholesale by a poll every second, so an index means
    /// a different card from one frame to the next. An id that is no longer in
    /// the list selects NOTHING (no cursor is painted and every answer key
    /// refuses out loud) until the operator picks again, which is the
    /// fail-closed direction on a surface whose whole job is a human reading a
    /// destructive tool call before approving it.
    selected_id: Option<String>,
    status: ChatStatus,
    feedback: Option<String>,
    in_flight: bool,
    last_poll_ms: i64,
}

impl ChatState {
    /// A fresh surface bound to the copilot channel, nothing loaded yet.
    #[must_use]
    pub fn opening() -> Self {
        Self {
            scope_key: None,
            target_session_key: None,
            messages: Vec::new(),
            confirms: Vec::new(),
            confirms_detail: None,
            session_detail: None,
            activity: Vec::new(),
            composer: String::new(),
            edit: None,
            focus: ChatFocus::Composer,
            selected_id: None,
            status: ChatStatus::Loading,
            feedback: None,
            in_flight: true,
            last_poll_ms: 0,
        }
    }

    /// The scope this surface reads and writes, once the daemon has named it.
    #[must_use]
    pub fn scope_key(&self) -> Option<&str> {
        self.scope_key.as_deref()
    }

    /// The copilot session the composer addresses, once resolved.
    #[must_use]
    pub fn target_session_key(&self) -> Option<&str> {
        self.target_session_key.as_deref()
    }

    /// Timeline rows, oldest first.
    #[must_use]
    pub fn messages(&self) -> &[ChatMessageRow] {
        &self.messages
    }

    /// Confirm cards, oldest first.
    #[must_use]
    pub fn confirms(&self) -> &[ChatConfirmCard] {
        &self.confirms
    }

    /// Copilot activity rows, oldest first.
    #[must_use]
    pub fn activity(&self) -> &[FleetActivityRow] {
        &self.activity
    }

    /// The composer's current text.
    #[must_use]
    pub fn composer(&self) -> &str {
        &self.composer
    }

    /// Which half of the surface has the keyboard.
    #[must_use]
    pub const fn focus(&self) -> ChatFocus {
        self.focus
    }

    /// Where the selected card currently sits, when it is still listed.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        let selected = self.selected_id.as_deref()?;
        self.confirms.iter().position(|card| card.confirm_id() == selected)
    }

    /// The selected card, resolved BY ID against the current list.
    #[must_use]
    pub fn selected_card(&self) -> Option<&ChatConfirmCard> {
        self.confirms.get(self.selected_index()?)
    }

    /// Select the card at `index`, recording its identity.
    fn select(&mut self, index: usize) {
        if let Some(card) = self.confirms.get(index) {
            self.selected_id = Some(card.confirm_id().to_string());
        }
    }

    /// What the surface can currently show.
    #[must_use]
    pub const fn status(&self) -> &ChatStatus {
        &self.status
    }

    /// The last thing the surface has to say to the operator.
    #[must_use]
    pub fn feedback(&self) -> Option<&str> {
        self.feedback.as_deref()
    }

    /// Whether the composer (or the argument editor) is swallowing text.
    #[must_use]
    pub const fn is_capturing_text(&self) -> bool {
        self.edit.is_some() || matches!(self.focus, ChatFocus::Composer)
    }

    /// Fold one fetched page in.
    pub fn apply_snapshot(&mut self, snapshot: ChatSnapshot) {
        self.in_flight = false;
        self.status = ChatStatus::Live;
        if snapshot.scope_key.is_some() {
            self.scope_key = snapshot.scope_key;
        }
        if snapshot.target_session_key.is_some() {
            self.target_session_key = snapshot.target_session_key;
        }
        let skip = snapshot.messages.len().saturating_sub(CHAT_TIMELINE_MAX);
        self.messages =
            snapshot.messages.into_iter().skip(skip).map(ChatMessageRow::from).collect();
        self.confirms = snapshot.confirms.iter().map(ChatConfirmCard::decode).collect();
        self.confirms_detail = snapshot.confirms_detail;
        self.session_detail = snapshot.session_detail;
        self.activity = snapshot.activity;
        // Selection survives a refresh ONLY by identity. A selected card that
        // this page no longer lists (answered from the CLI, from the macOS
        // client, or expired by the daemon) leaves the anchor dangling on
        // purpose: nothing is selected, nothing is answerable, and the operator
        // picks again. Adopting the row that slid into its index is precisely
        // the blind approve this surface must not have.
        if self.selected_id.is_none() {
            self.selected_id = self.confirms.first().map(|card| card.confirm_id().to_string());
        }
    }

    /// Record that the host's fetch failed.
    pub fn apply_failure(&mut self, detail: String) {
        self.in_flight = false;
        self.status = ChatStatus::Unavailable(detail);
    }

    /// Record that a send failed, keeping the operator's text.
    pub fn apply_send_failure(&mut self, detail: String) {
        self.in_flight = false;
        self.feedback = Some(format!("send failed: {detail}"));
    }
}

/// A side effect the chat surface needs the host to perform.
///
/// Kept separate from the pane's [`super::fleet::FleetIntent`] so the chat's
/// own tests can assert on it without building a whole pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatIntent {
    /// Resolve the copilot session and page the channel: `fleet/acp_session_create`
    /// (idempotent per live scope), then `fleet/message_list`, then the confirm
    /// and activity feeds once their dispatch arms exist.
    Refresh {
        /// The scope to page, when this surface already knows it. `None` asks
        /// the host to resolve the copilot channel first: only the daemon knows
        /// the minted `channel:<ulid>`.
        scope_key: Option<String>,
    },
    /// Post an operator message: `fleet/message_send`.
    ///
    /// No `actor` rides this: an operator send omits the key, which is exactly
    /// what the daemon defaults to. A copilot write is the daemon's own MCP
    /// path and never originates here.
    Send {
        /// The channel scope the row is filed under.
        scope_key: String,
        /// The copilot session the message is delivered to.
        target_session_key: String,
        /// The body.
        text: String,
        /// Replay identity for the send.
        request_id: String,
    },
    /// Answer a guardrail confirm card: `fleet/confirm_answer`.
    ConfirmAnswer(FleetConfirmAnswerParams),
}

/// What one key press did to the surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatKeyOutcome {
    /// Nothing the host has to act on.
    Handled,
    /// Leave the chat surface.
    Close,
    /// Perform this effect.
    Intent(ChatIntent),
}

/// The key vocabulary the chat surface consumes.
///
/// Mirrors [`super::fleet::FleetKey`] rather than importing it so this module
/// stays independently testable; the pane translates at the one call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatKey {
    /// A printable character.
    Char(char),
    /// Submit.
    Enter,
    /// Switch focus.
    Tab,
    /// Leave, or cancel the argument editor.
    Esc,
    /// Delete the character before the cursor.
    Backspace,
    /// Previous card.
    Up,
    /// Next card.
    Down,
    /// A literal space.
    Space,
}

/// Fold one key into the chat surface.
pub fn reduce_chat_key(state: &mut ChatState, key: ChatKey) -> ChatKeyOutcome {
    state.feedback = None;
    if state.edit.is_some() {
        return reduce_edit_key(state, key);
    }
    match state.focus {
        ChatFocus::Composer => reduce_composer_key(state, key),
        ChatFocus::Cards => reduce_cards_key(state, key),
    }
}

fn reduce_composer_key(state: &mut ChatState, key: ChatKey) -> ChatKeyOutcome {
    match key {
        ChatKey::Esc => ChatKeyOutcome::Close,
        ChatKey::Tab => {
            state.focus = ChatFocus::Cards;
            ChatKeyOutcome::Handled
        }
        ChatKey::Backspace => {
            state.composer.pop();
            ChatKeyOutcome::Handled
        }
        ChatKey::Char(character) => {
            state.composer.push(character);
            ChatKeyOutcome::Handled
        }
        ChatKey::Space => {
            state.composer.push(' ');
            ChatKeyOutcome::Handled
        }
        ChatKey::Enter => submit_composer(state),
        ChatKey::Up | ChatKey::Down => ChatKeyOutcome::Handled,
    }
}

fn submit_composer(state: &mut ChatState) -> ChatKeyOutcome {
    let text = state.composer.trim().to_string();
    if text.is_empty() {
        return ChatKeyOutcome::Handled;
    }
    let Some(target) = state.target_session_key.clone() else {
        // Refusing beats sending into a scope with no recipient: the daemon
        // requires at least one target and would reject the frame anyway, and
        // the operator deserves to know the copilot session is not up yet.
        state.feedback = Some("no copilot session yet, nothing to send to".into());
        return ChatKeyOutcome::Handled;
    };
    let Some(scope_key) = state.scope_key.clone() else {
        state.feedback = Some("no copilot channel yet, nothing to send to".into());
        return ChatKeyOutcome::Handled;
    };
    // UNIQUE PER COMPOSITION, not per screen-open. `insert_message_with_deliveries`
    // keys idempotency on this id: a counter restarting at 1 on every visit to
    // the chat means the second visit either replays the first visit's message
    // silently (same text) or is hard-rejected as a fingerprint mismatch
    // (different text), and in both cases the composer clears and the copilot
    // never hears it.
    let request_id = format!("fleet-chat-{scope_key}-{}", uuid::Uuid::new_v4());
    state.composer.clear();
    state.in_flight = true;
    ChatKeyOutcome::Intent(ChatIntent::Send {
        scope_key,
        target_session_key: target,
        text,
        request_id,
    })
}

fn reduce_cards_key(state: &mut ChatState, key: ChatKey) -> ChatKeyOutcome {
    match key {
        ChatKey::Esc | ChatKey::Tab => {
            state.focus = ChatFocus::Composer;
            ChatKeyOutcome::Handled
        }
        // Movement resolves the CURRENT index first, so a card that vanished
        // under the operator is re-selected deliberately rather than inherited.
        ChatKey::Up | ChatKey::Char('k') => {
            let next = state.selected_index().map_or(0, |index| index.saturating_sub(1));
            state.select(next);
            ChatKeyOutcome::Handled
        }
        ChatKey::Down | ChatKey::Char('j') => {
            let last = state.confirms.len().saturating_sub(1);
            let next = state.selected_index().map_or(0, |index| index.saturating_add(1).min(last));
            state.select(next);
            ChatKeyOutcome::Handled
        }
        ChatKey::Char('y') => answer_selected(state, FleetConfirmAnswer::Approve),
        ChatKey::Char('n') => answer_selected(state, FleetConfirmAnswer::Deny),
        ChatKey::Char('e') => begin_edit(state),
        _ => ChatKeyOutcome::Handled,
    }
}

/// Answer the selected card, or refuse and say why.
///
/// The answerability check happens HERE and not only in the renderer: a hint
/// the row does not advertise is still a key an operator can press, and a card
/// this build does not understand must not be answerable through either door.
fn answer_selected(state: &mut ChatState, answer: FleetConfirmAnswer) -> ChatKeyOutcome {
    let Some(index) = state.selected_index() else {
        state.feedback = Some(nothing_selected(state));
        return ChatKeyOutcome::Handled;
    };
    let card = &state.confirms[index];
    if !card.is_answerable() {
        let refusal = card.refusal();
        state.feedback = Some(refusal);
        return ChatKeyOutcome::Handled;
    }
    let confirm_id = card.confirm_id().to_string();
    state.in_flight = true;
    ChatKeyOutcome::Intent(ChatIntent::ConfirmAnswer(FleetConfirmAnswerParams {
        confirm_id,
        answer,
    }))
}

/// Why no card answered a key: never selected, or selected and since gone.
fn nothing_selected(state: &ChatState) -> String {
    match state.selected_id.as_deref() {
        Some(confirm_id) if !state.confirms.is_empty() => {
            format!("{confirm_id} is no longer listed; pick a card with ↑↓ before answering")
        }
        _ => "no confirm card selected".to_string(),
    }
}

fn begin_edit(state: &mut ChatState) -> ChatKeyOutcome {
    let Some(index) = state.selected_index() else {
        state.feedback = Some(nothing_selected(state));
        return ChatKeyOutcome::Handled;
    };
    let card = &state.confirms[index];
    if !card.is_answerable() {
        let refusal = card.refusal();
        state.feedback = Some(refusal);
        return ChatKeyOutcome::Handled;
    }
    // Anchored to the card's ID at the moment the editor opens: the answer this
    // buffer becomes is only ever sent to THIS card.
    state.edit = Some(CardEdit {
        confirm_id: card.confirm_id().to_string(),
        buffer: card.arguments_line(),
    });
    ChatKeyOutcome::Handled
}

fn reduce_edit_key(state: &mut ChatState, key: ChatKey) -> ChatKeyOutcome {
    let Some(edit) = state.edit.as_mut() else {
        return ChatKeyOutcome::Handled;
    };
    match key {
        ChatKey::Esc => {
            state.edit = None;
            ChatKeyOutcome::Handled
        }
        ChatKey::Backspace => {
            edit.buffer.pop();
            ChatKeyOutcome::Handled
        }
        ChatKey::Char(character) => {
            edit.buffer.push(character);
            ChatKeyOutcome::Handled
        }
        ChatKey::Space => {
            edit.buffer.push(' ');
            ChatKeyOutcome::Handled
        }
        ChatKey::Enter => submit_edit(state),
        ChatKey::Tab | ChatKey::Up | ChatKey::Down => ChatKeyOutcome::Handled,
    }
}

fn submit_edit(state: &mut ChatState) -> ChatKeyOutcome {
    let Some(edit) = state.edit.clone() else {
        return ChatKeyOutcome::Handled;
    };
    let arguments = match serde_json::from_str::<serde_json::Value>(&edit.buffer) {
        Ok(value) if value.is_object() => value,
        Ok(_) => {
            state.feedback = Some("edited arguments must be a JSON object".into());
            return ChatKeyOutcome::Handled;
        }
        Err(error) => {
            state.feedback = Some(format!("edited arguments are not valid JSON: {error}"));
            return ChatKeyOutcome::Handled;
        }
    };
    // Looked up BY THE ANCHORED ID, never by the cursor: a poll between opening
    // the editor and pressing Enter can drop, answer or expire this card, and
    // the row that took its place is a different tool call the operator has not
    // read. Re-checked at submit as well as at `e` for the same reason.
    let Some(card) = state.confirms.iter().find(|card| card.confirm_id() == edit.confirm_id) else {
        state.edit = None;
        state.feedback = Some(format!(
            "{} is no longer listed; the edited answer was not sent",
            edit.confirm_id
        ));
        return ChatKeyOutcome::Handled;
    };
    if !card.is_answerable() {
        let refusal = card.refusal();
        state.edit = None;
        state.feedback = Some(refusal);
        return ChatKeyOutcome::Handled;
    }
    let confirm_id = card.confirm_id().to_string();
    state.edit = None;
    state.in_flight = true;
    ChatKeyOutcome::Intent(ChatIntent::ConfirmAnswer(FleetConfirmAnswerParams {
        confirm_id,
        answer: FleetConfirmAnswer::Edit { arguments },
    }))
}

/// Emit a refresh when one is due and none is in flight.
///
/// The in-flight latch is what keeps this off the render loop's back: the pane
/// ticks on every frame, and without it every frame would spawn a fetch.
pub fn chat_tick(state: &mut ChatState, now_ms: i64) -> Option<ChatIntent> {
    if state.in_flight && state.last_poll_ms != 0 {
        return None;
    }
    if state.last_poll_ms != 0 && now_ms.saturating_sub(state.last_poll_ms) < CHAT_POLL_INTERVAL_MS
    {
        return None;
    }
    state.last_poll_ms = now_ms;
    state.in_flight = true;
    Some(ChatIntent::Refresh {
        scope_key: state.scope_key.clone(),
    })
}

/// Paint the chat surface over the whole pane area.
pub fn render_chat(
    buffer: &mut WireBuffer,
    area_width: u16,
    top: u16,
    bottom: u16,
    state: &ChatState,
) {
    if area_width < 24 || bottom <= top + 3 {
        return;
    }
    fill_background(buffer, 0, top, area_width, bottom, SURFACE);
    let right = area_width;
    let mut row = top;
    put_str_styled(
        buffer,
        1,
        row,
        &format!(
            "Fleet chat · #copilot · {}",
            state.scope_key.as_deref().unwrap_or("resolving channel")
        ),
        GOLD,
        Some(SURFACE),
        1,
        right,
    );
    row = row.saturating_add(1);
    let (status_text, status_color) = match &state.status {
        ChatStatus::Loading => ("LOADING".to_string(), MUTED),
        ChatStatus::Live => (
            format!(
                "LIVE · {} messages · copilot {}",
                state.messages.len(),
                // The daemon's own refusal, not a generic "not started": its
                // wording ("scope_key ... is already held by a session whose
                // cwd is X") is the only thing that tells the operator the TUI
                // was launched from the wrong directory.
                state.target_session_key.as_deref().map_or_else(
                    || {
                        state.session_detail.as_ref().map_or_else(
                            || "not started".to_string(),
                            |detail| format!("not started: {detail}"),
                        )
                    },
                    ToString::to_string,
                )
            ),
            GREEN,
        ),
        ChatStatus::Unavailable(detail) => (format!("UNAVAILABLE · {detail}"), ALERT),
    };
    put_str(buffer, 1, row, &status_text, status_color, right);
    row = row.saturating_add(2);

    // Reserve the bottom for the composer, its help line, and the feedback row.
    let composer_row = bottom.saturating_sub(3);
    let help_row = bottom.saturating_sub(2);
    let feedback_row = bottom.saturating_sub(1);
    let cards_height = cards_block_height(state);
    let timeline_bottom = composer_row.saturating_sub(cards_height).saturating_sub(1);

    row = render_timeline(buffer, row, timeline_bottom, right, state);
    let _ = row;
    render_cards(
        buffer,
        timeline_bottom.saturating_add(1),
        composer_row,
        right,
        state,
    );

    let composer = state.edit.as_ref().map_or_else(
        || format!("> {}", state.composer),
        |edit| format!("args[{}]> {}", edit.confirm_id, edit.buffer),
    );
    let composer_color = if state.edit.is_some() || state.focus == ChatFocus::Composer {
        FG
    } else {
        MUTED
    };
    put_str(
        buffer,
        1,
        composer_row,
        &truncate_ellipsis(&composer, usize::from(right.saturating_sub(2))),
        composer_color,
        right,
    );
    let help = if state.edit.is_some() {
        "Enter answers with the edited arguments · Esc cancels the edit"
    } else {
        match state.focus {
            ChatFocus::Composer => "Enter sends · Tab confirm cards · Esc back to the Fleet panel",
            ChatFocus::Cards => "↑↓ card · y approve · n deny · e answer · Tab composer",
        }
    };
    put_str(buffer, 1, help_row, help, MUTED, right);
    if let Some(feedback) = state.feedback.as_deref() {
        put_str(buffer, 1, feedback_row, feedback, GOLD, right);
    }
}

fn cards_block_height(state: &ChatState) -> u16 {
    let rows = state.confirms.len().min(CARDS_VISIBLE) as u16;
    let activity = state.activity.len().min(3) as u16;
    // header + cards + the "N more" row when any card is off the window +
    // activity header + activity rows, and a header even when the feed is empty
    // so "no open confirm cards" is a visible statement rather than an absence
    // the operator has to interpret.
    let overflow = u16::from(state.confirms.len() > CARDS_VISIBLE);
    2 + rows.max(1) + overflow + activity
}

fn render_timeline(
    buffer: &mut WireBuffer,
    top: u16,
    bottom: u16,
    right: u16,
    state: &ChatState,
) -> u16 {
    if bottom <= top {
        return top;
    }
    let capacity = usize::from(bottom.saturating_sub(top));
    let skip = state.messages.len().saturating_sub(capacity);
    let mut row = top;
    if state.messages.is_empty() {
        put_str(
            buffer,
            1,
            row,
            "no messages yet, type below to ask the copilot",
            MUTED,
            right,
        );
        return row.saturating_add(1);
    }
    for message in state.messages.iter().skip(skip) {
        if row >= bottom {
            break;
        }
        // The attribution column is FIXED WIDTH and followed by `│`, so a test
        // can anchor on the row it means instead of a substring that could
        // match anywhere in the pane.
        let label = message.actor.label();
        put_str_styled(
            buffer,
            1,
            row,
            &format!("{label:<12}"),
            message.actor.color(),
            Some(SURFACE),
            1,
            right,
        );
        put_str(buffer, 13, row, "│ ", MUTED, right);
        let body = format!("{}{}", message_kind_label(message.kind), message.body);
        put_str(
            buffer,
            15,
            row,
            &truncate_ellipsis(&body, usize::from(right.saturating_sub(16))),
            FG,
            right,
        );
        row = row.saturating_add(1);
    }
    row
}

/// The first card of the rendered window, chosen so the selection is always in
/// it (and is the LAST row of it once the list is longer than the window).
fn cards_window_start(state: &ChatState) -> usize {
    state.selected_index().map_or(0, |index| {
        index.saturating_sub(CARDS_VISIBLE.saturating_sub(1))
    })
}

fn render_cards(buffer: &mut WireBuffer, top: u16, bottom: u16, right: u16, state: &ChatState) {
    if bottom <= top {
        return;
    }
    let mut row = top;
    let header = match (state.confirms.len(), state.confirms_detail.as_deref()) {
        (0, Some(detail)) => format!("CONFIRM CARDS · {detail}"),
        (0, None) => "CONFIRM CARDS · none open".to_string(),
        (count, _) => format!("CONFIRM CARDS · {count}"),
    };
    put_str_styled(buffer, 1, row, &header, BLUE, Some(SURFACE), 1, right);
    row = row.saturating_add(1);
    // The window FOLLOWS the selection. Painting a fixed first four while the
    // cursor walks past them is a `y` against a destructive card that was never
    // on screen, with no ▶ anywhere to say so.
    let window_start = cards_window_start(state);
    let selected_index = state.selected_index();
    for (offset, card) in state.confirms.iter().skip(window_start).take(CARDS_VISIBLE).enumerate() {
        if row >= bottom {
            return;
        }
        let index = window_start.saturating_add(offset);
        let selected = selected_index == Some(index) && state.focus == ChatFocus::Cards;
        let cursor = if selected { '▶' } else { ' ' };
        let answer_hint = if card.is_answerable() {
            "y approve  n deny  e answer"
        } else {
            "not answerable"
        };
        let line = format!(
            "{cursor} [{}] {}  {}  {}",
            card.state_label(),
            card.tool(),
            truncate_ellipsis(&card.arguments_line(), 40),
            answer_hint
        );
        let color = if card.is_answerable() { FG } else { MUTED };
        put_str(
            buffer,
            1,
            row,
            &truncate_ellipsis(&line, usize::from(right.saturating_sub(2))),
            color,
            right,
        );
        row = row.saturating_add(1);
    }
    // A hidden card is an absence, and an absence is unreadable. Say how many.
    let hidden = state.confirms.len().saturating_sub(CARDS_VISIBLE);
    if hidden > 0 && row < bottom {
        put_str(
            buffer,
            1,
            row,
            &format!("  … {hidden} more card(s) not shown · ↑↓ to reach them"),
            MUTED,
            right,
        );
        row = row.saturating_add(1);
    }
    if state.confirms.is_empty() {
        row = row.saturating_add(1);
    }
    render_activity(buffer, row, bottom, right, state);
}

fn render_activity(buffer: &mut WireBuffer, top: u16, bottom: u16, right: u16, state: &ChatState) {
    let mut row = top;
    if state.activity.is_empty() || row >= bottom {
        return;
    }
    put_str_styled(
        buffer,
        1,
        row,
        "COPILOT ACTIVITY",
        BLUE,
        Some(SURFACE),
        1,
        right,
    );
    row = row.saturating_add(1);
    let skip = state.activity.len().saturating_sub(3);
    for activity in state.activity.iter().skip(skip) {
        if row >= bottom {
            return;
        }
        put_str_styled(
            buffer,
            1,
            row,
            &format!("{:<12}", activity_class_label(activity.class)),
            activity_class_color(activity.class),
            Some(SURFACE),
            0,
            right,
        );
        let line = format!(
            "{} {} {}",
            activity.tool,
            activity.target_session_key.as_deref().unwrap_or("-"),
            activity_outcome_label(activity.outcome)
        );
        put_str(
            buffer,
            14,
            row,
            &truncate_ellipsis(&line, usize::from(right.saturating_sub(15))),
            FG,
            right,
        );
        row = row.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scope shaped like the one a real daemon MINTS, never a literal
    /// `channel:copilot`: the surface learns its scope from `channel_list`.
    const MINTED_SCOPE: &str = "channel:01J0CHANNEL";

    fn message(sender: &str, body: &str) -> FleetMessage {
        FleetMessage {
            id: format!("01J0{sender}"),
            scope_key: MINTED_SCOPE.to_string(),
            origin_message_id: None,
            sender: sender.to_string(),
            kind: FleetMessageKind::User,
            body: body.to_string(),
            created_at: 1_700_000_000_000,
        }
    }

    fn confirm(state: FleetConfirmState) -> serde_json::Value {
        card("01J0CONFIRM", "kill", state)
    }

    fn card(confirm_id: &str, tool: &str, state: FleetConfirmState) -> serde_json::Value {
        serde_json::json!({
            "confirm_id": confirm_id,
            "scope_key": MINTED_SCOPE,
            "tool": tool,
            "arguments": { "session": "claude:one" },
            "target_session_key": "claude:one",
            "state": match state {
                FleetConfirmState::Open => "open",
                FleetConfirmState::Approved => "approved",
                FleetConfirmState::Denied => "denied",
                FleetConfirmState::Expired => "expired",
            },
            "created_at": 1_700_000_000_000_i64,
            "expires_at": 1_700_000_600_000_i64,
        })
    }

    fn loaded(confirms: Vec<serde_json::Value>) -> ChatState {
        let mut state = ChatState::opening();
        state.apply_snapshot(ChatSnapshot {
            scope_key: Some(MINTED_SCOPE.into()),
            target_session_key: Some("acp:01J0COPILOT".into()),
            messages: vec![message("operator", "what is blocked?")],
            confirms,
            confirms_detail: None,
            session_detail: None,
            activity: Vec::new(),
        });
        state
    }

    /// The panel maps `sender` TWICE: the wire token becomes a [`ChatActor`],
    /// and the actor becomes the label an operator reads. `acp` shipped with
    /// only the first half of exactly that shape and rendered as UNKNOWN on the
    /// one screen anybody looks at. This walks every actor the wire can carry
    /// through BOTH halves, and a new [`ChatActor`] variant fails to compile.
    #[test]
    fn every_wire_actor_renders_a_label_operators_can_tell_apart() {
        fn is_the_human(actor: &ChatActor) -> bool {
            match actor {
                ChatActor::Operator => true,
                ChatActor::Copilot | ChatActor::Session(_) | ChatActor::Unattributed => false,
            }
        }

        let mut labels = std::collections::BTreeSet::new();
        for sender in ["operator", "copilot", "claude:session-one", "", "   "] {
            let actor = ChatActor::from_wire(sender);
            let label = actor.label();
            assert!(!label.is_empty(), "{sender:?} renders as an empty label");
            assert_eq!(
                is_the_human(&actor),
                sender.trim() == "operator",
                "{sender:?} became {actor:?}, which reads as the operator"
            );
            labels.insert(label);
        }
        // A copilot write must not be able to wear the operator's name: the
        // whole point of the wire's `actor` field dies if the two paint the same.
        assert!(
            labels.contains("YOU") && labels.contains("COPILOT"),
            "operator and copilot rows are not distinguishable: {labels:?}"
        );
        assert_ne!(
            ChatActor::Operator.color(),
            ChatActor::Copilot.color(),
            "operator and copilot rows share a colour"
        );
        assert_ne!(
            ChatActor::Operator.label(),
            ChatActor::Unattributed.label(),
            "a blank sender renders as the operator"
        );
    }

    /// Every confirm lifecycle state gets a label, and exactly one of them is
    /// answerable. Exhaustive so a new state has to be classified here.
    #[test]
    fn every_confirm_state_is_labelled_and_only_open_is_answerable() {
        for state in [
            FleetConfirmState::Open,
            FleetConfirmState::Approved,
            FleetConfirmState::Denied,
            FleetConfirmState::Expired,
        ] {
            assert!(!confirm_state_label(state).is_empty());
            assert_eq!(
                confirm_state_is_answerable(state),
                state == FleetConfirmState::Open,
                "{state:?} answerability is wrong"
            );
        }
    }

    /// Every activity class and outcome gets a label; a new variant is a
    /// compile error in the mapping rather than a blank column.
    #[test]
    fn every_activity_class_and_outcome_renders_a_label() {
        for class in [
            FleetActivityClass::Read,
            FleetActivityClass::Write,
            FleetActivityClass::Destructive,
        ] {
            assert!(!activity_class_label(class).is_empty());
        }
        for outcome in [
            FleetActivityOutcome::Ok,
            FleetActivityOutcome::Denied,
            FleetActivityOutcome::Expired,
            FleetActivityOutcome::Error,
        ] {
            assert!(!activity_outcome_label(outcome).is_empty());
        }
    }

    /// A card in a state this build does not know decodes as UNRECOGNISED, is
    /// NOT answerable, and refuses every answer key. The rest of the page still
    /// decodes: one unknown token must not blank the operator's whole feed.
    #[test]
    fn an_unknown_confirm_state_is_rendered_but_never_answerable() {
        let mut unknown = confirm(FleetConfirmState::Open);
        unknown["state"] = serde_json::json!("escalated");
        unknown["confirm_id"] = serde_json::json!("01J0FUTURE");
        let mut state = loaded(vec![unknown, confirm(FleetConfirmState::Open)]);

        assert_eq!(state.confirms().len(), 2, "the known card was lost too");
        let card = &state.confirms()[0];
        assert_eq!(card.state_label(), "UNRECOGNISED");
        assert!(!card.is_answerable());

        state.focus = ChatFocus::Cards;
        state.selected_id = Some("01J0FUTURE".into());
        for key in [ChatKey::Char('y'), ChatKey::Char('n'), ChatKey::Char('e')] {
            let outcome = reduce_chat_key(&mut state, key);
            assert_eq!(
                outcome,
                ChatKeyOutcome::Handled,
                "{key:?} answered a card this build does not understand"
            );
            assert!(
                state.feedback().is_some_and(|f| f.contains("not answerable")),
                "{key:?} refused silently: {:?}",
                state.feedback()
            );
        }
        // The known card next to it still answers.
        state.selected_id = Some("01J0CONFIRM".into());
        let outcome = reduce_chat_key(&mut state, ChatKey::Char('y'));
        assert!(matches!(
            outcome,
            ChatKeyOutcome::Intent(ChatIntent::ConfirmAnswer(_))
        ));
    }

    #[test]
    fn an_answered_card_is_no_longer_answerable() {
        let mut state = loaded(vec![confirm(FleetConfirmState::Approved)]);
        state.focus = ChatFocus::Cards;
        let outcome = reduce_chat_key(&mut state, ChatKey::Char('y'));
        assert_eq!(outcome, ChatKeyOutcome::Handled);
        assert!(state.feedback().is_some_and(|f| f.contains("APPROVED")));
    }

    #[test]
    fn approve_and_deny_route_through_confirm_answer_params() {
        let mut state = loaded(vec![confirm(FleetConfirmState::Open)]);
        state.focus = ChatFocus::Cards;
        let approve = reduce_chat_key(&mut state, ChatKey::Char('y'));
        assert_eq!(
            approve,
            ChatKeyOutcome::Intent(ChatIntent::ConfirmAnswer(FleetConfirmAnswerParams {
                confirm_id: "01J0CONFIRM".into(),
                answer: FleetConfirmAnswer::Approve,
            }))
        );
        let deny = reduce_chat_key(&mut state, ChatKey::Char('n'));
        assert_eq!(
            deny,
            ChatKeyOutcome::Intent(ChatIntent::ConfirmAnswer(FleetConfirmAnswerParams {
                confirm_id: "01J0CONFIRM".into(),
                answer: FleetConfirmAnswer::Deny,
            }))
        );
    }

    #[test]
    fn answering_with_edited_arguments_requires_a_json_object() {
        let mut state = loaded(vec![confirm(FleetConfirmState::Open)]);
        state.focus = ChatFocus::Cards;
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Char('e')),
            ChatKeyOutcome::Handled
        );
        let editing = state.edit.clone().expect("the editor is open");
        assert_eq!(editing.buffer, r#"{"session":"claude:one"}"#);
        assert_eq!(
            editing.confirm_id, "01J0CONFIRM",
            "the editor is anchored to the card it was opened on"
        );

        // Garbage stays in the editor and says so.
        state.edit = Some(CardEdit {
            confirm_id: editing.confirm_id.clone(),
            buffer: "not json".into(),
        });
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Enter),
            ChatKeyOutcome::Handled
        );
        assert!(state.feedback().is_some_and(|f| f.contains("valid JSON")));

        state.edit = Some(CardEdit {
            confirm_id: editing.confirm_id,
            buffer: r#"{"session":"claude:two"}"#.into(),
        });
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Enter),
            ChatKeyOutcome::Intent(ChatIntent::ConfirmAnswer(FleetConfirmAnswerParams {
                confirm_id: "01J0CONFIRM".into(),
                answer: FleetConfirmAnswer::Edit {
                    arguments: serde_json::json!({ "session": "claude:two" }),
                },
            }))
        );
        assert!(state.edit.is_none());
    }

    #[test]
    fn typing_and_sending_addresses_the_copilot_session() {
        let mut state = loaded(Vec::new());
        for key in [
            ChatKey::Char('h'),
            ChatKey::Char('i'),
            ChatKey::Space,
            ChatKey::Char('x'),
            ChatKey::Backspace,
        ] {
            assert_eq!(reduce_chat_key(&mut state, key), ChatKeyOutcome::Handled);
        }
        assert_eq!(state.composer(), "hi ");
        let ChatKeyOutcome::Intent(ChatIntent::Send {
            scope_key,
            target_session_key,
            text,
            request_id,
        }) = reduce_chat_key(&mut state, ChatKey::Enter)
        else {
            panic!("Enter did not send");
        };
        assert_eq!(scope_key, MINTED_SCOPE);
        assert_eq!(target_session_key, "acp:01J0COPILOT");
        assert_eq!(text, "hi");
        assert!(
            request_id.starts_with(&format!("fleet-chat-{MINTED_SCOPE}-")),
            "unexpected request id {request_id}"
        );
        assert_eq!(state.composer(), "", "composer keeps a sent message");
    }

    /// The send id is UNIQUE PER COMPOSITION, across screen opens.
    ///
    /// A counter reset by `ChatState::opening()` gives the second visit to the
    /// same channel the same ids as the first, and the daemon keys idempotency
    /// on them: the same text replays silently (composer clears, nothing
    /// delivered) and different text is refused as a fingerprint mismatch.
    #[test]
    fn two_composed_messages_never_share_a_request_id() {
        fn send(state: &mut ChatState, text: &str) -> String {
            for character in text.chars() {
                reduce_chat_key(state, ChatKey::Char(character));
            }
            match reduce_chat_key(state, ChatKey::Enter) {
                ChatKeyOutcome::Intent(ChatIntent::Send { request_id, .. }) => request_id,
                other => panic!("Enter did not send: {other:?}"),
            }
        }

        let mut first = loaded(Vec::new());
        let one = send(&mut first, "hello");
        let two = send(&mut first, "hello");
        // A FRESH screen on the same channel, which is what reopening the chat
        // builds: `m` always constructs `ChatState::opening()`.
        let mut reopened = loaded(Vec::new());
        let three = send(&mut reopened, "hello");
        assert_ne!(one, two, "two sends in one screen shared an id");
        assert_ne!(
            one, three,
            "reopening the chat replayed the first send's id"
        );
    }

    #[test]
    fn an_empty_composer_sends_nothing() {
        let mut state = loaded(Vec::new());
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Enter),
            ChatKeyOutcome::Handled
        );
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Space),
            ChatKeyOutcome::Handled
        );
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Enter),
            ChatKeyOutcome::Handled
        );
    }

    #[test]
    fn sending_before_the_copilot_session_exists_refuses_out_loud() {
        let mut state = ChatState::opening();
        state.apply_snapshot(ChatSnapshot::default());
        for key in [ChatKey::Char('h'), ChatKey::Char('i')] {
            reduce_chat_key(&mut state, key);
        }
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Enter),
            ChatKeyOutcome::Handled
        );
        assert!(state.feedback().is_some_and(|f| f.contains("no copilot session")));
        assert_eq!(
            state.composer(),
            "hi",
            "a refused send ate the operator's text"
        );
    }

    #[test]
    fn esc_closes_from_the_composer_and_only_leaves_cards_first() {
        let mut state = loaded(vec![confirm(FleetConfirmState::Open)]);
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Tab),
            ChatKeyOutcome::Handled
        );
        assert_eq!(state.focus(), ChatFocus::Cards);
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Esc),
            ChatKeyOutcome::Handled,
            "Esc from the cards left the whole surface"
        );
        assert_eq!(state.focus(), ChatFocus::Composer);
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Esc),
            ChatKeyOutcome::Close
        );
    }

    /// Every printable key belongs to the composer while it has focus, and `q`
    /// is the one that proves it: the Fleet panel's browse mode quits on `q`,
    /// and a chat composer that inherited that would be unusable.
    #[test]
    fn the_composer_swallows_keys_the_panel_binds() {
        let mut state = loaded(Vec::new());
        assert!(state.is_capturing_text());
        for key in ['q', 'y', 'n', 'j', 'k', 'b', '5'] {
            assert_eq!(
                reduce_chat_key(&mut state, ChatKey::Char(key)),
                ChatKeyOutcome::Handled
            );
        }
        assert_eq!(state.composer(), "qynjkb5");
    }

    #[test]
    fn the_poll_latch_emits_one_refresh_per_interval() {
        let mut state = ChatState::opening();
        state.apply_snapshot(ChatSnapshot::default());
        // The FIRST refresh carries no scope: only the daemon knows the minted
        // one, so the surface asks rather than guessing a literal.
        assert_eq!(
            chat_tick(&mut state, 1_000),
            Some(ChatIntent::Refresh { scope_key: None })
        );
        // In flight: every subsequent frame is silent until the fetch answers.
        assert_eq!(chat_tick(&mut state, 1_500), None);
        assert_eq!(chat_tick(&mut state, 5_000), None);
        state.apply_snapshot(ChatSnapshot::default());
        assert_eq!(
            chat_tick(&mut state, 1_500),
            None,
            "polled inside the interval"
        );
        assert!(chat_tick(&mut state, 2_000).is_some());
    }

    /// Once a daemon has NAMED the channel, every later poll pages that exact
    /// scope. The surface never re-derives it, which is the half of the
    /// double-mapping bug that lives on this side of the socket.
    #[test]
    fn a_resolved_scope_is_carried_on_every_later_poll() {
        let mut state = loaded(Vec::new());
        assert_eq!(state.scope_key(), Some(MINTED_SCOPE));
        assert_eq!(
            chat_tick(&mut state, 9_000),
            Some(ChatIntent::Refresh {
                scope_key: Some(MINTED_SCOPE.into())
            })
        );
    }

    #[test]
    fn sending_before_the_channel_is_resolved_refuses_out_loud() {
        let mut state = ChatState::opening();
        state.apply_snapshot(ChatSnapshot {
            // A copilot session with no channel yet: the send has a recipient
            // but nowhere to file the row, and inventing a scope here is
            // exactly the guess this surface refuses to make.
            target_session_key: Some("acp:01J0COPILOT".into()),
            ..ChatSnapshot::default()
        });
        for key in [ChatKey::Char('h'), ChatKey::Char('i')] {
            reduce_chat_key(&mut state, key);
        }
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Enter),
            ChatKeyOutcome::Handled
        );
        assert!(state.feedback().is_some_and(|f| f.contains("no copilot channel")));
        assert_eq!(state.composer(), "hi");
    }

    #[test]
    fn a_failed_fetch_says_so_instead_of_rendering_an_empty_channel() {
        let mut state = ChatState::opening();
        state.apply_failure("connection refused".into());
        assert_eq!(
            state.status(),
            &ChatStatus::Unavailable("connection refused".into())
        );
        // And the next tick retries rather than latching forever.
        assert!(chat_tick(&mut state, 10_000).is_some());
    }

    #[test]
    fn the_timeline_keeps_the_newest_rows_only() {
        let mut state = ChatState::opening();
        let messages = (0..CHAT_TIMELINE_MAX + 5)
            .map(|index| message("copilot", &format!("row {index}")))
            .collect();
        state.apply_snapshot(ChatSnapshot {
            scope_key: Some(MINTED_SCOPE.into()),
            target_session_key: Some("acp:01J0COPILOT".into()),
            messages,
            confirms: Vec::new(),
            confirms_detail: None,
            session_detail: None,
            activity: Vec::new(),
        });
        assert_eq!(state.messages().len(), CHAT_TIMELINE_MAX);
        assert_eq!(
            state.messages().last().map(|row| row.body.as_str()),
            Some(format!("row {}", CHAT_TIMELINE_MAX + 4).as_str())
        );
    }

    /// The rendered pane, read as text, is the thing the tripwire asserts on.
    #[test]
    fn the_rendered_pane_separates_the_operator_from_the_copilot() {
        let mut state = ChatState::opening();
        state.apply_snapshot(ChatSnapshot {
            scope_key: Some(MINTED_SCOPE.into()),
            target_session_key: Some("acp:01J0COPILOT".into()),
            messages: vec![
                message("operator", "what is blocked?"),
                message("copilot", "session one is waiting on you"),
            ],
            confirms: vec![confirm(FleetConfirmState::Open)],
            confirms_detail: None,
            session_detail: None,
            activity: vec![FleetActivityRow {
                seq: 1,
                id: "01J0ACT".into(),
                scope_key: MINTED_SCOPE.into(),
                tool: "send_prompt".into(),
                class: FleetActivityClass::Write,
                target_session_key: Some("claude:one".into()),
                outcome: FleetActivityOutcome::Ok,
                detail: None,
                created_at: 1_700_000_000_000,
            }],
        });
        let text = render_to_text(&state, 100, 30);
        let rows: Vec<&str> = text.lines().collect();
        assert!(
            rows.iter().any(|row| row.contains("Fleet chat · #copilot")),
            "no chat header:\n{text}"
        );
        let operator_row =
            rows.iter().find(|row| row.contains("what is blocked?")).expect("operator row");
        let copilot_row = rows
            .iter()
            .find(|row| row.contains("session one is waiting on you"))
            .expect("copilot row");
        assert!(
            operator_row.contains("YOU") && !operator_row.contains("COPILOT"),
            "operator row is not attributed: {operator_row}"
        );
        assert!(
            copilot_row.contains("COPILOT"),
            "copilot row is not attributed: {copilot_row}"
        );
        assert!(
            rows.iter().any(|row| row.contains("[OPEN]") && row.contains("y approve")),
            "open card is not answerable on screen:\n{text}"
        );
        assert!(
            rows.iter().any(|row| row.contains("WRITE") && row.contains("send_prompt")),
            "copilot activity is not on screen:\n{text}"
        );
    }

    #[test]
    fn a_card_this_build_cannot_read_advertises_no_answer_key() {
        let mut unknown = confirm(FleetConfirmState::Open);
        unknown["state"] = serde_json::json!("escalated");
        let state = loaded(vec![unknown]);
        let text = render_to_text(&state, 100, 30);
        let row = text
            .lines()
            .find(|row| row.contains("UNRECOGNISED"))
            .unwrap_or_else(|| panic!("no unrecognised row:\n{text}"))
            .to_string();
        assert!(
            row.contains("not answerable") && !row.contains("y approve"),
            "an undecodable card advertised an approve key: {row}"
        );
    }

    /// A refresh that drops the selected card must not hand its keys to the
    /// card that slid into its place.
    ///
    /// The poll replaces the list wholesale every second, and cards are
    /// answered from the CLI, the macOS client and the daemon's own TTL. An
    /// index-anchored selection turns that into `y` approving a destructive
    /// tool call the operator never read.
    #[test]
    fn a_card_answered_elsewhere_never_hands_its_keys_to_the_next_one() {
        let mut state = loaded(vec![
            card("CARD-KILL", "kill", FleetConfirmState::Open),
            card("CARD-RMRF", "spawn_session", FleetConfirmState::Open),
        ]);
        state.focus = ChatFocus::Cards;
        assert_eq!(
            state.selected_card().map(ChatConfirmCard::confirm_id),
            Some("CARD-KILL")
        );

        // The editor opens on CARD-KILL, and the operator starts typing.
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Char('e')),
            ChatKeyOutcome::Handled
        );
        // One poll later CARD-KILL is gone: somebody answered it elsewhere.
        state.apply_snapshot(ChatSnapshot {
            scope_key: Some(MINTED_SCOPE.into()),
            target_session_key: Some("acp:01J0COPILOT".into()),
            messages: Vec::new(),
            confirms: vec![card("CARD-RMRF", "spawn_session", FleetConfirmState::Open)],
            confirms_detail: None,
            session_detail: None,
            activity: Vec::new(),
        });

        // Enter must NOT answer CARD-RMRF with arguments written for CARD-KILL.
        let outcome = reduce_chat_key(&mut state, ChatKey::Enter);
        assert_eq!(
            outcome,
            ChatKeyOutcome::Handled,
            "the editor answered a card it was not opened on"
        );
        assert!(
            state.feedback().is_some_and(|f| f.contains("CARD-KILL")),
            "the refusal does not name the card that vanished: {:?}",
            state.feedback()
        );
        assert!(
            state.edit.is_none(),
            "the editor stayed open on a dead card"
        );

        // And the plain answer keys refuse too, until the operator selects.
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Char('y')),
            ChatKeyOutcome::Handled,
            "`y` approved a card the operator never selected"
        );
        assert!(state.feedback().is_some_and(|f| f.contains("no longer listed")));
        // Selecting deliberately re-arms them.
        reduce_chat_key(&mut state, ChatKey::Down);
        assert_eq!(
            reduce_chat_key(&mut state, ChatKey::Char('y')),
            ChatKeyOutcome::Intent(ChatIntent::ConfirmAnswer(FleetConfirmAnswerParams {
                confirm_id: "CARD-RMRF".into(),
                answer: FleetConfirmAnswer::Approve,
            }))
        );
    }

    /// When there is no copilot session, the DAEMON's reason is on screen.
    ///
    /// Its refusals are the actionable ones ("already held by a session whose
    /// cwd is X"); a surface that swallows them leaves the operator reading
    /// "not started" forever with no way to learn why.
    #[test]
    fn the_status_line_carries_the_daemons_reason_for_no_copilot_session() {
        let mut state = ChatState::opening();
        state.apply_snapshot(ChatSnapshot {
            scope_key: Some(MINTED_SCOPE.into()),
            target_session_key: None,
            session_detail: Some(
                "scope_key is already held by a session whose cwd is /work/api".into(),
            ),
            ..ChatSnapshot::default()
        });
        let text = render_to_text(&state, 120, 30);
        assert!(
            text.lines().any(|row| row.contains("cwd is /work/api")),
            "the daemon's refusal was discarded:\n{text}"
        );
    }

    /// The cursor never walks off the painted window.
    ///
    /// With more cards than rows, a cursor clamped to the LIST rather than the
    /// WINDOW leaves no `▶` anywhere on screen while `y` still answers, which
    /// is blind approval on the one surface built for reading before approving.
    #[test]
    fn the_selected_card_is_always_on_screen() {
        let cards: Vec<_> = (0..6)
            .map(|index| {
                card(
                    &format!("CARD{index}"),
                    &format!("tool{index}"),
                    FleetConfirmState::Open,
                )
            })
            .collect();
        let mut state = loaded(cards);
        state.focus = ChatFocus::Cards;
        for _ in 0..5 {
            reduce_chat_key(&mut state, ChatKey::Down);
        }
        assert_eq!(
            state.selected_card().map(ChatConfirmCard::confirm_id),
            Some("CARD5")
        );

        let text = render_to_text(&state, 100, 40);
        let selected_row = text
            .lines()
            .find(|row| row.contains('▶'))
            .unwrap_or_else(|| panic!("nothing is marked as selected:\n{text}"))
            .to_string();
        assert!(
            selected_row.contains("tool5"),
            "the cursor marks a card that is not the selected one: {selected_row}"
        );
        assert!(
            text.lines().any(|row| row.contains("more card(s) not shown")),
            "hidden cards are invisible as an absence:\n{text}"
        );
    }

    fn render_to_text(state: &ChatState, width: u16, height: u16) -> String {
        let mut buffer = WireBuffer::new(width, height);
        render_chat(&mut buffer, width, 0, height, state);
        let mut grid = vec![vec![' '; usize::from(width)]; usize::from(height)];
        for (coord, cell) in &buffer.cells {
            let Some(character) = cell.symbol.chars().next() else {
                continue;
            };
            if usize::from(coord.y) < grid.len() && usize::from(coord.x) < grid[0].len() {
                grid[usize::from(coord.y)][usize::from(coord.x)] = character;
            }
        }
        grid.into_iter()
            .map(|row| row.into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}
