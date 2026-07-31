// ABOUTME: Tripwire for the "(broken)" workspace-name misclassification.
//
// Repro: `ainb run --repo <plain-clone>` (no `--worktree`) creates a session
// whose worktree_path IS a plain git checkout. The TUI re-derived the workspace
// name from paths on every load and, because `get_source_repository` only
// recognised a linked worktree (`.git` as a FILE), it returned None, the caller
// collapsed source_repository onto worktree_path, and `derive_workspace_name`
// read that collapse as "(broken)". Stopped sessions were dropped from the tree
// entirely by the same lookup.
//
// This drives the REAL loader (`AppState::load_real_workspaces`) against REAL
// git state and a REAL tmux session, then renders the REAL session-list
// component and asserts on the user-visible buffer.
//
// Own test binary on purpose: `AINB_HOME` is process-global, and it isolates
// BOTH the session store and the worktree manager so the developer's live
// sessions cannot leak into the assertions.

use std::path::{Path, PathBuf};
use std::process::Command;

use ainb::app::state::AppState;
use ainb::components::SessionListComponent;
use ainb::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use ainb::models::session::SessionAgentType;
use chrono::Utc;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use uuid::Uuid;

fn tool_available(bin: &str, arg: &str) -> bool {
    Command::new(bin).arg(arg).output().is_ok_and(|o| o.status.success())
}

/// Real git state only. Hand-faked `.git` fixtures are how this bug survived.
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

/// Kills by EXACT session name only, never kill-server / wildcards.
struct TmuxGuard(String);

impl Drop for TmuxGuard {
    fn drop(&mut self) {
        let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output();
    }
}

