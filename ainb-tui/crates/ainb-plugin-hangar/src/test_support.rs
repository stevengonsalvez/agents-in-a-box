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
/// Two things force it, and neither is free: the wildcard-free match below does
/// not compile when a `Screen` variant is added, and the count of DISTINCT
/// variants actually built here must equal [`SCREEN_VARIANTS`], so bumping the
/// count without building the variant fails loudly instead of silently skipping
/// it in every guard that walks this.
///
/// [`SCREEN_VARIANTS`] is HAND-MAINTAINED — the compiler cannot count enum
/// variants without a derive. Adding a variant means three edits: the match arm
/// (forced), the constant, and the entry here (the constant forces this one).
/// Nothing catches all three being forgotten together, but the match arm cannot
/// be.
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
    // `mem::discriminant` is `Hash + Eq` and ignores the payload, so this counts
    // VARIANTS rather than values — no hand-assigned tag table to keep in step.
    let built: std::collections::HashSet<_> = all.iter().map(std::mem::discriminant).collect();
    assert_eq!(
        built.len(),
        SCREEN_VARIANTS,
        "every_screen() builds {} of the {SCREEN_VARIANTS} Screen variants",
        built.len()
    );
    // Purely for the compile-time force: no wildcard arm, so a new `Screen`
    // variant breaks the build here, next to the list it has to be added to.
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

/// How many variants `Screen` has. Hand-maintained: bump it with the match arm.
const SCREEN_VARIANTS: usize = 20;

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
