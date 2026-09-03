//! Helpers shared by the crate's RENDER tests.
//!
//! A [`WireBuffer`] is a sparse `(Coord, Cell)` list in paint order, not a grid,
//! so every render test needs the same reconstruction before it can assert on
//! text. That reconstruction had drifted into five identical private copies
//! (crisp B1 review); it lives here once instead.

use ainb_plugin_sdk::WireBuffer;

use crate::screen::Screen;

/// Every [`Screen`] variant, for the guards that must hold across ALL of them:
/// the footer-hint grammar, the reserved-key hygiene, and the crisp B5 promise
/// that a screen off the tab strip is still reachable.
///
/// Exhaustive BY THE MATCH BELOW, which has no wildcard arm: adding a variant to
/// `Screen` fails to compile here, next to the list to add it to. (That forces an
/// author to look; it does not by itself prove the list holds every variant, so
/// keep the two together.)
pub fn every_screen() -> Vec<Screen> {
    use ainb_hangar_core::ids::{IssueId, TaskId};
    let issue = IssueId::from_str("i1").expect("valid issue id");
    let task = TaskId::from_str("01HANGARTASK000000000001").expect("valid task id");
    let all = vec![
        Screen::IssueList,
        Screen::TaskDetail(task),
        Screen::AgentPicker(issue.clone()),
        Screen::ActivityTimeline(issue),
        Screen::SkillManager,
        Screen::Autopilots,
        Screen::Kanban,
        Screen::Boards,
        Screen::DaemonHealth,
        Screen::Usage,
        Screen::Logs,
        Screen::Inbox,
        Screen::ControlCenter,
        Screen::Fleet,
        Screen::Squads,
        Screen::Profiles,
        Screen::Agents,
        Screen::Settings,
        Screen::Help,
        Screen::CommandPalette,
    ];
    for screen in &all {
        match screen {
            Screen::IssueList
            | Screen::TaskDetail(_)
            | Screen::AgentPicker(_)
            | Screen::ActivityTimeline(_)
            | Screen::SkillManager
            | Screen::Autopilots
            | Screen::Kanban
            | Screen::Boards
            | Screen::DaemonHealth
            | Screen::Usage
            | Screen::Logs
            | Screen::Inbox
            | Screen::ControlCenter
            | Screen::Fleet
            | Screen::Squads
            | Screen::Profiles
            | Screen::Agents
            | Screen::Settings
            | Screen::Help
            | Screen::CommandPalette => {}
        }
    }
    all
}

/// The full painted text of `buf` in ROW-MAJOR order, so an assertion can search
/// for a label wherever on the screen it landed.
///
/// Rows are concatenated with no separator: this answers "is this text on the
/// screen", not "which line is it on". A test that must pin text to a LINE
/// collects that row itself (`row_text`) rather than searching this.
pub fn painted_text(buf: &WireBuffer) -> String {
    let mut out = String::new();
    for y in 0..buf.height {
        for (coord, cell) in &buf.cells {
            if coord.y == y {
                out.push_str(&cell.symbol);
            }
        }
    }
    out
}
