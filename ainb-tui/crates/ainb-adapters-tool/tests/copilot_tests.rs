//! CopilotAdapter tests.

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{
    AcceptDecision, CopilotAdapter, ToolAdapter, adapter_by_name, all_adapters,
};
use ainb_skill_core::UnitKind;

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_copilot_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_COPILOT", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_COPILOT");
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
fn accepts_only_skill_and_agent() {
    let a = CopilotAdapter::new();
    assert_eq!(a.accepts(UnitKind::Skill), AcceptDecision::Yes);
    assert_eq!(a.accepts(UnitKind::Agent), AcceptDecision::Yes);
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
        assert!(reason.contains("copilot"), "got: {reason}");
    }
}

#[test]
fn plan_apply_skill_creates_files() {
    let (_src, unit) = skill_fixture();
    let dst = tempfile::tempdir().unwrap();
    with_copilot_home(dst.path(), || {
        let plan = CopilotAdapter::new().plan_install(&unit).unwrap();
        CopilotAdapter::new().apply(&plan).unwrap();
        assert!(dst.path().join("skills/commit/SKILL.md").exists());
    });
}

#[test]
fn template_substitutions_use_github_path() {
    let m = CopilotAdapter::new().template_substitutions();
    assert_eq!(
        m.get("TOOL_DIR").map(String::as_str),
        Some(".github/copilot")
    );
}

#[test]
fn all_adapters_includes_every_v1_tool() {
    let adapters = all_adapters();
    let names: Vec<&str> = adapters.iter().map(|a| a.name()).collect();
    assert_eq!(
        names,
        vec![
            "claude",
            "codex",
            "copilot",
            "gemini",
            "cursor",
            "amazonq",
            "claude-desktop",
            "cline",
            "roo",
        ]
    );
}

#[test]
fn adapter_by_name_looks_up_each() {
    for name in [
        "claude",
        "codex",
        "copilot",
        "gemini",
        "cursor",
        "amazonq",
        "claude-desktop",
        "cline",
        "roo",
    ] {
        assert_eq!(
            adapter_by_name(name).map(|a| a.name()),
            Some(name),
            "adapter_by_name({name}) returned None"
        );
    }
    assert!(adapter_by_name("does-not-exist").is_none());
}
