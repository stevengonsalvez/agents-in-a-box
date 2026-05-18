//! CursorAdapter tests — accept matrix + plan/apply/uninstall + list scope.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{AcceptDecision, CursorAdapter, ToolAdapter, plan::PlanOp};
use ainb_skill_core::{DeployedRef, UnitKind};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_cursor_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_CURSOR", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_CURSOR");
    r
}

fn skill_fixture() -> (tempfile::TempDir, ResolvedUnit) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills/commit")).unwrap();
    std::fs::write(
        dir.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\n---\n",
    )
    .unwrap();
    let r = RawAdapter::new().resolve_unit(dir.path(), "skills/commit").unwrap();
    (dir, r)
}

#[test]
fn accepts_skill_command_mcp() {
    let a = CursorAdapter::new();
    assert_eq!(a.accepts(UnitKind::Skill), AcceptDecision::Yes);
    assert_eq!(a.accepts(UnitKind::Command), AcceptDecision::Yes);
    assert_eq!(a.accepts(UnitKind::McpServer), AcceptDecision::Yes);
}

#[test]
fn declines_plugin_agent_hook_statusline_with_reason() {
    let a = CursorAdapter::new();
    for k in [
        UnitKind::Plugin,
        UnitKind::Agent,
        UnitKind::Hook,
        UnitKind::Statusline,
    ] {
        let d = a.accepts(k);
        assert!(!d.is_yes(), "expected No for {k}");
        let reason = d.reason().unwrap();
        assert!(reason.contains("cursor"), "got: {reason}");
    }
}

#[test]
fn plan_apply_skill_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_cursor_home(dst.path(), || {
        let plan = CursorAdapter::new().plan_install(&unit).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, PlanOp::Create { .. })));
        CursorAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn uninstall_removes_and_prunes() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_cursor_home(dst.path(), || {
        let plan = CursorAdapter::new().plan_install(&unit).unwrap();
        let report = CursorAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };
        CursorAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst.path().join("skills/commit").exists());
    });
}

#[test]
fn list_installed_scope_skips_unsupported_kinds() {
    // cursor scans skills/commands/mcp-servers; an agents/ deploy
    // shouldn't appear.
    let dst = tempfile::tempdir().unwrap();
    with_cursor_home(dst.path(), || {
        std::fs::create_dir_all(dst.path().join("agents")).unwrap();
        std::fs::write(dst.path().join("agents/foo.md"), "x").unwrap();
        let installed = CursorAdapter::new().list_installed().unwrap();
        assert!(
            installed.is_empty(),
            "cursor shouldn't list agents: {installed:?}"
        );
    });
}

#[test]
fn template_substitutions() {
    let m = CursorAdapter::new().template_substitutions();
    assert_eq!(m.get("TOOL_DIR").map(String::as_str), Some(".cursor"));
    assert_eq!(m.get("TOOL_NAME").map(String::as_str), Some("cursor"));
}
