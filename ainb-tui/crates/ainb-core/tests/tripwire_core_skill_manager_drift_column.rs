//! Tripwire: SkillManager Units panel renders a `status` column whose
//! cell symbol reflects the cached `DriftStatus` for each unit.
//!
//! Catches: the drift column being dropped from `render_units_table`;
//! the `drift_cache` field falling out of `SkillsScreenData`; the row
//! → cache lookup keying on the wrong field (e.g. `name` instead of
//! `declared_uri`); the placeholder glyph being rendered for cached
//! rows.
//!
//! Bead: v12.E.3 — DriftStatus → Units panel status column.

use std::collections::BTreeMap;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;

use ainb::components::skill_manager_screen::{
    SkillsScreenData, SourceRow, UnitRow, render,
};
use ainb_skill_core::drift::DriftStatus;

fn row(idx: usize, name: &str, declared_uri: &str) -> UnitRow {
    UnitRow {
        idx,
        name: name.to_string(),
        kind: "skill".to_string(),
        source: "gh:org/acme".to_string(),
        git_ref: "main".to_string(),
        targets: vec!["claude".to_string()],
        declared_uri: declared_uri.to_string(),
    }
}

fn render_data(data: &SkillsScreenData) -> String {
    let backend = TestBackend::new(160, 30);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 160, 30);
            render(frame, area, data);
        })
        .expect("draw");
    let buffer = terminal.backend().buffer().clone();
    buffer.content().iter().map(ratatui::buffer::Cell::symbol).collect()
}

#[test]
fn units_table_renders_in_sync_glyph_for_in_sync_unit() {
    let mut drift_cache: BTreeMap<String, DriftStatus> = BTreeMap::new();
    let uri = "gh:org/acme@main/skills/foo";
    drift_cache.insert(uri.to_string(), DriftStatus::InSync);

    let data = SkillsScreenData {
        sources: vec![SourceRow {
            name: "acme".to_string(),
            uri: "gh:org/acme".to_string(),
            enabled: true,
        }],
        units: vec![row(1, "foo", uri)],
        selected: 0,
        detail: None,
        banner: Default::default(),
        walker_cache: None,
        drift_cache,
    };

    let rendered = render_data(&data);
    // The in-sync glyph (✓) must appear in the rendered buffer.
    assert!(
        rendered.contains('✓'),
        "InSync should render ✓ glyph; rendered:\n{rendered}"
    );
    // Header should label the column "status".
    assert!(
        rendered.contains("status"),
        "Units header should include a 'status' column; rendered:\n{rendered}"
    );
}

#[test]
fn units_table_renders_warning_glyph_for_outdated_unit() {
    let mut drift_cache: BTreeMap<String, DriftStatus> = BTreeMap::new();
    let uri = "gh:org/acme@main/skills/foo";
    drift_cache.insert(uri.to_string(), DriftStatus::Outdated { behind: 4 });

    let data = SkillsScreenData {
        sources: Vec::new(),
        units: vec![row(1, "foo", uri)],
        selected: 0,
        detail: None,
        banner: Default::default(),
        walker_cache: None,
        drift_cache,
    };

    let rendered = render_data(&data);
    assert!(
        rendered.contains('⚠'),
        "Outdated should render ⚠ glyph; rendered:\n{rendered}"
    );
}

#[test]
fn units_table_renders_ahead_glyph_for_ahead_unit() {
    let mut drift_cache: BTreeMap<String, DriftStatus> = BTreeMap::new();
    let uri = "gh:org/acme@main/skills/foo";
    drift_cache.insert(uri.to_string(), DriftStatus::Ahead { ahead: 2 });

    let data = SkillsScreenData {
        sources: Vec::new(),
        units: vec![row(1, "foo", uri)],
        selected: 0,
        detail: None,
        banner: Default::default(),
        walker_cache: None,
        drift_cache,
    };

    let rendered = render_data(&data);
    assert!(
        rendered.contains('▲'),
        "Ahead should render ▲ glyph; rendered:\n{rendered}"
    );
}

#[test]
fn units_table_renders_diverged_glyph_for_diverged_unit() {
    let mut drift_cache: BTreeMap<String, DriftStatus> = BTreeMap::new();
    let uri = "gh:org/acme@main/skills/foo";
    drift_cache
        .insert(uri.to_string(), DriftStatus::Diverged { ahead: 1, behind: 3 });

    let data = SkillsScreenData {
        sources: Vec::new(),
        units: vec![row(1, "foo", uri)],
        selected: 0,
        detail: None,
        banner: Default::default(),
        walker_cache: None,
        drift_cache,
    };

    let rendered = render_data(&data);
    assert!(
        rendered.contains('⟷'),
        "Diverged should render ⟷ glyph; rendered:\n{rendered}"
    );
}

#[test]
fn units_table_renders_placeholder_when_cache_missing() {
    let drift_cache: BTreeMap<String, DriftStatus> = BTreeMap::new();
    let uri = "gh:org/acme@main/skills/foo";

    let data = SkillsScreenData {
        sources: Vec::new(),
        units: vec![row(1, "foo", uri)],
        selected: 0,
        detail: None,
        banner: Default::default(),
        walker_cache: None,
        drift_cache,
    };

    let rendered = render_data(&data);
    // No drift status known yet → an ellipsis placeholder appears in
    // the rendered buffer.
    assert!(
        rendered.contains('…'),
        "Missing drift entry should render placeholder; rendered:\n{rendered}"
    );
}
