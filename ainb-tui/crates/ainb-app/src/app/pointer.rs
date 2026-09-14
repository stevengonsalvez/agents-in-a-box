// ABOUTME: Commands a pointer press resolves to. A renderer hit-tests the press
// against what it drew and names the thing under the pointer by identity (a
// session id, a source URI, a sidebar item), never by where it was drawn.
// They are unbound keymap rows, so one registry lists every command.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::app::events::AppEvent;
use crate::app::intent::{Args, Intent};
use crate::app::keymap::CommandId;
use crate::app::state::{FocusedPane, SessionListRowId};
use crate::components::sidebar::SidebarItem;
use crate::components::skill_manager_screen::FocusedSkillPane;

/// Command ids of the pointer rows, `<context>.<row id>` like every keymap
/// command. Each row is unbound: its payload only a hit-test can supply.
pub mod ids {
    /// `{"target": SessionListRowId, "open": bool}`
    pub const SESSION_LIST_SELECT_ROW: &str = "session_list.select_row";
    /// `{"target": SessionListRowId}`
    pub const SESSION_LIST_OPEN_ROW_MENU: &str = "session_list.open_row_menu";
    /// `{"pane": "sessions" | "live_logs" | "preview"}`
    pub const SESSION_LIST_FOCUS_PANE: &str = "session_list.focus_pane";
    /// `{"width": u16, "collapsed": bool}`
    pub const SESSION_LIST_SAVE_PANE_LAYOUT: &str = "session_list.save_pane_layout";
    /// No arguments.
    pub const SKILL_MANAGER_ALL_SOURCES: &str = "skill_manager.all_sources";
    /// `{"uri": String}`
    pub const SKILL_MANAGER_SELECT_SOURCE: &str = "skill_manager.select_source";
    /// `{"uri": String}`, a unit's declared URI.
    pub const SKILL_MANAGER_SELECT_UNIT: &str = "skill_manager.select_unit";
    /// `{"pane": "sources" | "units"}`
    pub const SKILL_MANAGER_FOCUS_PANE: &str = "skill_manager.focus_pane";
    /// `{"width": u16}`
    pub const SKILL_MANAGER_SAVE_SOURCES_WIDTH: &str = "skill_manager.save_sources_width";
    /// No arguments.
    pub const HOME_BEGIN_SIDEBAR_RESIZE: &str = "home.begin_sidebar_resize";
    /// `{"item": SidebarItem id}`
    pub const HOME_CLICK_SIDEBAR_ITEM: &str = "home.click_sidebar_item";

    /// Every pointer command id.
    pub const ALL: &[&str] = &[
        SESSION_LIST_SELECT_ROW,
        SESSION_LIST_OPEN_ROW_MENU,
        SESSION_LIST_FOCUS_PANE,
        SESSION_LIST_SAVE_PANE_LAYOUT,
        SKILL_MANAGER_ALL_SOURCES,
        SKILL_MANAGER_SELECT_SOURCE,
        SKILL_MANAGER_SELECT_UNIT,
        SKILL_MANAGER_FOCUS_PANE,
        SKILL_MANAGER_SAVE_SOURCES_WIDTH,
        HOME_BEGIN_SIDEBAR_RESIZE,
        HOME_CLICK_SIDEBAR_ITEM,
    ];
}

fn command(id: &str, args: Args) -> Intent {
    Intent::Command(CommandId::new(id), args)
}

/// Select the session-list row `target`, attaching it when `open` (a
/// double-click).
#[must_use]
pub fn select_session_row(target: &SessionListRowId, open: bool) -> Intent {
    command(
        ids::SESSION_LIST_SELECT_ROW,
        json!({ "target": target, "open": open }),
    )
}

