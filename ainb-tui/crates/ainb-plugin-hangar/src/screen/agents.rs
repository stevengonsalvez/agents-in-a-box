//! Agents screen (hotkey `A`, slice 2): a first-class roster of the workspace's
//! named agents with inline create + delete.
//!
//! Before this screen the only TUI path to a named agent was Squads → `n`, which
//! nobody guessed. The Agents screen is the obvious place to SEE, CREATE, and
//! REMOVE agents. It lists every named agent from the SAME data source the pickers
//! use — the cached `hangar/agents_list` snapshot (`ActorRow`s, filtered to
//! `is_agent`) — so it invents no new list RPC. Each row carries the agent's name,
//! its subtitle, and a live presence dot resolved from that snapshot.
//!
//! Like every Hangar screen the reducer ([`reduce_agents`]) is **pure**: it folds a
//! key event into a new [`AgentsState`] plus an optional [`AgentsIntent`] (which the
//! plugin glue lifts into the matching daemon JSON-RPC — `hangar/agent_create` for
//! `n`, `hangar/agent_delete` for a confirmed `x`). The plugin owns zero domain
//! data (`project_ainb_plugin_owns_data_plane`); the roster comes from the daemon.
//!
//! Keys:
//!   * `n` — create: a guided wizard (name → provider picker → model →
//!     instructions → confirm) that collects the real config fields, not just a
//!     name, and shows a structured draft for review before Enter on the confirm
//!     step emits [`AgentsIntent::CreateAgent`]. The faithful hangar analog of
//!     multica's chat → structured-draft → confirm builder, minus the LLM turn.
//!   * `x` — delete the selected agent behind a confirm overlay (the issue-list
//!     delete-confirm pattern); Enter confirms → [`AgentsIntent::DeleteAgent`].
//!   * `j`/`k` — move the selection.
//!   * `Esc` — cancel whichever overlay (create input / delete confirm) is open in
//!     a single press; never traps the user (a bare Esc leaves navigation intact,
//!     the footer advertises the tab hotkeys + `q` to leave).

use ainb_hangar_proto::events::{ActorRow, PresenceState, Workload};
use ainb_plugin_sdk::{Cell, Color, Coord, WireBuffer};

use crate::widgets::presence_dot::presence_dot;

/// Title / accent gold.
const GOLD: Color = Color::rgb(255, 215, 0);
/// Selected-row marker green.
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);
/// Primary row text.
const SOFT_WHITE: Color = Color::rgb(220, 220, 230);
/// Muted text (headers, hints, empty state, subtitles).
const MUTED_GRAY: Color = Color::rgb(120, 120, 140);
/// Destructive-confirm red — a delete prompt is never painted like a success.
const ERROR_RED: Color = Color::rgb(235, 90, 90);
/// Workload `queued` amber — waiting-to-work, distinct from the `unstable`
/// availability dot (which is a runtime-degraded signal, never workload).
const WORKLOAD_AMBER: Color = Color::rgb(230, 180, 60);

/// One agent resolved for render: its canonical ref, display name, subtitle, and
/// live presence (drives the inline 3-state dot).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentView {
    /// The canonical actor-ref (`agent:<id>`).
    pub actor_ref: String,
    /// The display name (from the `agents_list` snapshot).
    pub name: String,
    /// A short subtitle (the snapshot's `subtitle`, e.g. `agent`).
    pub subtitle: String,
    /// Live presence — the availability dimension (drives the inline dot).
    pub presence: PresenceState,
    /// Live workload — the orthogonal second dimension (multica `Workload`),
    /// derived by the daemon from the agent's live task counts.
    pub workload: Workload,
}

/// The fixed provider choices the create wizard cycles through — mirrors
/// `bootstrap::SUPPORTED_PROVIDERS` so the daemon's `normalize_provider` never
/// rejects a wizard pick (the picker can only land on a supported value).
const SUPPORTED_PROVIDERS: [&str; 3] = ["claude", "codex", "copilot"];

/// A structured agent draft accumulated across the guided create wizard.
///
/// This is the faithful hangar analog of multica's `<agent_draft>` block (minus the
/// LLM turn): the user reviews it on the `Confirm` step before it fires a real
/// create. Kept a plain, serializable-shaped struct so a future LLM-draft parser
/// (agent-reference §1c fast-follow) can target it unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateDraft {
    /// The new agent's name (required, non-blank to advance past the `Name` step).
    pub name: String,
    /// The chosen provider — one of [`SUPPORTED_PROVIDERS`]; defaults to `claude`.
    pub provider: String,
    /// The optional per-agent model override; blank = the provider default.
    pub model: String,
    /// The optional free-form instructions / system prompt; blank = none.
    pub instructions: String,
}

impl Default for CreateDraft {
    fn default() -> Self {
        Self {
            name: String::new(),
            provider: SUPPORTED_PROVIDERS[0].to_string(),
            model: String::new(),
            instructions: String::new(),
        }
    }
}

/// The step the guided create wizard is on. Enter advances `Name → Provider →
/// Model → Instructions → Confirm`; Enter on `Confirm` fires the create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateStep {
    /// Type the agent name (required).
    Name,
    /// Pick the provider from the fixed list (a picker, never free text).
    Provider,
    /// Type an optional model override (blank = provider default).
    Model,
    /// Type optional instructions (blank = none).
    Instructions,
    /// Review the structured draft; Enter creates, Esc cancels.
    Confirm,
}

