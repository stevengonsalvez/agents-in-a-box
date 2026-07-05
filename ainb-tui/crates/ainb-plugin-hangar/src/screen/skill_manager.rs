//! P4.6 / P6.5 — Skill manager screen: the pure reducer + three-pane
//! width-aware render.
//!
//! The skill manager (hotkey `3`) is a three-region screen: a left skill list, a
//! middle file tree for the selected skill, and a right detail/editor pane.
//! Filter chips (`All` / `Used` / `Unused` / `Mine`) narrow the list. P6.5 wired
//! the action keys to live daemon RPCs: `s` runs the curated-skills importer
//! (`hangar/skills_sync`), `Enter` opens the detail pane (`hangar/skill_get`,
//! body + file tree), and `i` / `d` attach / detach the selected skill to / from
//! the selected agent (`hangar/skill_attach` / `hangar/skill_detach`). A remote
//! [`HangarEvent::SkillUpdated`] surfaces a conflict banner *only* when the local
//! copy is dirty — a clean copy refreshes silently.
//!
//! As with every Hangar screen the reducer ([`reduce_skill_manager`]) is **pure**:
//! it folds a key / host event into a new [`SkillManagerState`] plus an optional
//! [`SkillManagerIntent`] (which the plugin glue lifts into the matching daemon
//! RPC). The skill list + detail come from the daemon (`hangar/skills_list`,
//! `hangar/skill_get`); the plugin owns zero domain data
//! (`project_ainb_plugin_owns_data_plane`).

use std::collections::BTreeSet;

use ainb_hangar_proto::events::{HangarEvent, SkillFile, SkillRow};
use ainb_plugin_sdk::WireBuffer;

use crate::widgets::editor_pane::render_editor_pane;
use crate::widgets::file_tree::render_file_tree;
use crate::widgets::filter_chip::render_chip_bar;

/// Below this area width the middle file-tree column collapses away (P4.6
/// width-aware requirement), leaving the list + editor.
const MIDDLE_COLLAPSE_WIDTH: u16 = 100;

/// The skill-list filter chips (UX §2 journey 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillFilter {
    /// Every skill.
    All,
    /// Only skills referenced by at least one agent.
    Used,
    /// Only orphan skills (referenced by none).
    Unused,
    /// Only skills owned by the current member.
    Mine,
}

impl SkillFilter {
    /// The chip labels in tab order.
    #[must_use]
    pub const fn labels() -> [&'static str; 4] {
        ["All", "Used", "Unused", "Mine"]
    }

    /// This filter's index in [`Self::labels`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::All => 0,
            Self::Used => 1,
            Self::Unused => 2,
            Self::Mine => 3,
        }
    }
}

/// The render-state cache for the skill-manager screen.
///
/// Holds the skill snapshot, the active filter, the list selection, the loaded
/// file tree (+ its selection) for the selected skill, the set of locally-dirty
/// skill slugs, and the optional remote-conflict banner. All fields private;
/// tests and the renderer read through accessors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillManagerState {
    skills: Vec<SkillRow>,
    filter: SkillFilter,
    selected: usize,
    files: Vec<SkillFile>,
    file_selected: usize,
    dirty: BTreeSet<String>,
    conflict_banner: Option<String>,
    /// The detail body (`SKILL.md`) of the skill whose detail was last loaded,
    /// keyed by slug so a stale reply for a since-changed selection is ignored.
    /// `None` until the user opens a skill's detail (Enter). Drives the detail
    /// pane (P6.5).
    detail: Option<SkillDetail>,
    /// First visible card index — the list pane's vertical scroll offset (63l.6).
    scroll_offset: usize,
    /// The skill slug the pointer is hovering over (63l.6), or `None` off every
    /// card. The card-board render lifts the hovered card's border.
    hovered_slug: Option<String>,
}

/// The loaded detail of one skill: its slug (to guard stale replies) + the
/// `SKILL.md` body the detail pane renders.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillDetail {
    /// The skill the body belongs to.
    slug: String,
    /// The `SKILL.md` body (empty when the skill is file-only).
    body: String,
}

