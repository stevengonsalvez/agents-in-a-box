//! Tripwire: TestBackend renders the SkillManager screen against the
//! sandbox fixture's pre-seeded Full-tier manifest. Asserts the
//! seeded source name + unit URI appear in the rendered buffer.
//!
//! Catches: `SkillsScreenData::load_from_disk` regressing against the
//! manifest+lock the fixture writes; render path dropping the Sources
//! / Units panel content; the fixture's manifest.yaml content
//! diverging from what the screen expects to load.
//!
//! Bead `ai-e7t`. In-process counterpart to the live-tmux tripwire
//! `tripwire_core_skill_manager_sandbox_e2e.rs`; together they cover
//! both layers (load+render correctness AND real-binary keystroke
//! integration).

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use tempfile::TempDir;

use ainb::components::skill_manager_screen::{SkillsScreenData, render};
use ainb_skill_core::{build_skill_manager_sandbox, SandboxTier};

fn buffer_as_string(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut s = String::with_capacity(buffer.area.width as usize * buffer.area.height as usize);
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            s.push_str(buffer.get(x, y).symbol());
        }
        s.push('\n');
    }
    s
}

#[test]
fn sandbox_full_tier_manifest_renders_into_test_backend() {
    let tmp = TempDir::new().expect("tempdir");
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Full)
        .expect("sandbox full");

    // Load the screen state from the fixture-seeded ainb_home. Full
    // tier pre-seeds manifest.yaml with one source + two units (one
    // of which is shadowed_by the other), so the Units panel has
    // real content.
    let data = SkillsScreenData::load_from_disk(&layout.ainb_home);

    assert!(
        !data.sources.is_empty(),
        "Full-tier manifest must produce at least one source row (got 0)"
    );
    assert!(
        !data.units.is_empty(),
        "Full-tier manifest must produce at least one unit row (got 0)"
    );

    // Render to a 120x32 TestBackend so the layout has room for all
    // three panes + the help bar without clipping.
    let backend = TestBackend::new(120, 32);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 120, 32);
            render(frame, area, &data);
        })
        .expect("draw");

    let buf = buffer_as_string(&terminal);

    // Sources panel must show the fixture's source name.
    assert!(
        buf.contains("sandbox"),
        "rendered buffer missing seeded source name `sandbox`:\n{buf}"
    );
    // Units panel must show the seed unit name (the URI's path
    // terminal segment).
    assert!(
        buf.contains("initial-skill"),
        "rendered buffer missing seeded unit `initial-skill`:\n{buf}"
    );
    // Help bar must be alive — the [s] hotkey label.
    assert!(
        buf.contains("[s]"),
        "rendered buffer missing help-bar `[s]` hint:\n{buf}"
    );
}

#[test]
fn sandbox_minimal_tier_with_empty_manifest_yields_empty_screen_data() {
    // Minimal tier does NOT pre-seed manifest.yaml — load_from_disk
    // must handle that gracefully (zero sources, zero units, banner
    // ready to be triggered when walkers run).
    let tmp = TempDir::new().expect("tempdir");
    let layout = build_skill_manager_sandbox(tmp.path(), SandboxTier::Minimal)
        .expect("sandbox minimal");

    let data = SkillsScreenData::load_from_disk(&layout.ainb_home);
    assert!(data.sources.is_empty(), "Minimal tier: no manifest → no sources");
    assert!(data.units.is_empty(), "Minimal tier: no manifest → no units");
}
