//! ClaudeAdapter integration tests — plan + apply + uninstall round
//! trip with an isolated `AINB_TOOL_HOME_CLAUDE` so the test never
//! goes near the user's real `~/.claude`.

use std::path::PathBuf;

use ainb_adapters_source::{RawAdapter, ResolvedUnit, SourceAdapter};
use ainb_adapters_tool::{AcceptDecision, ClaudeAdapter, ToolAdapter, plan::PlanOp};
use ainb_skill_core::{DeployedRef, UnitKind};

/// Build a tempdir source layout that RawAdapter can list, and
/// return (source_root, ResolvedUnit for `skills/commit`).
fn fixture_skill() -> (tempfile::TempDir, ResolvedUnit) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("skills/commit/assets")).unwrap();
    std::fs::write(
        dir.path().join("skills/commit/SKILL.md"),
        "---\nname: commit\ndescription: well-formed commits\n---\nbody\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("skills/commit/assets/checklist.md"),
        "# checklist\n",
    )
    .unwrap();

    let resolved = RawAdapter::new().resolve_unit(dir.path(), "skills/commit").unwrap();
    (dir, resolved)
}

/// Set `AINB_TOOL_HOME_CLAUDE`, run a closure, then unset it.
/// Tests share process env, so a global Mutex serializes any
/// concurrent runs (cargo test parallelizes within a binary).
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_claude_home<R>(home: &std::path::Path, body: impl FnOnce() -> R) -> R {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::set_var("AINB_TOOL_HOME_CLAUDE", home);
    let r = body();
    std::env::remove_var("AINB_TOOL_HOME_CLAUDE");
    r
}

#[test]
fn accepts_every_kind() {
    let adapter = ClaudeAdapter::new();
    for k in UnitKind::all() {
        assert_eq!(adapter.accepts(k), AcceptDecision::Yes, "kind: {k}");
    }
}

#[test]
fn install_root_honors_env_override() {
    let dir = tempfile::tempdir().unwrap();
    with_claude_home(dir.path(), || {
        assert_eq!(ClaudeAdapter::new().install_root(), dir.path());
    });
}

#[test]
fn plan_create_for_new_files() {
    let (_src, unit) = fixture_skill();
    let dst_home = tempfile::tempdir().unwrap();
    with_claude_home(dst_home.path(), || {
        let plan = ClaudeAdapter::new().plan_install(&unit).unwrap();
        assert_eq!(plan.ops.len(), 2);
        for op in &plan.ops {
            assert!(matches!(op, PlanOp::Create { .. }), "got: {op:?}");
        }
    });
}

#[test]
fn apply_writes_files_and_returns_hashes() {
    let (_src, unit) = fixture_skill();
    let dst_home = tempfile::tempdir().unwrap();
    with_claude_home(dst_home.path(), || {
        let plan = ClaudeAdapter::new().plan_install(&unit).unwrap();
        let report = ClaudeAdapter::new().apply(&plan).unwrap();
        assert_eq!(report.tool, "claude");
        assert!(dst_home.path().join("skills/commit/SKILL.md").exists());
        assert!(dst_home.path().join("skills/commit/assets/checklist.md").exists());
        assert_eq!(report.file_hashes.len(), 2);
    });
}

#[test]
fn second_apply_is_idempotent_no_ops() {
    let (_src, unit) = fixture_skill();
    let dst_home = tempfile::tempdir().unwrap();
    with_claude_home(dst_home.path(), || {
        let plan1 = ClaudeAdapter::new().plan_install(&unit).unwrap();
        ClaudeAdapter::new().apply(&plan1).unwrap();
        // Re-plan after files are in place — every op should be skipped.
        let plan2 = ClaudeAdapter::new().plan_install(&unit).unwrap();
        assert!(plan2.is_empty(), "expected no-ops, got: {:?}", plan2.ops);
    });
}

#[test]
fn modified_file_yields_update_op() {
    let (_src, unit) = fixture_skill();
    let dst_home = tempfile::tempdir().unwrap();
    with_claude_home(dst_home.path(), || {
        let plan = ClaudeAdapter::new().plan_install(&unit).unwrap();
        ClaudeAdapter::new().apply(&plan).unwrap();
        // Tamper with the deployed file → next plan should reinstall.
        std::fs::write(dst_home.path().join("skills/commit/SKILL.md"), "TAMPERED").unwrap();
        let plan2 = ClaudeAdapter::new().plan_install(&unit).unwrap();
        let any_update = plan2.ops.iter().any(|op| matches!(op, PlanOp::Update { .. }));
        assert!(any_update, "expected Update, got: {:?}", plan2.ops);
    });
}

#[test]
fn uninstall_removes_deployed_files() {
    let (_src, unit) = fixture_skill();
    let dst_home = tempfile::tempdir().unwrap();
    with_claude_home(dst_home.path(), || {
        let plan = ClaudeAdapter::new().plan_install(&unit).unwrap();
        let report = ClaudeAdapter::new().apply(&plan).unwrap();
        let deployed = DeployedRef::Deployed {
            path: report.path.to_string_lossy().to_string(),
            file_hashes: report.file_hashes.clone(),
        };

        assert!(dst_home.path().join("skills/commit/SKILL.md").exists());
        ClaudeAdapter::new().uninstall(&deployed).unwrap();
        assert!(!dst_home.path().join("skills/commit/SKILL.md").exists());
        assert!(!dst_home.path().join("skills/commit/assets/checklist.md").exists());
        // Empty parent dirs are pruned best-effort.
        assert!(
            !dst_home.path().join("skills/commit").exists(),
            "expected empty unit dir to be pruned"
        );
    });
}

#[test]
fn list_installed_finds_deployed_units() {
    let (_src, unit) = fixture_skill();
    let dst_home = tempfile::tempdir().unwrap();
    with_claude_home(dst_home.path(), || {
        let plan = ClaudeAdapter::new().plan_install(&unit).unwrap();
        ClaudeAdapter::new().apply(&plan).unwrap();
        let installed = ClaudeAdapter::new().list_installed().unwrap();
        assert_eq!(installed.len(), 1);
        let DeployedRef::Deployed { path, file_hashes } = &installed[0] else {
            panic!("wrong variant")
        };
        assert!(path.ends_with("skills/commit"));
        assert_eq!(file_hashes.len(), 2);
    });
}

#[test]
fn template_substitutions_include_tool_dir() {
    let m = ClaudeAdapter::new().template_substitutions();
    assert_eq!(m.get("TOOL_DIR").map(String::as_str), Some(".claude"));
    assert_eq!(m.get("TOOL_NAME").map(String::as_str), Some("claude"));
}

// (PathBuf imported only because the rustc 'unused import' lint is
// satisfied via the helpers below; remove if it ever becomes truly
// unused.)
#[allow(dead_code)]
fn _path_buf_marker(_p: PathBuf) {}
