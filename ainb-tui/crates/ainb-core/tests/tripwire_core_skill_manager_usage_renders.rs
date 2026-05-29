//! Tripwire: SkillManager Detail pane renders per-unit usage telemetry
//! (invocation count + a "last used <time-ago>" string) when the
//! lockfile carries a populated `usage` record.
//!
//! Catches: `compute_detail_for_selected` still stubbing
//! `last_used`/`invocations` to `None` instead of pulling from
//! `LockedUnit.usage`; `render_detail_pane` printing the wrong
//! format string; refactor that drops the wire-up entirely.
//!
//! Bead: v12.B.4 — SkillsScreenData wires UsageCache into Detail pane.

use std::fs;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use tempfile::TempDir;

use ainb::components::skill_manager_screen::{SkillsScreenData, render};

const MANIFEST_YAML: &str = "\
version: 1
sources:
  - name: stevie-tools
    uri: gh:stevengonsalvez/tools
    enabled: true
units:
  - uri: gh:stevengonsalvez/tools@main/skills/commit
    kind: skill
    targets: [claude]
";

/// Lockfile with a single LockedUnit whose `declared_uri` matches the
/// manifest's only unit, populated with a usage record so the Detail
/// pane has something non-empty to render.
///
/// `last_used_at` is intentionally an old timestamp so the time-ago
/// formatter has a comfortable margin (any reasonable now() will be
/// at least minutes later); the tripwire only asserts on the
/// invocation count which is independent of clock state.
const LOCKFILE_YAML: &str = "\
schema_version: 2
sources: []
units:
  - uri: gh:stevengonsalvez/tools@abc123/skills/commit
    declared_uri: gh:stevengonsalvez/tools@main/skills/commit
    kind: skill
    sha: abc123
    deployed:
      claude:
        status: deployed
        path: ~/.claude/skills/commit/SKILL.md
    usage:
      last_used_at: \"2026-05-01T00:00:00Z\"
      invocations: 12
";

#[test]
fn detail_pane_renders_usage_invocations_when_lockfile_has_usage_record() {
    let tmp = TempDir::new().expect("tempdir");
    let home = tmp.path();
    fs::write(home.join("manifest.yaml"), MANIFEST_YAML).expect("write manifest");
    fs::write(home.join("lock.yaml"), LOCKFILE_YAML).expect("write lockfile");

    let data = SkillsScreenData::load_from_disk(home);
    assert!(
        !data.units.is_empty(),
        "manifest should have produced a unit row; got units={:?}",
        data.units.len()
    );
    assert!(
        data.detail.is_some(),
        "load_from_disk should have computed a Detail for the selected unit"
    );

    let backend = TestBackend::new(120, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");
    terminal
        .draw(|frame| {
            let area = Rect::new(0, 0, 120, 24);
            render(frame, area, &data);
        })
        .expect("draw");

    let buffer = terminal.backend().buffer().clone();
    let rendered: String = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    assert!(
        rendered.contains("12 invocations"),
        "Detail pane must show invocation count; rendered:\n{rendered}"
    );
}
