//! P7 — Squads screen: the pure reducer + width-aware render (D17).
//!
//! The Squads screen (hotkey `S`) is the TUI surface over the daemon-native team
//! primitive: a workspace-scoped squad is a LEADER agent plus member actors, and
//! assigning an issue to a squad briefs the leader (and fans out to the agent
//! members). This screen lists every squad with its leader + members, each row
//! carrying a live presence dot (resolved from the cached `hangar/agents_list`
//! snapshot), and drives the squad RPCs:
//!
//!   * `c` — create a squad (inline name input; the daemon `squad_create`, with
//!     the leader chosen by the glue from the cached agents);
//!   * `a` — add a member to the selected squad (`squad_member_add`);
//!   * `d` — remove the selected MEMBER row (`squad_member_remove`);
//!   * `x` — assign the current issue to the selected squad — leader-routing
//!     dispatch that fans out to the members (`squad_fanout`).
//!
//! Like every Hangar screen the reducer ([`reduce_squads`]) is **pure**: it folds
//! a key event into a new [`SquadsState`] plus an optional [`SquadsIntent`] (which
//! the plugin glue lifts into the matching daemon JSON-RPC). The squad rows come
//! from the daemon (`hangar/squads_list`); the plugin owns zero domain data
//! (`project_ainb_plugin_owns_data_plane`). The leader/member/issue *selection*
//! policy for the create/add/assign verbs lives in the glue (which caches the
//! agents + issues), so the intents carry only ids — keeping this reducer free of
//! IO and of the actor catalogue.

use ainb_hangar_proto::events::{ActorRow, PresenceState};
use ainb_hangar_proto::snapshots::{SquadWireRow, SquadsListResult};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::widgets::presence_dot::presence_dot;

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Selected-row marker + leader-tag green (also the OK-note color).
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Primary row text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (headers, hints, empty state, member indent guides).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Error-note red — shared convention with `boards.rs` so a rejection never reads
/// as a success confirmation.
const ERROR_RED: Color = Color::rgb(235, 90, 90);

/// Whether a transient note reports a success or a failure. Drives the note color
/// so an error (`squad error: …`, `assign failed: …`, `no agent available …`) is
/// never painted the same green as a success confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoteKind {
    /// A success confirmation (rendered green).
    Ok,
    /// A rejection / failure (rendered red).
    Err,
}

/// A transient status note: its kind (drives the color) plus the message text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    /// Whether the note is a success or a failure.
    pub kind: NoteKind,
    /// The message rendered above the list.
    pub text: String,
}

/// One actor (leader or member) resolved for render: its canonical ref, a display
/// name, live presence, and whether it is an agent (a human `member` carries no
/// runtime and so cannot be dispatched to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquadActor {
    /// The canonical actor-ref (`agent:<id>` / `member:<id>`).
    pub actor_ref: String,
    /// The display name (resolved from the actor snapshot, else the raw ref).
    pub display: String,
    /// Live presence (drives the inline 3-state dot).
    pub presence: PresenceState,
    /// `true` for an `agent`, `false` for a human `member`.
    pub is_agent: bool,
}

/// One squad resolved for render: its id + name, the leader, and the members.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquadView {
    /// The squad's id (`squad.id`).
    pub id: String,
    /// The squad's name (unique within its workspace).
    pub name: String,
    /// The squad's leader (the actor a squad-assigned task briefs).
    pub leader: SquadActor,
    /// The squad's members (may be empty).
    pub members: Vec<SquadActor>,
}

/// A flattened, keyboard-navigable row: a squad header or one of its members.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// A squad header row (carries the squad index).
    Header(usize),
    /// A member row (carries the squad index + the member index within it).
    Member(usize, usize),
}

/// The render-state cache for the Squads screen.
///
/// Holds the resolved squads, the flat-list selection, an optional create-name
/// input buffer (present only while creating), and a transient note (e.g. the last
/// assignment confirmation). All fields private; tests and the renderer read
/// through accessors. The vertical scroll offset is NOT held here — it is derived
/// per render from the selection + viewport height (see [`render_squads`]), the
/// same viewport-blind convention as `control_center.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SquadsState {
    squads: Vec<SquadView>,
    /// Selection index into the flattened [`Self::rows`] list.
    selected: usize,
    /// The create-squad name buffer — `Some` only while the create input is open.
    create_input: Option<String>,
    /// The create-AGENT name buffer — `Some` only while the `n` agent-create input
    /// is open. Mutually exclusive with `create_input` (only one input at a time).
    agent_input: Option<String>,
    /// A transient status note (last assignment / add / error), rendered above the
    /// list; its kind drives the color (green = ok, red = error).
    note: Option<Note>,
}