/// The render-state cache for the Agents screen.
///
/// Holds the resolved agents, the list selection, an optional guided create wizard
/// (`Some((draft, step))` only while `n` is open), and an optional pending-delete
/// confirm target (`Some(actor_ref)` only while the `x` overlay is open). The two
/// overlays are mutually exclusive (only one input at a time). All fields private;
/// tests and the renderer read through accessors. The scroll offset is derived per
/// render from the selection + viewport height (viewport-blind, like `squads.rs`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AgentsState {
    agents: Vec<AgentView>,
    selected: usize,
    create: Option<(CreateDraft, CreateStep)>,
    confirm_delete: Option<String>,
    /// A transient ERROR note rendered red above the list — a delete/create
    /// refusal (active tasks, FK-pinned history) so the rejection is never silent.
    /// `None` when idle.
    note: Option<String>,
}

impl AgentsState {
    /// A fresh screen over `agents`, first row selected, no overlay open.
    #[must_use]
    pub const fn new(agents: Vec<AgentView>) -> Self {
        Self {
            agents,
            selected: 0,
            create: None,
            confirm_delete: None,
            note: None,
        }
    }

    /// Build the screen from a cached `hangar/agents_list` actor snapshot, keeping
    /// only the agent actors (`is_agent`) — the human members belong to the Squads
    /// / picker surfaces, not the agent roster.
    #[must_use]
    pub fn from_actors(actors: &[ActorRow]) -> Self {
        let agents = actors
            .iter()
            .filter(|a| a.is_agent)
            .map(|a| AgentView {
                actor_ref: a.actor_ref.clone(),
                name: a.display_name.clone(),
                subtitle: a.subtitle.clone(),
                presence: a.presence,
                workload: a.workload,
            })
            .collect();
        Self::new(agents)
    }

    /// The resolved agents.
    #[must_use]
    pub fn agents(&self) -> &[AgentView] {
        &self.agents
    }

    /// The current list selection index.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the guided create wizard is open.
    #[must_use]
    pub const fn is_creating(&self) -> bool {
        self.create.is_some()
    }

    /// The in-flight create draft, if the wizard is open (render + tests read it).
    #[must_use]
    pub fn create_draft(&self) -> Option<&CreateDraft> {
        self.create.as_ref().map(|(draft, _)| draft)
    }

    /// The current wizard step, if the wizard is open.
    #[must_use]
    pub fn create_step(&self) -> Option<CreateStep> {
        self.create.as_ref().map(|(_, step)| *step)
    }

    /// Whether the `x` delete-confirm overlay is open.
    #[must_use]
    pub const fn is_confirming(&self) -> bool {
        self.confirm_delete.is_some()
    }

    /// The actor-ref of the agent the open delete-confirm targets, if any.
    #[must_use]
    pub fn confirm_target(&self) -> Option<&str> {
        self.confirm_delete.as_deref()
    }

    /// Whether the screen is capturing input (create name OR delete confirm), so
    /// the plugin glue routes every key — including the global tab-switch chars —
    /// into this screen's reducer rather than letting them switch tabs.
    #[must_use]
    pub const fn is_capturing(&self) -> bool {
        self.create.is_some() || self.confirm_delete.is_some()
    }

    /// Snapshot the in-flight wizard (draft + step) so a background roster refresh
    /// can restore it — a refresh mid-wizard must not wipe the collected draft.
    #[must_use]
    pub fn create_state(&self) -> Option<(CreateDraft, CreateStep)> {
        self.create.clone()
    }

    /// Restore (or clear) the guided create wizard after a snapshot refresh, so a
    /// background roster refresh mid-wizard does not wipe the collected draft.
    pub fn set_create_state(&mut self, create: Option<(CreateDraft, CreateStep)>) {
        self.create = create;
    }

    /// Restore (or clear) the delete-confirm target after a refresh — but ONLY when
    /// the targeted agent still exists on the fresh roster. An agent that vanished
    /// (deleted by this very confirm, or by another client) drops the stale overlay
    /// rather than confirming a delete of a row that is no longer there.
    pub fn restore_confirm(&mut self, target: Option<String>) {
        self.confirm_delete =
            target.filter(|actor_ref| self.agents.iter().any(|a| &a.actor_ref == actor_ref));
    }

    /// The transient error note, if any (rendered red above the list).
    #[must_use]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Raise a transient error note (a delete/create refusal). Rendered red so it
    /// never reads as a success.
    pub fn note_err(&mut self, text: impl Into<String>) {
        self.note = Some(text.into());
    }

    /// Set (or clear) the transient note — used to preserve it across a snapshot
    /// refresh.
    pub fn set_note(&mut self, note: Option<String>) {
        self.note = note;
    }

    /// The agent under the selection, if any.
    #[must_use]
    pub fn selected_agent(&self) -> Option<&AgentView> {
        self.agents.get(self.selected)
    }

    /// Restore the list selection after a refresh, clamped to the new row range.
    pub fn set_selected(&mut self, idx: usize) {
        let len = self.agents.len();
        self.selected = if len == 0 { 0 } else { idx.min(len - 1) };
    }

    /// Move the selection by `delta`, clamped to the row range.
    fn move_selection(&mut self, delta: i32) {
        let len = self.agents.len();
        if len == 0 {
            return;
        }
        let max = i32::try_from(len - 1).unwrap_or(0);
        let cur = i32::try_from(self.selected).unwrap_or(0);
        self.selected = usize::try_from((cur + delta).clamp(0, max)).unwrap_or(0);
    }
}

/// An input the agents reducer folds into [`AgentsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsEvent {
    /// A printable key (`'j'`, `'n'`, `'x'`, …) — or `'\n'` (Enter) / `'\u{8}'`
    /// (Backspace) while the create input is open.
    Key(char),
    /// The Escape key (cancels an open overlay; a no-op otherwise).
    Esc,
}

