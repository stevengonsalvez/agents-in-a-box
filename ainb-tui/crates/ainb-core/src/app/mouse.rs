// ABOUTME: Mouse handling for the terminal host. Clicks, drags and hovers are
// hit-tested against where this renderer last drew things (the sessions pane,
// the menu bar, the Skill Manager panels), which only the renderer knows, and
// then turned into state changes or AppEvents for the shared reducer.

use std::time::Instant;

use crate::app::AppState;
use crate::app::events::{AppEvent, EventHandler};
use crate::app::screens::ids as screen_ids;
use crate::app::ui_state::UiState;

// Layout configuration - sessions pane width as percentage of terminal width
const SESSIONS_PANE_WIDTH_PERCENTAGE: f32 = 0.4;

pub fn persist_sessions_pane_preferences(state: &mut AppState, ui: &UiState) {
    state.config.app_config.ui_preferences.sessions_sidebar_width =
        Some(ui.sessions_pane.preferred_width);
    state.config.app_config.ui_preferences.sessions_sidebar_collapsed =
        Some(ui.sessions_pane.collapsed);
    if let Err(e) = state.config.app_config.save() {
        tracing::warn!("Failed to persist Sessions pane preferences: {}", e);
    }
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
        state.skills.skill_manager_state.sources_width,
        term_w,
    );
    let sources_rect = Rect::new(0, 0, sources_w, top_h);
    let units_x = sources_w;
    let units_w = term_w.saturating_sub(sources_w);
    let units_rect = Rect::new(units_x, 0, units_w, top_h);
    Some((sources_rect, units_rect, sources_w))
}

/// True when `(x, y)` falls inside `rect` (half-open on the far
/// edges, matching ratatui's Rect convention).
fn point_in_rect(x: u16, y: u16, rect: ratatui::layout::Rect) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