fn seed(store: &mut SessionStore, tmux_name: &str, worktree: &Path, name: &str) -> Uuid {
    let id = Uuid::new_v4();
    store.upsert(SessionMetadata {
        session_id: id,
        tmux_session_name: tmux_name.to_string(),
        worktree_path: worktree.to_path_buf(),
        workspace_name: name.to_string(),
        created_at: Utc::now(),
        agent_type: SessionAgentType::default(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        model_source: ModelSource::default(),
        codex_model: None,
    });
    id
}

fn workspace_of(state: &AppState, id: Uuid) -> Option<(String, PathBuf)> {
    state
        .workspaces
        .iter()
        .find(|w| w.sessions.iter().any(|s| s.id == id))
        .map(|w| (w.name.clone(), PathBuf::from(&w.path)))
}

#[tokio::test]
async fn plain_checkout_session_renders_repo_name_not_broken() {
    if !tool_available("git", "--version") {
        eprintln!("SKIP: git unavailable");
        return;
    }
    if !tool_available("tmux", "-V") {
        eprintln!("SKIP: tmux unavailable");
        return;
    }

    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().canonicalize().expect("canonicalize home");
    // Isolates SessionStore AND WorktreeManager: no real session can leak in.
    std::env::set_var("AINB_HOME", &home_path);
    std::fs::create_dir_all(home_path.join(".agents-in-a-box")).unwrap();

    // ── Real git state ────────────────────────────────────────────────────
    // 1. Plain checkout (`.git` is a DIRECTORY), the shape that broke.
    let repo = home_path.join("src/myrepo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(git_ok(&repo, &["init"]), "git init");
    std::fs::write(repo.join("README.md"), "hi").unwrap();
    assert!(git_ok(&repo, &["add", "README.md"]), "git add");
    assert!(git_ok(&repo, &["commit", "-m", "init"]), "git commit");
    assert!(repo.join(".git").is_dir());

    // 2. Real linked worktree off the SAME repo (`.git` is a FILE).
    let worktree = home_path.join("src/myrepo--feature--abc123");
    assert!(
        git_ok(
            &repo,
            &[
                "worktree",
                "add",
                worktree.to_str().unwrap(),
                "-b",
                "feature",
            ],
        ),
        "git worktree add"
    );
    assert!(worktree.join(".git").is_file());

    // 3. A subdirectory of the plain checkout (`ainb run --repo <clone>/sub`).
    //    Combined with a STOPPED tmux session below, this is the exact
    //    combination that made sessions VANISH from the tree: the stopped-pass
    //    skip gate calls `get_source_repository`, which returned None for
    //    anything that was not a linked worktree, so the session was dropped
    //    with only a debug! line to show for it.
    let subdir = repo.join("services/api");
    std::fs::create_dir_all(&subdir).unwrap();

    // 4. Genuinely dead worktree: exists, inside no git repository at all.
    let dead = home_path.join("dead/dead-worktree");
    std::fs::create_dir_all(dead.join(".vite")).unwrap();

    // ── Seed the store: one LIVE session in the plain checkout, one STOPPED
    //    session in the linked worktree, one STOPPED session in a subdirectory
    //    of the plain checkout, one STOPPED session in the dead dir ─────────
    let live_tmux = format!("tmux_ainbfix-live-{}", std::process::id());
    let _ = Command::new("tmux").args(["kill-session", "-t", &live_tmux]).output();
    let created = Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &live_tmux,
            "-x",
            "80",
            "-y",
            "24",
            "sh",
        ])
        .status()
        .is_ok_and(|s| s.success());
    assert!(created, "failed to create tmux session {live_tmux}");
    let _guard = TmuxGuard(live_tmux.clone());

    let stopped_tmux = format!("tmux_ainbfix-stopped-{}", std::process::id());
    let stopped_sub_tmux = format!("tmux_ainbfix-stopped-sub-{}", std::process::id());
    let dead_tmux = format!("tmux_ainbfix-dead-{}", std::process::id());

    let mut store = SessionStore::default();
    let live_id = seed(&mut store, &live_tmux, &repo, "myrepo");
    let stopped_id = seed(&mut store, &stopped_tmux, &worktree, "myrepo");
    // `ainb run --repo <clone>/services/api` persists the --repo basename.
    let stopped_sub_id = seed(&mut store, &stopped_sub_tmux, &subdir, "api");
    let dead_id = seed(&mut store, &dead_tmux, &dead, "dead-worktree");
    store.save().expect("save sessions.json");

    // ── Drive the real loader ─────────────────────────────────────────────
    let mut state = AppState::new();
    state.load_real_workspaces().await;

    // 1. The plain-checkout session is named after its repository, NOT "(broken)".
    let (live_name, live_path) =
        workspace_of(&state, live_id).expect("live plain-checkout session must be visible");
    assert_eq!(
        live_name, "myrepo",
        "plain checkout must render its repo name, got {live_name:?}"
    );

    // 2. The STOPPED session in a linked worktree of the same repo is still
    //    visible AND lands in the SAME workspace row (no duplicate fabricated).
    let (stopped_name, stopped_path) =
        workspace_of(&state, stopped_id).expect("stopped linked-worktree session must be visible");
    assert_eq!(stopped_name, "myrepo");
    assert_eq!(
        stopped_path.canonicalize().unwrap(),
        live_path.canonicalize().unwrap(),
        "a plain checkout and a linked worktree of the same repo must share one workspace row"
    );
    // 3. STOPPED + rooted at a SUBDIRECTORY of the plain checkout: the
    //    combination that vanished. It must be visible AND land in the owning
    //    repository's row, named after the repository rather than the subdir
    //    basename that `ainb run` persisted.
    let (sub_name, sub_path) = workspace_of(&state, stopped_sub_id)
        .expect("stopped session in a subdirectory of a plain checkout must be visible");
    assert_eq!(
        sub_name, "myrepo",
        "a subdirectory session belongs to the owning repository, got {sub_name:?}"
    );
    assert_eq!(
        sub_path.canonicalize().unwrap(),
        live_path.canonicalize().unwrap(),
        "a subdirectory of the checkout must join the repository's row, not fabricate one"
    );

    assert_eq!(
        state.workspaces.iter().filter(|w| w.name == "myrepo").count(),
        1,
        "exactly one workspace row for the repo"
    );

    // 4. The genuinely dead worktree is still dropped from the tree (it is
    //    surfaced through /recover-sessions instead). This is what stops the
    //    test passing by simply deleting the sentinel.
    assert!(
        workspace_of(&state, dead_id).is_none(),
        "a worktree inside no git repository must not be rendered as a workspace"
    );
    assert!(
        !state.workspaces.iter().any(|w| w.name == "(broken)"),
        "no (broken) workspace should exist: {:?}",
        state.workspaces.iter().map(|w| w.name.clone()).collect::<Vec<_>>()
    );

    // ── User-visible proof: render the real session list ──────────────────
    let mut list = SessionListComponent::new();
    let mut term = Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
    term.draw(|f| list.render(f, f.area(), &mut state)).expect("draw");
    let painted: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    assert!(
        painted.contains("myrepo"),
        "workspace header should paint the repo name:\n{painted}"
    );
    assert!(
        !painted.contains("(broken)"),
        "workspace header must not paint (broken):\n{painted}"
    );

    std::env::remove_var("AINB_HOME");
}