/// Open the context menu of the session-list row `target`.
#[must_use]
pub fn open_session_row_menu(target: &SessionListRowId) -> Intent {
    command(ids::SESSION_LIST_OPEN_ROW_MENU, json!({ "target": target }))
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

/// Select the Skill Manager source with `uri`.
#[must_use]
pub fn select_skill_source(uri: &str) -> Intent {
    command(ids::SKILL_MANAGER_SELECT_SOURCE, json!({ "uri": uri }))
}

/// Select the unit declared as `uri`.
#[must_use]
pub fn select_skill_unit(uri: &str) -> Intent {
    command(ids::SKILL_MANAGER_SELECT_UNIT, json!({ "uri": uri }))
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

/// Click home sidebar `item`.
#[must_use]
pub fn click_home_sidebar_item(item: SidebarItem) -> Intent {
    command(ids::HOME_CLICK_SIDEBAR_ITEM, json!({ "item": item.id() }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RowArgs {
    target: SessionListRowId,
    #[serde(default)]
    open: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MenuArgs {
    target: SessionListRowId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UriArgs {
    uri: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PaneArgs {
    pane: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutArgs {
    width: u16,
    collapsed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WidthArgs {
    width: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ItemArgs {
    item: String,
}

fn parse<T: for<'de> Deserialize<'de>>(args: &Args) -> Option<T> {
    serde_json::from_value(args.clone()).ok()
}

/// The event a pointer row runs with `args` as its payload.
///
/// `None` when `event` is not a pointer row's event; `Some(None)` when it is
/// but `args` do not fit. Rows with a payload refuse `Null`, so running one
/// by name without a hit-test changes nothing; the one row without a payload
/// takes only `Null`.
pub(crate) fn with_args(event: &AppEvent, args: &Args) -> Option<Option<AppEvent>> {
    Some(match event {
        AppEvent::SessionListSelectRow { .. } => {
            parse::<RowArgs>(args).map(|args| AppEvent::SessionListSelectRow {
                target: args.target,
                open: args.open,
            })
        }
        AppEvent::SessionListOpenRowMenu { .. } => {
            parse::<MenuArgs>(args).map(|args| AppEvent::SessionListOpenRowMenu {
                target: args.target,
            })
        }
        AppEvent::SessionListFocusPane(_) => {
            parse::<PaneArgs>(args).and_then(|args| match args.pane.as_str() {
                "sessions" => Some(AppEvent::SessionListFocusPane(FocusedPane::Sessions)),
                "live_logs" => Some(AppEvent::SessionListFocusPane(FocusedPane::LiveLogs)),
                "preview" => Some(AppEvent::SessionListFocusPane(FocusedPane::Preview)),
                _ => None,
            })
        }
        AppEvent::SaveSessionsPaneLayout { .. } => {
            parse::<LayoutArgs>(args).map(|args| AppEvent::SaveSessionsPaneLayout {
                width: args.width,
                collapsed: args.collapsed,
            })
        }
        AppEvent::SkillManagerSourceClick { .. } => {
            parse::<UriArgs>(args).map(|args| AppEvent::SkillManagerSourceClick { uri: args.uri })
        }
        AppEvent::SkillManagerUnitClick { .. } => {
            parse::<UriArgs>(args).map(|args| AppEvent::SkillManagerUnitClick { uri: args.uri })
        }
        AppEvent::SkillManagerFocusPane(_) => {
            parse::<PaneArgs>(args).and_then(|args| match args.pane.as_str() {
                "sources" => Some(AppEvent::SkillManagerFocusPane(FocusedSkillPane::Sources)),
                "units" => Some(AppEvent::SkillManagerFocusPane(FocusedSkillPane::Units)),
                _ => None,
            })
        }
        AppEvent::SkillManagerSaveSourcesWidth { .. } => parse::<WidthArgs>(args)
            .map(|args| AppEvent::SkillManagerSaveSourcesWidth { width: args.width }),
        AppEvent::HomeSidebarClickItem { .. } => parse::<ItemArgs>(args)
            .and_then(|args| SidebarItem::from_id(&args.item))
            .map(|item| AppEvent::HomeSidebarClickItem { item }),
        AppEvent::HomeSidebarBeginResize => {
            args.is_null().then_some(AppEvent::HomeSidebarBeginResize)
        }
        _ => return None,
    })
}
