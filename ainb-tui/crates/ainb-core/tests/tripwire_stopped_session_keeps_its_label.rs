// ABOUTME: Tripwire for "stopping a session loses its label".
//
// A session the operator named shows `<label> · <branch>` in the list while it
// runs. Stop it and the row fell back to a bare branch name, which is the one
// moment the label matters: two stopped worktrees off the same repo are
// otherwise told apart only by an 8-hex branch suffix.
//
// The unit test for this calls `stopped_session_from_metadata` with a store it
// built itself. That proves the renderer and nothing about the wiring. This
// drives the REAL loader (`AppState::load_real_workspaces`) against a REAL git
// worktree, a REAL sessions.json and a REAL session-labels.json, then renders
// the REAL session-list component and asserts on the painted buffer.
//
// Own test binary on purpose: `AINB_HOME` is process-global, and isolating it
// keeps the developer's live sessions and labels out of the assertions.

use std::path::Path;
use std::process::Command;

use ainb::app::state::{AppState, SessionFilter};
use ainb::components::SessionListComponent;
use ainb::config::SessionLabelStore;
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use chrono::Utc;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use uuid::Uuid;

/// The label under test. Distinctive enough that finding it in the buffer
/// cannot be an accident of chrome, a repo name or a branch suffix.
const LABEL: &str = "prove-hangar-acp-leg";

fn tool_available(bin: &str, arg: &str) -> bool {
    Command::new(bin).arg(arg).output().is_ok_and(|o| o.status.success())
}

/// Real git state only. Hand-faked `.git` fixtures are how the sibling
/// workspace-name bug survived four releases.
fn git_ok(cwd: &Path, args: &[&str]) -> bool {
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

fn painted(state: &mut AppState) -> String {
    // What the operator does to look at stopped sessions: `F` cycles the filter
    // onto stopped-only (the default is active-only, which hides them), and the
    // workspace has to be expanded for its rows to paint at all. Without both,
    // the list renders an empty tree and every label assertion below passes or
    // fails for the wrong reason.
    state.session_filter = SessionFilter::StoppedOnly;
    state.expand_all_workspaces = true;

    let mut list = SessionListComponent::new();
    let mut ui = ainb::app::ui_state::UiState::default();
    let mut term = Terminal::new(TestBackend::new(140, 40)).expect("test terminal");
    term.draw(|f| list.render(f, f.area(), state, &mut ui)).expect("draw");
    term.backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

#[tokio::test]
async fn a_stopped_session_still_paints_the_label_it_ran_under() {
    if !tool_available("git", "--version") {
        eprintln!("SKIP: git unavailable");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().canonicalize().expect("canonicalize home");
    // Isolates SessionStore, SessionLabelStore AND WorktreeManager together, so
    // neither a real session nor a real label can leak into the assertion.
    std::env::set_var("AINB_HOME", &home_path);
    std::fs::create_dir_all(home_path.join(".agents-in-a-box")).unwrap();

    // Real repo plus a real linked worktree off it. The branch carries the
    // 8-hex shape a launched session actually gets, so the assertion below
    // cannot pass by matching some friendlier name.
    let repo = home_path.join("src/myrepo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git_ok(&repo, &["init"]), "git init");
    std::fs::write(repo.join("README.md"), "hi").unwrap();
    assert!(git_ok(&repo, &["add", "README.md"]), "git add");
    assert!(git_ok(&repo, &["commit", "-m", "init"]), "git commit");

    let worktree = home_path.join("src/myrepo--agents-abeb7e09");
    assert!(
        git_ok(
            &repo,
            &[
                "worktree",
                "add",
                worktree.to_str().unwrap(),
                "-b",
                "agents/abeb7e09"
            ],
        ),
        "git worktree add"
    );

    // No tmux session is created for this name, so the loader can only reach
    // this row through the STOPPED path. That is the path under test.
    let tmux_name = format!("tmux_ainblabel-stopped-{}", std::process::id());

    let mut store = SessionStore::default();
    let session_id = Uuid::new_v4();
    store.upsert(SessionMetadata {
        session_id,
        tmux_session_name: tmux_name.clone(),
        worktree_path: worktree.clone(),
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
    });
    store.save().expect("save sessions.json");

    let mut labels = SessionLabelStore::default();
    labels.set(tmux_name.clone(), Some(LABEL.to_string()));
    labels.save().expect("save session-labels.json");

    // ── Drive the real loader ────────────────────────────────────────────
    let mut state = AppState::new();
    state.load_real_workspaces().await;

    let session = state
        .workspaces
        .iter()
        .flat_map(|w| w.sessions.iter())
        .find(|s| s.id == session_id)
        .expect("the stopped session must be visible in the tree at all");
    assert_eq!(
        session.status,
        ainb::models::SessionStatus::Stopped,
        "fixture must exercise the STOPPED path, got {:?}",
        session.status
    );

    // ── User-visible proof ───────────────────────────────────────────────
    let buffer = painted(&mut state);
    assert!(
        buffer.contains(LABEL),
        "a stopped session lost the label it ran under:\n{buffer}"
    );
    // The label must PREFIX the branch, not replace it. An operator needs both:
    // the label to recognise the run, the branch to know what to do with it.
    assert!(
        buffer.contains("agents/abeb7e09"),
        "the label displaced the branch on the stopped row:\n{buffer}"
    );

    // Negative control. With the label removed from the store and nothing else
    // changed, the same render must lose it. Without this the assertion above
    // would also pass on a build that painted the label from anywhere else.
    let mut cleared = SessionLabelStore::default();
    cleared.set(tmux_name.clone(), None);
    cleared.save().expect("clear session-labels.json");

    let mut unlabelled = AppState::new();
    unlabelled.load_real_workspaces().await;
    let without = painted(&mut unlabelled);
    assert!(
        !without.contains(LABEL),
        "the label survived its own removal, so this test proves nothing:\n{without}"
    );
    assert!(
        without.contains("agents/abeb7e09"),
        "clearing the label should leave the branch, not the row:\n{without}"
    );

    std::env::remove_var("AINB_HOME");
}
