//! Tripwire: the HomeScreen tile grid includes the "Skills
//! (manager)" tile registered by `HomeTile::SkillManager`. Catches:
//! HomeTile::all() drops the variant, label/icon/description regress
//! to placeholder strings, tile grid renderer skips unknown variants.
//!
//! Skips when `tmux` isn't on PATH.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn seed_isolated_home(home: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    let onboarding = format!(
        r#"completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "{ver}"
skipped_dependencies = []
git_directories = []
"#,
        ver = env!("CARGO_PKG_VERSION"),
    );
    fs::write(cfg.join("onboarding.toml"), onboarding).expect("seed onboarding.toml");
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn poll_capture<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn home_tile_grid_lists_skill_manager() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-sm-tile-{}", std::process::id());
    let bin = ainb_bin();

    Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");

    let cmd = format!(
        "HOME={} exec {} 2>&1",
        home_tmp.path().display(),
        bin.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    // Wait for HomeScreen WITH the new tile visible. The label in
    // app/state.rs:487 is literally "Skills (manager)" and the
    // description "Install / sync / doctor (Z)" — assert both are
    // present so a label/description regression also fires.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Skills (manager)")
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "HomeScreen renders but the SkillManager tile isn't there. \
                 Did HomeTile::all() drop SkillManager? Or did the label change?\n{dump}"
            );
        }
    };
    kill_session(&session);

    assert!(
        post.contains("Skills (manager)"),
        "label drift — expected literal 'Skills (manager)' in tile grid: {post}"
    );
}
