//! ClineAdapter tests — accept matrix + plan/apply/uninstall + list scope.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{
    plan::PlanOp, AcceptDecision, ClineAdapter, ToolAdapter,
};
use ainb_skill_core::{DeployedRef, UnitKind};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_cline_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_CLINE", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_CLINE");
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
    let r = RawAdapter::new()
        .resolve_unit(dir.path(), "skills/commit")
        .unwrap();
    (dir, r)
}

#[test]
fn accepts_skill_and_mcp() {
    let a = ClineAdapter::new();
    assert_eq!(a.accepts(UnitKind::Skill), AcceptDecision::Yes);
    assert_eq!(a.accepts(UnitKind::McpServer), AcceptDecision::Yes);
}

#[test]
fn declines_plugin_agent_command_hook_statusline_with_reason() {
    let a = ClineAdapter::new();
    for k in [
        UnitKind::Plugin,
        UnitKind::Agent,
        UnitKind::Command,
        UnitKind::Hook,
        UnitKind::Statusline,
    ] {
        let d = a.accepts(k);
        assert!(!d.is_yes(), "expected No for {k}");
        let reason = d.reason().unwrap();
        assert!(reason.contains("cline"), "got: {reason}");
    }
}

#[test]
fn plan_apply_skill_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_cline_home(dst.path(), || {
        let plan = ClineAdapter::new().plan_install(&unit).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, PlanOp::Create { .. })));
        ClineAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn uninstall_removes_and_prunes() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_cline_home(dst.path(), || {
        let plan = ClineAdapter::new().plan_install(&unit).unwrap();
        let report = ClineAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };
        ClineAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst.path().join("skills/commit").exists());
    });
}

#[test]
fn list_installed_scope_skills_and_mcp() {
    let dst = tempfile::tempdir().unwrap();
    with_cline_home(dst.path(), || {
        // Drop something in an unsupported subdir; ensure it doesn't surface.
        std::fs::create_dir_all(dst.path().join("agents")).unwrap();
        std::fs::write(dst.path().join("agents/foo.md"), "x").unwrap();
        let installed = ClineAdapter::new().list_installed().unwrap();
        assert!(installed.is_empty(), "cline shouldn't list agents: {installed:?}");
    });
}

#[test]
fn template_substitutions() {
    let m = ClineAdapter::new().template_substitutions();
    assert_eq!(m.get("TOOL_DIR").map(String::as_str), Some(".cline"));
    assert_eq!(m.get("TOOL_NAME").map(String::as_str), Some("cline"));
}
