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

    /// The fixed-width badge label + its colour.
    #[must_use]
    const fn badge(self) -> (&'static str, Color) {
        match self {
            Self::Ask => ("ASK ", GOLD),
            Self::Approval => ("PERM", GOLD),
            Self::CodexRequestUser => ("CODX", CORNFLOWER_BLUE),
            Self::Error => ("ERR ", ALERT_RED),
            Self::Escalation => ("ESCL", ALERT_RED),
            Self::Waiting => ("WAIT", WAIT_AMBER),
            Self::Other => ("????", MUTED_GRAY),
        }
    }
}

/// One answer option on an ASK card (a label + an optional one-line description).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardOption {
    /// The option label — the exact text delivered as the answer.
    pub label: String,
    /// An optional descriptive sub-line.
    pub description: Option<String>,
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

    /// A short human label: the cwd's final path component, else the session id
    /// (truncated). Char-safe throughout.
    #[must_use]
    fn short_label(&self) -> String {
        let base = self.cwd.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or("");
        if base.is_empty() {
            truncate_chars(&self.session_id, 12)
        } else {
            base.to_string()
        }
    }

    /// The answer options, when this is an answerable ASK card.
    #[must_use]
    fn options(&self) -> &[CardOption] {
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
fn parse_body(payload: &str) -> CardBody {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return CardBody::Other {
            raw: truncate_chars(payload, 200),
        };
    };
    let tag = v.get("kind").and_then(serde_json::Value::as_str).unwrap_or("");
    let ctx = v.get("context").unwrap_or(&serde_json::Value::Null);
    match tag {
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
                    })
                })
                .collect()
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
}

impl ControlCenterState {
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

    /// The `(attention_id, answer_label)` the selected ASK's highlighted option
    /// would deliver, if a card + option are selected. `None` for a non-ASK card
    /// or an ASK with no options.
    #[must_use]
    pub fn pending_answer(&self) -> Option<(String, String)> {
        let card = self.selected_card()?;
        let label = card.options().get(self.option_cursor)?.label.clone();
        Some((card.id.clone(), label))
    }

