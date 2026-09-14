//! The hangar screen's `ui.state` view and the actions a renderer that draws
//! it can run over `plugin/handle_action`.
//!
//! A renderer that paints hangar itself, rather than the plugin's own frame,
//! reads [`view`] from the `ui.state` topic and sends back an action id from
//! [`ACTIONS`]. Both are this plugin's contract with such renderers; the host
//! stores the view and forwards the action without reading either.
//!
//! View shape, version 1:
//!
//! ```json
//! {
//!   "version": 1,
//!   "screen": "issue_list",
//!   "selected_issue": "iss-1",
//!   "issues": [{ "id": "iss-1", "display_id": "HGR-1", "title": "…", "state": "open" }],
//!   "focused_card": "task-9"
//! }
//! ```

use ainb_hangar_core::ids::IssueId;
use serde_json::{Value, json};

use crate::screen::app_screens::{NavIntent, ScreenStates};
use crate::screen::command_palette::GO_SCREENS;
use crate::screen::{AppState, Screen};

/// Every action id [`nav_for`] accepts, with its payload shape.
pub const ACTIONS: &[(&str, &str)] = &[
    ("screen.go", r#"{"screen": "<screen token>"}"#),
    ("issue.open", r#"{"id": "<issue id>"}"#),
    ("issue.mark_done", r#"{"id": "<issue id>"}"#),
    ("modal.close", "null"),
];

/// The routing token of a top-level screen a renderer can open by name.
fn screen_for_token(token: &str) -> Option<Screen> {
    match token {
        "issue_list" => Some(Screen::IssueList),
        "kanban" => Some(Screen::Kanban),
        "boards" => Some(Screen::Boards),
        "inbox" => Some(Screen::Inbox),
        "agents" => Some(Screen::Agents),
        "settings" => Some(Screen::Settings),
        _ => GO_SCREENS
            .iter()
            .find_map(|(name, screen)| (*name == token).then(|| screen.clone())),
    }
}

/// The token [`view`] names `screen` by. Screens that carry an entity name
/// the kind; the entity itself is in the rest of the view.
fn token_for_screen(screen: &Screen) -> &'static str {
    match screen {
        Screen::IssueList => "issue_list",
        Screen::TaskDetail(_) => "task_detail",
        Screen::AgentPicker(_) => "agent_picker",
        Screen::ActivityTimeline(_) => "activity_timeline",
        Screen::SkillManager => "skills",
        Screen::Autopilots => "autopilots",
        Screen::Kanban => "kanban",
        Screen::Boards => "boards",
        Screen::DaemonHealth => "daemon",
        Screen::Usage => "usage",
        Screen::Logs => "logs",
        Screen::Inbox => "inbox",
        Screen::ControlCenter => "control",
        Screen::Fleet => "fleet",
        Screen::Squads => "squads",
        Screen::Profiles => "profiles",
        Screen::Agents => "agents",
        Screen::Settings => "settings",
        Screen::Help => "help",
        Screen::CommandPalette => "command_palette",
    }
}

fn issue_id(payload: &Value) -> Option<IssueId> {
    IssueId::from_str(payload.get("id")?.as_str()?).ok()
}

/// The navigation `action_id` with `payload` asks for, or `None` for an
/// unknown action or a payload that does not fit it.
#[must_use]
pub fn nav_for(action_id: &str, payload: &Value) -> Option<NavIntent> {
    match action_id {
        "screen.go" => payload
            .get("screen")
            .and_then(Value::as_str)
            .and_then(screen_for_token)
            .map(NavIntent::GoToScreen),
        "issue.open" => issue_id(payload).map(NavIntent::OpenTaskForIssue),
        "issue.mark_done" => issue_id(payload).map(NavIntent::MarkIssueDone),
        "modal.close" => Some(NavIntent::CloseModal),
        _ => None,
    }
}

/// The view a renderer draws the hangar screen from.
#[must_use]
pub fn view(app: &AppState, screens: &ScreenStates) -> Value {
    let issues: Vec<Value> = screens
        .issue_list
        .visible_rows()
        .map(|row| {
            json!({
                "id": row.id.as_str(),
                "display_id": row.display_id,
                "title": row.title,
                "state": row.state,
            })
        })
        .collect();
    json!({
        "version": 1,
        "screen": token_for_screen(&app.screen),
        "selected_issue": screens.issue_list.selected_row().map(|row| row.id.as_str()),
        "issues": issues,
        "focused_card": screens.kanban.focused_card().map(|card| card.task_id.as_str()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_listed_action_maps_to_a_navigation() {
        let payloads = [
            json!({ "screen": "kanban" }),
            json!({ "id": "iss-1" }),
            json!({ "id": "iss-1" }),
            Value::Null,
        ];
        for ((action, _), payload) in ACTIONS.iter().zip(payloads) {
            assert!(nav_for(action, &payload).is_some(), "{action}");
        }
    }

    #[test]
    fn a_payload_that_does_not_fit_or_an_unknown_action_navigates_nowhere() {
        assert_eq!(nav_for("screen.go", &json!({ "screen": "nowhere" })), None);
        assert_eq!(nav_for("issue.open", &Value::Null), None);
        assert_eq!(nav_for("issue.open", &json!({ "id": "" })), None);
        assert_eq!(nav_for("board.explode", &Value::Null), None);
    }

    #[test]
    fn screen_go_reaches_the_palette_screens_and_the_tabs() {
        assert_eq!(
            nav_for("screen.go", &json!({ "screen": "usage" })),
            Some(NavIntent::GoToScreen(Screen::Usage))
        );
        assert_eq!(
            nav_for("screen.go", &json!({ "screen": "issue_list" })),
            Some(NavIntent::GoToScreen(Screen::IssueList))
        );
    }

    #[test]
    fn every_openable_token_round_trips_through_the_view_token() {
        for token in [
            "issue_list",
            "kanban",
            "boards",
            "inbox",
            "agents",
            "settings",
        ]
        .into_iter()
        .chain(GO_SCREENS.iter().map(|(name, _)| *name))
        {
            let screen = screen_for_token(token).expect(token);
            assert_eq!(token_for_screen(&screen), token);
        }
    }
}