impl SkillManagerState {
    /// A fresh skill manager over `skills`, `All` filter, first skill selected,
    /// no files loaded, nothing dirty.
    #[must_use]
    pub const fn new(skills: Vec<SkillRow>) -> Self {
        Self {
            skills,
            filter: SkillFilter::All,
            selected: 0,
            files: Vec::new(),
            file_selected: 0,
            dirty: BTreeSet::new(),
            conflict_banner: None,
            detail: None,
            scroll_offset: 0,
            hovered_slug: None,
        }
    }

    /// The current list-selection index (into [`Self::visible_skills`]).
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// The active filter.
    #[must_use]
    pub const fn filter(&self) -> SkillFilter {
        self.filter
    }

    /// The loaded file tree for the selected skill.
    #[must_use]
    pub fn files(&self) -> &[SkillFile] {
        &self.files
    }

    /// The remote-conflict banner text, when a dirty skill was updated remotely.
    #[must_use]
    pub fn conflict_banner(&self) -> Option<&str> {
        self.conflict_banner.as_deref()
    }

    /// The loaded detail body for the currently-selected skill, if one is loaded
    /// AND it still matches the selection (a stale body for a since-changed
    /// selection reads as `None`). Drives the detail pane.
    #[must_use]
    pub fn detail_body(&self) -> Option<&str> {
        let selected = self.selected_skill()?;
        self.detail
            .as_ref()
            .filter(|d| d.slug == selected.slug)
            .map(|d| d.body.as_str())
    }

    /// The skills visible under the active filter.
    #[must_use]
    pub fn visible_skills(&self) -> Vec<SkillRow> {
        self.skills
            .iter()
            .filter(|s| match self.filter {
                SkillFilter::All | SkillFilter::Mine => true,
                SkillFilter::Used => s.used,
                SkillFilter::Unused => !s.used,
            })
            .cloned()
            .collect()
    }

    /// The currently-selected visible skill, if any.
    #[must_use]
    pub fn selected_skill(&self) -> Option<SkillRow> {
        self.visible_skills().into_iter().nth(self.selected)
    }

    /// Flatten the visible skills into a SINGLE card-board column (63l.6) the list
    /// pane paints and the mouse layer hit-tests against. Skills are a flat list
    /// (no status columns), so the board is one `Skills (N)` column of cards; each
    /// card carries the skill name (id line) and its used/unused state (title).
    /// The same geometry feeds the render and the hit-map.
    #[must_use]
    pub fn board_columns(&self) -> Vec<crate::widgets::card_board::BoardColumn> {
        use crate::widgets::card_board::{BoardCard, BoardColumn, PriorityChip};
        let cards = self
            .visible_skills()
            .into_iter()
            .map(|s| BoardCard {
                issue_id: s.slug.clone(),
                display_id: s.name.clone(),
                title: if s.used {
                    "used".to_string()
                } else {
                    "unused".to_string()
                },
                priority: PriorityChip::from_priority(0),
                assignee_initial: s.name.chars().next(),
            })
            .collect::<Vec<_>>();
        vec![BoardColumn {
            glyph: '◆',
            name: "Skills".to_string(),
            cards,
            scroll_offset: self.scroll_offset.min(self.visible_skills().len().saturating_sub(1)),
        }]
    }

    /// The `(0, card)` slot the render draws with the heavy highlight border
    /// (63l.6): the hovered card when the pointer is over one, else the keyboard
    /// selection. `None` when the visible list is empty.
    #[must_use]
    pub fn highlight_board_card(&self) -> Option<(usize, usize)> {
        let visible = self.visible_skills();
        if let Some(hover) = self.hovered_slug.as_deref() {
            if let Some(idx) = visible.iter().position(|s| s.slug == hover) {
                return Some((0, idx));
            }
        }
        (!visible.is_empty()).then_some((0, self.selected))
    }

    /// Set (or clear) the hovered skill slug (63l.6).
    pub fn set_hover(&mut self, slug: Option<String>) {
        self.hovered_slug = slug;
    }

