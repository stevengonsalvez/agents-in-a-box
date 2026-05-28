//! Tripwire (finding #2): a non-favorite git repo under a configured
//! `workspace_scan_paths` entry surfaces as a 📁 row in the unified picker.
//!
//! Pre-fix the dispatcher called `PickRepoState::from_disk_no_locals()` so
//! 📁 rows were unreachable in production despite `RowKind::Local` being a
//! documented row kind. Fix: wire `WorkspaceScanner` into
//! `AppEvent::NewSession` using `workspace_defaults`.
//!
//! Phase 6 follow-up of `plans/new-session-redesign-spec.md`.

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

fn seed_config_with_scan_path(home: &Path, scan_dir: &Path) {
    let cfg_dir = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg_dir).unwrap();
    let toml_src = format!(
        r#"[workspace_defaults]
workspace_scan_paths = ["{}"]
"#,
        scan_dir.display()
    );
    fs::write(cfg_dir.join("config.toml"), toml_src).unwrap();
}

#[test]
fn local_scan_path_surfaces_repo_as_folder_row() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    if !git_available() {
        eprintln!("SKIP: git not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    // Empty favorites so the only candidate is a 📁 local row.
    let root = home_tmp.path().join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("favorites.yaml"),
        "version: 1\nfavorites: []\nsettings:\n  auto_promote_threshold: 5\n",
    )
    .unwrap();
    // Seed a real git repo under a workspace-scan path.
    let scan_dir = home_tmp.path().join("scanned");
    let repo = seed_local_git_repo_at(&scan_dir, "scanned-repo");
    seed_config_with_scan_path(home_tmp.path(), &scan_dir);

    let session = format!("tripwire-local-folder-{}", std::process::id());
    let ainb = ainb_bin();
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_tmp.path().display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux launch");

    let home_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture(&session, home_deadline, |c| c.contains("Stats") && c.contains("[i]")).is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last:\n---\n{last}\n---");
    }

    send_key(&session, "n");
    // 📁 = U+1F4C1; require it to appear. Retry once on dropped keystroke.
    let pick_deadline = Instant::now() + Duration::from_secs(15);
    let mut on_pick = poll_capture(&session, pick_deadline, |c| {
        c.contains("Enter=Select") && c.contains('\u{1f4c1}')
    });
    if on_pick.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(15);
        on_pick = poll_capture(&session, retry_deadline, |c| {
            c.contains("Enter=Select") && c.contains('\u{1f4c1}')
        });
    }
    let last = capture(&session);
    kill_session(&session);
    let _ = repo; // silence unused
    assert!(
        on_pick.is_some(),
        "📁 local-scan row never surfaced (finding #2 regressed). \
         Last:\n---\n{last}\n---"
    );
}

fn seed_local_git_repo_at(parent: &Path, name: &str) -> std::path::PathBuf {
    let repo = parent.join(name);
    fs::create_dir_all(&repo).unwrap();
    let status = Command::new("git")
        .args(["-c", "init.defaultBranch=main", "init"])
        .current_dir(&repo)
        .status()
        .expect("git init");
    assert!(status.success(), "git init failed");
    fs::write(repo.join("README.md"), "seeded\n").unwrap();
    let _ = Command::new("git")
        .args([
            "-c", "user.email=trip@example.com", "-c", "user.name=trip", "add", "README.md",
        ])
        .current_dir(&repo)
        .status();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=trip@example.com",
            "-c",
            "user.name=trip",
            "commit",
            "-m",
            "seed",
        ])
        .current_dir(&repo)
        .status();
    repo
}