impl SquadsState {
    /// A fresh screen over `squads`, first row selected, no create input.
    #[must_use]
    pub fn new(squads: Vec<SquadView>) -> Self {
        Self {
            squads,
            selected: 0,
            create_input: None,
            agent_input: None,
            note: None,
        }
    }

    /// Build the screen from a `hangar/squads_list` snapshot, resolving each
    /// leader/member actor-ref against the cached `hangar/agents_list` actors so a
    /// row carries a real display name + live presence. An unknown ref falls back
    /// to the raw ref with an offline dot (never silently dropped).
    #[must_use]
    pub fn from_snapshot(snapshot: &SquadsListResult, actors: &[ActorRow]) -> Self {
        let squads = snapshot.squads.iter().map(|s| resolve_squad(s, actors)).collect();
        Self::new(squads)
    }

    /// The resolved squads.
    #[must_use]
    pub fn squads(&self) -> &[SquadView] {
        &self.squads
    }

    /// The current flat-list selection index.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the create-name input is open.
    #[must_use]
    pub const fn is_creating(&self) -> bool {
        self.create_input.is_some()
    }

    /// The current create-name buffer, if the input is open.
    #[must_use]
    pub fn create_buffer(&self) -> Option<&str> {
        self.create_input.as_deref()
    }

    /// The current create-AGENT name buffer, if the `n` input is open.
    #[must_use]
    pub fn agent_buffer(&self) -> Option<&str> {
        self.agent_input.as_deref()
    }

    /// Restore (or clear) the create-AGENT input buffer after a refresh, so a
    /// snapshot arriving mid-typing does not wipe the half-typed name.
    pub fn set_agent_buffer(&mut self, buf: Option<String>) {
        self.agent_input = buf;
    }

    /// Set (or clear) the transient status note the render shows above the list.
    /// Used to preserve the note across a snapshot refresh; call [`Self::note_ok`]
    /// / [`Self::note_err`] to raise a fresh one.
    pub fn set_note(&mut self, note: Option<Note>) {
        self.note = note;
    }

    /// Raise a success note (rendered green).
    pub fn note_ok(&mut self, text: impl Into<String>) {
        self.note = Some(Note {
            kind: NoteKind::Ok,
            text: text.into(),
        });
    }

    /// Raise an error note (rendered red) — a rejection never reads as a success.
    pub fn note_err(&mut self, text: impl Into<String>) {
        self.note = Some(Note {
            kind: NoteKind::Err,
            text: text.into(),
        });
    }

    /// Clear the transient note.
    pub fn clear_note(&mut self) {
        self.note = None;
    }

    /// Restore the flat-list selection after a snapshot refresh, clamped to the
    /// new row range (so a selection past a since-shrunk list lands on the last
    /// row rather than out of bounds).
    pub fn set_selected(&mut self, idx: usize) {
        let len = self.rows().len();
        self.selected = if len == 0 { 0 } else { idx.min(len - 1) };
    }

    /// Restore (or clear) the create-name input buffer after a refresh, so a
    /// snapshot arriving mid-typing does not wipe the half-typed name.
    pub fn set_create_buffer(&mut self, buf: Option<String>) {
        self.create_input = buf;
    }

    /// The transient status note, if any (its kind drives the render color).
    #[must_use]
    pub fn note(&self) -> Option<&Note> {
        self.note.as_ref()
    }