    /// The hovered skill slug, if any (63l.6).
    #[must_use]
    pub fn hovered_slug(&self) -> Option<&str> {
        self.hovered_slug.as_deref()
    }

    /// Scroll the single board column vertically by `delta` rows (63l.6),
    /// saturating at `0` and capped at the last visible card.
    pub fn scroll(&mut self, delta: i32) {
        let len = self.visible_skills().len();
        let next = if delta >= 0 {
            self.scroll_offset.saturating_add(delta.unsigned_abs() as usize)
        } else {
            self.scroll_offset.saturating_sub(delta.unsigned_abs() as usize)
        };
        self.scroll_offset = next.min(len.saturating_sub(1));
    }

    /// Move the list selection onto the visible skill carrying `slug` (63l.6 mouse
    /// click). A no-op when no visible skill carries that slug.
    pub fn select_by_slug(&mut self, slug: &str) {
        if let Some(idx) = self.visible_skills().iter().position(|s| s.slug == slug) {
            self.selected = idx;
        }
    }
}

/// An input the skill-manager reducer folds into [`SkillManagerState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillManagerEvent {
    /// A printable key (`'j'`, `'i'`, `'\n'`, …).
    Key(char),
    /// Set the active filter chip.
    SetFilter(SkillFilter),
    /// The daemon replied to a load-files intent with the skill's file list.
    FilesLoaded {
        /// The skill the files belong to.
        slug: String,
        /// The flat file list.
        files: Vec<SkillFile>,
    },
    /// The daemon replied to a [`SkillManagerIntent::LoadDetail`] with the skill's
    /// detail: its `SKILL.md` body plus its file list (P6.5).
    DetailLoaded {
        /// The skill the detail belongs to.
        slug: String,
        /// The `SKILL.md` body (empty when file-only).
        body: String,
        /// The skill's file list (the file tree).
        files: Vec<SkillFile>,
    },
    /// Mark a skill's local copy dirty (an in-progress edit; P6 wires real edits,
    /// but the dirty flag drives the P4 conflict banner already).
    MarkDirty(String),
    /// A host stream event (e.g. [`HangarEvent::SkillUpdated`]).
    Event(HangarEvent),
}

/// A side-effect the plugin glue performs after a skill-manager reduction.
///
/// Each variant maps to one daemon JSON-RPC the plugin glue fires (P6.5):
/// `LoadDetail` → `hangar/skill_get`, `Sync` → `hangar/skills_sync`,
/// `Attach`/`Detach` → `hangar/skill_attach`/`hangar/skill_detach`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillManagerIntent {
    /// Load the file list for a skill (legacy P4 Enter intent; superseded by
    /// [`Self::LoadDetail`], retained for the file-tree-only path).
    LoadFiles(String),
    /// Open the detail pane for a skill: fetch its `SKILL.md` body + files
    /// (`hangar/skill_get`). Raised by Enter on a list row (P6.5).
    LoadDetail(String),
    /// Run the curated-skills importer (`s`) — `hangar/skills_sync` (P6.5).
    Sync,
    /// Attach the selected skill to the currently-selected agent (`i`) —
    /// `hangar/skill_attach` (P6.5). Carries the skill slug; the glue supplies
    /// the agent from the routing state's selected actor.
    Attach(String),
    /// Detach the selected skill from the selected agent (`d`) —
    /// `hangar/skill_detach` (P6.5).
    Detach(String),
}

/// The result of folding one [`SkillManagerEvent`] into a [`SkillManagerState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillManagerReduction {
    /// The next skill-manager state.
    pub state: SkillManagerState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<SkillManagerIntent>,
}

/// Fold one [`SkillManagerEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_skill_manager(
    state: &SkillManagerState,
    ev: SkillManagerEvent,
) -> SkillManagerReduction {
    match ev {
        SkillManagerEvent::Key(c) => reduce_key(state, c),
        SkillManagerEvent::SetFilter(f) => set_filter(state, f),
        SkillManagerEvent::FilesLoaded { slug, files } => files_loaded(state, &slug, files),
        SkillManagerEvent::DetailLoaded { slug, body, files } => {
            detail_loaded(state, &slug, body, files)
        }
        SkillManagerEvent::MarkDirty(slug) => mark_dirty(state, slug),
        SkillManagerEvent::Event(event) => fold_event(state, event),
    }
}

