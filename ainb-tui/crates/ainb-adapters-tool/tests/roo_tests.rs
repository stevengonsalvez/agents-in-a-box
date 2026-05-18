//! RooAdapter tests — accept matrix + plan/apply/uninstall + list scope.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{AcceptDecision, RooAdapter, ToolAdapter, plan::PlanOp};
use ainb_skill_core::{DeployedRef, UnitKind};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_roo_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_ROO", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_ROO");
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
fn accepts_skill_and_mcp() {
    let a = RooAdapter::new();
    assert_eq!(a.accepts(UnitKind::Skill), AcceptDecision::Yes);
    assert_eq!(a.accepts(UnitKind::McpServer), AcceptDecision::Yes);
}

#[test]
fn declines_plugin_agent_command_hook_statusline_with_reason() {
    let a = RooAdapter::new();
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
        assert!(reason.contains("roo"), "got: {reason}");
    }
}

#[test]
fn plan_apply_skill_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_roo_home(dst.path(), || {
        let plan = RooAdapter::new().plan_install(&unit).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, PlanOp::Create { .. })));
        RooAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn uninstall_removes_and_prunes() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_roo_home(dst.path(), || {
        let plan = RooAdapter::new().plan_install(&unit).unwrap();
        let report = RooAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };
        RooAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst.path().join("skills/commit").exists());
    });
}

#[test]
fn list_installed_scope_skills_and_mcp() {
    let dst = tempfile::tempdir().unwrap();
    with_roo_home(dst.path(), || {
        std::fs::create_dir_all(dst.path().join("hooks/foo")).unwrap();
        std::fs::write(dst.path().join("hooks/foo/hook.yaml"), "x").unwrap();
        let installed = RooAdapter::new().list_installed().unwrap();
        assert!(
            installed.is_empty(),
            "roo shouldn't list hooks: {installed:?}"
        );
    });
}

#[test]
fn template_substitutions() {
    let m = RooAdapter::new().template_substitutions();
    assert_eq!(m.get("TOOL_DIR").map(String::as_str), Some(".roo"));
    assert_eq!(m.get("TOOL_NAME").map(String::as_str), Some("roo"));
}