/// A side-effect the plugin glue performs after an agents reduction.
///
/// Each variant maps to one daemon RPC the glue fires; the intent carries only the
/// values the reducer owns (the daemon fills every FK for create, and scopes the
/// delete by id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentsIntent {
    /// Create an AGENT from the guided wizard (`n` → name → provider → model →
    /// instructions → confirm) — `hangar/agent_create`; the glue fires it with no
    /// ids (the daemon fills workspace / runtime / owner) and folds the refreshed
    /// roster back. Carries the full structured draft, not just a name.
    CreateAgent {
        /// The new agent's name (non-blank).
        name: String,
        /// The chosen provider (one of [`SUPPORTED_PROVIDERS`]).
        provider: String,
        /// The optional model override — `None` when the model step was left blank.
        model: Option<String>,
        /// The optional instructions — `None` when the step was left blank.
        instructions: Option<String>,
    },
    /// Delete `actor_ref` (Enter on the `x` confirm overlay) — `hangar/agent_delete`;
    /// the glue extracts the id from the ref and scopes the delete to the workspace.
    DeleteAgent {
        /// The agent to delete, in canonical `agent:<id>` form.
        actor_ref: String,
    },
}

/// The result of folding one [`AgentsEvent`] into an [`AgentsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsReduction {
    /// The next state.
    pub state: AgentsState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<AgentsIntent>,
}

/// Fold one [`AgentsEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_agents(state: &AgentsState, ev: AgentsEvent) -> AgentsReduction {
    match ev {
        AgentsEvent::Key(c) => {
            if state.create.is_some() {
                reduce_create_key(state, c)
            } else if state.confirm_delete.is_some() {
                reduce_confirm_key(state, c)
            } else {
                reduce_key(state, c)
            }
        }
        AgentsEvent::Esc => reduce_esc(state),
    }
}

/// Handle a key while the guided create wizard is open. Step-aware:
///
/// * `Name` / `Model` / `Instructions` are free-text fields — printable chars
///   append, Backspace pops, Enter advances (blank `Name` is a no-op, preserving
///   the old name-only guard; blank `Model` / `Instructions` are allowed).
/// * `Provider` is a **picker** — `←`/`→` (mapped to `h`/`l`) or `k`/`j` cycle the
///   fixed [`SUPPORTED_PROVIDERS`] list; Enter advances. Free text never lands an
///   unsupported provider, so the daemon's `normalize_provider` never rejects.
/// * `Confirm` reviews the structured draft — Enter emits the full
///   [`AgentsIntent::CreateAgent`] and closes the wizard; other keys hold.
fn reduce_create_key(state: &AgentsState, c: char) -> AgentsReduction {
    let Some((draft, step)) = state.create.clone() else {
        return unchanged(state);
    };
    match step {
        CreateStep::Name => reduce_text_step(state, draft, c, CreateStep::Name, |d| &mut d.name),
        CreateStep::Model => reduce_text_step(state, draft, c, CreateStep::Model, |d| &mut d.model),
        CreateStep::Instructions => {
            reduce_text_step(state, draft, c, CreateStep::Instructions, |d| {
                &mut d.instructions
            })
        }
        CreateStep::Provider => reduce_provider_step(state, draft, c),
        CreateStep::Confirm => reduce_confirm_step(state, draft, c),
    }
}

/// The next wizard step after `step` (Enter advances; `Confirm` fires instead of
/// advancing, so it is handled by [`reduce_confirm_step`], never here).
const fn next_step(step: CreateStep) -> CreateStep {
    match step {
        CreateStep::Name => CreateStep::Provider,
        CreateStep::Provider => CreateStep::Model,
        CreateStep::Model => CreateStep::Instructions,
        CreateStep::Instructions | CreateStep::Confirm => CreateStep::Confirm,
    }
}

/// Fold a key into one free-text wizard step (`Name` / `Model` / `Instructions`).
/// `field` selects which draft field the step edits. Enter advances to the next
/// step — except a blank `Name`, which is a no-op (keeps the old name-only guard).
fn reduce_text_step(
    state: &AgentsState,
    mut draft: CreateDraft,
    c: char,
    step: CreateStep,
    field: impl Fn(&mut CreateDraft) -> &mut String,
) -> AgentsReduction {
    match c {
        '\n' => {
            if matches!(step, CreateStep::Name) && draft.name.trim().is_empty() {
                // Blank name is a no-op — the wizard stays on the Name step.
                return unchanged(state);
            }
            let mut next = state.clone();
            next.create = Some((draft, next_step(step)));
            no_intent(next)
        }
        '\u{8}' => {
            field(&mut draft).pop();
            let mut next = state.clone();
            next.create = Some((draft, step));
            no_intent(next)
        }
        c if !c.is_control() => {
            field(&mut draft).push(c);
            let mut next = state.clone();
            next.create = Some((draft, step));
            no_intent(next)
        }
        _ => unchanged(state),
    }
}

/// Fold a key into the `Provider` picker step: `l`/`j` (or `→`/`↓`) cycle forward,
/// `h`/`k` (or `←`/`↑`) cycle backward through [`SUPPORTED_PROVIDERS`], Enter
/// advances. The picker can only ever land on a supported provider.
fn reduce_provider_step(state: &AgentsState, mut draft: CreateDraft, c: char) -> AgentsReduction {
    let cur = SUPPORTED_PROVIDERS
        .iter()
        .position(|p| *p == draft.provider)
        .unwrap_or(0);
    let len = SUPPORTED_PROVIDERS.len();
    let next_idx = match c {
        '\n' => {
            let mut next = state.clone();
            next.create = Some((draft, next_step(CreateStep::Provider)));
            return no_intent(next);
        }
        'l' | 'j' => (cur + 1) % len,
        'h' | 'k' => (cur + len - 1) % len,
        _ => return unchanged(state),
    };
    draft.provider = SUPPORTED_PROVIDERS[next_idx].to_string();
    let mut next = state.clone();
    next.create = Some((draft, CreateStep::Provider));
    no_intent(next)
}

