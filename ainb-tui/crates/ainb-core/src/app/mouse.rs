// ABOUTME: Mouse handling for the terminal host. A press is hit-tested against
// where this renderer last drew things (the sessions pane, the menu bar, the
// Skill Manager panels), which only the renderer knows, and answered with the
// intent it means for dispatch. Drags, releases and hovers stay here.

use std::time::Instant;

use crate::app::pointer;
use crate::app::screens::ids as screen_ids;
use crate::app::ui_state::UiState;
use crate::app::{AppState, Args, Btn, CommandId, Intent, Pos};

// Layout configuration - sessions pane width as percentage of terminal width
const SESSIONS_PANE_WIDTH_PERCENTAGE: f32 = 0.4;

/// A keymap command a click runs, the mouse twin of its key.
fn keymap_command(id: &str) -> Intent {
    Intent::Command(CommandId::new(id), Args::Null)
}

/// Save the sessions pane layout the renderer just changed, as a fraction of
/// the row it was drawn in.
fn save_sessions_pane_layout(ui: &UiState) -> Option<Intent> {
    let row = ui
        .sessions_pane
        .last_content_width()
        .unwrap_or_else(|| crossterm::terminal::size().unwrap_or((80, 24)).0);
    ui.sessions_pane.save_layout(row)
}

/// Recompute the SkillManager top-row rects (Sources panel + Units
/// table) from the current terminal size + persisted `sources_width`,
/// mirroring the deterministic layout in `skill_manager_screen::render`:
///
/// ```text
/// outer (vertical):  [ Min(8) top ][ Length(8) detail ][ Length(1) help ]
/// top   (horizontal):[ Length(sources_w) ][ Min(40) units ]
/// ```
///
/// The render path always draws into the full terminal Rect
/// `(0,0,w,h)`, so we reconstruct that here rather than threading a
/// Rect through the immutable render. Returns `(sources_rect,
/// units_rect, sources_w)` or `None` when the terminal is too small
/// to host the top row.
fn skill_manager_top_rects(
    state: &AppState,
    ui: &UiState,
) -> Option<(ratatui::layout::Rect, ratatui::layout::Rect, u16)> {
    use ratatui::layout::Rect;
    let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
    // Vertical layout: the top row is everything above the 8-row
    // detail pane + 1-row help bar. Mirror `Constraint::Min(8)`.
    let top_h = term_h.saturating_sub(9);
    if term_w == 0 || top_h == 0 {
        return None;
    }
    let sources_w = crate::components::skill_manager_screen::clamp_sources_width(
        ui.skill_sources.preferred_width(
            saved_sources_fraction(state),
            term_w,
            crate::components::skill_manager_screen::DEFAULT_SOURCES_WIDTH,
        ),
        term_w,
    );
    let sources_rect = Rect::new(0, 0, sources_w, top_h);
    let units_x = sources_w;
    let units_w = term_w.saturating_sub(sources_w);
    let units_rect = Rect::new(units_x, 0, units_w, top_h);
    Some((sources_rect, units_rect, sources_w))
}

/// The Sources panel width the user saved, a fraction of the screen, which a
/// surface starts from.
fn saved_sources_fraction(state: &AppState) -> Option<f64> {
    state.config.app_config.ui_preferences.skill_manager_sources_fraction
}

/// Columns either side of the home sidebar's right border that grab it.
const HOME_SIDEBAR_EDGE_SLOP: u16 = 1;

/// Whether (`x`, `y`) grabs the right border of a sidebar drawn at `rect`.
fn on_home_sidebar_edge(rect: ratatui::layout::Rect, x: u16, y: u16) -> bool {
    if y < rect.y || y >= rect.y.saturating_add(rect.height) || rect.width == 0 {
        return false;
    }
    let edge_x = rect.x.saturating_add(rect.width.saturating_sub(1));
    x.abs_diff(edge_x) <= HOME_SIDEBAR_EDGE_SLOP
}

/// True when `(x, y)` falls inside `rect` (half-open on the far
/// edges, matching ratatui's Rect convention).
fn point_in_rect(x: u16, y: u16, rect: ratatui::layout::Rect) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

