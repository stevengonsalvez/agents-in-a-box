// ABOUTME: Commands a pointer press resolves to. A renderer hit-tests the press
// against what it drew and names the thing under the pointer by its place in
// state (a session row, a source, a unit), never by where it was drawn.

use serde_json::{Value, json};

use crate::app::events::AppEvent;
use crate::app::intent::{Args, Intent};
use crate::app::keymap::CommandId;
use crate::app::state::FocusedPane;
use crate::components::skill_manager_screen::FocusedSkillPane;

/// Ids of the pointer commands. They sit beside the keymap's commands in the
/// same `<context>.<name>` namespace but have no key: each takes a payload
/// only a hit-test can supply.
pub mod ids {
    /// `{"row": usize, "open": bool}`
    pub const SESSION_LIST_SELECT_ROW: &str = "session_list.select_row";
    /// `{"row": usize}`
    pub const SESSION_LIST_OPEN_ROW_MENU: &str = "session_list.open_row_menu";
    /// `{"pane": "sessions" | "live_logs" | "preview"}`
    pub const SESSION_LIST_FOCUS_PANE: &str = "session_list.focus_pane";
    /// `{"width": u16, "collapsed": bool}`
    pub const SESSION_LIST_SAVE_PANE_LAYOUT: &str = "session_list.save_pane_layout";
    /// No arguments.
    pub const SKILL_MANAGER_ALL_SOURCES: &str = "skill_manager.all_sources";
    /// `{"index": usize}`
    pub const SKILL_MANAGER_SELECT_SOURCE: &str = "skill_manager.select_source";
    /// `{"position": usize}`, a position among the visible units.
    pub const SKILL_MANAGER_SELECT_UNIT: &str = "skill_manager.select_unit";
    /// `{"pane": "sources" | "units"}`
    pub const SKILL_MANAGER_FOCUS_PANE: &str = "skill_manager.focus_pane";
    /// `{"width": u16}`
    pub const SKILL_MANAGER_SAVE_SOURCES_WIDTH: &str = "skill_manager.save_sources_width";
    /// No arguments.
    pub const HOME_BEGIN_SIDEBAR_RESIZE: &str = "home.begin_sidebar_resize";
    /// `{"index": usize}`
    pub const HOME_CLICK_SIDEBAR_ITEM: &str = "home.click_sidebar_item";
}

fn command(id: &str, args: Args) -> Intent {
    Intent::Command(CommandId::new(id), args)
}

/// Select session-list row `row`, attaching it when `open` (a double-click).
#[must_use]
pub fn select_session_row(row: usize, open: bool) -> Intent {
    command(
        ids::SESSION_LIST_SELECT_ROW,
        json!({ "row": row, "open": open }),
    )
}

/// Open the context menu of session-list row `row`.
#[must_use]
pub fn open_session_row_menu(row: usize) -> Intent {
    command(ids::SESSION_LIST_OPEN_ROW_MENU, json!({ "row": row }))
}

/// Focus the sessions list, or a pane beside it.
#[must_use]
pub fn focus_session_pane(pane: &FocusedPane) -> Intent {
    let pane = match pane {
        FocusedPane::Sessions => "sessions",
        FocusedPane::LiveLogs => "live_logs",
        FocusedPane::Preview => "preview",
    };
    command(ids::SESSION_LIST_FOCUS_PANE, json!({ "pane": pane }))
}

/// Persist the sessions pane layout a renderer just changed.
#[must_use]
pub fn save_sessions_pane_layout(width: u16, collapsed: bool) -> Intent {
    command(
        ids::SESSION_LIST_SAVE_PANE_LAYOUT,
        json!({ "width": width, "collapsed": collapsed }),
    )
}

/// Clear the Skill Manager's source filter ("All sources").
#[must_use]
pub fn all_skill_sources() -> Intent {
    command(ids::SKILL_MANAGER_ALL_SOURCES, Value::Null)
}

/// Select Skill Manager source `index`.
#[must_use]
pub fn select_skill_source(index: usize) -> Intent {
    command(ids::SKILL_MANAGER_SELECT_SOURCE, json!({ "index": index }))
}

/// Select the unit at `position` among the visible units.
#[must_use]
pub fn select_skill_unit(position: usize) -> Intent {
    command(
        ids::SKILL_MANAGER_SELECT_UNIT,
        json!({ "position": position }),
    )
}

/// Focus a Skill Manager panel.
#[must_use]
pub fn focus_skill_pane(pane: FocusedSkillPane) -> Intent {
    let pane = match pane {
        FocusedSkillPane::Sources => "sources",
        FocusedSkillPane::Units => "units",
    };
    command(ids::SKILL_MANAGER_FOCUS_PANE, json!({ "pane": pane }))
}

