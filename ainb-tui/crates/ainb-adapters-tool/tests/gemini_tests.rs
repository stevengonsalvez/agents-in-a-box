//! GeminiAdapter tests — accept matrix + plan/apply/uninstall + list scope.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{
    plan::PlanOp, AcceptDecision, GeminiAdapter, ToolAdapter,
};
use ainb_skill_core::{DeployedRef, UnitKind};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_gemini_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_GEMINI", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_GEMINI");
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
fn accepts_skill_and_agent() {
    let a = GeminiAdapter::new();
    assert_eq!(a.accepts(UnitKind::Skill), AcceptDecision::Yes);
    assert_eq!(a.accepts(UnitKind::Agent), AcceptDecision::Yes);
}

#[test]
fn declines_everything_else_with_reason() {
    let a = GeminiAdapter::new();
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
        assert!(reason.contains("gemini"), "got: {reason}");
    }
}

#[test]
fn plan_apply_skill_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_gemini_home(dst.path(), || {
        let plan = GeminiAdapter::new().plan_install(&unit).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, PlanOp::Create { .. })));
        GeminiAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn uninstall_removes_and_prunes() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_gemini_home(dst.path(), || {
        let plan = GeminiAdapter::new().plan_install(&unit).unwrap();
        let report = GeminiAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };
        GeminiAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst.path().join("skills/commit").exists());
    });
}

#[test]
fn list_installed_scope_is_skills_and_agents() {
    // gemini's list_installed scans skills/agents only — plugins/hooks
    // deployed manually shouldn't appear.
    let dst = tempfile::tempdir().unwrap();
    with_gemini_home(dst.path(), || {
        std::fs::create_dir_all(dst.path().join("hooks/foo")).unwrap();
        std::fs::write(dst.path().join("hooks/foo/hook.yaml"), "x").unwrap();
        let installed = GeminiAdapter::new().list_installed().unwrap();
        assert!(installed.is_empty(), "gemini shouldn't list hooks: {installed:?}");
    });
}

#[test]
fn template_substitutions() {
    let m = GeminiAdapter::new().template_substitutions();
    assert_eq!(m.get("TOOL_DIR").map(String::as_str), Some(".gemini"));
    assert_eq!(m.get("TOOL_NAME").map(String::as_str), Some("gemini"));
}
