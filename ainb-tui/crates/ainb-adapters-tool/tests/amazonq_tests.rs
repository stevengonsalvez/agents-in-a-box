//! AmazonqAdapter tests — accept matrix + plan/apply/uninstall + list scope.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{AcceptDecision, AmazonqAdapter, ToolAdapter, plan::PlanOp};
use ainb_skill_core::{DeployedRef, UnitKind};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_amazonq_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_AMAZONQ", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_AMAZONQ");
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
fn accepts_only_skill() {
    let a = AmazonqAdapter::new();
    assert_eq!(a.accepts(UnitKind::Skill), AcceptDecision::Yes);
}

#[test]
fn declines_everything_else_with_reason() {
    let a = AmazonqAdapter::new();
    for k in [
        UnitKind::Plugin,
        UnitKind::Agent,
        UnitKind::Command,
        UnitKind::Hook,
        UnitKind::McpServer,
        UnitKind::Statusline,
    ] {
        let d = a.accepts(k);
        assert!(!d.is_yes(), "expected No for {k}");
        let reason = d.reason().unwrap();
        assert!(reason.contains("amazonq"), "got: {reason}");
    }
}

#[test]
fn plan_apply_skill_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_amazonq_home(dst.path(), || {
        let plan = AmazonqAdapter::new().plan_install(&unit).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, PlanOp::Create { .. })));
        AmazonqAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn uninstall_removes_and_prunes() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_amazonq_home(dst.path(), || {
        let plan = AmazonqAdapter::new().plan_install(&unit).unwrap();
        let report = AmazonqAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };
        AmazonqAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst.path().join("skills/commit").exists());
    });
}

#[test]
fn list_installed_scope_is_skills_only() {
    let dst = tempfile::tempdir().unwrap();
    with_amazonq_home(dst.path(), || {
        std::fs::create_dir_all(dst.path().join("mcp-servers/foo")).unwrap();
        std::fs::write(dst.path().join("mcp-servers/foo/cfg.json"), "{}").unwrap();
        let installed = AmazonqAdapter::new().list_installed().unwrap();
        assert!(
            installed.is_empty(),
            "amazonq shouldn't list mcp: {installed:?}"
        );
    });
}

#[test]
fn template_substitutions() {
    let m = AmazonqAdapter::new().template_substitutions();
    assert_eq!(m.get("TOOL_DIR").map(String::as_str), Some(".aws/amazonq"));
    assert_eq!(m.get("TOOL_NAME").map(String::as_str), Some("amazonq"));
}
