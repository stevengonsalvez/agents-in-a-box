//! P7 — Squads screen render snapshots (design gate c: empty / loaded / create /
//! narrow), plus non-vacuous content + colour backstops.
//!
//! The insta glyph snapshots are the permanent layout-regression net; the paired
//! assertions prove the render is not vacuously blank (a dropped squad / member /
//! presence dot fails here). Trailing newline trimmed per
//! `reference_insta_trailing_newline_trap`.

use ainb_hangar_proto::events::{ActorRow, PresenceState};
use ainb_hangar_proto::snapshots::{SquadWireRow, SquadsListResult};
use ainb_plugin_hangar::screen::squads::{SquadsEvent, SquadsState, reduce_squads, render_squads};
use ainb_plugin_sdk::{Color, WireBuffer};

/// Online-green — the leader/member online presence dot colour (also the OK-note).
const ONLINE_GREEN: Color = Color::rgb(100, 200, 100);
/// Error-red — the rejection-note colour (shared with `boards.rs`).
const ERROR_RED: Color = Color::rgb(235, 90, 90);

fn actor(actor_ref: &str, name: &str, presence: PresenceState, is_agent: bool) -> ActorRow {
    ActorRow {
        actor_ref: actor_ref.into(),
        display_name: name.into(),
        subtitle: String::new(),
        presence,
        is_agent,
        recent_rank: None,
    }
}

fn wire_squad(id: &str, name: &str, leader: &str, members: &[&str]) -> SquadWireRow {
    SquadWireRow {
        id: id.into(),
        name: name.into(),
        leader: leader.into(),
        members: members.iter().map(|m| (*m).to_string()).collect(),
    }
}

fn loaded_state() -> SquadsState {
    let snapshot = SquadsListResult {
        squads: vec![
            wire_squad(
                "s1",
                "shippers",
                "agent:a-lead",
                &["agent:a-1", "member:u-1"],
            ),
            wire_squad("s2", "reviewers", "agent:a-rev", &["agent:a-2"]),
        ],
    };
    let actors = vec![
        actor("agent:a-lead", "lead-bot", PresenceState::Online, true),
        actor("agent:a-1", "worker-bot", PresenceState::Unstable, true),
        actor("agent:a-rev", "review-bot", PresenceState::Online, true),
        actor("agent:a-2", "second-bot", PresenceState::Offline, true),
        actor("member:u-1", "alice", PresenceState::Online, false),
    ];
    SquadsState::from_snapshot(&snapshot, &actors)
}