/// Persist the Sources panel width a renderer just set.
#[must_use]
pub fn save_skill_sources_width(width: u16) -> Intent {
    command(
        ids::SKILL_MANAGER_SAVE_SOURCES_WIDTH,
        json!({ "width": width }),
    )
}

/// Start dragging the home sidebar's resize edge.
#[must_use]
pub fn begin_home_sidebar_resize() -> Intent {
    command(ids::HOME_BEGIN_SIDEBAR_RESIZE, Value::Null)
}

/// Click home sidebar item `index`.
#[must_use]
pub fn click_home_sidebar_item(index: usize) -> Intent {
    command(ids::HOME_CLICK_SIDEBAR_ITEM, json!({ "index": index }))
}

/// The event a pointer command applies, or `None` when `id` is not a pointer
/// command or `args` do not fit it.
pub(crate) fn event_for(id: &CommandId, args: &Args) -> Option<AppEvent> {
    let index = |key: &str| args.get(key)?.as_u64().and_then(|n| usize::try_from(n).ok());
    let flag = |key: &str| args.get(key)?.as_bool();
    let width = || index("width").and_then(|n| u16::try_from(n).ok());
    let pane = || args.get("pane")?.as_str();
    let bare = |event: AppEvent| args.is_null().then_some(event);
    Some(match id.as_str() {
        ids::SESSION_LIST_SELECT_ROW => AppEvent::SessionListSelectRow {
            row: index("row")?,
            open: flag("open")?,
        },
        ids::SESSION_LIST_OPEN_ROW_MENU => AppEvent::SessionListOpenRowMenu { row: index("row")? },
        ids::SESSION_LIST_FOCUS_PANE => AppEvent::SessionListFocusPane(match pane()? {
            "sessions" => FocusedPane::Sessions,
            "live_logs" => FocusedPane::LiveLogs,
            "preview" => FocusedPane::Preview,
            _ => return None,
        }),
        ids::SESSION_LIST_SAVE_PANE_LAYOUT => AppEvent::SaveSessionsPaneLayout {
            width: width()?,
            collapsed: flag("collapsed")?,
        },
        ids::SKILL_MANAGER_ALL_SOURCES => bare(AppEvent::SkillManagerClearSourceFilter)?,
        ids::SKILL_MANAGER_SELECT_SOURCE => AppEvent::SkillManagerSourceClick {
            index: index("index")?,
        },
        ids::SKILL_MANAGER_SELECT_UNIT => AppEvent::SkillManagerUnitClick {
            position: index("position")?,
        },
        ids::SKILL_MANAGER_FOCUS_PANE => AppEvent::SkillManagerFocusPane(match pane()? {
            "sources" => FocusedSkillPane::Sources,
            "units" => FocusedSkillPane::Units,
            _ => return None,
        }),
        ids::SKILL_MANAGER_SAVE_SOURCES_WIDTH => {
            AppEvent::SkillManagerSaveSourcesWidth { width: width()? }
        }
        ids::HOME_BEGIN_SIDEBAR_RESIZE => bare(AppEvent::HomeSidebarBeginResize)?,
        ids::HOME_CLICK_SIDEBAR_ITEM => AppEvent::HomeSidebarClickItem {
            index: index("index")?,
        },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every constructor produces a command `event_for` accepts, so a renderer
    /// using them can never send a misshapen payload.
    #[test]
    fn every_constructor_resolves_to_an_event() {
        let intents = [
            select_session_row(2, true),
            open_session_row_menu(2),
            focus_session_pane(&FocusedPane::LiveLogs),
            save_sessions_pane_layout(40, false),
            all_skill_sources(),
            select_skill_source(1),
            select_skill_unit(3),
            focus_skill_pane(FocusedSkillPane::Units),
            save_skill_sources_width(48),
            begin_home_sidebar_resize(),
            click_home_sidebar_item(0),
        ];
        for intent in intents {
            let Intent::Command(id, args) = intent else {
                panic!("pointer constructors build commands");
            };
            assert!(event_for(&id, &args).is_some(), "{id} {args}");
        }
    }

    #[test]
    fn misshapen_arguments_resolve_to_nothing() {
        let rejected = [
            (ids::SESSION_LIST_SELECT_ROW, json!({ "row": 1 })),
            (ids::SESSION_LIST_FOCUS_PANE, json!({ "pane": "sidebar" })),
            (
                ids::SESSION_LIST_SAVE_PANE_LAYOUT,
                json!({ "width": 70_000, "collapsed": false }),
            ),
            (ids::SKILL_MANAGER_ALL_SOURCES, json!({ "index": 0 })),
            ("session_list.no_such_pointer_command", Value::Null),
        ];
        for (id, args) in rejected {
            assert!(
                event_for(&CommandId::new(id), &args).is_none(),
                "{id} {args}"
            );
        }
    }
}
