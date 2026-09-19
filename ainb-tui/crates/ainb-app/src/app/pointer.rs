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
use crate::components::code_review::render::ReviewRowId;
use crate::components::session_tabs::SessionTab;
use crate::components::sidebar::SidebarItem;
use crate::components::skill_manager_screen::FocusedSkillPane;
use crate::config::settings_model::ConfigRowEdit;

/// Command ids of the pointer rows, `<context>.<row id>` like every keymap
/// command. Each row is unbound: its payload only a hit-test can supply.
pub mod ids {
    /// `{"target": SessionListRowId, "open": bool}`
    pub const SESSION_LIST_SELECT_ROW: &str = "session_list.select_row";
    /// `{"target": SessionListRowId}`
    pub const SESSION_LIST_OPEN_ROW_MENU: &str = "session_list.open_row_menu";
    /// `{"pane": "sessions" | "live_logs" | "preview"}`
    pub const SESSION_LIST_FOCUS_PANE: &str = "session_list.focus_pane";
    /// `{"tab": SessionTab}`, spelled as the frame spells it.
    pub const SESSION_LIST_SELECT_TAB: &str = "session_list.select_tab";
    /// `{"session_key": String | null}`: null closes.
    pub const SESSION_LIST_OPEN_TRANSCRIPT: &str = "session_list.open_transcript";
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
    /// `{"fraction": f64}`, of the screen width.
    pub const SKILL_MANAGER_SAVE_SOURCES_WIDTH: &str = "skill_manager.save_sources_width";
    /// `{"fraction": f64}`, of the screen width.
    pub const HOME_SAVE_SIDEBAR_WIDTH: &str = "home.save_sidebar_width";
    /// `{"item": SidebarItem id}`
    pub const HOME_CLICK_SIDEBAR_ITEM: &str = "home.click_sidebar_item";
    /// `{"target": ReviewRowId}`
    pub const GIT_VIEW_SELECT_REVIEW_ROW: &str = "git_view.select_review_row";
    /// `{"lines": i32}`, down when positive.
    pub const GIT_VIEW_SCROLL: &str = "git_view.scroll";
    /// `{"key": String, "value": ConfigRowEdit}`, the row by its registry key.
    pub const CONFIG_SET_ROW: &str = "config.set_row";

