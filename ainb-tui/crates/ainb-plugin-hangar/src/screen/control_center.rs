//! P2 — Control-center screen: the fleet-wide agentpeek "who-needs-you" board.
//!
//! The control center (hotkey `C`) is the converged control plane's answerable
//! inbox rendered as agentpeek session cards. It is fed entirely by the P2
//! attention RPCs — `attention/list` + `attention/subscribe` seed and refresh the
//! open [`AttentionRow`](ainb_hangar_proto::events::AttentionRow)s, and
//! `attention/answer` delivers the picked option back into the raising session.
//! The plugin owns **zero domain data**: every card here is a render shape pulled
//! over the socket; the daemon's `SQLite` store is the source of truth.
//!
//! ## The four agentpeek elements (D9)
//!
//! 1. **Session cards + live status line + auto-shuffle.** The left column is one
//!    card per open attention row, sorted so the sessions that need a decision
//!    float to the top ([`sort_cards`]) without ever stealing the keyboard
//!    selection ([`ControlCenterState::set_attention`] pins the selection by
//!    `attention_id`, not by index). Each card's status line is coloured by kind
//!    (amber "waiting for your input" for an ASK, red for an error, muted for an
//!    idle/waiting row). A per-card **stat strip** carries the age (live, derived
//!    from `created_at`), the source (hooked vs the degraded pane fallback), and
//!    the owning workspace / host tag.
//! 2. **LAST REPLY pane.** The detail column's top section renders the assistant's
//!    last reply / request context the attention payload carries (the IDLE row's
//!    `last_assistant_text`, the ASK's question, the ERR's snippet, the WAIT's
//!    marker text).
//! 3. **Tool-call TIMELINE.** Below the reply, the detail column renders the
//!    session's attention timeline — the raise event, its kind, and how long it
//!    has been waiting. (The richer per-tool-call JSONL timeline with individual
//!    durations is the P10 observability deliverable, spec §4.9; this region is
//!    its render seam.)
//! 4. **Inline ASK answering.** An ASK card's options render with circled-digit
//!    ①②③ glyphs; `Enter` answers the highlighted option and the number keys
//!    `1`..`9` answer directly, both raising [`ControlCenterIntent::Answer`] which
//!    the plugin glue turns into the one `attention/answer` RPC.
//!
//! The module is a **pure** reducer + width-aware renderer (no IO, no `tokio`),
//! mirroring [`super::inbox`] and [`super::issue_list`]: the plugin glue folds
//! forwarded keys through [`reduce_control_center`] and paints via
//! [`render_control_center`].

use ainb_hangar_proto::events::AttentionRow;
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Primary text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted secondary text (stat strip, hints, idle rows).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Selection / "running" green + the highlighted answer option.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Error red.
const ALERT_RED: Color = Color::rgb(220, 100, 100);
/// Waiting / ASK amber ("waiting for your input").
const WAIT_AMBER: Color = Color::rgb(220, 180, 90);
/// Info / panel cornflower blue.
const CORNFLOWER_BLUE: Color = Color::rgb(100, 149, 237);

/// The kind family an attention row belongs to, parsed from the wire kind token.
///
/// Drives the badge, the status-line colour, and the auto-shuffle urgency rank.
/// An unrecognised token maps to [`AttentionKind::Other`] so a kind the daemon
/// grows never breaks the board (forward-compatible, never a panic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttentionKind {
    /// `ask_user_question` — a structured multiple-choice question (answerable
    /// inline with ①②③).
    Ask,
    /// `approval` — a yes/no permission prompt.
    Approval,
    /// `codex_request_user` — a Codex free-text request-user prompt.
    CodexRequestUser,
    /// `error` — an API / runtime error the session hit.
    Error,
    /// `escalation` — an ATC / autopilot escalation to the human.
    Escalation,
    /// `waiting` — an idle-at-prompt or explicit waiting marker.
    Waiting,
    /// An unrecognised (forward-compat) kind token.
    Other,
}

impl AttentionKind {
    /// Parse the wire kind token into a family.
    #[must_use]
    pub fn parse(kind: &str) -> Self {
        match kind {
            "ask_user_question" => Self::Ask,
            "approval" => Self::Approval,
            "codex_request_user" => Self::CodexRequestUser,
            "error" => Self::Error,
            "escalation" => Self::Escalation,
            "waiting" => Self::Waiting,
            _ => Self::Other,
        }
    }

    /// The auto-shuffle urgency rank (LOWER sorts higher = floats to the top).
    ///
    /// The three "needs a decision from you" kinds (ASK, approval, Codex
    /// request-user) rank `0` so they always shuffle above errors and idle rows —
    /// the D9 "needs-input to the top" rule. Errors + escalations rank `1`; a
    /// bare waiting/idle row ranks `2`.
    #[must_use]
    const fn urgency_rank(self) -> u8 {
        match self {
            Self::Ask | Self::Approval | Self::CodexRequestUser => 0,
            Self::Error | Self::Escalation => 1,
            Self::Waiting | Self::Other => 2,
        }
    }

    /// `true` when this kind is answerable inline (renders ①②③ options / accepts
    /// a picked answer). Only structured ASKs are; the rest are surfaced but
    /// answered by other means (a later free-text compose, not this phase).
    #[must_use]
    pub const fn is_answerable(self) -> bool {
        matches!(self, Self::Ask)
    }

    /// The accent this family paints in.
    ///
    /// The colour is per WIRE family, finer-grained than the four vocabulary
    /// words it accompanies: a Codex request-user reads `ASK` like any other
    /// question but wears its own blue. The WORD comes from
    /// [`AttentionCard::badge`], never from here — this type used to carry a
    /// second label table (`PERM` / `CODX` / `ESCL` / `????`) and the two
    /// surfaces promptly named the same card two different ways.
    #[must_use]
    const fn color(self) -> Color {
        match self {
            Self::Ask | Self::Approval => GOLD,
            Self::CodexRequestUser => CORNFLOWER_BLUE,
            Self::Error | Self::Escalation => ALERT_RED,
            Self::Waiting => WAIT_AMBER,
            Self::Other => MUTED_GRAY,
        }
    }
}

/// One answer option on an ASK card (a label + an optional one-line description).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardOption {
    /// The option label, rendered beside the glyph.
    pub label: String,
    /// An optional descriptive sub-line.
    pub description: Option<String>,
    /// What `attention/answer` carries when this option is picked, when that is
    /// NOT the label: an ACP adapter's stable `optionId`.
    ///
    /// The label is display text and two options may share one. The daemon
    /// refuses an ambiguous answer rather than guessing, so delivering a shared
    /// label would leave the permission permanently unanswerable from here
    /// while still answerable by id elsewhere. Delivering the id cannot be
    /// ambiguous. `None` for a classifier ASK, whose options have no id and
    /// whose label IS the answer.
    pub answer: Option<String>,
}

impl CardOption {
    /// What `attention/answer` carries when this option is picked: the id when
    /// the option has one, else the label.
    #[must_use]
    pub fn delivered(&self) -> String {
        self.answer.clone().unwrap_or_else(|| self.label.clone())
    }
}

/// The rendered body of a card, parsed from the attention row's payload
/// (a serialised needs-classifier context).
///
/// The payload is the classifier's `NeedsContext` — `{"kind":"ASK"|"ERR"|"IDLE"|
/// "WAIT","context":{…}}`. The plugin has no dependency on the classifier crate
/// (it owns zero domain types), so it parses the payload generically here into
/// the small render shape the detail pane needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardBody {
    /// A structured question with answerable options.
    Ask {
        /// An optional header rendered above the question.
        header: Option<String>,
        /// The question text.
        question: String,
        /// The answer options (rendered ①②③).
        options: Vec<CardOption>,
    },
    /// An error: the matched pattern + the raw snippet.
    Err {
        /// The error pattern token (e.g. `rate_limited`).
        pattern: String,
        /// The raw snippet the classifier matched.
        snippet: String,
    },
    /// An idle-at-prompt row: minutes idle + the last assistant reply text.
    Idle {
        /// How many minutes the session has sat idle at its prompt.
        minutes: i64,
        /// The assistant's last reply text (the LAST REPLY the payload carries).
        last_reply: Option<String>,
    },
    /// An explicit waiting marker + its text.
    Wait {
        /// The marker (e.g. `WAITING:`).
        marker: String,
        /// The free-text the marker carried.
        text: String,
    },
    /// A payload the plugin could not parse into a known shape (rendered raw).
    Other {
        /// The raw payload (truncated on render).
        raw: String,
    },
}

