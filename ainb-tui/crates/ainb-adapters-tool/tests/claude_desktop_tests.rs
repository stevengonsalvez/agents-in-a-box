//! ClaudeDesktopAdapter tests — accept matrix + plan/apply/uninstall + list scope.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{AcceptDecision, ClaudeDesktopAdapter, ToolAdapter, plan::PlanOp};
use ainb_skill_core::{DeployedRef, UnitKind};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_claude_desktop_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // The env-var transform uppercases `claude-desktop` to
    // `AINB_TOOL_HOME_CLAUDE_DESKTOP` which is still a valid env name
    // for std::env. Set it accordingly.
    std::env::set_var("AINB_TOOL_HOME_CLAUDE_DESKTOP", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_CLAUDE_DESKTOP");
    r
}

fn skill_fixture() -> (tempfile::TempDir, ResolvedUnit) {
    // Use a generic skill fixture as the deploy-mechanics vehicle. The
    // accept-matrix test asserts skill is *declined* for claude-desktop,
    // but plan_install / apply don't gate on accepts — they purely copy
    // files. The list-installed scope test below verifies the adapter
    // only surfaces mcp-server entries from disk.
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
fn accepts_only_mcp_server() {
    let a = ClaudeDesktopAdapter::new();
    assert_eq!(a.accepts(UnitKind::McpServer), AcceptDecision::Yes);
}

#[test]
fn declines_everything_else_with_reason() {
    let a = ClaudeDesktopAdapter::new();
    for k in [
        UnitKind::Skill,
        UnitKind::Plugin,
        UnitKind::Agent,
        UnitKind::Command,
        UnitKind::Hook,
        UnitKind::Statusline,
    ] {
        let d = a.accepts(k);
        assert!(!d.is_yes(), "expected No for {k}");
        let reason = d.reason().unwrap();
        assert!(reason.contains("claude-desktop"), "got: {reason}");
    }
}

#[test]
fn plan_apply_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_claude_desktop_home(dst.path(), || {
        let plan = ClaudeDesktopAdapter::new().plan_install(&unit).unwrap();
        assert!(plan.ops.iter().any(|o| matches!(o, PlanOp::Create { .. })));
        ClaudeDesktopAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn uninstall_removes_and_prunes() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_claude_desktop_home(dst.path(), || {
        let plan = ClaudeDesktopAdapter::new().plan_install(&unit).unwrap();
        let report = ClaudeDesktopAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };
        ClaudeDesktopAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst.path().join("skills/commit").exists());
    });
}

#[test]
fn list_installed_scope_is_mcp_only() {
    let dst = tempfile::tempdir().unwrap();
    with_claude_desktop_home(dst.path(), || {
        std::fs::create_dir_all(dst.path().join("skills/foo")).unwrap();
        std::fs::write(dst.path().join("skills/foo/SKILL.md"), "x").unwrap();
        let installed = ClaudeDesktopAdapter::new().list_installed().unwrap();
        assert!(
            installed.is_empty(),
            "claude-desktop shouldn't list skills: {installed:?}"
        );
    });
}

#[test]
fn template_substitutions() {
    let m = ClaudeDesktopAdapter::new().template_substitutions();
    assert_eq!(
        m.get("TOOL_DIR").map(String::as_str),
        Some("Library/Application Support/Claude")
    );
    assert_eq!(
        m.get("TOOL_NAME").map(String::as_str),
        Some("claude-desktop")
    );
}