/// Handle a printable key (P6.5 bindings):
/// - `j`/`k` move the list selection;
/// - `s` runs the curated-skills sync (`hangar/skills_sync`);
/// - `i`/`d` attach/detach the selected skill to/from the selected agent;
/// - `Enter` opens the detail pane (`hangar/skill_get`);
/// - `r` refreshes / dismisses the conflict banner.
fn reduce_key(state: &SkillManagerState, c: char) -> SkillManagerReduction {
    match c {
        'j' => move_selection_down(state),
        'k' => move_selection_up(state),
        's' => with_intent(state.clone(), SkillManagerIntent::Sync),
        'i' => skill_intent(state, SkillManagerIntent::Attach),
        'd' => skill_intent(state, SkillManagerIntent::Detach),
        '\n' | '\r' => enter(state),
        'r' => refresh(state),
        _ => unchanged(state),
    }
}

/// Build an intent that needs the selected skill's slug (`i`/`d`), or a no-op
/// when nothing is selected.
fn skill_intent(
    state: &SkillManagerState,
    make: impl FnOnce(String) -> SkillManagerIntent,
) -> SkillManagerReduction {
    match state.selected_skill() {
        Some(skill) => with_intent(state.clone(), make(skill.slug)),
        None => unchanged(state),
    }
}

/// Move the list selection down one row, clamped to the last visible skill.
fn move_selection_down(state: &SkillManagerState) -> SkillManagerReduction {
    let mut next = state.clone();
    let max = next.visible_skills().len().saturating_sub(1);
    next.selected = (next.selected + 1).min(max);
    no_intent(next)
}

/// Move the list selection up one row, clamped to the first visible skill.
fn move_selection_up(state: &SkillManagerState) -> SkillManagerReduction {
    let mut next = state.clone();
    next.selected = next.selected.saturating_sub(1);
    no_intent(next)
}

/// Enter on a skill: emit the load-detail intent for the selected skill (fetches
/// the `SKILL.md` body + file list for the detail pane).
fn enter(state: &SkillManagerState) -> SkillManagerReduction {
    match state.selected_skill() {
        Some(skill) => with_intent(state.clone(), SkillManagerIntent::LoadDetail(skill.slug)),
        None => unchanged(state),
    }
}

/// Apply a filter chip, clamping the selection into the narrowed list.
fn set_filter(state: &SkillManagerState, filter: SkillFilter) -> SkillManagerReduction {
    let mut next = state.clone();
    next.filter = filter;
    let len = next.visible_skills().len();
    if len == 0 {
        next.selected = 0;
    } else if next.selected >= len {
        next.selected = len - 1;
    }
    no_intent(next)
}

/// Populate the file tree for the selected skill from a daemon reply.
fn files_loaded(
    state: &SkillManagerState,
    slug: &str,
    files: Vec<SkillFile>,
) -> SkillManagerReduction {
    let mut next = state.clone();
    // Only adopt files for the still-selected skill (avoid a stale reply
    // clobbering a fresh selection).
    if next.selected_skill().is_some_and(|s| s.slug == slug) {
        next.files = files;
        next.file_selected = 0;
    }
    no_intent(next)
}

/// Populate the detail pane (body + file tree) for the selected skill from a
/// daemon reply, ignoring a reply for a since-changed selection.
fn detail_loaded(
    state: &SkillManagerState,
    slug: &str,
    body: String,
    files: Vec<SkillFile>,
) -> SkillManagerReduction {
    let mut next = state.clone();
    if next.selected_skill().is_some_and(|s| s.slug == slug) {
        next.detail = Some(SkillDetail {
            slug: slug.to_string(),
            body,
        });
        next.files = files;
        next.file_selected = 0;
    }
    no_intent(next)
}