/// One session card: an open attention row flattened into its render shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionCard {
    /// The attention id — the `attention/answer` target + the focus key.
    pub id: String,
    /// The raising session id.
    pub session_id: String,
    /// The raising session's cwd (empty when unknown).
    pub cwd: String,
    /// The owning workspace, or `None` for a hand-started host session.
    pub workspace_id: Option<String>,
    /// The parsed kind family.
    pub kind: AttentionKind,
    /// `true` when sourced from the degraded pane-classifier fallback.
    pub degraded: bool,
    /// Ingest timestamp (epoch ms) — drives the card age + the recency tiebreak.
    pub created_at: i64,
    /// The parsed body the detail pane renders.
    pub body: CardBody,
}

impl AttentionCard {
    /// Flatten a wire [`AttentionRow`] into a card, parsing its payload.
    #[must_use]
    pub fn from_row(row: &AttentionRow) -> Self {
        Self {
            id: row.id.clone(),
            session_id: row.session_id.clone(),
            cwd: row.cwd.clone(),
            workspace_id: row.workspace_id.clone(),
            kind: AttentionKind::parse(&row.kind),
            degraded: row.degraded,
            created_at: row.created_at,
            body: parse_body(&row.payload),
        }
    }

    /// The fixed-width code + accent this card wears on EVERY surface.
    ///
    /// The one badge: this board's card list, its detail header, and the Inbox's
    /// `needs you` row all read it, so a card cannot be `PERM` on `C` and `ASK`
    /// on `I`. The word is the vocabulary's ([`Self::vocab_kind`]), the colour
    /// the wire family's ([`AttentionKind::color`]).
    #[must_use]
    pub(crate) fn badge(&self) -> (String, Color) {
        (
            format!("{:<4}", self.vocab_kind().code()),
            self.kind.color(),
        )
    }

    /// The vocabulary code this card paints (crisp B2 §2.1): the ONE mapping of
    /// the attention wire families onto [`crate::vocab::AttentionKind`].
    ///
    /// The seven wire families collapse onto four codes. [`AttentionKind::Waiting`]
    /// splits on the body it parsed: a session parked at its prompt is `IDLE`, an
    /// explicit `WAITING:` marker is `WAIT`. That body split is why the mapping
    /// lives here and not on the vocabulary type — the wire token alone cannot
    /// tell the two apart.
    #[must_use]
    pub(crate) fn vocab_kind(&self) -> crate::vocab::AttentionKind {
        use crate::vocab::AttentionKind as Vocab;
        match self.kind {
            AttentionKind::Ask | AttentionKind::Approval | AttentionKind::CodexRequestUser => {
                Vocab::Ask
            }
            AttentionKind::Error | AttentionKind::Escalation => Vocab::Err,
            AttentionKind::Waiting if matches!(self.body, CardBody::Idle { .. }) => Vocab::Idle,
            AttentionKind::Waiting | AttentionKind::Other => Vocab::Wait,
        }
    }

    /// A short human label: the cwd's final path component, else the session id
    /// (truncated). Char-safe throughout.
    #[must_use]
    pub(crate) fn short_label(&self) -> String {
        let base = self.cwd.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or("");
        if base.is_empty() {
            truncate_chars(&self.session_id, 12)
        } else {
            base.to_string()
        }
    }

    /// `true` when this card answers inline (renders ①②③ and takes a digit).
    ///
    /// The WIRE family decides it for the classifier's kinds, and the parsed
    /// BODY decides it for a row whose kind is coarser than its payload: an ACP
    /// permission arrives as `approval`, a family that carries no options in
    /// general, while its payload carries the adapter's own option list. The
    /// union, so no `ask_user_question` row's behaviour moves.
    #[must_use]
    pub(crate) fn is_answerable(&self) -> bool {
        self.kind.is_answerable() || matches!(self.body, CardBody::Ask { .. })
    }

    /// The answer options, when this is an answerable ASK card.
    #[must_use]
    pub(crate) fn options(&self) -> &[CardOption] {
        match &self.body {
            CardBody::Ask { options, .. } => options,
            _ => &[],
        }
    }
}