/// Handle mouse events and convert to appropriate app events
pub fn handle_mouse_event(
    event: AppEvent,
    state: &mut AppState,
    ui: &mut UiState,
) -> Option<AppEvent> {
    // Mode boundary (defense in depth): while the interactive embed owns
    // input, host mouse handling must never mutate focus/selection under
    // the live pane. main.rs already swallows/forwards mouse events before
    // calling this, but the boundary must hold even if a future call site
    // forgets the gate. Pinned by the mode-boundary tripwire.
    if state.is_interactive_pane() {
        return None;
    }
    match event {
        AppEvent::MouseRightClick { x, y } => {
            if state.shell.current_screen == screen_ids::SESSION_LIST && !state.shell.help_visible {
                if let Some(crate::app::state::SessionListRowTarget::Attachable(target)) =
                    state.session_list_row_at_mouse(&ui.sessions_pane, x, y)
                {
                    if matches!(
                        target,
                        crate::app::state::AttachableRef::WorkspaceSession { .. }
                            | crate::app::state::AttachableRef::SshSession { .. }
                    ) {
                        state.open_session_context_menu(target);
                    }
                }
            }
            None
        }
        AppEvent::MouseClick { x, y } => {
            if state.shell.current_screen == screen_ids::HOME && !state.shell.help_visible {
                if state.shell.home_screen_v2_state.begin_sidebar_resize(x, y) {
                    return None;
                }

                if let Some(outcome) =
                    state.shell.home_screen_v2_state.click_sidebar_item_at(x, y, Instant::now())
                {
                    if outcome.double_click {
                        return Some(AppEvent::HomeScreenSidebarSelect);
                    }
                }

                return None;
            }

            // SkillManager: divider-drag-resize + click-to-select on
            // Sources / Units. Guarded so clicks meant for an open
            // overlay (banner / input / library / browse / help)
            // don't leak through to the panels.
            if state.shell.current_screen == screen_ids::SKILL_MANAGER
                && !EventHandler::skill_manager_overlay_open(state)
            {
                if let Some((sources_rect, units_rect, sources_w)) = skill_manager_top_rects(state)
                {
                    // Resize edge = the Sources panel's right border
                    // column. Begin a drag (consumed on subsequent
                    // MouseDragging events).
                    let edge_x = sources_w.saturating_sub(1);
                    let on_edge = x == edge_x
                        && y >= sources_rect.y
                        && y < sources_rect.y.saturating_add(sources_rect.height);
                    if on_edge {
                        state.skills.skill_manager_state.resize_active = true;
                        return None;
                    }

                    // Click inside the Sources panel body → focus +
                    // select that source (applies the filter). Source
                    // rows start at `rect.y + 1` (after the top
                    // border); row 0 is the "All sources" affordance,
                    // rows 1.. map onto `sources[index]`.
                    if point_in_rect(x, y, sources_rect) {
                        let row = y.saturating_sub(sources_rect.y).saturating_sub(1);
                        if row == 0 {
                            // "All sources" → clear the filter.
                            return Some(AppEvent::SkillManagerClearSourceFilter);
                        }
                        let index = usize::from(row.saturating_sub(1));
                        if index < state.skills.skill_manager_state.sources.len() {
                            return Some(AppEvent::SkillManagerSourceClick { index });
                        }
                        // Empty area inside the panel → just focus it.
                        state.skills.skill_manager_state.focused_pane =
                            crate::components::skill_manager_screen::FocusedSkillPane::Sources;
                        return None;
                    }

                    // Click inside the Units table → focus + select
                    // the clicked unit. Unit data rows start at
                    // `rect.y + 2` (top border + header row); map y
                    // onto a position within `visible_indices()`.
                    if point_in_rect(x, y, units_rect) {
                        let data_y = sources_rect.y.saturating_add(2);
                        if y >= data_y {
                            let position = usize::from(y - data_y);
                            let visible_len =
                                state.skills.skill_manager_state.visible_indices().len();
                            if position < visible_len {
                                return Some(AppEvent::SkillManagerUnitClick { position });
                            }
                        }
                        state.skills.skill_manager_state.focused_pane =
                            crate::components::skill_manager_screen::FocusedSkillPane::Units;
                        return None;
                    }
                }
                return None;
            }

            // Determine which pane was clicked based on terminal dimensions
            // The layout splits at 40% for sessions, 60% for logs
            let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0;
            let split_point = (term_width as f32 * SESSIONS_PANE_WIDTH_PERCENTAGE) as u16;

            // Check if we're in the main view (not in overlays)
            if state.shell.current_screen == screen_ids::SESSION_LIST && !state.shell.help_visible {
                // Click on the bottom keymap legend (or its collapsed hint
                // row) toggles it — the mouse twin of ⇧M.
                if let Some(area) = ui.menu_bar_area {
                    if point_in_rect(x, y, area) {
                        return Some(AppEvent::ToggleSessionMenuBar);
                    }
                }

                if ui.sessions_pane.is_on_filter_toggle(x, y) {
                    return Some(AppEvent::CycleSessionFilter);
                }

                if ui.sessions_pane.is_on_toggle(x, y) {
                    ui.sessions_pane.toggle_collapsed();
                    persist_sessions_pane_preferences(state, ui);
                    return None;
                }

                if ui.sessions_pane.begin_resize(x, y) {
                    return None;
                }

                if let Some(target) = state.session_list_row_at_mouse(&ui.sessions_pane, x, y) {
                    let double_click = ui.sessions_pane.record_row_click(target, Instant::now());
                    state.select_session_list_row(target);
                    if double_click {
                        return Some(AppEvent::AttachTmuxSession);
                    }
                    return None;
                }

                if ui.sessions_pane.contains_sessions_point(x, y) {
                    state.shell.focused_pane = crate::app::state::FocusedPane::Sessions;
                    return None;
                }

                if ui.sessions_pane.contains_preview_point(x, y) {
                    state.shell.focused_pane = crate::app::state::FocusedPane::LiveLogs;
                    return None;
                }

                if x < split_point {
                    state.shell.focused_pane = crate::app::state::FocusedPane::Sessions;
                } else {
                    state.shell.focused_pane = crate::app::state::FocusedPane::LiveLogs;
                }
                None
            } else {
                None
            }
        }
        AppEvent::MouseDragStart { x: _, y: _ } => {
            // Start text selection in logs pane
            if state.shell.focused_pane == crate::app::state::FocusedPane::LiveLogs {
                // This will be handled in Phase 2
                None
            } else {
                None
            }
        }
        AppEvent::MouseDragging { x, y: _ } => {
            if state.shell.current_screen == screen_ids::HOME && !state.shell.help_visible {
                let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0;
                state.shell.home_screen_v2_state.drag_sidebar_resize(x, term_width);
                return None;
            }

            if state.shell.current_screen == screen_ids::SESSION_LIST && !state.shell.help_visible {
                let width = ui
                    .sessions_pane
                    .last_content_width()
                    .unwrap_or_else(|| crossterm::terminal::size().unwrap_or((80, 24)).0);
                ui.sessions_pane.drag_resize(x, width);
                return None;
            }

            // SkillManager divider drag: the new Sources width is the
            // pointer's x + 1 (the panel spans columns 0..=x). Clamped
            // by `grow`/`shrink`'s shared clamp via the setter below.
            if state.shell.current_screen == screen_ids::SKILL_MANAGER
                && state.skills.skill_manager_state.resize_active
            {
                let term_w = crossterm::terminal::size().unwrap_or((80, 24)).0;
                let requested = x.saturating_add(1);
                state.skills.skill_manager_state.sources_width =
                    crate::components::skill_manager_screen::clamp_sources_width(requested, term_w);
                return None;
            }

            // Update selection during drag
            if state.shell.focused_pane == crate::app::state::FocusedPane::LiveLogs {
                // This will be handled in Phase 2
                None
            } else {
                None
            }
        }
        AppEvent::MouseDragEnd { x, y } => {
            if state.shell.current_screen == screen_ids::HOME && !state.shell.help_visible {
                state.shell.home_screen_v2_state.update_sidebar_edge_hover(x, y);
                if state.shell.home_screen_v2_state.finish_sidebar_resize() {
                    let width = state.shell.home_screen_v2_state.sidebar.preferred_width;
                    state.config.app_config.ui_preferences.home_sidebar_width = Some(width);
                    if let Err(e) = state.config.app_config.save() {
                        tracing::warn!("Failed to persist HomeScreen sidebar width: {}", e);
                    }
                }
                return None;
            }

            if state.shell.current_screen == screen_ids::SESSION_LIST && !state.shell.help_visible {
                ui.sessions_pane.update_hover(x, y);
                if ui.sessions_pane.finish_resize() {
                    persist_sessions_pane_preferences(state, ui);
                }
                return None;
            }

            if state.shell.current_screen == screen_ids::SKILL_MANAGER {
                let _ = (x, y);
                if state.skills.skill_manager_state.resize_active {
                    state.skills.skill_manager_state.resize_active = false;
                    return Some(AppEvent::SkillManagerPersistSourcesWidth);
                }
                return None;
            }

            // Finalize text selection
            if state.shell.focused_pane == crate::app::state::FocusedPane::LiveLogs {
                // This will be handled in Phase 2
                None
            } else {
                None
            }
        }
        AppEvent::MouseMove { x, y } => {
            if state.shell.current_screen == screen_ids::HOME && !state.shell.help_visible {
                state.shell.home_screen_v2_state.update_sidebar_edge_hover(x, y);
            }
            if state.shell.current_screen == screen_ids::SESSION_LIST && !state.shell.help_visible {
                ui.sessions_pane.update_hover(x, y);
            }
            None
        }
        _ => None,
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

        let inside = handle_mouse_event(AppEvent::MouseClick { x: 10, y: 22 }, &mut state, &mut ui);
        assert!(matches!(inside, Some(AppEvent::ToggleSessionMenuBar)));

        let outside = handle_mouse_event(AppEvent::MouseClick { x: 10, y: 5 }, &mut state, &mut ui);
        assert!(!matches!(outside, Some(AppEvent::ToggleSessionMenuBar)));
    }
}