/// Hit-test a press at `pos` and return the intent it means.
///
/// Reads state only. Renderer-local changes (collapsing the sessions pane,
/// arming a pane resize, remembering a row click for double-click) land in
/// `ui`; everything else is an intent the reducer applies.
pub fn press(state: &AppState, ui: &mut UiState, pos: Pos, btn: Btn) -> Option<Intent> {
    // Mode boundary (defense in depth): while the interactive embed owns
    // input, host mouse handling must never change focus or selection under
    // the live pane. main.rs already swallows/forwards mouse events before
    // dispatching, but the boundary must hold even if a future call site
    // forgets the gate. Pinned by the mode-boundary tripwire.
    if state.is_interactive_pane() {
        return None;
    }
    let (x, y) = (pos.x, pos.y);
    match btn {
        Btn::Left => left_press(state, ui, x, y),
        Btn::Right => {
            if state.shell.current_screen != screen_ids::SESSION_LIST || state.shell.help_visible {
                return None;
            }
            let target = state.session_list_row_target(ui.sessions_pane.row_index_at(x, y)?)?;
            let id = state.session_list_row_id(target)?;
            Some(pointer::open_session_row_menu(&id))
        }
        Btn::Middle => None,
    }
}

fn left_press(state: &AppState, ui: &mut UiState, x: u16, y: u16) -> Option<Intent> {
    // Code review sidebar: name the row under the press by its path.
    if state.shell.current_screen == screen_ids::GIT_VIEW && !state.shell.help_visible {
        let git = state.git_view.git_view_state.as_ref()?;
        if git.active_tab != crate::components::git_view::GitTab::Review {
            return None;
        }
        let row = crate::components::code_review::render::sidebar_row_at(&ui.review_sidebar, x, y)?;
        return git.review_row_id(row).map(|id| pointer::select_review_row(&id));
    }

    if state.shell.current_screen == screen_ids::HOME && !state.shell.help_visible {
        let rect = ui.home_sidebar_rect?;
        let on_edge = on_home_sidebar_edge(rect, x, y);
        ui.home_sidebar.resize_active = on_edge;
        ui.home_sidebar.edge_hovered = on_edge;
        if on_edge || !point_in_rect(x, y, rect) {
            return None;
        }
        let selected = state.shell.home_screen_v2_state.sidebar.selected_index;
        return crate::components::sidebar::item_index_at(rect, y, selected)
            .and_then(|index| crate::components::sidebar::SidebarItem::all().get(index).copied())
            .map(pointer::click_home_sidebar_item);
    }

    // SkillManager: divider-drag-resize + click-to-select on
    // Sources / Units. Guarded so clicks meant for an open
    // overlay (banner / input / library / browse / help)
    // don't leak through to the panels.
    if state.shell.current_screen == screen_ids::SKILL_MANAGER {
        if crate::app::skill_manager_overlay_open(state) {
            return None;
        }
        let (sources_rect, units_rect, sources_w) = skill_manager_top_rects(state, ui)?;
        // Resize edge = the Sources panel's right border column. Begin a drag
        // (consumed on subsequent drag gestures).
        let edge_x = sources_w.saturating_sub(1);
        if x == edge_x
            && y >= sources_rect.y
            && y < sources_rect.y.saturating_add(sources_rect.height)
        {
            ui.skill_sources.resize_active = true;
            return None;
        }

        // Click inside the Sources panel body → focus + select that source
        // (applies the filter). Source rows start at `rect.y + 1` (after the
        // top border); row 0 is the "All sources" affordance, rows 1.. map
        // onto `sources[index]`.
        if point_in_rect(x, y, sources_rect) {
            let row = y.saturating_sub(sources_rect.y).saturating_sub(1);
            if row == 0 {
                return Some(pointer::all_skill_sources());
            }
            let index = usize::from(row.saturating_sub(1));
            if let Some(source) = state.skills.skill_manager_state.sources.get(index) {
                return Some(pointer::select_skill_source(&source.uri));
            }
            // Empty area inside the panel → just focus it.
            return Some(pointer::focus_skill_pane(
                crate::components::skill_manager_screen::FocusedSkillPane::Sources,
            ));
        }

        // Click inside the Units table → focus + select the clicked unit.
        // Unit data rows start at `rect.y + 2` (top border + header row); map
        // y onto a position within `visible_indices()`.
        if point_in_rect(x, y, units_rect) {
            let data_y = sources_rect.y.saturating_add(2);
            if y >= data_y {
                let skills = &state.skills.skill_manager_state;
                let position = usize::from(y - data_y);
                if let Some(&index) = skills.visible_indices().get(position) {
                    return Some(pointer::select_skill_unit(
                        &skills.units[index].declared_uri,
                    ));
                }
            }
            return Some(pointer::focus_skill_pane(
                crate::components::skill_manager_screen::FocusedSkillPane::Units,
            ));
        }
        return None;
    }

    if state.shell.current_screen != screen_ids::SESSION_LIST || state.shell.help_visible {
        return None;
    }

    // Click on the bottom keymap legend (or its collapsed hint row) toggles
    // it, the mouse twin of ⇧M.
    if ui.menu_bar_area.is_some_and(|area| point_in_rect(x, y, area)) {
        return Some(keymap_command("session_list.menu_bar"));
    }

    // A click on a label of the right pane's tab strip shows that tab, the
    // mouse twin of `tab`. The padding and separators fall through to the
    // pane-focus click below.
    if let Some(tab) = ui.sessions_pane.tab_at(x, y) {
        return Some(pointer::select_session_tab(tab));
    }

    if ui.sessions_pane.is_on_filter_toggle(x, y) {
        return Some(keymap_command("session_list.cycle_filter"));
    }

    if ui.sessions_pane.is_on_toggle(x, y) {
        ui.sessions_pane.toggle_collapsed();
        return save_sessions_pane_layout(ui);
    }

    if ui.sessions_pane.begin_resize(x, y) {
        return None;
    }

    let row = ui.sessions_pane.row_index_at(x, y);
    let hit = row
        .and_then(|row| state.session_list_row_target(row))
        .and_then(|target| Some((target, state.session_list_row_id(target)?)));
    if let Some((target, id)) = hit {
        let open = ui.sessions_pane.record_row_click(target, Instant::now());
        return Some(pointer::select_session_row(&id, open));
    }

    let pane = if ui.sessions_pane.contains_sessions_point(x, y) {
        crate::app::state::FocusedPane::Sessions
    } else if ui.sessions_pane.contains_preview_point(x, y) {
        crate::app::state::FocusedPane::LiveLogs
    } else {
        // Determine which pane was clicked based on terminal dimensions.
        // The layout splits at 40% for sessions, 60% for logs.
        let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0;
        let split_point = (f32::from(term_width) * SESSIONS_PANE_WIDTH_PERCENTAGE) as u16;
        if x < split_point {
            crate::app::state::FocusedPane::Sessions
        } else {
            crate::app::state::FocusedPane::LiveLogs
        }
    };
    Some(pointer::focus_session_pane(&pane))
}