/// Parse an attention payload (a serialised `NeedsContext`) into a [`CardBody`].
///
/// The classifier serialises `NeedsContext` as `{"kind": <TAG>, "context": {…}}`
/// (tag = `ASK`/`ERR`/`IDLE`/`WAIT`). An unparseable / unknown payload falls back
/// to [`CardBody::Other`] so the card always renders something.
///
/// One payload does NOT come from the classifier: an ACP adapter's parked
/// permission (`kind = "acp_permission"`, written by the daemon's
/// `acp_pool::raise_permission`) carries the adapter's OWN options and is
/// answered through the same `attention/answer` RPC. It reads as an ASK here so
/// the one inline-answer affordance serves it too: without this arm the row
/// renders as unparseable and no key can answer it, which is the whole
/// human-in-the-loop for a task running on the ACP executor.
fn parse_body(payload: &str) -> CardBody {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return CardBody::Other {
            raw: truncate_chars(payload, 200),
        };
    };
    let tag = v.get("kind").and_then(serde_json::Value::as_str).unwrap_or("");
    let ctx = v.get("context").unwrap_or(&serde_json::Value::Null);
    match tag {
        "acp_permission" => CardBody::Ask {
            header: v
                .get("toolCall")
                .and_then(|t| t.get("kind"))
                .and_then(serde_json::Value::as_str)
                .map(|kind| format!("permission · {kind}")),
            question: v
                .get("toolCall")
                .and_then(|t| t.get("title"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(the agent asked to run a tool)")
                .to_string(),
            options: parse_acp_options(v.get("options")),
        },
        "ASK" => CardBody::Ask {
            header: ctx.get("header").and_then(serde_json::Value::as_str).map(str::to_string),
            question: ctx
                .get("question")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("(no question text)")
                .to_string(),
            options: parse_options(ctx.get("options")),
        },
        "ERR" => CardBody::Err {
            pattern: ctx
                .get("pattern")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("error")
                .to_string(),
            snippet: ctx
                .get("snippet")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        "IDLE" => CardBody::Idle {
            minutes: ctx.get("idle_minutes").and_then(serde_json::Value::as_i64).unwrap_or(0),
            last_reply: ctx
                .get("last_assistant_text")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        },
        "WAIT" => CardBody::Wait {
            marker: ctx
                .get("marker")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("WAITING:")
                .to_string(),
            text: ctx
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        _ => CardBody::Other {
            raw: truncate_chars(payload, 200),
        },
    }
}

/// Parse the ASK `options` array into [`CardOption`]s (skips malformed entries).
fn parse_options(v: Option<&serde_json::Value>) -> Vec<CardOption> {
    v.and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|o| {
                    Some(CardOption {
                        label: o.get("label").and_then(serde_json::Value::as_str)?.to_string(),
                        description: o
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        // The classifier's options have no id; the label is the answer.
                        answer: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse an ACP permission's `options` array into [`CardOption`]s.
///
/// The adapter's wire shape is `{optionId, name, kind}` (ACP `options_wire`),
/// not the classifier's `{label, description}`: `name` renders, `optionId` is
/// what gets delivered.
///
/// BOTH fields are required, and one malformed entry voids the whole list
/// rather than dropping a row. Dropping one would renumber every glyph below
/// it, and the daemon reads a bare digit as a 1-based index into the options it
/// holds, so the operator would press ③ and answer something else. The daemon
/// voids the same payload for the same reason
/// (`answer::acp_permission_from_payload`); an empty list here renders "this
/// ASK carries no options", which is the truth about a payload neither side
/// will act on.
fn parse_acp_options(v: Option<&serde_json::Value>) -> Vec<CardOption> {
    v.and_then(serde_json::Value::as_array)
        .and_then(|arr| {
            arr.iter()
                .map(|o| {
                    Some(CardOption {
                        label: o.get("name").and_then(serde_json::Value::as_str)?.to_string(),
                        description: None,
                        answer: Some(
                            o.get("optionId").and_then(serde_json::Value::as_str)?.to_string(),
                        ),
                    })
                })
                .collect::<Option<Vec<_>>>()
        })
        .unwrap_or_default()
}

/// The control-center render state.
///
/// Holds the shuffled session cards, the focused card's `attention_id` (so the
/// selection survives an auto-shuffle without steal — see [`Self::set_attention`]),
/// and the ASK option cursor. Default is the empty pane shown before the first
/// `attention/list` snapshot lands.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ControlCenterState {
    /// The open cards, in shuffled (urgency-then-recency) order.
    cards: Vec<AttentionCard>,
    /// The focused card's `attention_id`, or `None` when the board is empty.
    /// Keyed by id — NOT index — so a re-shuffle never moves the human's focus.
    selected_id: Option<String>,
    /// The highlighted ASK option on the selected card.
    option_cursor: usize,
    /// The last `attention/answer` verdict that did NOT deliver (refused as
    /// ambiguous, no live target, delivery failed, already answered elsewhere),
    /// painted on the title row. Keyed to the card it was about: it clears on
    /// the next delivered answer, and on any refresh where that card is no
    /// longer open (answered elsewhere, session gone). A swallowed refusal read
    /// as "I pressed 2 and nothing happened" while the agent stayed blocked.
    note: Option<(String, String)>,
}

impl ControlCenterState {
    /// Surface an answer verdict the daemon returned instead of a delivery,
    /// about the card `attention_id`.
    pub fn set_note(&mut self, attention_id: impl Into<String>, note: impl Into<String>) {
        self.note = Some((attention_id.into(), note.into()));
    }

    /// Clear the answer note (a delivered answer, or a fresh board).
    pub fn clear_note(&mut self) {
        self.note = None;
    }

    /// The current answer note, if any.
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_ref().map(|(_, n)| n.as_str())
    }

    /// Rebuild the board from an `attention/list` / `attention/subscribe`
    /// snapshot, preserving the human's focus and option cursor.
    ///
    /// The rows are shuffled by [`sort_cards`] (needs-input to the top, recency
    /// tiebreak). The previously-selected card is re-selected by its
    /// `attention_id` when it survives the refresh; if it was answered away (no
    /// longer in the open set) the selection falls to the first card. This is the
    /// D9 "auto-shuffle WITHOUT stealing keyboard focus" contract: a fresh ASK
    /// jumping to row 0 never yanks the selection off the card the human was
    /// reading.
    pub fn set_attention(&mut self, rows: &[AttentionRow]) {
        let mut cards: Vec<AttentionCard> = rows.iter().map(AttentionCard::from_row).collect();
        sort_cards(&mut cards);

        // Preserve focus by id: keep the current selection if it survived; else
        // land on the first card (or clear when the board emptied).
        let keep = self
            .selected_id
            .as_ref()
            .filter(|id| cards.iter().any(|c| &c.id == *id))
            .cloned();
        self.selected_id = keep.or_else(|| cards.first().map(|c| c.id.clone()));
        // A refusal about a card that left the open set is stale: the row was
        // answered from another surface or its session is gone.
        if self.note.as_ref().is_some_and(|(id, _)| !cards.iter().any(|c| &c.id == id)) {
            self.note = None;
        }
        self.cards = cards;
        self.clamp_option_cursor();
    }

    /// The cards in shuffled order (read accessor for the renderer / tests).
    #[must_use]
    pub fn cards(&self) -> &[AttentionCard] {
        &self.cards
    }

    /// The focused card's `attention_id`, if any.
    #[must_use]
    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    /// The 0-based index of the focused card in the shuffled order.
    #[must_use]
    pub fn selected_index(&self) -> Option<usize> {
        let id = self.selected_id.as_ref()?;
        self.cards.iter().position(|c| &c.id == id)
    }

    /// The focused card, if any.
    #[must_use]
    pub fn selected_card(&self) -> Option<&AttentionCard> {
        self.selected_index().and_then(|i| self.cards.get(i))
    }

    /// The highlighted ASK option index on the selected card.
    #[must_use]
    pub const fn option_cursor(&self) -> usize {
        self.option_cursor
    }

    /// Move the card selection down by one (saturating at the last card); resets
    /// the option cursor since a different card has a different option set.
    pub fn select_next(&mut self) {
        let Some(i) = self.selected_index() else {
            self.selected_id = self.cards.first().map(|c| c.id.clone());
            return;
        };
        let next = (i + 1).min(self.cards.len().saturating_sub(1));
        self.selected_id = self.cards.get(next).map(|c| c.id.clone());
        self.option_cursor = 0;
    }

    /// Move the card selection up by one (saturating at the first card); resets
    /// the option cursor.
    pub fn select_prev(&mut self) {
        let Some(i) = self.selected_index() else {
            self.selected_id = self.cards.first().map(|c| c.id.clone());
            return;
        };
        let prev = i.saturating_sub(1);
        self.selected_id = self.cards.get(prev).map(|c| c.id.clone());
        self.option_cursor = 0;
    }

    /// Move the ASK option cursor forward, wrapping within the option count.
    pub fn option_next(&mut self) {
        let n = self.selected_option_count();
        if n > 0 {
            self.option_cursor = (self.option_cursor + 1) % n;
        }
    }

    /// Move the ASK option cursor back, wrapping within the option count.
    pub fn option_prev(&mut self) {
        let n = self.selected_option_count();
        if n > 0 {
            self.option_cursor = (self.option_cursor + n - 1) % n;
        }
    }

    /// The number of options on the selected card (0 when not an ASK).
    #[must_use]
    fn selected_option_count(&self) -> usize {
        self.selected_card().map_or(0, |c| c.options().len())
    }

    /// Keep the option cursor within the selected card's option count.
    fn clamp_option_cursor(&mut self) {
        let n = self.selected_option_count();
        if n == 0 {
            self.option_cursor = 0;
        } else if self.option_cursor >= n {
            self.option_cursor = n - 1;
        }
    }

    /// How many open cards need a DECISION from the human (the rank-0 kinds:
    /// ASK, approval, Codex request-user), as opposed to being surfaced for
    /// visibility.
    ///
    /// The one place that count is computed. The Control title's `N need you`
    /// and the Inbox header's `[N need you]` are both this number over this
    /// store, so the two attention surfaces cannot disagree about how much is
    /// waiting (crisp B3 §2.4).
    #[must_use]
    pub fn needs_you_count(&self) -> usize {
        self.cards.iter().filter(|c| c.kind.urgency_rank() == 0).count()
    }

    /// The `(attention_id, answer)` the selected ASK's highlighted option would
    /// deliver, if a card + option are selected. `None` for a non-ASK card or an
    /// ASK with no options.
    #[must_use]
    pub fn pending_answer(&self) -> Option<(String, String)> {
        let card = self.selected_card()?;
        Some((
            card.id.clone(),
            card.options().get(self.option_cursor)?.delivered(),
        ))
    }

    /// The `(attention_id, answer)` for a direct 1-based option pick on the
    /// selected ASK (the ①..⑨ number keys). `None` when the pick is out of range
    /// or the card is not an answerable ASK.
    #[must_use]
    pub fn answer_at(&self, one_based: usize) -> Option<(String, String)> {
        let card = self.selected_card()?;
        let idx = one_based.checked_sub(1)?;
        Some((card.id.clone(), card.options().get(idx)?.delivered()))
    }
}

/// Sort cards into the auto-shuffle order: needs-input to the top, recency
/// (most-recently-raised) as the tiebreak, then id for a stable total order.
///
/// The urgency rank ([`AttentionKind::urgency_rank`]) is the primary key so a
/// fresh ASK always floats above idle rows; within a rank the newest raise wins
/// (a `created_at` descending compare); the id breaks any remaining tie so the
/// order is deterministic (golden-snapshot stable).
pub fn sort_cards(cards: &mut [AttentionCard]) {
    cards.sort_by(|a, b| {
        a.kind
            .urgency_rank()
            .cmp(&b.kind.urgency_rank())
            .then(b.created_at.cmp(&a.created_at))
            .then(a.id.cmp(&b.id))
    });
}

/// A forwarded key folded into the control-center reducer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlCenterEvent {
    /// A printable key (`'j'`, `'k'`, `'h'`, `'l'`, `'\n'` for Enter, `'1'`..).
    Key(char),
}

/// A cross-layer side effect the reducer raises (answering an ASK).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlCenterIntent {
    /// Deliver `answer` to the raising session of `attention_id` via the one
    /// `attention/answer` RPC (first-answer-wins + C1 guard live in the daemon).
    Answer {
        /// The attention row to answer.
        attention_id: String,
        /// The picked option label.
        answer: String,
    },
}

/// The result of folding one key into the control-center state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControlCenterReduction {
    /// The next state.
    pub state: ControlCenterState,
    /// The answer side effect, if any.
    pub intent: Option<ControlCenterIntent>,
}

/// Fold one [`ControlCenterEvent`] into `state`, returning the next state and any
/// [`ControlCenterIntent`]. Pure: no IO, no mutation of `state`.
///
/// Keys:
/// - `j` / `k` — move the card selection down / up (the caller maps ↓/↑ to these).
/// - `h` / `l` — move the ASK option cursor back / forward (↔ mapped by caller).
/// - `\n` (Enter) — answer the selected ASK with the highlighted option.
/// - `1`..`9` — answer the selected ASK with that option directly (①②③).
///
/// A key that does not apply (e.g. `l` on a non-ASK card, or `5` when the ASK has
/// four options) folds as an unmodelled no-op.
#[must_use]
pub fn reduce_control_center(
    state: &ControlCenterState,
    ev: ControlCenterEvent,
) -> ControlCenterReduction {
    let mut next = state.clone();
    let intent = match ev {
        ControlCenterEvent::Key('j') => {
            next.select_next();
            None
        }
        ControlCenterEvent::Key('k') => {
            next.select_prev();
            None
        }
        ControlCenterEvent::Key('l') => {
            next.option_next();
            None
        }
        ControlCenterEvent::Key('h') => {
            next.option_prev();
            None
        }
        ControlCenterEvent::Key('\n') => {
            next.pending_answer().map(|(attention_id, answer)| ControlCenterIntent::Answer {
                attention_id,
                answer,
            })
        }
        ControlCenterEvent::Key(c) if c.is_ascii_digit() && c != '0' => {
            let n = (c as usize) - ('0' as usize);
            next.answer_at(n).map(|(attention_id, answer)| ControlCenterIntent::Answer {
                attention_id,
                answer,
            })
        }
        ControlCenterEvent::Key(_) => None,
    };
    ControlCenterReduction {
        state: next,
        intent,
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The circled-digit glyph for a 1-based option index (①..⑳). Falls back to a
/// parenthesised number past 20 (the ASK tool caps options far below this).
fn circled_digit(one_based: usize) -> String {
    if (1..=20).contains(&one_based) {
        char::from_u32(0x245F + u32::try_from(one_based).unwrap_or(0))
            .map_or_else(|| format!("({one_based})"), |c| c.to_string())
    } else {
        format!("({one_based})")
    }
}

/// Render the control-center screen into `buf` between `top` and `bottom`.
///
/// Layout — a left card list and a right detail pane split at ~42% of the width:
///
/// ```text
/// Control · 3 sessions · 2 need you                    C control-center
/// ┌ sessions ─────────────┐ ┌ deploy · ASK ──────────────────────┐
/// │▶ASK  deploy   waiting… │ │ waiting for your input · 2m · hook  │
/// │ ERR  api      rate_lim │ │ LAST REPLY                          │
/// │ IDLE ui       idle 7m  │ │  Ready to ship to which environment │
/// │                        │ │ TIMELINE                            │
/// │                        │ │  · raised 2m ago (ask_user_question)│
/// │                        │ │ ① staging   safe rollout            │
/// │                        │ │ ② prod                              │
/// └────────────────────────┘ └─ h/l option · enter/1-9 answer ─────┘
/// ```
///
/// `now_ms` is the render clock the card ages are derived against.
pub fn render_control_center(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &ControlCenterState,
    now_ms: i64,
) {
    render_title(buf, area_w, top, state);
    let body_top = top + 1;
    if body_top > bottom {
        return;
    }

    // Split: left card list ~42%, right detail pane. A one-column gutter between.
    let list_w = (area_w * 42 / 100).max(20).min(area_w.saturating_sub(1));
    let detail_x = list_w + 1;

    if state.cards.is_empty() {
        put_str(
            buf,
            0,
            body_top + 1,
            "no sessions need you right now",
            MUTED_GRAY,
            area_w,
        );
        put_str(
            buf,
            0,
            body_top + 2,
            "every AskUserQuestion, error, and idle session lands here",
            MUTED_GRAY,
            area_w,
        );
        return;
    }

    render_card_list(buf, list_w, body_top, bottom, state, now_ms);
    render_divider(buf, list_w, body_top, bottom);
    render_detail(buf, detail_x, area_w, body_top, bottom, state, now_ms);
}

/// The first-visible card index for a viewport of `visible_rows` rows that must
/// keep `selected` in view.
///
/// The control-center card list has no stored scroll cursor (the reducer is pure
/// and viewport-blind), so the offset is derived here per render from the
/// selected index: it is the smallest offset that keeps `selected` inside
/// `[offset, offset + visible_rows)`. While the selection sits within the first
/// `visible_rows` cards the list is top-anchored (offset `0`); once the human
/// scrolls (`j`) past the fold the window follows so the `▶` cursor is always
/// painted — never walking below the bottom row and vanishing.
///
/// Shared with the Inbox's `needs you` block, which has the same shape and the
/// same hazard: a selection that leaves the viewport is still answerable, so
/// `enter` and `1`-`9` act on a card nobody can see.
pub(crate) fn first_visible(selected: usize, visible_rows: usize) -> usize {
    selected.saturating_sub(visible_rows.saturating_sub(1))
}

/// Paint the muted vertical rule in the gutter column (`x = list_w`) for every
/// body row, framing the card list against the detail pane so a section label in
/// the right column is never misread as belonging to the card on the same row.
fn render_divider(buf: &mut WireBuffer, list_w: u16, top: u16, bottom: u16) {
    let mut row = top;
    while row <= bottom {
        put_str(buf, list_w, row, "│", MUTED_GRAY, list_w + 1);
        row += 1;
    }
}

/// Render the title row: `Control · N sessions · M need you` + the hotkey hint.
fn render_title(buf: &mut WireBuffer, area_w: u16, row: u16, state: &ControlCenterState) {
    let total = state.cards.len();
    let need = state.needs_you_count();
    let mut x = put_str(buf, 0, row, "Control", GOLD, area_w);
    x = put_str(
        buf,
        x,
        row,
        &format!("  · {total} sessions · "),
        SOFT_WHITE,
        area_w,
    );
    x = put_str(buf, x, row, &format!("{need} need you"), WAIT_AMBER, area_w);
    if let Some(note) = state.note() {
        x = put_str(buf, x, row, &format!("   ⚠ {note}"), ALERT_RED, area_w);
    }
    // The hotkey hint next to the control (feedback_keybinding_hints_near_control).
    let hint = "C control-center";
    let hint_w = u16::try_from(hint.chars().count()).unwrap_or(0);
    if let Some(start) = area_w.checked_sub(hint_w) {
        if start > x {
            put_str(buf, start, row, hint, MUTED_GRAY, area_w);
        }
    }
}

/// Render the left column: one row per shuffled card (badge + label + status +
/// age), the focused row marked with `▶` in selection green.
fn render_card_list(
    buf: &mut WireBuffer,
    list_w: u16,
    top: u16,
    bottom: u16,
    state: &ControlCenterState,
    now_ms: i64,
) {
    let selected = state.selected_id();
    // Follow the selection: derive the first-visible offset so the `▶` cursor is
    // always inside the viewport even when the board is taller than the pane (a
    // board with more open rows than fit would otherwise paint from index 0 and
    // hide the selection once it walked below the fold).
    let visible_rows = usize::from(bottom.saturating_sub(top)) + 1;
    let offset = first_visible(state.selected_index().unwrap_or(0), visible_rows);
    // Bounded zip: one card per row from `top` until `bottom`, so the list can
    // never overrun the pane and no manual counter is needed.
    for (row, card) in (top..=bottom).zip(state.cards.iter().skip(offset)) {
        let is_sel = Some(card.id.as_str()) == selected;
        let (badge, badge_color) = card.badge();
        let marker = if is_sel { "▶" } else { " " };
        let mut x = put_str(buf, 0, row, marker, SELECTION_GREEN, list_w);
        x = put_str(buf, x, row, &badge, badge_color, list_w);
        x = put_str(buf, x, row, " ", MUTED_GRAY, list_w);
        let label = truncate_chars(&card.short_label(), 12);
        x = put_str(buf, x, row, &pad_to(&label, 12), SOFT_WHITE, list_w);
        x = put_str(buf, x, row, " ", MUTED_GRAY, list_w);
        // The age is the one live stat the attention feed carries.
        let age = crate::vocab::age_word(now_ms.saturating_sub(card.created_at));
        put_str(buf, x, row, &age, MUTED_GRAY, list_w);
    }
}

/// Render the right detail pane for the selected card: the status line + stat
/// strip, the LAST REPLY, the TIMELINE, and (for an ASK) the inline options.
fn render_detail(
    buf: &mut WireBuffer,
    x0: u16,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &ControlCenterState,
    now_ms: i64,
) {
    let Some(card) = state.selected_card() else {
        return;
    };
    let mut row = top;

    // Pane header — names the selected card so the detail column is visibly
    // anchored to the left-list selection (`proj-0 · ASK`), not an orphan pane.
    // Reuses `short_label` (the same label the card list renders) + the kind
    // badge; painted in gold above the status line.
    let (badge, _) = card.badge();
    let header = format!("{} · {}", card.short_label(), badge.trim());
    row = put_line(buf, x0, row, bottom, area_w, &header, GOLD);

    // Status line — coloured by kind (D9 "amber waiting / red error / …").
    let (status, status_color) = status_line(card);
    row = put_line(buf, x0, row, bottom, area_w, &status, status_color);

    // Stat strip: age (live) + source + workspace. Only what is known: the
    // token / tool / diff columns have no producer on this row yet (P10 §4.9),
    // and three permanent dash placeholders read as dead data (crisp B1, Q16);
    // they return with the numbers.
    let age = crate::vocab::age_word(now_ms.saturating_sub(card.created_at));
    let source = if card.degraded { "~pane" } else { "hook" };
    let scope = card.workspace_id.as_deref().unwrap_or("host");
    let strip = format!("age {age} · {source} · {scope}");
    row = put_line(buf, x0, row, bottom, area_w, &strip, MUTED_GRAY);
    row = row.saturating_add(1);

    // LAST REPLY.
    row = put_line(buf, x0, row, bottom, area_w, "LAST REPLY", GOLD);
    row = put_wrapped(buf, x0, row, bottom, area_w, &last_reply(card), SOFT_WHITE);
    row = row.saturating_add(1);

    // TIMELINE (the attention timeline; the per-tool JSONL timeline lands in P10).
    row = put_line(buf, x0, row, bottom, area_w, "TIMELINE", GOLD);
    let ago = crate::vocab::age_word(now_ms.saturating_sub(card.created_at));
    let tl = format!("· raised {ago} ago · {}", kind_token(card.kind));
    row = put_line(buf, x0, row, bottom, area_w, &tl, MUTED_GRAY);
    row = row.saturating_add(1);

    // Inline ASK answering — options with ①②③, then the answer hints. Nothing
    // follows it in this pane, so the height it returns has no caller here.
    let _ = render_options(buf, x0, row, bottom, area_w, card, state.option_cursor());
}

/// How many rows [`render_options`] paints for `card`, given room for all of them.
///
/// Lives next to the renderer and is the ONLY derivation of that shape. The
/// Inbox sizes its `needs you` block and starts its next card row from this, so
/// a row added below (a `(Recommended)` marker, a wrapped label) has to be
/// counted here or the two paints overlap — and the overlap is silent, since a
/// clipped renderer never overruns the pane.
/// `render_options_paints_exactly_options_height_rows` is what makes the miss loud.
#[must_use]
pub(crate) fn options_height(card: &AttentionCard) -> u16 {
    if !card.is_answerable() || card.options().is_empty() {
        // The single "(no inline options …)" / "(this ASK carries no options)" note.
        return 1;
    }
    // `OPTIONS` header + one row per option (two when it carries a description)
    // + the answer hint line.
    let options: u16 = card.options().iter().map(|o| u16::from(o.description.is_some()) + 1).sum();
    options.saturating_add(2)
}

/// Render the ASK options with circled-digit glyphs + the answer hint bar, or a
/// note for a non-answerable card.
///
/// Shared with the Inbox's `needs you` block (crisp B3 §2.4): the inline answer
/// there is THIS renderer paired with [`reduce_control_center`], not a second
/// implementation of the ①②③ affordance that could drift from the one the
/// `attention/answer` path expects.
///
/// RETURNS the rows the block occupies, which the caller must use to place
/// whatever comes next. That return is the whole point: a row added here shows
/// up in it whether or not that row paints a glyph, so a blank spacer (an idiom
/// `render_detail` already uses twice) cannot slip past a guard that counts
/// painted cells. The count is what [`options_height`] must equal.
///
/// It is the FULL height, not the clipped one: `bottom` stops the paint walk,
/// not the arithmetic, so a caller sizing a block gets the same answer whether
/// or not the pane happened to be tall enough this frame.
pub(crate) fn render_options(
    buf: &mut WireBuffer,
    x0: u16,
    top: u16,
    bottom: u16,
    area_w: u16,
    card: &AttentionCard,
    option_cursor: usize,
) -> u16 {
    if !card.is_answerable() {
        put_line(
            buf,
            x0,
            top,
            bottom,
            area_w,
            "(no inline options — surfaced for visibility)",
            MUTED_GRAY,
        );
        return 1;
    }
    let options = card.options();
    if options.is_empty() {
        put_line(
            buf,
            x0,
            top,
            bottom,
            area_w,
            "(this ASK carries no options)",
            MUTED_GRAY,
        );
        return 1;
    }

    // `used` counts every row the block owns; `row` only walks as far as the
    // pane allows, because `put_str` clips on x and would happily paint below
    // `bottom` if the walk were unguarded.
    let mut used: u16 = 0;
    let mut row = top;
    row = put_line(buf, x0, row, bottom, area_w, "OPTIONS", GOLD);
    used += 1;
    for (i, opt) in options.iter().enumerate() {
        if row <= bottom {
            let color = if i == option_cursor {
                SELECTION_GREEN
            } else {
                SOFT_WHITE
            };
            let glyph = circled_digit(i + 1);
            let mut x = put_str(buf, x0, row, &glyph, color, area_w);
            x = put_str(buf, x, row, " ", color, area_w);
            put_str(buf, x, row, &opt.label, color, area_w);
            row += 1;
        }
        used = used.saturating_add(1);
        if let Some(desc) = &opt.description {
            if row <= bottom {
                let dx = put_str(buf, x0, row, "   ", MUTED_GRAY, area_w);
                put_str(buf, dx, row, &truncate_chars(desc, 48), MUTED_GRAY, area_w);
                row += 1;
            }
            used = used.saturating_add(1);
        }
    }
    // Hints next to the control (feedback_keybinding_hints_near_control).
    put_line(
        buf,
        x0,
        row,
        bottom,
        area_w,
        "h/l option · enter/1-9 answer",
        MUTED_GRAY,
    );
    used.saturating_add(1)
}

/// The status line text + colour for a card.
fn status_line(card: &AttentionCard) -> (String, Color) {
    match card.kind {
        AttentionKind::Ask | AttentionKind::Approval | AttentionKind::CodexRequestUser => {
            ("waiting for your input".to_string(), WAIT_AMBER)
        }
        AttentionKind::Error => {
            let pat = match &card.body {
                CardBody::Err { pattern, .. } => pattern.as_str(),
                _ => "error",
            };
            (format!("error · {pat}"), ALERT_RED)
        }
        AttentionKind::Escalation => ("escalation · needs a call".to_string(), ALERT_RED),
        AttentionKind::Waiting => {
            let detail = match &card.body {
                CardBody::Idle { minutes, .. } => format!("idle {minutes}m at prompt"),
                CardBody::Wait { text, .. } if !text.is_empty() => format!("waiting · {text}"),
                _ => "waiting".to_string(),
            };
            (detail, MUTED_GRAY)
        }
        AttentionKind::Other => ("needs attention".to_string(), MUTED_GRAY),
    }
}

/// The LAST REPLY / request-context text a card carries.
///
/// Also the one line the Inbox's `needs you` row reads (crisp B3 §2.4): the ASK's
/// question, the error's snippet, the idle session's last words.
pub(crate) fn last_reply(card: &AttentionCard) -> String {
    match &card.body {
        CardBody::Ask {
            header, question, ..
        } => header
            .as_ref()
            .map_or_else(|| question.clone(), |h| format!("{h} — {question}")),
        CardBody::Err { snippet, pattern } => {
            if snippet.is_empty() {
                pattern.clone()
            } else {
                snippet.clone()
            }
        }
        CardBody::Idle { last_reply, .. } => {
            last_reply.clone().unwrap_or_else(|| "(no assistant text captured)".to_string())
        }
        CardBody::Wait { text, marker } => {
            if text.is_empty() {
                marker.clone()
            } else {
                text.clone()
            }
        }
        CardBody::Other { raw } => raw.clone(),
    }
}

/// The wire kind token for the timeline line.
const fn kind_token(kind: AttentionKind) -> &'static str {
    match kind {
        AttentionKind::Ask => "ask_user_question",
        AttentionKind::Approval => "approval",
        AttentionKind::CodexRequestUser => "codex_request_user",
        AttentionKind::Error => "error",
        AttentionKind::Escalation => "escalation",
        AttentionKind::Waiting => "waiting",
        AttentionKind::Other => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Cell helpers (mirroring `super::inbox` — char-safe, width-clipped)
// ---------------------------------------------------------------------------

/// Write one line at `(x0, row)` if `row <= bottom`, returning the next row.
fn put_line(
    buf: &mut WireBuffer,
    x0: u16,
    row: u16,
    bottom: u16,
    right: u16,
    s: &str,
    color: Color,
) -> u16 {
    if row > bottom {
        return row;
    }
    put_str(buf, x0, row, s, color, right);
    row + 1
}

/// Write `s` wrapped to the available width from `x0`, one continuation line,
/// returning the next free row. Keeps the reply readable without overflowing.
fn put_wrapped(
    buf: &mut WireBuffer,
    x0: u16,
    row: u16,
    bottom: u16,
    right: u16,
    s: &str,
    color: Color,
) -> u16 {
    let avail = right.saturating_sub(x0) as usize;
    if avail == 0 || row > bottom {
        return row;
    }
    let chars: Vec<char> = s.chars().collect();
    let mut r = row;
    let mut i = 0;
    // At most two wrapped lines so the reply never floods the pane.
    while i < chars.len() && r <= bottom && r < row + 2 {
        let end = (i + avail).min(chars.len());
        let line: String = chars[i..end].iter().collect();
        put_str(buf, x0, r, &line, color, right);
        i = end;
        r += 1;
    }
    r
}

/// Right-pad `s` to `width` chars (char-safe).
fn pad_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        s.to_string()
    } else {
        let mut out = s.to_string();
        out.extend(std::iter::repeat_n(' ', width - len));
        out
    }
}

/// Truncate to `max` display chars with a trailing ellipsis on overflow
/// (char-safe — never byte-slices, the utf8-truncate trap).
fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

/// Write `s` at `(x, row)` in `color`, clipping at `right`. Returns the next free
/// column. Char-safe (iterates `char`s, not bytes).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, right: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= right {
            break;
        }
        // Harden against terminal escape injection AND bidi reordering: this
        // screen renders fleet-wide, session-originated free text (assistant
        // replies, error snippets, ASK labels, cwd-derived labels) char by char,
        // and each char becomes a Cell symbol the host paints verbatim. The one
        // choke point every rendered string flows through, so the rule lives in
        // `super::display_char` and the Inbox applies the identical one.
        let mut cell = Cell::new(super::display_char(ch).to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}

/// Colours + glyphs exported so a snapshot/render test can assert the status-line
/// colouring + selection marker non-vacuously without re-declaring the triples.
pub mod colors {
    use ainb_plugin_sdk::Color;

    /// ASK / waiting-for-input amber.
    pub const WAIT: Color = super::WAIT_AMBER;
    /// Error red.
    pub const ERROR: Color = super::ALERT_RED;
    /// Selection green (the `▶` marker + the highlighted option).
    pub const SELECTION: Color = super::SELECTION_GREEN;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, kind: &str, created_at: i64, payload: &str) -> AttentionRow {
        AttentionRow {
            id: id.to_string(),
            session_id: format!("sess-{id}"),
            cwd: format!("/work/{id}"),
            workspace_id: None,
            kind: kind.to_string(),
            payload: payload.to_string(),
            degraded: false,
            created_at,
            channels: ainb_hangar_proto::ChannelSet::NONE,
        }
    }

    fn ask_payload(q: &str, opts: &[&str]) -> String {
        let options: Vec<serde_json::Value> =
            opts.iter().map(|l| serde_json::json!({ "label": l })).collect();
        serde_json::json!({ "kind": "ASK", "context": { "question": q, "options": options } })
            .to_string()
    }

    // --- shuffle ordering --------------------------------------------------

    #[test]
    fn needs_input_shuffles_above_error_and_idle() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[
            row(
                "idle",
                "waiting",
                100,
                r#"{"kind":"IDLE","context":{"idle_minutes":7}}"#,
            ),
            row(
                "err",
                "error",
                200,
                r#"{"kind":"ERR","context":{"pattern":"rate_limited"}}"#,
            ),
            row(
                "ask",
                "ask_user_question",
                50,
                &ask_payload("Ship?", &["yes", "no"]),
            ),
        ]);
        let order: Vec<&str> = state.cards().iter().map(|c| c.id.as_str()).collect();
        // ASK (rank 0) first despite being the OLDEST, then ERR (rank 1), then
        // the idle row (rank 2). Needs-input to the top regardless of age.
        assert_eq!(order, ["ask", "err", "idle"]);
    }

    #[test]
    fn recency_breaks_ties_within_a_rank() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[
            row(
                "old-ask",
                "ask_user_question",
                100,
                &ask_payload("q1", &["a"]),
            ),
            row(
                "new-ask",
                "ask_user_question",
                300,
                &ask_payload("q2", &["b"]),
            ),
            row(
                "mid-ask",
                "ask_user_question",
                200,
                &ask_payload("q3", &["c"]),
            ),
        ]);
        let order: Vec<&str> = state.cards().iter().map(|c| c.id.as_str()).collect();
        // Same rank → newest raised first (recency tiebreak).
        assert_eq!(order, ["new-ask", "mid-ask", "old-ask"]);
    }

    #[test]
    fn shuffle_does_not_steal_keyboard_focus() {
        let mut state = ControlCenterState::default();
        // Focus the ERR card.
        state.set_attention(&[
            row(
                "err",
                "error",
                200,
                r#"{"kind":"ERR","context":{"pattern":"boom"}}"#,
            ),
            row("ask1", "ask_user_question", 100, &ask_payload("q", &["a"])),
        ]);
        // Move selection onto the ERR row explicitly.
        while state.selected_id() != Some("err") {
            state.select_next();
        }
        assert_eq!(state.selected_id(), Some("err"));

        // A fresh ASK arrives and shuffles to the TOP; focus must stay on ERR.
        state.set_attention(&[
            row(
                "err",
                "error",
                200,
                r#"{"kind":"ERR","context":{"pattern":"boom"}}"#,
            ),
            row("ask1", "ask_user_question", 100, &ask_payload("q", &["a"])),
            row(
                "ask2",
                "ask_user_question",
                999,
                &ask_payload("new!", &["x"]),
            ),
        ]);
        assert_eq!(
            state.cards()[0].id,
            "ask2",
            "the fresh ASK floats to the top"
        );
        assert_eq!(
            state.selected_id(),
            Some("err"),
            "the human's focus stays on the card they were reading"
        );
    }

    #[test]
    fn selection_falls_to_first_when_the_focused_card_is_answered_away() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[
            row("a", "ask_user_question", 100, &ask_payload("q", &["y"])),
            row(
                "b",
                "error",
                200,
                r#"{"kind":"ERR","context":{"pattern":"x"}}"#,
            ),
        ]);
        while state.selected_id() != Some("b") {
            state.select_next();
        }
        // "b" answered away → gone from the open set; selection lands on the first.
        state.set_attention(&[row(
            "a",
            "ask_user_question",
            100,
            &ask_payload("q", &["y"]),
        )]);
        assert_eq!(state.selected_id(), Some("a"));
    }

    #[test]
    fn empty_snapshot_clears_selection() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[row("a", "ask_user_question", 1, &ask_payload("q", &["y"]))]);
        assert_eq!(state.selected_id(), Some("a"));
        state.set_attention(&[]);
        assert_eq!(state.selected_id(), None);
        assert!(state.cards().is_empty());
    }

    // --- inline ASK answering ---------------------------------------------

    #[test]
    fn enter_answers_the_highlighted_option() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[row(
            "ask",
            "ask_user_question",
            1,
            &ask_payload("Ship to which env?", &["staging", "prod"]),
        )]);
        // Move the option cursor to "prod".
        let out = reduce_control_center(&state, ControlCenterEvent::Key('l'));
        state = out.state;
        assert_eq!(state.option_cursor(), 1);
        let out = reduce_control_center(&state, ControlCenterEvent::Key('\n'));
        assert_eq!(
            out.intent,
            Some(ControlCenterIntent::Answer {
                attention_id: "ask".into(),
                answer: "prod".into(),
            })
        );
    }

    #[test]
    fn number_key_answers_that_option_directly() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[row(
            "ask",
            "ask_user_question",
            1,
            &ask_payload("q", &["one", "two", "three"]),
        )]);
        state.set_attention(&[row(
            "ask",
            "ask_user_question",
            1,
            &ask_payload("q", &["one", "two", "three"]),
        )]);
        let out = reduce_control_center(&state, ControlCenterEvent::Key('3'));
        assert_eq!(
            out.intent,
            Some(ControlCenterIntent::Answer {
                attention_id: "ask".into(),
                answer: "three".into(),
            })
        );
    }

    /// An ACP adapter's parked permission is answerable inline, and delivers
    /// the adapter's own option id.
    ///
    /// The payload is `acp_pool::raise_permission`'s, verbatim in shape: an
    /// `approval` row whose options are `{optionId, name, kind}`. Pressing `3`
    /// must deliver the id `reject`, not the display name: two options may
    /// share a name and the daemon refuses an ambiguous answer, which would
    /// leave the row unanswerable from here. The wrong option here is the
    /// defect-26 class (the store recording one pick while the agent acts on
    /// another), and before this the row carried no options at all and the
    /// digit fell through to the tab router.
    #[test]
    fn acp_permission_answers_by_the_adapters_own_option() {
        let payload = serde_json::json!({
            "kind": "acp_permission",
            "sessionKey": "acp:claude:01J",
            "requestFingerprint": "f00d",
            "rpcId": 9000,
            "options": [
                { "optionId": "allow_always", "name": "Always Allow", "kind": "allow_always" },
                { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
                { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
            ],
            "toolCall": { "toolCallId": "tool-1", "title": "printf 'api/app.db' > DBPATH.txt", "kind": "execute" },
        })
        .to_string();
        let mut state = ControlCenterState::default();
        state.set_attention(&[row("perm", "approval", 1, &payload)]);

        let card = state.selected_card().expect("the permission card");
        assert!(card.is_answerable(), "an acp permission answers inline");
        assert_eq!(
            card.options().iter().map(|o| o.label.as_str()).collect::<Vec<_>>(),
            ["Always Allow", "Allow", "Reject"],
            "the adapter's own labels, in the adapter's own order"
        );
        assert_eq!(
            card.body,
            CardBody::Ask {
                header: Some("permission · execute".into()),
                question: "printf 'api/app.db' > DBPATH.txt".into(),
                options: card.options().to_vec(),
            }
        );

        let out = reduce_control_center(&state, ControlCenterEvent::Key('3'));
        assert_eq!(
            out.intent,
            Some(ControlCenterIntent::Answer {
                attention_id: "perm".into(),
                answer: "reject".into(),
            }),
            "the id is delivered, never the display name"
        );
    }

    /// Two options sharing a display name still answer, because the id is what
    /// travels: on the name the daemon would refuse as ambiguous and the row
    /// would reopen unanswerable.
    #[test]
    fn acp_permission_with_twin_labels_still_delivers_a_distinct_id() {
        let payload = serde_json::json!({
            "kind": "acp_permission",
            "sessionKey": "k",
            "requestFingerprint": "f",
            "options": [
                { "optionId": "allow_always", "name": "Allow", "kind": "allow_always" },
                { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
            ],
        })
        .to_string();
        let mut state = ControlCenterState::default();
        state.set_attention(&[row("perm", "approval", 1, &payload)]);
        let out = reduce_control_center(&state, ControlCenterEvent::Key('2'));
        assert_eq!(
            out.intent,
            Some(ControlCenterIntent::Answer {
                attention_id: "perm".into(),
                answer: "allow".into(),
            })
        );
    }

    /// An option missing either field voids the WHOLE list rather than dropping
    /// a row: dropping one renumbers every glyph below it, and the daemon reads
    /// a bare digit as an index into the options IT holds.
    #[test]
    fn acp_permission_with_a_malformed_option_offers_none() {
        let payload = serde_json::json!({
            "kind": "acp_permission",
            "sessionKey": "k",
            "requestFingerprint": "f",
            "options": [
                { "name": "Always Allow", "kind": "allow_always" },
                { "optionId": "reject", "name": "Reject", "kind": "reject_once" },
            ],
        })
        .to_string();
        let mut state = ControlCenterState::default();
        state.set_attention(&[row("perm", "approval", 1, &payload)]);
        assert!(state.selected_card().expect("card").options().is_empty());
        assert_eq!(state.answer_at(1), None, "no glyph answers a voided list");
    }

    #[test]
    fn number_key_out_of_range_is_a_noop() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[row(
            "ask",
            "ask_user_question",
            1,
            &ask_payload("q", &["a", "b"]),
        )]);
        let out = reduce_control_center(&state, ControlCenterEvent::Key('9'));
        assert_eq!(out.intent, None, "option 9 of a 2-option ASK is a no-op");
    }

    #[test]
    fn enter_on_a_non_ask_card_is_a_noop() {
        let state = {
            let mut s = ControlCenterState::default();
            s.set_attention(&[row(
                "err",
                "error",
                1,
                r#"{"kind":"ERR","context":{"pattern":"x"}}"#,
            )]);
            s
        };
        let out = reduce_control_center(&state, ControlCenterEvent::Key('\n'));
        assert_eq!(out.intent, None);
    }

    #[test]
    fn option_cursor_wraps_within_the_ask() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[row(
            "ask",
            "ask_user_question",
            1,
            &ask_payload("q", &["a", "b", "c"]),
        )]);
        assert_eq!(state.option_cursor(), 0);
        state.option_next();
        state.option_next();
        state.option_next();
        assert_eq!(state.option_cursor(), 0, "wraps past the last option");
        state.option_prev();
        assert_eq!(state.option_cursor(), 2, "wraps back from the first");
    }

    // --- payload parsing ---------------------------------------------------

    #[test]
    fn parses_each_needs_context_shape() {
        let ask = parse_body(&ask_payload("Q?", &["yes"]));
        assert!(matches!(ask, CardBody::Ask { .. }));
        let err =
            parse_body(r#"{"kind":"ERR","context":{"pattern":"rate_limited","snippet":"429"}}"#);
        assert!(matches!(err, CardBody::Err { .. }));
        let idle = parse_body(
            r#"{"kind":"IDLE","context":{"idle_minutes":9,"last_assistant_text":"all done"}}"#,
        );
        match idle {
            CardBody::Idle {
                minutes,
                last_reply,
            } => {
                assert_eq!(minutes, 9);
                assert_eq!(last_reply.as_deref(), Some("all done"));
            }
            other => panic!("expected Idle, got {other:?}"),
        }
        let junk = parse_body("not json at all");
        assert!(matches!(junk, CardBody::Other { .. }));
    }

    /// The seven wire families collapse onto the vocabulary's four codes, and
    /// all four are reachable — the Inbox's `needs you` row and this board read
    /// the same table, so one card is never named two ways (crisp B3 §2.4).
    ///
    /// MUTATION GUARD: the expectation is the SET of families, not a count. A
    /// family that stopped mapping, or started mapping somewhere else, fails
    /// here; a guard on "four codes exist" would not notice.
    #[test]
    fn every_attention_family_maps_to_a_vocab_code() {
        use crate::vocab::AttentionKind as Vocab;
        let wait = r#"{"kind":"WAIT","context":{"marker":"WAITING:"}}"#;
        for (wire, code) in [
            ("ask_user_question", Vocab::Ask),
            ("approval", Vocab::Ask),
            ("codex_request_user", Vocab::Ask),
            ("error", Vocab::Err),
            ("escalation", Vocab::Err),
            ("waiting", Vocab::Wait),
            ("a_family_a_newer_daemon_grew", Vocab::Wait),
        ] {
            let card = AttentionCard::from_row(&row("c", wire, 1, wait));
            assert_eq!(card.vocab_kind(), code, "wire kind {wire:?}");
        }
        // IDLE is the one code that comes from the BODY, not the wire kind: a
        // waiting row whose payload parsed as an idle-at-prompt session.
        let idle = AttentionCard::from_row(&row(
            "i",
            "waiting",
            1,
            r#"{"kind":"IDLE","context":{"idle_minutes":7}}"#,
        ));
        assert_eq!(idle.vocab_kind(), Vocab::Idle);
    }

    /// The `N need you` count is the rank-0 kinds and nothing else: the number
    /// the Control title and the Inbox badge both read.
    #[test]
    fn needs_you_count_is_the_decisions_not_the_board_size() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[
            row("ask", "ask_user_question", 1, &ask_payload("q", &["y"])),
            row("perm", "approval", 2, r#"{"kind":"WAIT","context":{}}"#),
            row(
                "err",
                "error",
                3,
                r#"{"kind":"ERR","context":{"pattern":"x"}}"#,
            ),
            row("idle", "waiting", 4, r#"{"kind":"IDLE","context":{}}"#),
        ]);
        assert_eq!(state.cards().len(), 4);
        assert_eq!(
            state.needs_you_count(),
            2,
            "the ASK and the approval, not the error or the idle row"
        );
    }

    /// `options_height` is exactly the height `render_options` reports, for
    /// every card shape the board can hold, clipped pane or not.
    ///
    /// MUTATION GUARD: this is the binding the Inbox's block sizing rests on.
    /// Add a row to the renderer without counting it in `options_height` and the
    /// Inbox paints its next card row over the last option.
    ///
    /// Asserts the RETURNED height, not the count of painted rows: a spacer that
    /// advances `row` without painting a glyph (an idiom `render_detail` uses
    /// twice) would change the real height and leave a painted-cell count
    /// unmoved, which is a guard that cannot fail on the thing it is for.
    #[test]
    fn render_options_reports_exactly_options_height_rows() {
        let described = serde_json::json!({
            "kind": "ASK",
            "context": { "question": "q", "options": [
                { "label": "a", "description": "why a" },
                { "label": "b" },
            ]}
        })
        .to_string();
        let empty_ask =
            serde_json::json!({ "kind": "ASK", "context": { "question": "q", "options": [] } })
                .to_string();
        for (name, kind, payload) in [
            (
                "two plain options",
                "ask_user_question",
                ask_payload("q", &["a", "b"]),
            ),
            ("one described option", "ask_user_question", described),
            ("an ASK with no options", "ask_user_question", empty_ask),
            (
                "a card that is not answerable",
                "error",
                r#"{"kind":"ERR","context":{"pattern":"x"}}"#.to_string(),
            ),
        ] {
            let card = AttentionCard::from_row(&row("c", kind, 1, &payload));
            let mut buf = WireBuffer::new(120, 40);
            let used = render_options(&mut buf, 0, 0, 39, 120, &card, 0);
            assert_eq!(used, options_height(&card), "{name}: reported height");

            // Every reported row is a row the caller must skip, so with room for
            // all of them none may be left unpainted and none may land below.
            let painted: std::collections::BTreeSet<u16> =
                buf.cells.iter().map(|(c, _)| c.y).collect();
            assert_eq!(
                painted.iter().copied().max().map_or(0, |y| y + 1),
                used,
                "{name}: painted rows {painted:?} do not fill the reported height"
            );

            // And the height does not shrink when the pane is too short to paint
            // it: a caller sizing a block gets the same answer either way.
            let mut cramped = WireBuffer::new(120, 40);
            assert_eq!(
                render_options(&mut cramped, 0, 0, 0, 120, &card, 0),
                used,
                "{name}: height changed under clipping"
            );
        }
    }

    #[test]
    fn circled_digits_map_one_to_three() {
        assert_eq!(circled_digit(1), "①");
        assert_eq!(circled_digit(2), "②");
        assert_eq!(circled_digit(3), "③");
        assert_eq!(circled_digit(99), "(99)");
    }

    // --- rendering ---------------------------------------------------------

    #[test]
    fn first_visible_follows_the_selection_past_the_fold() {
        // Selection within the first `visible` cards → top-anchored.
        assert_eq!(first_visible(0, 9), 0);
        assert_eq!(first_visible(8, 9), 0);
        // Selection past the fold → window follows so the selection is the last
        // visible row (never below it).
        assert_eq!(first_visible(9, 9), 1);
        assert_eq!(first_visible(29, 9), 21);
    }

    #[test]
    fn selection_stays_painted_when_the_board_overflows_the_pane() {
        // A board far taller than the pane: 30 answerable ASK rows.
        let mut state = ControlCenterState::default();
        let rows: Vec<AttentionRow> = (0..30)
            .map(|i| {
                row(
                    &format!("ask-{i:02}"),
                    "ask_user_question",
                    i,
                    &ask_payload("q", &["a"]),
                )
            })
            .collect();
        state.set_attention(&rows);
        // Walk the selection to the very last card (like pressing `j` 29 times).
        for _ in 0..40 {
            state.select_next();
        }
        let last_id = state.cards().last().unwrap().id.clone();
        assert_eq!(state.selected_id(), Some(last_id.as_str()));

        // Render into a short pane: body rows are 1..=9 (nine visible), far fewer
        // than the 30 cards.
        let mut buf = ainb_plugin_sdk::WireBuffer::new(100, 10);
        render_control_center(&mut buf, 100, 0, 9, &state, 1_000);

        // The `▶` selection marker MUST be painted — the whole point of the
        // scroll-follow is that the cursor never walks below the fold and vanishes.
        let marker_painted = buf.cells.iter().any(|(_, c)| c.symbol == "▶");
        assert!(
            marker_painted,
            "the selection marker must stay on-screen when the board overflows"
        );
    }

    /// The painted text of row 0, left to right (cells are painted in order).
    fn title_row_text(buf: &ainb_plugin_sdk::WireBuffer) -> String {
        let mut cells: Vec<_> = buf.cells.iter().filter(|(c, _)| c.y == 0).collect();
        cells.sort_by_key(|(c, _)| c.x);
        cells.iter().map(|(_, cell)| cell.symbol.as_str()).collect()
    }

    /// A non-delivered answer verdict is painted on the title row (the operator
    /// pressed a digit and the agent is STILL blocked); a delivered one clears it.
    #[test]
    fn answer_note_renders_on_the_title_row_until_cleared() {
        let mut state = ControlCenterState::default();
        state.set_attention(&[row(
            "a1",
            "ask_user_question",
            1,
            &ask_payload("q?", &["x", "y"]),
        )]);
        state.set_note("a1", "not delivered (no live session): target exited");
        let mut buf = ainb_plugin_sdk::WireBuffer::new(140, 20);
        render_control_center(&mut buf, 140, 0, 19, &state, 1_000);
        let title = title_row_text(&buf);
        assert!(
            title.contains("not delivered (no live session)"),
            "the refusal must be visible on the title row: {title:?}"
        );

        state.clear_note();
        let mut buf = ainb_plugin_sdk::WireBuffer::new(140, 20);
        render_control_center(&mut buf, 140, 0, 19, &state, 1_000);
        let title = title_row_text(&buf);
        assert!(
            !title.contains("not delivered"),
            "a delivered answer clears the note"
        );
    }

    /// The note is about ONE card: a refresh that still lists that card keeps
    /// it (the agent is still blocked), a refresh where the card is gone clears
    /// it (answered elsewhere or the session exited), so a stale refusal never
    /// hangs over the cards that come after.
    #[test]
    fn answer_note_clears_when_its_card_leaves_the_board() {
        let mut state = ControlCenterState::default();
        let a1 = row(
            "a1",
            "ask_user_question",
            1,
            &ask_payload("q?", &["x", "y"]),
        );
        let a2 = row(
            "a2",
            "ask_user_question",
            2,
            &ask_payload("r?", &["p", "q"]),
        );
        state.set_attention(&[a1.clone(), a2.clone()]);
        state.set_note("a1", "not delivered (ambiguous target): 2 sessions");
        state.set_attention(&[a1, a2.clone()]);
        assert!(
            state.note().is_some(),
            "card still open: the refusal stands"
        );
        state.set_attention(&[a2]);
        assert!(state.note().is_none(), "card gone: the refusal is stale");
    }

    #[test]
    fn control_chars_in_session_text_are_sanitized_on_render() {
        // A crafted IDLE payload carrying a raw ESC + BEL (an OSC 52 clipboard
        // write in the wild) in the assistant text the detail pane renders.
        let mut state = ControlCenterState::default();
        state.set_attention(&[row(
            "idle",
            "waiting",
            1,
            "{\"kind\":\"IDLE\",\"context\":{\"last_assistant_text\":\"\u{1b}]52;c;AAAA\u{07}\"}}",
        )]);
        let mut buf = ainb_plugin_sdk::WireBuffer::new(100, 20);
        render_control_center(&mut buf, 100, 0, 19, &state, 1_000);

        // No rendered cell may carry a control char — every one must have been
        // replaced with the visible placeholder before reaching the buffer.
        let has_control = buf.cells.iter().any(|(_, c)| c.symbol.chars().any(char::is_control));
        assert!(
            !has_control,
            "no control char may survive into a rendered cell"
        );
    }
}