    /// The `(attention_id, answer_label)` for a direct 1-based option pick on the
    /// selected ASK (the ①..⑨ number keys). `None` when the pick is out of range
    /// or the card is not an answerable ASK.
    #[must_use]
    pub fn answer_at(&self, one_based: usize) -> Option<(String, String)> {
        let card = self.selected_card()?;
        let idx = one_based.checked_sub(1)?;
        let label = card.options().get(idx)?.label.clone();
        Some((card.id.clone(), label))
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
/// │ WAIT ui       idle 7m  │ │  Ready to ship to which environment │
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
fn first_visible(selected: usize, visible_rows: usize) -> usize {
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
    let need = state.cards.iter().filter(|c| c.kind.urgency_rank() == 0).count();
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
        let (badge, badge_color) = card.kind.badge();
        let marker = if is_sel { "▶" } else { " " };
        let mut x = put_str(buf, 0, row, marker, SELECTION_GREEN, list_w);
        x = put_str(buf, x, row, badge, badge_color, list_w);
        x = put_str(buf, x, row, " ", MUTED_GRAY, list_w);
        let label = truncate_chars(&card.short_label(), 12);
        x = put_str(buf, x, row, &pad_to(&label, 12), SOFT_WHITE, list_w);
        x = put_str(buf, x, row, " ", MUTED_GRAY, list_w);
        // The age is the one live stat the attention feed carries.
        let age = format_age(now_ms.saturating_sub(card.created_at));
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
    let (badge, _) = card.kind.badge();
    let header = format!("{} · {}", card.short_label(), badge.trim());
    row = put_line(buf, x0, row, bottom, area_w, &header, GOLD);

    // Status line — coloured by kind (D9 "amber waiting / red error / …").
    let (status, status_color) = status_line(card);
    row = put_line(buf, x0, row, bottom, area_w, &status, status_color);

    // Stat strip: age (live) + source + workspace. The token/tool/diff columns
    // are the P10 observability fill-in (spec §4.9) — rendered as `—` so the
    // agentpeek strip shape is present without inventing numbers we don't have.
    let age = format_age(now_ms.saturating_sub(card.created_at));
    let source = if card.degraded { "~pane" } else { "hook" };
    let scope = card.workspace_id.as_deref().unwrap_or("host");
    let strip = format!("age {age} · tok — · tools — · Δ — · {source} · {scope}");
    row = put_line(buf, x0, row, bottom, area_w, &strip, MUTED_GRAY);
    row = row.saturating_add(1);

    // LAST REPLY.
    row = put_line(buf, x0, row, bottom, area_w, "LAST REPLY", GOLD);
    row = put_wrapped(buf, x0, row, bottom, area_w, &last_reply(card), SOFT_WHITE);
    row = row.saturating_add(1);

    // TIMELINE (the attention timeline; the per-tool JSONL timeline lands in P10).
    row = put_line(buf, x0, row, bottom, area_w, "TIMELINE", GOLD);
    let ago = format_age(now_ms.saturating_sub(card.created_at));
    let tl = format!("· raised {ago} ago · {}", kind_token(card.kind));
    row = put_line(buf, x0, row, bottom, area_w, &tl, MUTED_GRAY);
    row = row.saturating_add(1);

    // Inline ASK answering — options with ①②③, then the answer hints.
    render_options(buf, x0, row, bottom, area_w, card, state.option_cursor());
}

/// Render the ASK options with circled-digit glyphs + the answer hint bar, or a
/// note for a non-answerable card.
fn render_options(
    buf: &mut WireBuffer,
    x0: u16,
    top: u16,
    bottom: u16,
    area_w: u16,
    card: &AttentionCard,
    option_cursor: usize,
) {
    if !card.kind.is_answerable() {
        put_line(
            buf,
            x0,
            top,
            bottom,
            area_w,
            "(no inline options — surfaced for visibility)",
            MUTED_GRAY,
        );
        return;
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
        return;
    }

    let mut row = top;
    row = put_line(buf, x0, row, bottom, area_w, "OPTIONS", GOLD);
    for (i, opt) in options.iter().enumerate() {
        if row > bottom {
            break;
        }
        let selected = i == option_cursor;
        let color = if selected {
            SELECTION_GREEN
        } else {
            SOFT_WHITE
        };
        let glyph = circled_digit(i + 1);
        let mut x = put_str(buf, x0, row, &glyph, color, area_w);
        x = put_str(buf, x, row, " ", color, area_w);
        put_str(buf, x, row, &opt.label, color, area_w);
        row += 1;
        if let Some(desc) = &opt.description {
            if row <= bottom {
                let dx = put_str(buf, x0, row, "   ", MUTED_GRAY, area_w);
                put_str(buf, dx, row, &truncate_chars(desc, 48), MUTED_GRAY, area_w);
                row += 1;
            }
        }
    }
    // Hints next to the control (feedback_keybinding_hints_near_control).
    if row <= bottom {
        put_line(
            buf,
            x0,
            row,
            bottom,
            area_w,
            "h/l option · enter/1-9 answer",
            MUTED_GRAY,
        );
    }
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
fn last_reply(card: &AttentionCard) -> String {
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

/// Format an age in ms as a compact `Ns` / `Nm` / `Nh` string.
fn format_age(ms: i64) -> String {
    let secs = ms.max(0) / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
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
        // Harden against terminal escape / control-char injection: this screen
        // renders fleet-wide, session-originated free text (assistant replies,
        // error snippets, ASK labels, cwd-derived labels) char-by-char, and each
        // char becomes a Cell symbol the host paints verbatim. A raw ESC/BEL/C1
        // byte in a crafted attention payload would reassemble on flush into a
        // live control sequence (OSC 52 clipboard write, title set, cursor moves)
        // in the operator's terminal. Replace every control char (C0 + DEL + C1,
        // all Unicode category Cc) with a visible middot so the text is surfaced,
        // never executed. Applies to every field since put_str is the one choke
        // point all rendered strings flow through.
        let sym = if ch.is_control() { '·' } else { ch };
        let mut cell = Cell::new(sym.to_string());
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
        // the idle WAIT (rank 2). Needs-input to the top regardless of age.
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
        let has_control = buf
            .cells
            .iter()
            .any(|(_, c)| c.symbol.chars().any(char::is_control));
        assert!(
            !has_control,
            "no control char may survive into a rendered cell"
        );
    }
}