    /// Every pointer command id.
    pub const ALL: &[&str] = &[
        SESSION_LIST_SELECT_ROW,
        SESSION_LIST_OPEN_ROW_MENU,
        SESSION_LIST_FOCUS_PANE,
        SESSION_LIST_SELECT_TAB,
        SESSION_LIST_OPEN_TRANSCRIPT,
        SESSION_LIST_SAVE_PANE_LAYOUT,
        SKILL_MANAGER_ALL_SOURCES,
        SKILL_MANAGER_SELECT_SOURCE,
        SKILL_MANAGER_SELECT_UNIT,
        SKILL_MANAGER_FOCUS_PANE,
        SKILL_MANAGER_SAVE_SOURCES_WIDTH,
        HOME_SAVE_SIDEBAR_WIDTH,
        HOME_CLICK_SIDEBAR_ITEM,
        GIT_VIEW_SELECT_REVIEW_ROW,
        GIT_VIEW_SCROLL,
        CONFIG_SET_ROW,
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

/// Show the session tab a click names.
///
/// The strip's own key cycles; a pointer, and a board card, name the pane they
/// want. The reducer still resolves it, so naming a disabled tab lands where
/// the strip would have.
#[must_use]
pub fn select_session_tab(tab: SessionTab) -> Intent {
    command(ids::SESSION_LIST_SELECT_TAB, json!({ "tab": tab }))
}

/// Open the ACP transcript of the Fleet session `session_key`, or close the
/// open one with `None`.
///
/// An ACP session has no tmux pane and no session list row, so its board card
/// names it by the Fleet session key the frame carries.
#[must_use]
pub fn open_transcript(session_key: Option<&str>) -> Intent {
    command(
        ids::SESSION_LIST_OPEN_TRANSCRIPT,
        json!({ "session_key": session_key }),
    )
}

/// Persist the sessions pane layout a renderer just changed: the sidebar's
/// share of its row as the user asked for it, before any clamp, and whether
/// it is collapsed.
#[must_use]
pub fn save_sessions_pane_layout(fraction: f64, collapsed: bool) -> Intent {
    command(
        ids::SESSION_LIST_SAVE_PANE_LAYOUT,
        json!({ "fraction": fraction.clamp(0.0, 1.0), "collapsed": collapsed }),
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

/// The fraction `width` columns make of a `columns`-wide screen.
fn fraction_of(width: u16, columns: u16) -> f64 {
    if columns == 0 {
        return 0.0;
    }
    (f64::from(width) / f64::from(columns)).clamp(0.0, 1.0)
}

/// Persist the Sources panel width a renderer just set, `width` columns of a
/// `columns`-wide screen.
#[must_use]
pub fn save_skill_sources_width(width: u16, columns: u16) -> Intent {
    command(
        ids::SKILL_MANAGER_SAVE_SOURCES_WIDTH,
        json!({ "fraction": fraction_of(width, columns) }),
    )
}

/// Persist the home sidebar width a renderer just set, `width` columns of a
/// `columns`-wide screen.
#[must_use]
pub fn save_home_sidebar_width(width: u16, columns: u16) -> Intent {
    command(
        ids::HOME_SAVE_SIDEBAR_WIDTH,
        json!({ "fraction": fraction_of(width, columns) }),
    )
}

/// Click home sidebar `item`.
#[must_use]
pub fn click_home_sidebar_item(item: SidebarItem) -> Intent {
    command(ids::HOME_CLICK_SIDEBAR_ITEM, json!({ "item": item.id() }))
}

/// Click the code review sidebar row `target`.
#[must_use]
pub fn select_review_row(target: &ReviewRowId) -> Intent {
    command(ids::GIT_VIEW_SELECT_REVIEW_ROW, json!({ "target": target }))
}

/// Scroll the git view's active tab by `lines`, down when positive.
#[must_use]
pub fn scroll_git_view(lines: i32) -> Intent {
    command(ids::GIT_VIEW_SCROLL, json!({ "lines": lines }))
}

/// Set the settings row `key` to what a form chose, and write that one key.
///
/// The row is named by its registry key, so a form resolved against one frame
/// edits the same row after the rows were reordered or filtered.
#[must_use]
pub fn set_config_row(key: &str, edit: ConfigRowEdit) -> Intent {
    command(ids::CONFIG_SET_ROW, json!({ "key": key, "value": edit }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewRowArgs {
    target: ReviewRowId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinesArgs {
    lines: i32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RowEditArgs {
    key: String,
    value: ConfigRowEdit,
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
struct TabArgs {
    tab: SessionTab,
}

/// A present `session_key`, which may be null: a bare `Null` payload is not
/// this, so the palette, which sends one, cannot run the row.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TranscriptArgs {
    session_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutArgs {
    fraction: f64,
    collapsed: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FractionArgs {
    fraction: f64,
}

impl FractionArgs {
    fn in_range(self) -> Option<f64> {
        (0.0..=1.0).contains(&self.fraction).then_some(self.fraction)
    }
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
/// but `args` do not fit. Every pointer row that carries a payload refuses
/// `Null`, so running one by name without a hit-test changes nothing.
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
        AppEvent::SessionListSelectTab(_) => {
            parse::<TabArgs>(args).map(|args| AppEvent::SessionListSelectTab(args.tab))
        }
        AppEvent::SessionListOpenTranscript(_) => parse::<TranscriptArgs>(args)
            .map(|args| AppEvent::SessionListOpenTranscript(args.session_key)),
        AppEvent::SaveSessionsPaneLayout { .. } => parse::<LayoutArgs>(args)
            .filter(|args| (0.0..=1.0).contains(&args.fraction))
            .map(|args| AppEvent::SaveSessionsPaneLayout {
                fraction: args.fraction,
                collapsed: args.collapsed,
            }),
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
        AppEvent::SkillManagerSaveSourcesWidth { .. } => parse::<FractionArgs>(args)
            .and_then(FractionArgs::in_range)
            .map(|fraction| AppEvent::SkillManagerSaveSourcesWidth { fraction }),
        AppEvent::HomeSidebarSaveWidth { .. } => parse::<FractionArgs>(args)
            .and_then(FractionArgs::in_range)
            .map(|fraction| AppEvent::HomeSidebarSaveWidth { fraction }),
        AppEvent::HomeSidebarClickItem { .. } => parse::<ItemArgs>(args)
            .and_then(|args| SidebarItem::from_id(&args.item))
            .map(|item| AppEvent::HomeSidebarClickItem { item }),
        AppEvent::GitReviewSelectRow { .. } => {
            parse::<ReviewRowArgs>(args).map(|args| AppEvent::GitReviewSelectRow {
                target: args.target,
            })
        }
        AppEvent::GitViewScrollBy(_) => parse::<LinesArgs>(args)
            .filter(|args| args.lines != 0)
            .map(|args| AppEvent::GitViewScrollBy(args.lines)),
        AppEvent::ConfigSetRow { .. } => parse::<RowEditArgs>(args)
            .filter(|args| !args.key.is_empty())
            .map(|args| AppEvent::ConfigSetRow {
                key: args.key,
                edit: args.value,
            }),
        _ => return None,
    })
}