    /// The flattened, navigable rows: each squad's header followed by its members.
    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (si, squad) in self.squads.iter().enumerate() {
            rows.push(Row::Header(si));
            for mi in 0..squad.members.len() {
                rows.push(Row::Member(si, mi));
            }
        }
        rows
    }

    /// The squad that OWNS the selected row (a header selects its own squad; a
    /// member row selects its parent squad). `None` when the list is empty.
    #[must_use]
    pub fn selected_squad(&self) -> Option<&SquadView> {
        match self.rows().get(self.selected)? {
            Row::Header(si) | Row::Member(si, _) => self.squads.get(*si),
        }
    }

    /// The member under the selection, if the selected row is a member row (not a
    /// squad header). Returns `(squad_id, member_ref)`.
    #[must_use]
    pub fn selected_member(&self) -> Option<(&str, &str)> {
        match self.rows().get(self.selected)? {
            Row::Member(si, mi) => {
                let squad = self.squads.get(*si)?;
                let member = squad.members.get(*mi)?;
                Some((squad.id.as_str(), member.actor_ref.as_str()))
            }
            Row::Header(_) => None,
        }
    }

    /// Move the flat-list selection by `delta`, clamped to the row range.
    fn move_selection(&mut self, delta: i32) {
        let len = self.rows().len();
        if len == 0 {
            return;
        }
        let max = i32::try_from(len - 1).unwrap_or(0);
        let cur = i32::try_from(self.selected).unwrap_or(0);
        self.selected = usize::try_from((cur + delta).clamp(0, max)).unwrap_or(0);
    }

    /// Move the flat-list selection onto the squad carrying `id` (its header row),
    /// so a mouse click / external jump lands the selection there. A no-op when no
    /// squad carries that id.
    pub fn select_squad_by_id(&mut self, id: &str) {
        if let Some(pos) = self
            .rows()
            .iter()
            .position(|r| matches!(r, Row::Header(si) if self.squads[*si].id == id))
        {
            self.selected = pos;
        }
    }
}

/// Resolve one wire squad row against the actor snapshot.
fn resolve_squad(row: &SquadWireRow, actors: &[ActorRow]) -> SquadView {
    SquadView {
        id: row.id.clone(),
        name: row.name.clone(),
        leader: resolve_actor(&row.leader, actors),
        members: row.members.iter().map(|m| resolve_actor(m, actors)).collect(),
    }
}

/// Resolve one canonical actor-ref against the actor snapshot: a matching row
/// lends its display name + presence, an unknown ref falls back to the raw ref
/// with an offline dot (its kind still read from the `agent:` / `member:` prefix).
fn resolve_actor(actor_ref: &str, actors: &[ActorRow]) -> SquadActor {
    let is_agent = actor_ref.starts_with("agent:");
    actors.iter().find(|a| a.actor_ref == actor_ref).map_or_else(
        || SquadActor {
            actor_ref: actor_ref.to_string(),
            display: actor_ref.to_string(),
            presence: PresenceState::Offline,
            is_agent,
        },
        |a| SquadActor {
            actor_ref: actor_ref.to_string(),
            display: a.display_name.clone(),
            presence: a.presence,
            is_agent: a.is_agent,
        },
    )
}

/// An input the squads reducer folds into [`SquadsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SquadsEvent {
    /// A printable key (`'j'`, `'c'`, `'x'`, …) — or `'\n'` (Enter) / `'\u{8}'`
    /// (Backspace) while the create input is open.
    Key(char),
    /// The Escape key (cancels the create input; a no-op otherwise).
    Esc,
}

/// A side-effect the plugin glue performs after a squads reduction.
///
/// Each variant maps to one daemon squad RPC the glue fires. The leader (create),
/// member (add), and issue (assign) *selection* is resolved by the glue from its
/// cached agents/issues, so these intents carry only ids — the reducer stays free
/// of the actor catalogue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SquadsIntent {
    /// Create an AGENT named `name` (`n` + Enter) — `hangar/agent_create`; the glue
    /// fires it with no ids (the daemon fills workspace/runtime/owner) and folds the
    /// refreshed roster back, so the "no agent available" squad gate clears live.
    CreateAgent {
        /// The new agent's name (non-blank).
        name: String,
    },
    /// Create a squad named `name` (`c` + Enter) — `hangar/squad_create`; the glue
    /// picks the leader from the cached agents.
    CreateSquad {
        /// The new squad's name (non-blank).
        name: String,
    },
    /// Add a member to `squad_id` (`a`) — `hangar/squad_member_add`; the glue picks
    /// the next cached agent not already on the squad.
    AddMember {
        /// The squad to add a member to.
        squad_id: String,
    },
    /// Remove `member_ref` from `squad_id` (`d` on a member row) —
    /// `hangar/squad_member_remove`.
    RemoveMember {
        /// The squad to remove the member from.
        squad_id: String,
        /// The member actor-ref to remove.
        member_ref: String,
    },
    /// Assign the current issue to `squad_id` (`x`) — `hangar/squad_fanout`; the
    /// glue picks the issue and fans the brief to the leader + agent members.
    AssignIssue {
        /// The squad the issue is assigned to.
        squad_id: String,
    },
}

/// The result of folding one [`SquadsEvent`] into a [`SquadsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SquadsReduction {
    /// The next state.
    pub state: SquadsState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<SquadsIntent>,
}