/// Flatten the buffer into a `\n`-joined glyph map, each line `trim_end`-ed and the
/// whole map trailing-newline-trimmed (insta trap).
fn glyph_map(buf: &WireBuffer, cols: u16) -> String {
    let mut grid = vec![vec![' '; cols as usize]; buf.height as usize];
    for (coord, cell) in &buf.cells {
        if coord.y < buf.height && coord.x < cols {
            if let Some(ch) = cell.symbol.chars().next() {
                grid[coord.y as usize][coord.x as usize] = ch;
            }
        }
    }
    grid.into_iter()
        .map(|r| r.into_iter().collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
        .trim_end_matches('\n')
        .to_string()
}

/// The empty list shows the create-help line + the `[c]reate` hint.
#[test]
fn render_empty_shows_help() {
    let state = SquadsState::new(Vec::new());
    let mut buf = WireBuffer::new(100, 8);
    render_squads(&mut buf, 100, 0, 8, &state);
    let full = glyph_map(&buf, 100);
    insta::assert_snapshot!(full);
    assert!(
        full.contains("Press 'n' to create an agent, 'c' to create a squad"),
        "empty help line missing:\n{full}"
    );
    assert!(full.contains("[n]ew-agent"), "new-agent hint missing:\n{full}");
    assert!(full.contains("[c]reate"), "create hint missing:\n{full}");
}

/// A loaded board renders each squad header with its leader + member rows, the
/// first row carrying the `▶` selection marker.
#[test]
fn render_loaded_lists_squads_leaders_and_members() {
    let state = loaded_state();
    let mut buf = WireBuffer::new(100, 14);
    render_squads(&mut buf, 100, 0, 14, &state);
    let full = glyph_map(&buf, 100);
    insta::assert_snapshot!(full);

    // POSITIVE: both squads, the leaders, and the members are visible.
    for marker in [
        "shippers",
        "reviewers",
        "lead-bot",
        "worker-bot",
        "alice",
        "review-bot",
        "leader:",
    ] {
        assert!(full.contains(marker), "missing {marker:?}:\n{full}");
    }
    // The human member is tagged distinctly from an agent.
    assert!(
        full.contains("· human"),
        "human member tag missing:\n{full}"
    );
    assert!(
        full.contains("· agent"),
        "agent member tag missing:\n{full}"
    );
    // The selection marker sits on the first (header) row.
    assert!(full.contains('▶'), "selection marker missing:\n{full}");

    // NON-VACUOUS COLOUR CHECK: an online presence dot is painted in online-green,
    // not the default text colour.
    let online_dot = buf
        .cells
        .iter()
        .any(|(_, cell)| cell.symbol == "●" && cell.fg == Some(ONLINE_GREEN));
    assert!(
        online_dot,
        "an online member/leader must carry the online-green presence dot"
    );
}

/// The create-name input takes over the body: the prompt + the typed buffer.
#[test]
fn render_create_input_shows_prompt_and_buffer() {
    let state = loaded_state();
    let opened = reduce_squads(&state, SquadsEvent::Key('c')).state;
    let typed = reduce_squads(&opened, SquadsEvent::Key('q')).state;
    let typed = reduce_squads(&typed, SquadsEvent::Key('a')).state;

    let mut buf = WireBuffer::new(100, 8);
    render_squads(&mut buf, 100, 0, 8, &typed);
    let full = glyph_map(&buf, 100);
    insta::assert_snapshot!(full);

    assert!(
        full.contains("New squad name: qa"),
        "create buffer not rendered:\n{full}"
    );
    assert!(
        full.contains("Esc to cancel"),
        "create cancel hint missing:\n{full}"
    );
    // NEGATIVE: the squad list is hidden while the create input is open.
    assert!(
        !full.contains("shippers"),
        "the list must be hidden during create:\n{full}"
    );
}

/// The colour at buffer coordinate `(x, y)`, if any cell is painted there.
fn color_at(buf: &WireBuffer, x: u16, y: u16) -> Option<Color> {
    buf.cells
        .iter()
        .find(|(coord, _)| coord.x == x && coord.y == y)
        .and_then(|(_, cell)| cell.fg)
}

/// A rejection note (`squad error: …`, `assign failed: …`, `no agent …`) must be
/// painted in ERROR_RED, never the green of a success confirmation — the two
/// states are visually distinct.
#[test]
fn error_note_renders_red_not_green() {
    // Success note → green.
    let mut ok = loaded_state();
    ok.note_ok("briefed lead-bot + 2 members");
    let mut buf = WireBuffer::new(100, 14);
    render_squads(&mut buf, 100, 0, 14, &ok);
    assert_eq!(
        color_at(&buf, 0, 1),
        Some(ONLINE_GREEN),
        "a success confirmation must be painted green"
    );

    // Rejection note → red, at the same position, so it can never be mistaken for
    // the success above.
    let mut err = loaded_state();
    err.note_err("squad error: duplicate name");
    let mut buf = WireBuffer::new(100, 14);
    render_squads(&mut buf, 100, 0, 14, &err);
    let note_color = color_at(&buf, 0, 1);
    assert_eq!(
        note_color,
        Some(ERROR_RED),
        "a rejection must be painted red, not green"
    );
    assert_ne!(
        note_color,
        Some(ONLINE_GREEN),
        "an error must NOT read as a success confirmation"
    );
}

/// When the squads + members overflow the pane, the render follows the selection:
/// the selected row stays on-screen and the top rows scroll off.
#[test]
fn overflow_follows_the_selection_off_the_fold() {
    // 30 single-line squads → 30 rows, far more than a 8-row pane holds.
    let squads: Vec<_> = (0..30)
        .map(|i| {
            wire_squad(
                &format!("s{i}"),
                &format!("squad-{i:02}"),
                "agent:a-lead",
                &[],
            )
        })
        .collect();
    let actors = vec![actor(
        "agent:a-lead",
        "lead-bot",
        PresenceState::Online,
        true,
    )];
    let mut state = SquadsState::from_snapshot(&SquadsListResult { squads }, &actors);

    // Drive the selection to the last row.
    for _ in 0..29 {
        state = reduce_squads(&state, SquadsEvent::Key('j')).state;
    }
    assert_eq!(state.selected_index(), 29);

    let mut buf = WireBuffer::new(100, 8);
    render_squads(&mut buf, 100, 0, 8, &state);
    let full = glyph_map(&buf, 100);

    // The selected (last) squad is on-screen, carrying the ▶ marker…
    assert!(
        full.contains("squad-29"),
        "the selected row must scroll into view:\n{full}"
    );
    assert!(
        full.contains('▶'),
        "the selection marker must be visible:\n{full}"
    );
    // …and the top rows have scrolled off (no scroll-follow would strand them on
    // screen and push the selection past the fold).
    assert!(
        !full.contains("squad-00"),
        "the top rows must scroll off when the selection is past the fold:\n{full}"
    );
}

/// Narrow-width floor (design gate c overflow leg): the body still renders squad
/// names without writing a single cell outside the area.
#[test]
fn render_narrow_width_stays_in_bounds() {
    const W: u16 = 44;
    let state = loaded_state();
    let mut buf = WireBuffer::new(W, 12);
    render_squads(&mut buf, W, 0, 12, &state);
    let full = glyph_map(&buf, W);
    insta::assert_snapshot!(full);

    assert!(
        full.contains("shippers"),
        "squad name missing at narrow width:\n{full}"
    );
    for (coord, _) in &buf.cells {
        assert!(
            coord.x < W,
            "squads render wrote out-of-bounds cell at x={}",
            coord.x
        );
    }
}