/// Fold a key into the `Confirm` review step: Enter emits the full
/// [`AgentsIntent::CreateAgent`] (blank model / instructions collapse to `None`)
/// and closes the wizard; any other key holds the review open.
fn reduce_confirm_step(state: &AgentsState, draft: CreateDraft, c: char) -> AgentsReduction {
    if c != '\n' {
        return unchanged(state);
    }
    let model = non_blank(&draft.model);
    let instructions = non_blank(&draft.instructions);
    let mut next = state.clone();
    next.create = None;
    with_intent(
        next,
        AgentsIntent::CreateAgent {
            name: draft.name.trim().to_string(),
            provider: draft.provider,
            model,
            instructions,
        },
    )
}

/// Trim a draft field, collapsing a blank value to `None` (so a skipped optional
/// step never writes an empty string).
fn non_blank(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Handle a key while the delete-confirm overlay is open: Enter confirms and emits
/// [`AgentsIntent::DeleteAgent`], every other key holds the overlay open (Esc — the
/// abort — is handled in [`reduce_esc`], never here). Mirrors the issue-list
/// confirm: only the explicit Enter deletes, so a stray keystroke never removes an
/// agent.
fn reduce_confirm_key(state: &AgentsState, c: char) -> AgentsReduction {
    match c {
        '\n' => {
            let Some(actor_ref) = state.confirm_delete.clone() else {
                return unchanged(state);
            };
            let mut next = state.clone();
            next.confirm_delete = None;
            with_intent(next, AgentsIntent::DeleteAgent { actor_ref })
        }
        _ => unchanged(state),
    }
}

/// Handle a normal-mode key.
fn reduce_key(state: &AgentsState, c: char) -> AgentsReduction {
    match c {
        'n' => {
            let mut next = state.clone();
            next.create = Some((CreateDraft::default(), CreateStep::Name));
            // A fresh interaction clears any stale refusal note.
            next.note = None;
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
        // `x` opens the delete-confirm over the selected agent; a no-op on an empty
        // roster (nothing to confirm).
        'x' => state.selected_agent().map_or_else(
            || unchanged(state),
            |a| {
                let mut next = state.clone();
                next.confirm_delete = Some(a.actor_ref.clone());
                // A fresh interaction clears any stale refusal note.
                next.note = None;
                no_intent(next)
            },
        ),
        _ => unchanged(state),
    }
}

/// Handle Esc: cancel whichever overlay (delete-confirm or create-name) is open in
/// a SINGLE press; a no-op otherwise. Esc closes the open overlay outright, never
/// stepping back through state.
fn reduce_esc(state: &AgentsState) -> AgentsReduction {
    if state.confirm_delete.is_some() {
        let mut next = state.clone();
        next.confirm_delete = None;
        no_intent(next)
    } else if state.create.is_some() {
        // A single Esc cancels the whole wizard from ANY step (not step-by-step).
        let mut next = state.clone();
        next.create = None;
        no_intent(next)
    } else {
        unchanged(state)
    }
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: AgentsState) -> AgentsReduction {
    AgentsReduction {
        state,
        intent: None,
    }
}

/// A reduction carrying `intent` alongside `state`.
const fn with_intent(state: AgentsState, intent: AgentsIntent) -> AgentsReduction {
    AgentsReduction {
        state,
        intent: Some(intent),
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &AgentsState) -> AgentsReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware render
// ---------------------------------------------------------------------------

/// Render the Agents screen into `buf` between rows `top` and `bottom`.
///
/// The header row carries the action-key hints right-aligned. Below it, either the
/// create-name input (when open), the delete-confirm overlay (when open), the
/// empty-state help line (no agents), or the list of agent rows — each showing the
/// `▶` selection marker, the name, a presence dot + word, and the subtitle.
pub fn render_agents(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &AgentsState,
) {
    render_action_hints(buf, top, area_w);
    let mut row = top + 1;

    // A transient refusal note (delete/create rejected), if any — rendered red so
    // it never reads as a success confirmation.
    if let Some(note) = state.note() {
        put_str(buf, 0, row, note, ERROR_RED, area_w);
        row = row.saturating_add(1);
    }

    // The guided create wizard takes over the body while open (the `n` flow). It
    // shows the accumulated draft, then the current step's prompt — and on the
    // final `Confirm` step the full structured draft for review before create.
    if let Some((draft, step)) = state.create.as_ref() {
        render_create_wizard(buf, area_w, row, draft, *step);
        return;
    }

    // Delete-confirm overlay takes over the body while open (the `x` prompt). Its
    // red ink marks it as destructive so it never reads as a success confirmation.
    if let Some(actor_ref) = state.confirm_target() {
        let name = state
            .agents()
            .iter()
            .find(|a| a.actor_ref == actor_ref)
            .map_or(actor_ref, |a| a.name.as_str());
        let line = format!("Delete agent \"{name}\"?");
        put_str(buf, 0, row, &line, ERROR_RED, area_w);
        put_str(
            buf,
            0,
            row.saturating_add(1),
            "Enter to confirm, Esc to cancel",
            MUTED_GRAY,
            area_w,
        );
        return;
    }

    if state.agents().is_empty() {
        put_str(
            buf,
            0,
            row,
            "No agents yet — press n to create one",
            MUTED_GRAY,
            area_w,
        );
        return;
    }

    // Follow the selection so the `▶` cursor stays on-screen when the roster
    // overflows the pane (viewport-blind, the same convention as `squads.rs`).
    let visible_rows = usize::from(bottom.saturating_sub(row));
    let visible_from = first_visible(state.selected_index(), visible_rows);
    let mut y = row;
    for (idx, agent) in state.agents().iter().enumerate().skip(visible_from) {
        if y >= bottom {
            break;
        }
        render_agent_row(buf, y, area_w, agent, idx == state.selected_index());
        y = y.saturating_add(1);
    }
}

/// Render the guided create wizard body from `row` down: a title, the draft
/// collected so far, and the active step's prompt. On the final `Confirm` step it
/// paints the full structured draft (name / provider / model / instructions) as the
/// review beat before the create fires — the faithful analog of multica's
/// draft-then-confirm.
fn render_create_wizard(
    buf: &mut WireBuffer,
    area_w: u16,
    row: u16,
    draft: &CreateDraft,
    step: CreateStep,
) {
    put_str(buf, 0, row, "✦ New agent", GOLD, area_w);
    let mut y = row.saturating_add(1);

    // The accumulated draft above the active prompt (only fields already entered).
    let show_field = |buf: &mut WireBuffer, y: u16, label: &str, val: &str| {
        let line = format!("{label}: {val}");
        put_str(buf, 0, y, &line, MUTED_GRAY, area_w);
    };

    match step {
        CreateStep::Name => {
            put_str(
                buf,
                0,
                y,
                "Name the agent, Enter to continue, Esc to cancel",
                MUTED_GRAY,
                area_w,
            );
            let line = format!("Name: {}▏", draft.name);
            put_str(buf, 0, y.saturating_add(1), &line, GOLD, area_w);
        }
        CreateStep::Provider => {
            show_field(buf, y, "Name", &draft.name);
            y = y.saturating_add(1);
            put_str(
                buf,
                0,
                y,
                "Pick a provider (←/→), Enter to continue, Esc to cancel",
                MUTED_GRAY,
                area_w,
            );
            // The current choice, framed so the picker reads as a selectable value.
            let line = format!("Provider: ‹ {} ›", draft.provider);
            put_str(buf, 0, y.saturating_add(1), &line, GOLD, area_w);
        }
        CreateStep::Model => {
            show_field(buf, y, "Name", &draft.name);
            y = y.saturating_add(1);
            show_field(buf, y, "Provider", &draft.provider);
            y = y.saturating_add(1);
            put_str(
                buf,
                0,
                y,
                "Optional model override, Enter to continue (blank = default)",
                MUTED_GRAY,
                area_w,
            );
            let line = format!("Model: {}▏", draft.model);
            put_str(buf, 0, y.saturating_add(1), &line, GOLD, area_w);
        }
        CreateStep::Instructions => {
            show_field(buf, y, "Name", &draft.name);
            y = y.saturating_add(1);
            show_field(buf, y, "Provider", &draft.provider);
            y = y.saturating_add(1);
            put_str(
                buf,
                0,
                y,
                "Optional instructions, Enter to continue (blank = none)",
                MUTED_GRAY,
                area_w,
            );
            let line = format!("Instructions: {}▏", draft.instructions);
            put_str(buf, 0, y.saturating_add(1), &line, GOLD, area_w);
        }
        CreateStep::Confirm => {
            put_str(
                buf,
                0,
                y,
                "Review the draft — Enter to create, Esc to cancel",
                MUTED_GRAY,
                area_w,
            );
            y = y.saturating_add(1);
            show_field(buf, y, "Name", &draft.name);
            y = y.saturating_add(1);
            show_field(buf, y, "Provider", &draft.provider);
            y = y.saturating_add(1);
            let model = if draft.model.trim().is_empty() {
                "(default)"
            } else {
                draft.model.trim()
            };
            show_field(buf, y, "Model", model);
            y = y.saturating_add(1);
            let first_line = draft.instructions.lines().next().unwrap_or("");
            let instr = if first_line.trim().is_empty() {
                "(none)"
            } else {
                first_line
            };
            show_field(buf, y, "Instructions", instr);
        }
    }
}

/// The first-visible row index for a viewport of `visible_rows` rows that must keep
/// `selected` on-screen (mirrors `squads::first_visible`).
const fn first_visible(selected: usize, visible_rows: usize) -> usize {
    if visible_rows == 0 {
        return selected;
    }
    selected.saturating_sub(visible_rows - 1)
}

/// Paint the action-key hints on the top row, right-aligned. Dropped on a terminal
/// too narrow to hold them (the footer carries them too).
fn render_action_hints(buf: &mut WireBuffer, row: u16, area_w: u16) {
    const HINTS: &str = "[n]ew [x]remove";
    let hint_w = u16::try_from(HINTS.chars().count()).unwrap_or(0);
    if hint_w >= area_w {
        return;
    }
    put_str(buf, area_w - hint_w, row, HINTS, GOLD, area_w);
}

/// Render one agent row:
/// `▶ <name>  <dot> <presence> · <wl-glyph> <workload>  · <subtitle>`.
///
/// The two dimensions are painted side by side (multica `presence × workload`):
/// the availability dot+word, then ` · ` + the workload glyph+word colour-coded
/// by its state, so an `online` agent visibly reads `working` vs `idle`.
fn render_agent_row(
    buf: &mut WireBuffer,
    row: u16,
    area_w: u16,
    agent: &AgentView,
    selected: bool,
) {
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
        &agent.name,
        if selected { GOLD } else { SOFT_WHITE },
        area_w,
    );
    x = put_str(buf, x, row, "  ", MUTED_GRAY, area_w);
    let (glyph, color) = presence_dot(agent.presence);
    x = put_cell(buf, x, row, glyph, color, area_w);
    x = put_str(buf, x, row, " ", MUTED_GRAY, area_w);
    x = put_str(
        buf,
        x,
        row,
        presence_word(agent.presence),
        MUTED_GRAY,
        area_w,
    );
    // The orthogonal workload dimension, painted right after availability.
    let (wl_glyph, wl_color) = workload_dot(agent.workload);
    x = put_str(buf, x, row, " · ", MUTED_GRAY, area_w);
    x = put_cell(buf, x, row, wl_glyph, wl_color, area_w);
    x = put_str(buf, x, row, " ", MUTED_GRAY, area_w);
    x = put_str(buf, x, row, workload_word(agent.workload), wl_color, area_w);
    if !agent.subtitle.is_empty() {
        x = put_str(buf, x, row, "  · ", MUTED_GRAY, area_w);
        put_str(buf, x, row, &agent.subtitle, MUTED_GRAY, area_w);
    }
}

/// The lowercase presence word rendered next to the dot.
const fn presence_word(presence: PresenceState) -> &'static str {
    match presence {
        PresenceState::Online => "online",
        PresenceState::Unstable => "unstable",
        PresenceState::Offline => "offline",
    }
}

/// The lowercase workload word rendered next to its glyph.
const fn workload_word(workload: Workload) -> &'static str {
    match workload {
        Workload::Working => "working",
        Workload::Queued => "queued",
        Workload::Idle => "idle",
    }
}