/// Mark a skill's local copy dirty.
fn mark_dirty(state: &SkillManagerState, slug: String) -> SkillManagerReduction {
    let mut next = state.clone();
    next.dirty.insert(slug);
    no_intent(next)
}

/// Dismiss the conflict banner and clear the dirty flag for the refreshed skill
/// (`r`). Refresh re-pulls the remote copy, so the local edit is discarded.
fn refresh(state: &SkillManagerState) -> SkillManagerReduction {
    let mut next = state.clone();
    if let Some(skill) = next.selected_skill() {
        next.dirty.remove(&skill.slug);
    }
    next.conflict_banner = None;
    no_intent(next)
}

/// Fold a host event: a remote `SkillUpdated` raises the conflict banner only
/// when the named skill's local copy is dirty.
fn fold_event(state: &SkillManagerState, event: HangarEvent) -> SkillManagerReduction {
    let mut next = state.clone();
    if let HangarEvent::SkillUpdated { skill, .. } = event {
        if next.dirty.contains(&skill) {
            next.conflict_banner = Some(format!(
                "⚠ skill {skill} updated remotely · press r to refresh"
            ));
        }
        // Clean local copy: refresh silently (no banner, nothing to discard).
    }
    no_intent(next)
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: SkillManagerState) -> SkillManagerReduction {
    SkillManagerReduction {
        state,
        intent: None,
    }
}