/// A pointer movement after a press, or without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// Moved with the left button held.
    Drag,
    /// Left button released.
    Release,
    /// Moved with no button held.
    Move,
}

/// Apply a drag, release or hover at `pos`.
///
/// Resize drags and edge hovers are layout the renderer is in the middle of
/// changing, so they are applied here and never reach the reducer. When one
/// finishes, the returned intent persists the new width.
pub fn gesture(gesture: Gesture, pos: Pos, state: &AppState, ui: &mut UiState) -> Option<Intent> {
    if state.is_interactive_pane() {
        return None;
    }
    let (x, y) = (pos.x, pos.y);
    let on_home = state.shell.current_screen == screen_ids::HOME && !state.shell.help_visible;
    let on_sessions =
        state.shell.current_screen == screen_ids::SESSION_LIST && !state.shell.help_visible;
    let on_skills = state.shell.current_screen == screen_ids::SKILL_MANAGER;
    match gesture {
        Gesture::Drag => {
            if on_home && ui.home_sidebar.resize_active {
                if let Some(rect) = ui.home_sidebar_rect {
                    let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0;
                    let requested = x.saturating_sub(rect.x).saturating_add(1);
                    ui.home_sidebar.set_width(
                        crate::components::sidebar::SidebarState::clamp_width(
                            requested, term_width,
                        ),
                    );
                    ui.needs_redraw = true;
                }
            } else if on_sessions {
                let width = ui
                    .sessions_pane
                    .last_content_width()
                    .unwrap_or_else(|| crossterm::terminal::size().unwrap_or((80, 24)).0);
                ui.sessions_pane.drag_resize(x, width);
            } else if on_skills && ui.skill_sources.resize_active {
                // SkillManager divider drag: the new Sources width is the
                // pointer's x + 1 (the panel spans columns 0..=x), clamped
                // by the same clamp the `[`/`]` steps use.
                let term_w = crossterm::terminal::size().unwrap_or((80, 24)).0;
                ui.skill_sources.set_width(
                    crate::components::skill_manager_screen::clamp_sources_width(
                        x.saturating_add(1),
                        term_w,
                    ),
                );
            }
            None
        }
        Gesture::Release => {
            if on_home {
                ui.home_sidebar.edge_hovered =
                    ui.home_sidebar_rect.is_some_and(|rect| on_home_sidebar_edge(rect, x, y));
                if !std::mem::take(&mut ui.home_sidebar.resize_active) {
                    return None;
                }
                let columns = crossterm::terminal::size().unwrap_or((80, 24)).0;
                let width = ui.home_sidebar.preferred_width(
                    state.config.app_config.ui_preferences.home_sidebar_fraction,
                    columns,
                    crate::components::sidebar::DEFAULT_SIDEBAR_WIDTH,
                );
                Some(pointer::save_home_sidebar_width(
                    crate::components::sidebar::SidebarState::clamp_width(width, columns),
                    columns,
                ))
            } else if on_sessions {
                ui.sessions_pane.update_hover(x, y);
                ui.sessions_pane
                    .finish_resize()
                    .then(|| save_sessions_pane_layout(ui))
                    .flatten()
            } else if on_skills && ui.skill_sources.resize_active {
                ui.skill_sources.resize_active = false;
                let columns = crossterm::terminal::size().unwrap_or((80, 24)).0;
                let width = ui.skill_sources.preferred_width(
                    saved_sources_fraction(state),
                    columns,
                    crate::components::skill_manager_screen::DEFAULT_SOURCES_WIDTH,
                );
                Some(pointer::save_skill_sources_width(width, columns))
            } else {
                None
            }
        }
        Gesture::Move => {
            if on_home {
                let hovered =
                    ui.home_sidebar_rect.is_some_and(|rect| on_home_sidebar_edge(rect, x, y));
                if hovered != ui.home_sidebar.edge_hovered {
                    ui.home_sidebar.edge_hovered = hovered;
                    ui.needs_redraw = true;
                }
            }
            if on_sessions {
                ui.sessions_pane.update_hover(x, y);
            }
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::screens::ids;

    /// A click anywhere on the published menu-bar rect toggles the legend
    /// (the mouse twin of ⇧M); a click above it does not.
    #[test]
    fn click_on_menu_bar_toggles_the_legend() {
        use ratatui::layout::Rect;
        let mut state = AppState::default();
        state.shell.current_screen = ids::SESSION_LIST.to_string();
        let mut ui = UiState::default();
        ui.menu_bar_area = Some(Rect::new(0, 20, 100, 6));
        let toggle = Some(keymap_command("session_list.menu_bar"));

        let inside = press(&state, &mut ui, Pos { x: 10, y: 22 }, Btn::Left);
        assert_eq!(inside, toggle);

        let outside = press(&state, &mut ui, Pos { x: 10, y: 5 }, Btn::Left);
        assert_ne!(outside, toggle);
    }

    /// A left click on a painted strip label names that tab; a click on the
    /// strip's padding does not.
    #[test]
    fn click_on_a_tab_label_selects_that_tab() {
        use crate::components::session_tabs::SessionTab;
        use ratatui::layout::Rect;
        let mut state = AppState::default();
        state.shell.current_screen = ids::SESSION_LIST.to_string();
        let mut ui = UiState::default();
        ui.sessions_pane.set_tab_strip(vec![
            (SessionTab::Pal, Rect::new(72, 3, 3, 1)),
            (SessionTab::Log, Rect::new(78, 3, 3, 1)),
        ]);

        let on_log = press(&state, &mut ui, Pos { x: 79, y: 3 }, Btn::Left);
        assert_eq!(on_log, Some(pointer::select_session_tab(SessionTab::Log)));
        let on_pal = press(&state, &mut ui, Pos { x: 72, y: 3 }, Btn::Left);
        assert_eq!(on_pal, Some(pointer::select_session_tab(SessionTab::Pal)));

        // Between the labels: no tab at all, not merely neither neighbour.
        let on_bar = press(&state, &mut ui, Pos { x: 76, y: 3 }, Btn::Left);
        for tab in crate::components::session_tabs::ALL_TABS {
            assert_ne!(on_bar, Some(pointer::select_session_tab(tab)), "{tab:?}");
        }
    }
}