/// Fold one [`SquadsEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_squads(state: &SquadsState, ev: SquadsEvent) -> SquadsReduction {
    match ev {
        SquadsEvent::Key(c) => {
            if state.agent_input.is_some() {
                reduce_agent_key(state, c)
            } else if state.create_input.is_some() {
                reduce_create_key(state, c)
            } else {
                reduce_key(state, c)
            }
        }
        SquadsEvent::Esc => reduce_esc(state),
    }
}

/// Handle a key while the create-name input is open: Enter submits (when
/// non-blank), Backspace deletes, any other printable char appends.
fn reduce_create_key(state: &SquadsState, c: char) -> SquadsReduction {
    let mut buf = state.create_input.clone().unwrap_or_default();
    match c {
        '\n' => {
            let name = buf.trim().to_string();
            if name.is_empty() {
                // Blank submit is a no-op — keep the input open.
                return unchanged(state);
            }
            let mut next = state.clone();
            next.create_input = None;
            with_intent(next, SquadsIntent::CreateSquad { name })
        }
        '\u{8}' => {
            buf.pop();
            let mut next = state.clone();
            next.create_input = Some(buf);
            no_intent(next)
        }
        c if !c.is_control() => {
            buf.push(c);
            let mut next = state.clone();
            next.create_input = Some(buf);
            no_intent(next)
        }
        _ => unchanged(state),
    }
}

/// Handle a key while the create-AGENT input is open: Enter submits (when
/// non-blank) and emits [`SquadsIntent::CreateAgent`], Backspace deletes, any
/// other printable char appends. Mirrors [`reduce_create_key`] exactly so the
/// Esc-cancels-in-one-press semantics are identical.
fn reduce_agent_key(state: &SquadsState, c: char) -> SquadsReduction {
    let mut buf = state.agent_input.clone().unwrap_or_default();
    match c {
        '\n' => {
            let name = buf.trim().to_string();
            if name.is_empty() {
                // Blank submit is a no-op — keep the input open.
                return unchanged(state);
            }
            let mut next = state.clone();
            next.agent_input = None;
            with_intent(next, SquadsIntent::CreateAgent { name })
        }
        '\u{8}' => {
            buf.pop();
            let mut next = state.clone();
            next.agent_input = Some(buf);
            no_intent(next)
        }
        c if !c.is_control() => {
            buf.push(c);
            let mut next = state.clone();
            next.agent_input = Some(buf);
            no_intent(next)
        }
        _ => unchanged(state),
    }
}

/// Handle a normal-mode key (P7 bindings).
fn reduce_key(state: &SquadsState, c: char) -> SquadsReduction {
    match c {
        'n' => {
            let mut next = state.clone();
            next.agent_input = Some(String::new());
            no_intent(next)
        }
        'j' => {
            let mut next = state.clone();
            next.move_selection(1);
            no_intent(next)
        }
        'k' => {
            let mut next = state.clone();
            next.move_selection(-1);
            no_intent(next)
        }
        'c' => {
            let mut next = state.clone();
            next.create_input = Some(String::new());
            no_intent(next)
        }
        'a' => state.selected_squad().map_or_else(
            || unchanged(state),
            |s| {
                with_intent(
                    state.clone(),
                    SquadsIntent::AddMember {
                        squad_id: s.id.clone(),
                    },
                )
            },
        ),
        'd' => state.selected_member().map_or_else(
            || unchanged(state),
            |(squad_id, member_ref)| {
                with_intent(
                    state.clone(),
                    SquadsIntent::RemoveMember {
                        squad_id: squad_id.to_string(),
                        member_ref: member_ref.to_string(),
                    },
                )
            },
        ),
        'x' => state.selected_squad().map_or_else(
            || unchanged(state),
            |s| {
                with_intent(
                    state.clone(),
                    SquadsIntent::AssignIssue {
                        squad_id: s.id.clone(),
                    },
                )
            },
        ),
        _ => unchanged(state),
    }
}

/// Handle Esc: cancel whichever input (agent-create or squad-create) is open in
/// a SINGLE press; a no-op otherwise. Esc never steps back through state — it
/// closes the open input outright.
fn reduce_esc(state: &SquadsState) -> SquadsReduction {
    if state.agent_input.is_some() {
        let mut next = state.clone();
        next.agent_input = None;
        no_intent(next)
    } else if state.create_input.is_some() {
        let mut next = state.clone();
        next.create_input = None;
        no_intent(next)
    } else {
        unchanged(state)
    }
}

/// A reduction that changes state but emits no intent.
fn no_intent(state: SquadsState) -> SquadsReduction {
    SquadsReduction {
        state,
        intent: None,
    }
}

