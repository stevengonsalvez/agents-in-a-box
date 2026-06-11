//! P6.5 — skill-manager screen render snapshots + the `s`→sync RPC proof.
//!
//! Three behaviours are pinned:
//!
//!   * `test_skill_list_renders_imported_skills` — three seeded skills render in
//!     the left list pane (insta glyph snapshot, trailing newline trimmed per
//!     `reference_insta_trailing_newline_trap`), with a non-vacuous colour check
//!     on the `▶` selection marker so a dropped marker/colour fails here.
//!   * `test_skill_detail_view_shows_content` — Enter (→ `LoadDetail`) then a
//!     `DetailLoaded` reply: the detail pane shows the `SKILL.md` body and the
//!     file list renders in the tree.
//!   * `test_skill_sync_action_invokes_rpc` — the REAL [`HangarPlugin`] behind
//!     the REAL SDK [`Server`], over a mock daemon socket: pressing `s` on the
//!     skill screen makes the plugin issue a `hangar/skills_sync` JSON-RPC, which
//!     the mock daemon records.

use ainb_hangar_proto::events::{SkillFile, SkillRow};
use ainb_plugin_hangar::screen::skill_manager::{
    reduce_skill_manager, render_skill_manager, SkillManagerEvent, SkillManagerState,
};
use ainb_plugin_sdk::{Color, WireBuffer};

/// `SELECTION_GREEN` from the TUI palette (the active-row marker colour).
const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

fn skill(slug: &str, name: &str, used: bool) -> SkillRow {
    SkillRow {
        slug: slug.into(),
        name: name.into(),
        used,
        updated_at: 0,
    }
}

/// Three imported skills (the importer's curated set, abbreviated).
fn three_skills() -> Vec<SkillRow> {
    vec![
        skill("commit", "commit", true),
        skill("find-missing-tests", "find-missing-tests", true),
        skill("brainstorm", "brainstorm", false),
    ]
}

/// Flatten the buffer into a `\n`-joined glyph map, each line `trim_end`-ed and
/// the whole map trailing-newline-trimmed (insta trap).
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

/// The list pane (left ~22 cols) of the render, as a glyph map.
fn list_pane(buf: &WireBuffer) -> String {
    glyph_map(buf, 22)
}

/// Three imported skills render in the list with the first selected, the
/// action-key hints painted on the chip row.
#[test]
fn test_skill_list_renders_imported_skills() {
    let state = SkillManagerState::new(three_skills());
    // Narrow width keeps the middle file-tree collapsed so the list snapshot is
    // stable (the list always occupies the left 22 cols).
    let mut buf = WireBuffer::new(80, 10);
    render_skill_manager(&mut buf, 80, 0, 10, &state);

    // POSITIVE: all three skills render, `commit` carries the `▶` selection.
    insta::assert_snapshot!(list_pane(&buf), @r###"
    [All] [Used] [Unused]
    ▶ commit
      find-missing-tests
      brainstorm
    "###);

    // NON-VACUOUS COLOUR CHECK: the selected row's `▶` marker is SELECTION_GREEN.
    let green_marker = buf
        .cells
        .iter()
        .any(|(_, cell)| cell.symbol == "▶" && cell.fg == Some(SELECTION_GREEN));
    assert!(
        green_marker,
        "the selected skill's `▶` marker must be painted in SELECTION_GREEN"
    );

    // The action-key hints sit on the chip row (P6.5 keybinding-near-control).
    let full = glyph_map(&buf, 80);
    assert!(full.contains("s sync"), "missing `s sync` hint:\n{full}");
    assert!(
        full.contains("i attach"),
        "missing `i attach` hint:\n{full}"
    );
    assert!(
        full.contains("d detach"),
        "missing `d detach` hint:\n{full}"
    );
}

/// Enter loads the detail; folding the reply renders the SKILL.md body in the
/// editor pane and the file list in the tree.
#[test]
fn test_skill_detail_view_shows_content() {
    let state = SkillManagerState::new(three_skills());

    // Enter → LoadDetail("commit").
    let after_enter = reduce_skill_manager(&state, SkillManagerEvent::Key('\n'));

    // The daemon replies with the SKILL.md body + file list.
    let loaded = reduce_skill_manager(
        &after_enter.state,
        SkillManagerEvent::DetailLoaded {
            slug: "commit".into(),
            body: "# Commit skill\nCreate well-formatted commits.".into(),
            files: vec![
                SkillFile {
                    path: "SKILL.md".into(),
                },
                SkillFile {
                    path: "assets/checklist.md".into(),
                },
            ],
        },
    )
    .state;

    // Wide enough that the middle file tree is shown alongside the editor.
    let mut buf = WireBuffer::new(120, 12);
    render_skill_manager(&mut buf, 120, 0, 12, &loaded);
    let full = glyph_map(&buf, 120);

    // POSITIVE: the detail pane renders the body's first line.
    assert!(
        full.contains("# Commit skill"),
        "detail pane must render the SKILL.md body:\n{full}"
    );
    // POSITIVE: the file list renders SKILL.md + the nested asset.
    assert!(
        full.contains("SKILL.md"),
        "file tree must list SKILL.md:\n{full}"
    );
    assert!(
        full.contains("checklist.md"),
        "file tree must list the nested asset:\n{full}"
    );
    // NEGATIVE: the empty-detail placeholder is gone now that a body is loaded.
    assert!(
        !full.contains("(select a file)"),
        "placeholder must be replaced by the loaded body:\n{full}"
    );
    // State accessor agrees.
    assert_eq!(
        loaded.detail_body(),
        Some("# Commit skill\nCreate well-formatted commits.")
    );
}
