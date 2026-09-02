//! AntigravityAdapter tests — accept matrix + plan/apply/uninstall + list scope.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{
    AcceptDecision, AntigravityAdapter, ToolAdapter, adapter_by_name, all_adapters, plan::PlanOp,
};
use ainb_skill_core::{DeployedRef, UnitKind};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_antigravity_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_ANTIGRAVITY", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_ANTIGRAVITY");
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
fn accepts_skill_and_agent() {
    let a = AntigravityAdapter::new();
    assert_eq!(a.accepts(UnitKind::Skill), AcceptDecision::Yes);
    assert_eq!(a.accepts(UnitKind::Agent), AcceptDecision::Yes);
}

#[test]
fn declines_everything_else_with_reason() {
    let a = AntigravityAdapter::new();
    for k in [
        UnitKind::Plugin,
        UnitKind::Command,
        UnitKind::Hook,
        UnitKind::McpServer,
        UnitKind::Statusline,
    ] {
        let d = a.accepts(k);
        assert!(!d.is_yes(), "expected No for {k}");
        let reason = d.reason().unwrap();
        assert!(reason.contains("antigravity"), "got: {reason}");
    }
}

#[test]
fn plan_apply_skill_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_antigravity_home(dst.path(), || {
        let plan = AntigravityAdapter::new().plan_install(&unit).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, PlanOp::Create { .. })));
        AntigravityAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn uninstall_removes_and_prunes() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_antigravity_home(dst.path(), || {
        let plan = AntigravityAdapter::new().plan_install(&unit).unwrap();
        let report = AntigravityAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };
        AntigravityAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst.path().join("skills/commit").exists());
    });
}

#[test]
fn list_installed_scope_is_skills_and_agents() {
    let dst = tempfile::tempdir().unwrap();
    with_antigravity_home(dst.path(), || {
        std::fs::create_dir_all(dst.path().join("hooks/foo")).unwrap();
        std::fs::write(dst.path().join("hooks/foo/hook.yaml"), "x").unwrap();
        let installed = AntigravityAdapter::new().list_installed().unwrap();
        assert!(
            installed.is_empty(),
            "antigravity shouldn't list hooks: {installed:?}"
        );
    });
}

#[test]
fn template_substitutions() {
    let m = AntigravityAdapter::new().template_substitutions();
    assert_eq!(
        m.get("TOOL_DIR").map(String::as_str),
        Some(".gemini/antigravity-cli")
    );
    assert_eq!(m.get("TOOL_NAME").map(String::as_str), Some("antigravity"));
    assert_eq!(
        m.get("HOME_TOOL_DIR").map(String::as_str),
        Some("~/.gemini/antigravity-cli")
    );
}

#[test]
fn all_adapters_includes_antigravity() {
    let adapters = all_adapters();
    let names: Vec<&str> = adapters.iter().map(|a| a.name()).collect();
    assert!(names.contains(&"antigravity"));
}

#[test]
fn adapter_by_name_finds_antigravity_and_agy() {
    assert_eq!(
        adapter_by_name("antigravity").map(|a| a.name()),
        Some("antigravity")
    );
    assert_eq!(
        adapter_by_name("agy").map(|a| a.name()),
        Some("antigravity")
    );
}