/// The workload glyph + colour: `⚙ working` (green), `◔ queued` (amber),
/// `· idle` (gray) — mirroring the availability `presence_dot` idiom.
const fn workload_dot(workload: Workload) -> (char, Color) {
    match workload {
        Workload::Working => ('⚙', SELECTION_GREEN),
        Workload::Queued => ('◔', WORKLOAD_AMBER),
        Workload::Idle => ('·', MUTED_GRAY),
    }
}

/// Write a single glyph at `(x, row)` in `color`, clipping at `area_w`. Returns the
/// next free column.
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

    fn actor(actor_ref: &str, name: &str, is_agent: bool) -> ActorRow {
        ActorRow {
            actor_ref: actor_ref.into(),
            display_name: name.into(),
            subtitle: if is_agent {
                "claude".into()
            } else {
                "member".into()
            },
            presence: PresenceState::Online,
            workload: Workload::Idle,
            is_agent,
            recent_rank: None,
        ..ActorRow::default()
        }
    }

    fn snapshot() -> Vec<ActorRow> {
        vec![
            actor("agent:a-1", "backend-bot", true),
            actor("member:u-1", "alice", false),
            actor("agent:a-2", "review-bot", true),
        ]
    }

    /// `from_actors` keeps only the agent actors (members are filtered out) and
    /// resolves each row's name + subtitle + presence.
    #[test]
    fn from_actors_keeps_only_agents() {
        let state = AgentsState::from_actors(&snapshot());
        assert_eq!(state.agents().len(), 2, "the human member is filtered out");
        assert_eq!(state.agents()[0].name, "backend-bot");
        assert_eq!(state.agents()[0].subtitle, "claude");
        assert_eq!(state.agents()[1].name, "review-bot");
    }

    /// `j`/`k` navigate the roster, clamped at both ends.
    #[test]
    fn navigation_is_clamped() {
        let state = AgentsState::from_actors(&snapshot());
        assert_eq!(state.selected_index(), 0);
        let down = |s: &AgentsState| reduce_agents(s, AgentsEvent::Key('j')).state;
        let s = down(&state);
        assert_eq!(s.selected_index(), 1);
        // Clamped at the last row.
        let s = down(&s);
        assert_eq!(s.selected_index(), 1);
        let up = reduce_agents(&s, AgentsEvent::Key('k')).state;
        assert_eq!(up.selected_index(), 0);
    }

    /// Drive the whole wizard by pressing the given keys in order, returning the
    /// final reduction (state + any emitted intent).
    fn drive(state: &AgentsState, keys: &[char]) -> AgentsReduction {
        let mut cur = state.clone();
        let mut last = AgentsReduction {
            state: cur.clone(),
            intent: None,
        };
        for &c in keys {
            last = reduce_agents(&cur, AgentsEvent::Key(c));
            cur = last.state.clone();
        }
        last
    }

    /// `n` opens the wizard at `Name`; the full step sequence collects
    /// name → provider → model → instructions and Enter on `Confirm` emits the
    /// widened `CreateAgent` intent with every field; a single Esc cancels.
    #[test]
    fn create_flow_raises_intent_and_esc_cancels() {
        let state = AgentsState::from_actors(&snapshot());
        let opened = reduce_agents(&state, AgentsEvent::Key('n')).state;
        assert_eq!(opened.create_step(), Some(CreateStep::Name), "n opens Name");
        assert_eq!(opened.create_draft().map(|d| d.name.as_str()), Some(""));
        assert!(opened.is_capturing());

        // Name "qa" + Enter -> Provider; '→'(l) cycles claude->codex; Enter -> Model;
        // type model + Enter -> Instructions; type text + Enter -> Confirm.
        let at_confirm = drive(
            &opened,
            &[
                'q', 'a', '\n', // Name -> Provider
                'l', '\n', // pick codex -> Model
                'g', 'p', 't', '\n', // Model -> Instructions
                'b', 'e', ' ', 't', 'e', 'r', 's', 'e', '\n', // -> Confirm
            ],
        )
        .state;
        assert_eq!(at_confirm.create_step(), Some(CreateStep::Confirm));
        assert_eq!(
            at_confirm.create_draft().map(|d| d.provider.as_str()),
            Some("codex"),
            "the provider picker landed on codex"
        );

        // Enter on Confirm fires the full structured intent and closes the wizard.
        let out = reduce_agents(&at_confirm, AgentsEvent::Key('\n'));
        assert_eq!(
            out.intent,
            Some(AgentsIntent::CreateAgent {
                name: "qa".into(),
                provider: "codex".into(),
                model: Some("gpt".into()),
                instructions: Some("be terse".into()),
            })
        );
        assert!(!out.state.is_creating(), "wizard closes on create");

        // A single Esc mid-wizard (from the Confirm step) cancels with no intent.
        let cancel = reduce_agents(&at_confirm, AgentsEvent::Esc);
        assert!(!cancel.state.is_creating());
        assert!(cancel.intent.is_none());
    }

    /// A blank name Enter is a no-op — the wizard stays on the `Name` step, no
    /// intent (preserves the old name-only guard).
    #[test]
    fn blank_create_submit_is_a_noop() {
        let state = AgentsState::from_actors(&snapshot());
        let opened = reduce_agents(&state, AgentsEvent::Key('n')).state;
        let out = reduce_agents(&opened, AgentsEvent::Key('\n'));
        assert!(out.intent.is_none());
        assert_eq!(
            out.state.create_step(),
            Some(CreateStep::Name),
            "blank name keeps the wizard on the Name step"
        );
    }

    /// Blank model AND blank instructions collapse to `None` on the emitted intent
    /// (no spurious empty-string write), and the default provider (`claude`) is
    /// carried when the picker is left untouched.
    #[test]
    fn wizard_blank_optionals_are_none() {
        let state = AgentsState::from_actors(&snapshot());
        let opened = reduce_agents(&state, AgentsEvent::Key('n')).state;
        // Name "bot" + Enter; Provider Enter (leave claude); Model Enter (blank);
        // Instructions Enter (blank); Confirm Enter.
        let out = drive(
            &opened,
            &['b', 'o', 't', '\n', '\n', '\n', '\n', '\n'],
        );
        assert_eq!(
            out.intent,
            Some(AgentsIntent::CreateAgent {
                name: "bot".into(),
                provider: "claude".into(),
                model: None,
                instructions: None,
            })
        );
    }

    /// A single Esc at the `Provider` step (not the last one) cancels the whole
    /// wizard in one press — never steps back to `Name`.
    #[test]
    fn wizard_esc_cancels_from_any_step() {
        let state = AgentsState::from_actors(&snapshot());
        let opened = reduce_agents(&state, AgentsEvent::Key('n')).state;
        let at_provider = drive(&opened, &['a', '\n']).state;
        assert_eq!(at_provider.create_step(), Some(CreateStep::Provider));
        let cancelled = reduce_agents(&at_provider, AgentsEvent::Esc);
        assert!(!cancelled.state.is_creating(), "one Esc closes the wizard");
        assert!(cancelled.intent.is_none());
    }

    /// The provider picker cycles the fixed list forward AND backward, wrapping.
    #[test]
    fn wizard_provider_picker_cycles() {
        let state = AgentsState::from_actors(&snapshot());
        let opened = reduce_agents(&state, AgentsEvent::Key('n')).state;
        let at_provider = drive(&opened, &['a', '\n']).state;
        assert_eq!(
            at_provider.create_draft().map(|d| d.provider.as_str()),
            Some("claude")
        );
        // Forward: claude -> codex -> copilot -> claude (wrap).
        let fwd = drive(&at_provider, &['l', 'l', 'l']).state;
        assert_eq!(fwd.create_draft().map(|d| d.provider.as_str()), Some("claude"));
        // Backward from claude wraps to copilot.
        let back = drive(&at_provider, &['h']).state;
        assert_eq!(
            back.create_draft().map(|d| d.provider.as_str()),
            Some("copilot")
        );
    }

    /// `x` opens the delete-confirm over the selected agent; Enter confirms →
    /// `DeleteAgent` for that agent; Esc aborts with no intent.
    #[test]
    fn delete_confirm_flow_raises_intent_and_esc_aborts() {
        let state = AgentsState::from_actors(&snapshot());
        let confirming = reduce_agents(&state, AgentsEvent::Key('x')).state;
        assert_eq!(confirming.confirm_target(), Some("agent:a-1"));
        assert!(confirming.is_capturing());

        // Enter confirms the delete of the selected agent.
        let out = reduce_agents(&confirming, AgentsEvent::Key('\n'));
        assert_eq!(
            out.intent,
            Some(AgentsIntent::DeleteAgent {
                actor_ref: "agent:a-1".into()
            })
        );
        assert!(!out.state.is_confirming(), "confirm closes on delete");

        // Esc aborts the confirm with no intent.
        let aborted = reduce_agents(&confirming, AgentsEvent::Esc);
        assert!(!aborted.state.is_confirming());
        assert!(aborted.intent.is_none());
    }

    /// A key that is NOT Enter holds the confirm overlay open (no stray keystroke
    /// ever deletes an agent).
    #[test]
    fn confirm_holds_open_on_a_stray_key() {
        let state = AgentsState::from_actors(&snapshot());
        let confirming = reduce_agents(&state, AgentsEvent::Key('x')).state;
        let out = reduce_agents(&confirming, AgentsEvent::Key('j'));
        assert!(out.intent.is_none());
        assert!(
            out.state.is_confirming(),
            "a non-Enter key keeps the confirm open"
        );
    }

    /// `x` on an empty roster is a no-op (nothing to confirm).
    #[test]
    fn delete_on_empty_roster_is_a_noop() {
        let state = AgentsState::from_actors(&[]);
        let out = reduce_agents(&state, AgentsEvent::Key('x'));
        assert!(out.intent.is_none());
        assert!(!out.state.is_confirming());
    }

    /// `restore_confirm` keeps an overlay whose agent still exists, and drops one
    /// whose agent has vanished from the fresh roster (deleted out from under it).
    #[test]
    fn restore_confirm_drops_a_vanished_target() {
        let mut state = AgentsState::from_actors(&snapshot());
        state.restore_confirm(Some("agent:a-1".into()));
        assert_eq!(state.confirm_target(), Some("agent:a-1"));

        // A ref no longer on the roster is dropped, not confirmed.
        let mut shrunk = AgentsState::from_actors(&[actor("agent:a-2", "review-bot", true)]);
        shrunk.restore_confirm(Some("agent:a-1".into()));
        assert!(shrunk.confirm_target().is_none());
    }

    /// The render lists the injected agents (name + subtitle visible) with the
    /// selection marker on the first row.
    #[test]
    fn render_lists_agents() {
        let state = AgentsState::from_actors(&snapshot());
        let mut buf = WireBuffer::new(80, 24);
        render_agents(&mut buf, 80, 1, 23, &state);
        let text = buffer_text(&buf, 80, 24);
        assert!(
            text.contains("backend-bot"),
            "roster must list the agent name"
        );
        assert!(
            text.contains("claude"),
            "roster must show the subtitle/provider"
        );
        assert!(text.contains("review-bot"));
        assert!(text.contains('▶'), "the selected row carries the marker");
    }

    /// The empty-state line renders when there are zero agents.
    #[test]
    fn render_shows_empty_state() {
        let state = AgentsState::from_actors(&[]);
        let mut buf = WireBuffer::new(80, 24);
        render_agents(&mut buf, 80, 1, 23, &state);
        let text = buffer_text(&buf, 80, 24);
        assert!(
            text.contains("No agents yet"),
            "empty roster must show the create hint"
        );
    }

    /// The delete-confirm overlay renders the targeted agent's name.
    #[test]
    fn render_shows_delete_confirm() {
        let state = AgentsState::from_actors(&snapshot());
        let confirming = reduce_agents(&state, AgentsEvent::Key('x')).state;
        let mut buf = WireBuffer::new(80, 24);
        render_agents(&mut buf, 80, 1, 23, &confirming);
        let text = buffer_text(&buf, 80, 24);
        assert!(text.contains("Delete agent"), "confirm overlay must render");
        assert!(text.contains("backend-bot"), "confirm names the agent");
    }

    /// `from_actors` carries the workload dimension through from the snapshot.
    #[test]
    fn from_actors_carries_workload() {
        let mut queued = actor("agent:a-1", "backend-bot", true);
        queued.workload = Workload::Queued;
        let state = AgentsState::from_actors(&[queued]);
        assert_eq!(state.agents()[0].workload, Workload::Queued);
    }

    /// The render paints BOTH dimensions: the availability word AND the
    /// orthogonal workload word are independently present on the same row.
    #[test]
    fn render_shows_both_presence_and_workload() {
        let state = AgentsState::new(vec![
            AgentView {
                actor_ref: "agent:a-1".into(),
                name: "worker".into(),
                subtitle: "claude".into(),
                presence: PresenceState::Online,
                workload: Workload::Working,
            },
            AgentView {
                actor_ref: "agent:a-2".into(),
                name: "waiter".into(),
                subtitle: "claude".into(),
                presence: PresenceState::Online,
                workload: Workload::Queued,
            },
        ]);
        let mut buf = WireBuffer::new(80, 24);
        render_agents(&mut buf, 80, 1, 23, &state);
        let text = buffer_text(&buf, 80, 24);
        // Availability dimension still independently present.
        assert!(text.contains("online"), "availability word must render");
        // Orthogonal workload dimension present for both agents.
        assert!(
            text.contains("working"),
            "a running agent renders `working`"
        );
        assert!(text.contains("queued"), "a queued agent renders `queued`");
    }

    /// An idle agent renders the `idle` workload word (the default dimension is
    /// still surfaced, not blank).
    #[test]
    fn render_shows_idle_workload() {
        let state = AgentsState::new(vec![AgentView {
            actor_ref: "agent:a-1".into(),
            name: "resting".into(),
            subtitle: "claude".into(),
            presence: PresenceState::Online,
            workload: Workload::Idle,
        }]);
        let mut buf = WireBuffer::new(80, 24);
        render_agents(&mut buf, 80, 1, 23, &state);
        let text = buffer_text(&buf, 80, 24);
        assert!(text.contains("idle"), "an idle agent renders `idle`");
    }

    /// The `Confirm` step render shows the structured draft for review: the chosen
    /// provider AND model AND a slice of the instructions are all visible before the
    /// create fires.
    #[test]
    fn render_confirm_step_shows_structured_draft() {
        let state = AgentsState::from_actors(&snapshot());
        let opened = reduce_agents(&state, AgentsEvent::Key('n')).state;
        let at_confirm = drive(
            &opened,
            &[
                'q', 'a', '\n', // Name -> Provider
                'l', '\n', // codex -> Model
                'g', 'p', 't', '-', '5', '\n', // Model -> Instructions
                'b', 'e', ' ', 't', 'e', 'r', 's', 'e', '\n', // -> Confirm
            ],
        )
        .state;
        assert_eq!(at_confirm.create_step(), Some(CreateStep::Confirm));
        let mut buf = WireBuffer::new(80, 24);
        render_agents(&mut buf, 80, 1, 23, &at_confirm);
        let text = buffer_text(&buf, 80, 24);
        assert!(text.contains("codex"), "confirm shows the chosen provider");
        assert!(text.contains("gpt-5"), "confirm shows the chosen model");
        assert!(
            text.contains("be terse"),
            "confirm shows a slice of the instructions"
        );
    }

    /// The `Provider` picker step renders the current provider choice.
    #[test]
    fn render_provider_step_shows_choice() {
        let state = AgentsState::from_actors(&snapshot());
        let opened = reduce_agents(&state, AgentsEvent::Key('n')).state;
        // Name + Enter -> Provider, then cycle to codex.
        let at_provider = drive(&opened, &['a', '\n', 'l']).state;
        assert_eq!(at_provider.create_step(), Some(CreateStep::Provider));
        let mut buf = WireBuffer::new(80, 24);
        render_agents(&mut buf, 80, 1, 23, &at_provider);
        let text = buffer_text(&buf, 80, 24);
        assert!(
            text.contains("codex"),
            "the provider picker shows the current choice"
        );
    }

    /// Reassemble the buffer text for render assertions.
    fn buffer_text(buf: &WireBuffer, w: u16, h: u16) -> String {
        let mut grid = vec![' '; (w as usize) * (h as usize)];
        for (coord, cell) in &buf.cells {
            if coord.x < w && coord.y < h {
                if let Some(ch) = cell.symbol.chars().next() {
                    grid[(coord.y as usize) * (w as usize) + coord.x as usize] = ch;
                }
            }
        }
        grid.into_iter().collect()
    }
}