/// A reduction carrying `intent` alongside `state`.
const fn with_intent(
    state: SkillManagerState,
    intent: SkillManagerIntent,
) -> SkillManagerReduction {
    SkillManagerReduction {
        state,
        intent: Some(intent),
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &SkillManagerState) -> SkillManagerReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware three-pane render
// ---------------------------------------------------------------------------

/// Render the skill manager into `buf` between rows `top` and `bottom`.
///
/// Three columns derived from `area_w`: a fixed-width skill list (~22 cols), a
/// fixed-width file tree (~26 cols) that *collapses away* below
/// [`MIDDLE_COLLAPSE_WIDTH`], and the editor pane taking the remainder. The
/// filter chip bar paints on `top`; the conflict banner (when present) paints on
/// the row below the chips.
pub fn render_skill_manager(
    buf: &mut WireBuffer,
    area_w: u16,
    top: u16,
    bottom: u16,
    state: &SkillManagerState,
) {
    // Chip bar on the top row, with the action-key hints painted to its right so
    // each key sits next to the control it affects
    // (`feedback_keybinding_hints_near_control`).
    let labels = SkillFilter::labels();
    render_chip_bar(buf, top, area_w, &labels, state.filter.index());
    render_action_hints(buf, top, area_w);

    let mut content_top = top + 1;
    if let Some(banner) = state.conflict_banner() {
        crate::widgets::filter_chip::render_chip_bar(buf, content_top, area_w, &[banner], 0);
        content_top += 1;
    }

    // The list pane is widened to 28 cols so the card-board column (min 14) paints
    // a readable bordered card (63l.6).
    let list_w: u16 = 28;
    let tree_w: u16 = if area_w >= MIDDLE_COLLAPSE_WIDTH {
        26
    } else {
        0
    };
    let editor_x = list_w + tree_w;
    let editor_w = area_w.saturating_sub(editor_x);

    render_skill_list(buf, list_w, content_top, bottom, state);
    if tree_w > 0 {
        render_file_tree(
            buf,
            list_w,
            content_top,
            bottom,
            tree_w,
            &state.files,
            state.file_selected,
        );
    }
    // P6.5: the editor pane renders the loaded `SKILL.md` body for the selected
    // skill (the detail pane). `None` until the user opens a skill (Enter), when
    // the placeholder is shown.
    render_editor_pane(
        buf,
        editor_x,
        content_top,
        bottom,
        editor_w,
        state.detail_body(),
    );
}

/// Paint the action-key hints (`s sync · i attach · d detach · ⏎ open`) on the
/// chip row, right-aligned so each key sits beside the chip bar it acts on. Kept
/// short (caveman) and muted so it reads as a hint, not a chip.
fn render_action_hints(buf: &mut WireBuffer, row: u16, area_w: u16) {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const HINT: Color = Color::rgb(120, 120, 140);
    const HINTS: &str = "s sync · i attach · d detach · ⏎ open";
    let hint_w = u16::try_from(HINTS.chars().count()).unwrap_or(0);
    if hint_w >= area_w {
        return;
    }
    let start = area_w.saturating_sub(hint_w);
    for (ch, cx) in HINTS.chars().zip(start..area_w) {
        let mut cell = Cell::new(ch.to_string());
        cell.fg = Some(HINT);
        buf.push(Coord::new(cx, row), cell);
    }
}

/// Render the left skill list as a single card-board column (63l.6): one
/// `Skills (N)` column of bordered cards, each carrying the skill name (id line)
/// and its used/unused state (title), with the hovered/selected card raised by
/// the heavy clay border. Painted through the shared card-board so the list pane
/// hit-tests against exactly the geometry it draws.
fn render_skill_list(
    buf: &mut WireBuffer,
    width: u16,
    top: u16,
    bottom: u16,
    state: &SkillManagerState,
) {
    let columns = state.board_columns();
    let highlight = state.highlight_board_card();
    let _ =
        crate::widgets::card_board::render_card_board(buf, width, top, bottom, &columns, highlight);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(slug: &str, name: &str, used: bool) -> SkillRow {
        SkillRow {
            slug: slug.into(),
            name: name.into(),
            used,
            updated_at: 0,
        }
    }

    /// `board_columns` flattens the visible skills into a single card-board column
    /// whose cards carry the skill name + used/unused state, so the list pane
    /// renders THROUGH the shared card-board (63l.6).
    #[test]
    fn board_columns_map_skills_to_a_single_column_of_cards() {
        let state = SkillManagerState::new(vec![
            skill("commit", "commit", true),
            skill("brainstorm", "brainstorm", false),
        ]);
        let cols = state.board_columns();
        assert_eq!(cols.len(), 1, "skills are a single flat column");
        assert_eq!(cols[0].name, "Skills");
        assert_eq!(cols[0].cards.len(), 2);
        assert_eq!(cols[0].cards[0].issue_id, "commit");
        assert_eq!(cols[0].cards[0].display_id, "commit");
        assert_eq!(cols[0].cards[0].title, "used");
        assert_eq!(cols[0].cards[1].title, "unused");
    }

    /// The board column respects the active filter: the `Unused` chip drops the
    /// used skill from the cards (the board paints exactly the visible list).
    #[test]
    fn board_columns_respect_the_active_filter() {
        let state = SkillManagerState::new(vec![
            skill("commit", "commit", true),
            skill("brainstorm", "brainstorm", false),
        ]);
        let filtered = set_filter(&state, SkillFilter::Unused).state;
        let cols = filtered.board_columns();
        assert_eq!(cols[0].cards.len(), 1, "only the unused skill is a card");
        assert_eq!(cols[0].cards[0].issue_id, "brainstorm");
    }

    /// A hover overrides the selection for the highlight; clearing it falls back.
    #[test]
    fn highlight_resolves_hover_over_selection() {
        let mut state = SkillManagerState::new(vec![
            skill("commit", "commit", true),
            skill("brainstorm", "brainstorm", false),
        ]);
        assert_eq!(state.highlight_board_card(), Some((0, 0)));
        state.set_hover(Some("brainstorm".to_string()));
        assert_eq!(state.highlight_board_card(), Some((0, 1)));
        state.set_hover(None);
        assert_eq!(state.highlight_board_card(), Some((0, 0)));
    }

    /// A mouse click selects the clicked skill by slug (no-op for an unknown slug).
    #[test]
    fn select_by_slug_moves_the_selection() {
        let mut state = SkillManagerState::new(vec![
            skill("commit", "commit", true),
            skill("brainstorm", "brainstorm", false),
        ]);
        state.select_by_slug("brainstorm");
        assert_eq!(state.selected_index(), 1);
        state.select_by_slug("ghost");
        assert_eq!(state.selected_index(), 1, "an unknown slug is a no-op");
    }
}
