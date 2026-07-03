//! P5 — Profile-editor screen render snapshots.
//!
//! Pins the profile-editor layout across four states with
//! `insta::assert_snapshot!` (trailing newline trimmed per
//! `reference_insta_trailing_newline_trap`):
//!
//!   * `empty` — the "no profiles yet" placeholder;
//!   * `loaded` — a three-profile roster with the selected profile's detail +
//!     BOTH compile previews (Claude lossless, Codex lossy + dropped-field
//!     warnings) visible;
//!   * `narrow_80` — the same load at the 80-column floor, proving the roster
//!     column + rule + preview pane clip cleanly without bleeding;
//!   * `loading` — a selection whose detail has not yet arrived (the transient
//!     "loading…" pane).
//!
//! A colour assertion guards the selection green, the section blue (CLAUDE /
//! CODEX labels), and the dropped-field warning amber independently of the golden
//! text.

use ainb_plugin_hangar::screen::profiles::{
    colors, render_profiles, ProfileDetailView, ProfileRosterEntry, ProfilesState,
};
use ainb_plugin_sdk::WireBuffer;

/// A three-profile roster (slug-ordered, as the daemon returns it).
fn roster() -> Vec<ProfileRosterEntry> {
    vec![
        ProfileRosterEntry { slug: "author".into(), tier: "balanced".into() },
        ProfileRosterEntry { slug: "code-reviewer".into(), tier: "premium".into() },
        ProfileRosterEntry { slug: "docs-writer".into(), tier: "fast".into() },
    ]
}

/// The detail for `author` (the default selection), with a Codex-incompatible
/// field set so both dropped-field warnings render.
fn author_detail() -> ProfileDetailView {
    ProfileDetailView {
        slug: "author".into(),
        description: "Drafts release notes from a diff".into(),
        tier: "balanced".into(),
        tools: vec!["Read".into(), "Grep".into()],
        color: "cyan".into(),
        claude_preview: "---\nname: author\nmodel: sonnet\ntools: Read, Grep\n---\nYou draft notes."
            .into(),
        codex_fragment: "[profiles.author]\nmodel = \"gpt-5-codex\"".into(),
        codex_prompt: "You draft notes.".into(),
        codex_warnings: vec![
            "profile \"author\": dropped Claude-only field `tools` (Read, Grep)".into(),
            "profile \"author\": dropped Claude-only field `color` (cyan)".into(),
        ],
    }
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

#[test]
fn empty_profiles_snapshot() {
    let state = ProfilesState::default();
    let mut buf = WireBuffer::new(100, 24);
    render_profiles(&mut buf, 100, 0, 23, &state);
    let full = glyph_map(&buf, 100);
    assert!(full.contains("Profiles"), "title:\n{full}");
    assert!(full.contains("0 profiles"), "count:\n{full}");
    assert!(full.contains("no profiles yet"), "empty placeholder:\n{full}");
    insta::assert_snapshot!(full);
}

#[test]
fn loaded_profiles_snapshot() {
    let mut state = ProfilesState::default();
    state.set_roster(roster());
    state.set_detail(author_detail());
    let mut buf = WireBuffer::new(100, 24);
    render_profiles(&mut buf, 100, 0, 23, &state);
    let full = glyph_map(&buf, 100);

    assert!(full.contains("3 profiles"), "count:\n{full}");
    // The roster shows every profile + its tier.
    assert!(full.contains("code-reviewer"), "roster:\n{full}");
    // BOTH compile previews are labelled.
    assert!(full.contains("CLAUDE"), "claude preview label:\n{full}");
    assert!(full.contains("CODEX"), "codex preview label:\n{full}");
    // The Claude preview resolves the tier to a model; the Codex fragment too.
    assert!(full.contains("model: sonnet"), "claude resolved model:\n{full}");
    assert!(full.contains("gpt-5-codex"), "codex resolved model:\n{full}");
    // Dropped-field warnings render.
    assert!(full.contains("dropped Claude-only field"), "codex warnings:\n{full}");
    insta::assert_snapshot!(full);
}

#[test]
fn narrow_80col_snapshot() {
    let mut state = ProfilesState::default();
    state.set_roster(roster());
    state.set_detail(author_detail());
    let mut buf = WireBuffer::new(80, 24);
    render_profiles(&mut buf, 80, 0, 23, &state);
    let full = glyph_map(&buf, 80);
    for line in full.lines() {
        assert!(line.chars().count() <= 80, "line over 80 cols: {line:?}");
    }
    assert!(full.contains("Profiles"), "title at 80col:\n{full}");
    insta::assert_snapshot!(full);
}

#[test]
fn loading_detail_snapshot() {
    let mut state = ProfilesState::default();
    state.set_roster(roster()); // selection = author, no detail set yet
    let mut buf = WireBuffer::new(100, 24);
    render_profiles(&mut buf, 100, 0, 23, &state);
    let full = glyph_map(&buf, 100);
    assert!(full.contains("loading"), "loading pane:\n{full}");
    insta::assert_snapshot!(full);
}

#[test]
fn selection_is_green_section_blue_warnings_amber() {
    let mut state = ProfilesState::default();
    state.set_roster(roster());
    state.set_detail(author_detail());
    let mut buf = WireBuffer::new(100, 24);
    render_profiles(&mut buf, 100, 0, 23, &state);

    // The selected row's ▶ marker is selection-green (non-vacuous).
    let green_marker = buf
        .cells
        .iter()
        .any(|(_, c)| c.symbol == "▶" && c.fg == Some(colors::SELECTION));
    assert!(green_marker, "the selected profile's ▶ marker must be selection-green");

    // A CLAUDE/CODEX section label is painted section-blue.
    let section_blue = buf.cells.iter().any(|(_, c)| c.fg == Some(colors::SECTION));
    assert!(section_blue, "the preview section labels must be section-blue");

    // A dropped-field warning glyph is amber.
    let warn_amber = buf.cells.iter().any(|(_, c)| c.fg == Some(colors::WARN));
    assert!(warn_amber, "the Codex dropped-field warnings must be warning-amber");
}
