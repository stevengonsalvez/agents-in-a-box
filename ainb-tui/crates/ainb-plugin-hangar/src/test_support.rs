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
/// Both halves are forced. [`variant_tag`] has no wildcard arm, so a new `Screen`
/// variant does not compile until it is tagged; and the count of DISTINCT tags in
/// the list below must equal the number of arms, so tagging a variant and
/// forgetting to build one here fails too. Without the second half a forgotten
/// entry is silently skipped by every guard that walks this — a screen with no
/// tab and no `Go:` row would read as covered.
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
    let tags: std::collections::BTreeSet<u8> = all.iter().map(variant_tag).collect();
    assert_eq!(
        tags.len(),
        SCREEN_VARIANTS,
        "every_screen() builds {} of the {SCREEN_VARIANTS} Screen variants",
        tags.len()
    );
    all
}

/// The number of arms in [`variant_tag`], and so of `Screen` variants.
const SCREEN_VARIANTS: usize = 20;

/// A distinct tag per `Screen` variant. No wildcard arm: a new variant fails to
/// compile here, and bumping [`SCREEN_VARIANTS`] to match then fails
/// [`every_screen`] until the variant is built there too.
fn variant_tag(screen: &Screen) -> u8 {
    match screen {
        Screen::IssueList => 0,
        Screen::TaskDetail(_) => 1,
        Screen::AgentPicker(_) => 2,
        Screen::ActivityTimeline(_) => 3,
        Screen::SkillManager => 4,
        Screen::Autopilots => 5,
        Screen::Kanban => 6,
        Screen::Boards => 7,
        Screen::DaemonHealth => 8,
        Screen::Usage => 9,
        Screen::Logs => 10,
        Screen::Inbox => 11,
        Screen::ControlCenter => 12,
        Screen::Fleet => 13,
        Screen::Squads => 14,
        Screen::Profiles => 15,
        Screen::Agents => 16,
        Screen::Settings => 17,
        Screen::Help => 18,
        Screen::CommandPalette => 19,
    }
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
