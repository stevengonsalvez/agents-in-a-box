//! Shared setup for the P6e reader tests: a real git worktree the loader can
//! place a stopped session in, and the reads of what the loader produced.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb::interactive::session_manager::{ModelSource, SessionMetadata};
use ainb::models::session::SessionAgentType;
use chrono::Utc;
use uuid::Uuid;

pub fn git_ok(cwd: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args([
            "-c",
            "user.name=ainb-test",
            "-c",
            "user.email=ainb@test.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A real repo with one real linked worktree off it, so the loader's
/// stopped-session path can place a row for it.
pub fn real_worktree(home: &Path, suffix: &str) -> PathBuf {
    let repo = home.join("src/myrepo");
    if !repo.exists() {
        std::fs::create_dir_all(&repo).unwrap();
        assert!(git_ok(&repo, &["init"]), "git init");
        std::fs::write(repo.join("README.md"), "hi").unwrap();
        assert!(git_ok(&repo, &["add", "README.md"]), "git add");
        assert!(git_ok(&repo, &["commit", "-m", "init"]), "git commit");
    }
    let worktree = home.join(format!("src/myrepo--agents-{suffix}"));
    let branch = format!("agents/{suffix}");
    assert!(
        git_ok(
            &repo,
            &["worktree", "add", worktree.to_str().unwrap(), "-b", &branch]
        ),
        "git worktree add"
    );
    worktree
}

pub fn session_at(worktree: &Path, tmux: &str) -> SessionMetadata {
    SessionMetadata {
        session_id: Uuid::new_v4(),
        tmux_session_name: tmux.to_string(),
        worktree_path: worktree.to_path_buf(),
        workspace_name: "myrepo".to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::default(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ModelSource::default(),
        codex_model: None,
        codex_thread_id: None,
    }
}

pub fn row_of(meta: &SessionMetadata) -> ainb_hangar_store::repo::sessions::SessionRow {
    let e = ainb::cli::util::metadata_to_entry(meta);
    ainb_hangar_store::repo::sessions::SessionRow {
        session_id: e.session_id,
        tmux_session_name: e.tmux_session_name,
        worktree_path: e.worktree_path,
        workspace_name: e.workspace_name,
        created_at: e.created_at,
        agent_type: e.agent_type,
        headroom_enabled: e.headroom_enabled,
        rtk_enabled: e.rtk_enabled,
        skip_permissions: e.skip_permissions,
        model: e.model,
        model_source: e.model_source,
        codex_model: e.codex_model,
        codex_thread_id: e.codex_thread_id,
    }
}

pub fn listed(state: &ainb::app::state::AppState, id: Uuid) -> bool {
    state
        .sessions
        .workspaces
        .iter()
        .flat_map(|w| w.sessions.iter())
        .any(|s| s.id == id)
}

pub fn notices(state: &ainb::app::state::AppState) -> Vec<String> {
    state.shell.notifications.iter().map(|n| n.message.clone()).collect()
}