/// A reduction carrying `intent` alongside `state`.
fn with_intent(state: SquadsState, intent: SquadsIntent) -> SquadsReduction {
    SquadsReduction {
        state,
        intent: Some(intent),
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &SquadsState) -> SquadsReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware render
// ---------------------------------------------------------------------------

/// Render the Squads screen into `buf` between rows `top` and `bottom`.
///
/// The header row carries the action-key hints right-aligned beside the controls
/// they drive. Below it, an optional transient note, then either the create-name
/// input (when open), the empty-state help line (no squads), or the flat list of
/// squad headers + member rows. Each squad header shows the squad name, a
/// `leader:` tag, and the leader's presence dot + name; each member row is
/// indented with its own presence dot. The selected row carries the `▶` marker.
pub fn render_squads(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &SquadsState,
) {
    render_action_hints(buf, top, area_w);
    let mut row = top + 1;

    // Transient note (assignment / add confirmation or a rejection), if any. Its
    // kind drives the color: green for a success, red for an error — so a rejection
    // is never painted the same as a confirmation.
    if let Some(note) = state.note() {
        let color = match note.kind {
            NoteKind::Ok => SELECTION_GREEN,
            NoteKind::Err => ERROR_RED,
        };
        put_str(buf, 0, row, &note.text, color, area_w);
        row = row.saturating_add(1);
    }

    // Create-AGENT input takes over the body while open (the `n` prompt).
    if let Some(buffer) = state.agent_buffer() {
        let line = format!("New agent name: {buffer}▏");
        put_str(
            buf,
            0,
            row,
            "Enter an agent name, Esc to cancel",
            MUTED_GRAY,
            area_w,
        );
        put_str(buf, 0, row.saturating_add(1), &line, GOLD, area_w);
        return;
    }

    // Create-squad name input takes over the body while open.
    if let Some(buffer) = state.create_buffer() {
        let line = format!("New squad name: {buffer}▏");
        put_str(
            buf,
            0,
            row,
            "Enter a squad name, Esc to cancel",
            MUTED_GRAY,
            area_w,
        );
        put_str(buf, 0, row.saturating_add(1), &line, GOLD, area_w);
        return;
    }

    if state.squads.is_empty() {
        put_str(
            buf,
            0,
            row,
            "No squads. Press 'n' to create an agent, 'c' to create a squad",
            MUTED_GRAY,
            area_w,
        );
        return;
    }

    let rows = state.rows();
    // Follow the selection: derive the first-visible offset from the selection +
    // remaining viewport height so the `▶` cursor stays on-screen even when the
    // squads + members overflow the pane (state stays viewport-blind, the same
    // convention as `control_center.rs`).
    let visible_rows = usize::from(bottom.saturating_sub(row));
    let visible_from = first_visible(state.selected, visible_rows);
    for (idx, r) in rows.iter().enumerate().skip(visible_from) {
        if row >= bottom {
            break;
        }
        let selected = idx == state.selected;
        match r {
            Row::Header(si) => render_header(buf, row, area_w, &state.squads[*si], selected),
            Row::Member(si, mi) => {
                render_member(buf, row, area_w, &state.squads[*si].members[*mi], selected);
            }
        }
        row = row.saturating_add(1);
    }
}

/// The first-visible row index for a viewport of `visible_rows` rows that must
/// keep `selected` on-screen. While the selection sits within the first
/// `visible_rows` rows the list is top-anchored (offset `0`); past that the offset
/// tracks so `selected` lands on the last visible row (never below it). A
/// zero-height viewport pins the offset to the selection.
const fn first_visible(selected: usize, visible_rows: usize) -> usize {
    if visible_rows == 0 {
        return selected;
    }
    selected.saturating_sub(visible_rows - 1)
}

/// Paint the action-key hints on the top row, right-aligned so each key sits
/// beside the controls it drives (`feedback_keybinding_hints_near_control`).
/// Dropped on a terminal too narrow to hold them (the footer carries them too).
fn render_action_hints(buf: &mut WireBuffer, row: u16, area_w: u16) {
    const HINTS: &str = "[n]ew-agent [c]reate [a]dd [d]el [x]assign";
    let hint_w = u16::try_from(HINTS.chars().count()).unwrap_or(0);
    if hint_w >= area_w {
        return;
    }
    put_str(buf, area_w - hint_w, row, HINTS, GOLD, area_w);
}

/// Render one squad header row: `▶ <name>  leader: <dot> <leader>  (N members)`.
fn render_header(buf: &mut WireBuffer, row: u16, area_w: u16, squad: &SquadView, selected: bool) {
    let mut x = 0u16;
    x = put_str(
        buf,
        x,
        row,
        if selected { "▶ " } else { "  " },
        SELECTION_GREEN,
        area_w,
    );
    x = put_str(
        buf,
        x,
        row,
        &squad.name,
        if selected { GOLD } else { SOFT_WHITE },
        area_w,
    );
    x = put_str(buf, x, row, "  leader: ", MUTED_GRAY, area_w);
    let (glyph, color) = presence_dot(squad.leader.presence);
    x = put_cell(buf, x, row, glyph, color, area_w);
    x = put_str(buf, x, row, " ", MUTED_GRAY, area_w);
    x = put_str(buf, x, row, &squad.leader.display, SOFT_WHITE, area_w);
    let count = format!(
        "  ({} member{})",
        squad.members.len(),
        plural(squad.members.len())
    );
    put_str(buf, x, row, &count, MUTED_GRAY, area_w);
}

/// Render one member row: `    <dot> <member> [agent|human]`, indented under its
/// squad, the `▶` marker when selected.
fn render_member(buf: &mut WireBuffer, row: u16, area_w: u16, member: &SquadActor, selected: bool) {
    let mut x = 0u16;
    x = put_str(
        buf,
        x,
        row,
        if selected { "  ▶ " } else { "    " },
        SELECTION_GREEN,
        area_w,
    );
    x = put_str(buf, x, row, "└ ", MUTED_GRAY, area_w);
    let (glyph, color) = presence_dot(member.presence);
    x = put_cell(buf, x, row, glyph, color, area_w);
    x = put_str(buf, x, row, " ", MUTED_GRAY, area_w);
    x = put_str(buf, x, row, &member.display, SOFT_WHITE, area_w);
    let tag = if member.is_agent {
        "  · agent"
    } else {
        "  · human"
    };
    put_str(buf, x, row, tag, MUTED_GRAY, area_w);
}

/// The plural suffix for a count (`""` at 1, else `"s"`).
const fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// Write a single glyph at `(x, row)` in `color`, clipping at `area_w`. Returns
/// the next free column.
fn put_cell(buf: &mut WireBuffer, x: u16, row: u16, ch: char, color: Color, area_w: u16) -> u16 {
    if x >= area_w {
        return x;
    }
    let mut cell = Cell::new(ch.to_string());
    cell.fg = Some(color);
    buf.push(Coord::new(x, row), cell);
    x.saturating_add(1)
}

/// Write `s` at `(x, row)` in `color`, clipping at `area_w`. Returns the next free
/// column. Char-safe (iterates `char`s, not bytes).
fn put_str(buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color, area_w: u16) -> u16 {
    let mut cx = x;
    for ch in s.chars() {
        if cx >= area_w {
            break;
        }
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(color);
        buf.push(Coord::new(cx, row), cell);
        cx = cx.saturating_add(1);
    }
    cx
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor(actor_ref: &str, name: &str, presence: PresenceState, is_agent: bool) -> ActorRow {
        ActorRow {
            actor_ref: actor_ref.into(),
            display_name: name.into(),
            subtitle: String::new(),
            presence,
            workload: ainb_hangar_proto::events::Workload::Idle,
            is_agent,
            recent_rank: None,
        }
    }

    fn wire_squad(id: &str, name: &str, leader: &str, members: &[&str]) -> SquadWireRow {
        SquadWireRow {
            id: id.into(),
            name: name.into(),
            leader: leader.into(),
            members: members.iter().map(|m| (*m).to_string()).collect(),
        }
    }

    fn snapshot() -> SquadsListResult {
        SquadsListResult {
            squads: vec![
                wire_squad(
                    "s1",
                    "shippers",
                    "agent:a-lead",
                    &["agent:a-1", "member:u-1"],
                ),
                wire_squad("s2", "reviewers", "agent:a-rev", &[]),
            ],
        }
    }

    fn actors() -> Vec<ActorRow> {
        vec![
            actor("agent:a-lead", "lead-bot", PresenceState::Online, true),
            actor("agent:a-1", "worker-bot", PresenceState::Unstable, true),
            actor("agent:a-rev", "review-bot", PresenceState::Offline, true),
            actor("member:u-1", "alice", PresenceState::Online, false),
        ]
    }

    /// `from_snapshot` resolves each leader/member ref to its display + presence,
    /// falling back to the raw ref (offline) for an unknown actor.
    #[test]
    fn from_snapshot_resolves_actors_and_falls_back() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        assert_eq!(state.squads().len(), 2);
        let s1 = &state.squads()[0];
        assert_eq!(s1.leader.display, "lead-bot");
        assert_eq!(s1.leader.presence, PresenceState::Online);
        assert_eq!(s1.members.len(), 2);
        assert_eq!(s1.members[0].display, "worker-bot");
        assert_eq!(s1.members[0].presence, PresenceState::Unstable);
        assert!(!s1.members[1].is_agent, "the human member is not an agent");

        // An unknown ref falls back to the raw ref with an offline dot.
        let orphan = SquadsListResult {
            squads: vec![wire_squad("s3", "ghosts", "agent:missing", &[])],
        };
        let st = SquadsState::from_snapshot(&orphan, &actors());
        assert_eq!(st.squads()[0].leader.display, "agent:missing");
        assert_eq!(st.squads()[0].leader.presence, PresenceState::Offline);
    }

    /// `j`/`k` navigate the FLAT list (squad headers + member rows), clamped.
    #[test]
    fn navigation_walks_headers_and_members_clamped() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        // Rows: s1 header, s1/a-1, s1/u-1, s2 header  → 4 rows.
        assert_eq!(state.selected_index(), 0);
        let down = |s: &SquadsState| reduce_squads(s, SquadsEvent::Key('j')).state;
        let s = down(&state); // s1/a-1
        let s = down(&s); // s1/u-1
        let s = down(&s); // s2 header
        assert_eq!(s.selected_index(), 3);
        // Clamped at the last row.
        let s = down(&s);
        assert_eq!(s.selected_index(), 3);
        // Back up to the top.
        let up = reduce_squads(&s, SquadsEvent::Key('k')).state;
        assert_eq!(up.selected_index(), 2);
    }

    /// `selected_squad` follows the selection onto a member's parent squad, and
    /// `selected_member` is `Some` only on a member row.
    #[test]
    fn selection_resolves_owning_squad_and_member() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        // Header row: owns s1, no member.
        assert_eq!(state.selected_squad().unwrap().id, "s1");
        assert!(state.selected_member().is_none());
        // Move onto s1's first member row.
        let s = reduce_squads(&state, SquadsEvent::Key('j')).state;
        assert_eq!(s.selected_squad().unwrap().id, "s1");
        assert_eq!(s.selected_member(), Some(("s1", "agent:a-1")));
    }

    /// `c` opens the create input; typing + Enter raises `CreateSquad`; Esc cancels.
    #[test]
    fn create_input_flow_raises_intent_and_cancels() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        let opened = reduce_squads(&state, SquadsEvent::Key('c')).state;
        assert!(opened.is_creating());

        // Type "qa".
        let typed = reduce_squads(&opened, SquadsEvent::Key('q')).state;
        let typed = reduce_squads(&typed, SquadsEvent::Key('a')).state;
        assert_eq!(typed.create_buffer(), Some("qa"));

        // Enter submits and closes the input.
        let out = reduce_squads(&typed, SquadsEvent::Key('\n'));
        assert_eq!(
            out.intent,
            Some(SquadsIntent::CreateSquad { name: "qa".into() })
        );
        assert!(!out.state.is_creating());

        // Esc on an open input cancels with no intent.
        let cancel = reduce_squads(&opened, SquadsEvent::Esc);
        assert!(!cancel.state.is_creating());
        assert!(cancel.intent.is_none());
    }

    /// `n` opens the create-AGENT input; typing + Enter raises a `CreateAgent`
    /// intent; a single Esc cancels it outright (never stepping back).
    #[test]
    fn create_agent_flow_raises_intent_and_esc_cancels_in_one_press() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        let opened = reduce_squads(&state, SquadsEvent::Key('n')).state;
        assert_eq!(
            opened.agent_buffer(),
            Some(""),
            "n opens the agent-create input"
        );
        assert!(!opened.is_creating(), "the squad-create input stays closed");

        // Type "bot".
        let typed = reduce_squads(&opened, SquadsEvent::Key('b')).state;
        let typed = reduce_squads(&typed, SquadsEvent::Key('o')).state;
        let typed = reduce_squads(&typed, SquadsEvent::Key('t')).state;
        assert_eq!(typed.agent_buffer(), Some("bot"));

        // Enter submits and closes the input.
        let out = reduce_squads(&typed, SquadsEvent::Key('\n'));
        assert_eq!(
            out.intent,
            Some(SquadsIntent::CreateAgent { name: "bot".into() })
        );
        assert!(out.state.agent_buffer().is_none(), "input closes on submit");

        // A SINGLE Esc on the open input cancels with no intent.
        let cancel = reduce_squads(&typed, SquadsEvent::Esc);
        assert!(
            cancel.state.agent_buffer().is_none(),
            "one Esc closes the agent input"
        );
        assert!(cancel.intent.is_none());
    }

    /// A blank agent-create submit is a no-op — the input stays open, no intent.
    #[test]
    fn blank_agent_create_submit_is_a_noop() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        let opened = reduce_squads(&state, SquadsEvent::Key('n')).state;
        let out = reduce_squads(&opened, SquadsEvent::Key('\n'));
        assert!(out.intent.is_none());
        assert_eq!(
            out.state.agent_buffer(),
            Some(""),
            "blank submit keeps the input open"
        );
    }

    /// A blank create submit is a no-op — the input stays open, no intent.
    #[test]
    fn blank_create_submit_is_a_noop() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        let opened = reduce_squads(&state, SquadsEvent::Key('c')).state;
        let out = reduce_squads(&opened, SquadsEvent::Key('\n'));
        assert!(out.intent.is_none());
        assert!(out.state.is_creating(), "blank submit keeps the input open");
    }

    /// `a` adds a member to the selected squad; `x` assigns; both carry the squad id.
    #[test]
    fn add_and_assign_carry_the_selected_squad() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        let add = reduce_squads(&state, SquadsEvent::Key('a'));
        assert_eq!(
            add.intent,
            Some(SquadsIntent::AddMember {
                squad_id: "s1".into()
            })
        );
        let assign = reduce_squads(&state, SquadsEvent::Key('x'));
        assert_eq!(
            assign.intent,
            Some(SquadsIntent::AssignIssue {
                squad_id: "s1".into()
            })
        );
    }

    /// `d` removes only when a MEMBER row is selected; on a header it is a no-op.
    #[test]
    fn remove_only_on_a_member_row() {
        let state = SquadsState::from_snapshot(&snapshot(), &actors());
        // On the header: no-op.
        assert!(reduce_squads(&state, SquadsEvent::Key('d')).intent.is_none());
        // Move onto a member row and remove.
        let s = reduce_squads(&state, SquadsEvent::Key('j')).state;
        let out = reduce_squads(&s, SquadsEvent::Key('d'));
        assert_eq!(
            out.intent,
            Some(SquadsIntent::RemoveMember {
                squad_id: "s1".into(),
                member_ref: "agent:a-1".into(),
            })
        );
    }

    /// `note_ok` / `note_err` carry the kind so the render can distinguish a
    /// success confirmation from a rejection (they are NOT the same color).
    #[test]
    fn note_kind_distinguishes_ok_from_err() {
        let mut state = SquadsState::from_snapshot(&snapshot(), &actors());
        state.note_ok("briefed lead-bot + 2 members");
        assert_eq!(
            state.note(),
            Some(&Note {
                kind: NoteKind::Ok,
                text: "briefed lead-bot + 2 members".into(),
            })
        );
        state.note_err("squad error: duplicate name");
        assert_eq!(state.note().map(|n| n.kind), Some(NoteKind::Err));
        state.clear_note();
        assert!(state.note().is_none());
    }

    /// The derived scroll offset follows the selection: top-anchored while it fits,
    /// then tracking so the selected row lands on the last visible row.
    #[test]
    fn first_visible_follows_the_selection_past_the_fold() {
        // Selection within the first `visible` rows → top-anchored.
        assert_eq!(first_visible(0, 5), 0);
        assert_eq!(first_visible(4, 5), 0);
        // Past the fold: the selected row pins to the last visible row.
        assert_eq!(first_visible(5, 5), 1);
        assert_eq!(first_visible(20, 5), 16);
        // A zero-height viewport pins the offset to the selection (no underflow).
        assert_eq!(first_visible(7, 0), 7);
    }

    /// `select_squad_by_id` lands the selection on the squad's header row.
    #[test]
    fn select_by_id_lands_on_the_header() {
        let mut state = SquadsState::from_snapshot(&snapshot(), &actors());
        state.select_squad_by_id("s2");
        // s2's header is row index 3 (s1 header + 2 members + s2 header).
        assert_eq!(state.selected_index(), 3);
        assert_eq!(state.selected_squad().unwrap().id, "s2");
        // Unknown id is a no-op.
        state.select_squad_by_id("ghost");
        assert_eq!(state.selected_index(), 3);
    }
}
