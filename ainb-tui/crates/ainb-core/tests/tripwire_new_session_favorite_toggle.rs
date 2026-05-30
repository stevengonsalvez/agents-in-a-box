//! Tripwire: `^F` on the highlighted row toggles its favorite status.
//! Pressing `^F` on a non-favorite row adds it (★ marker appears AND
//! `favorites.yaml` gains the entry); pressing `^F` a second time
//! removes it.
//!
//! Phase 4 of `plans/new-session-redesign-spec.md`.


#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// Tripwire-specific seed: an empty favorites.yaml + a `session-defaults.yaml`
/// containing a single `my-recent-repo` recent. This is the row we toggle.
fn seed_fav_toggle_fixtures(home: &Path) {
    let root = home.join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("favorites.yaml"),
        "version: 1\nfavorites: []\nsettings:\n  auto_promote_threshold: 5\n",
    )
    .unwrap();
    let defaults_yaml = r#"last_repo: my-recent-repo
per_repo:
  my-recent-repo:
    last_preset: claude-interactive-yolo
    last_branch_override: null
    last_prompt: null
    last_used_at: "2026-05-01T00:00:00Z"
    source_type: github_shorthand
    source: stevengonsalvez/my-recent-repo
"#;
    fs::write(root.join("session-defaults.yaml"), defaults_yaml).unwrap();
}

#[test]
fn ctrl_f_toggles_favorite_on_and_off() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let home_path = home_tmp.path().to_path_buf();
    seed_isolated_home(&home_path);
    seed_fav_toggle_fixtures(&home_path);

    let session = format!("tripwire-fav-toggle-{}", std::process::id());
    let ainb = ainb_bin();

    Command::new("tmux")
        .args([
            "new-session",
            "-d",
            "-s",
            &session,
            "-x",
            "180",
            "-y",
            "50",
        ])
        .status()
        .expect("tmux new-session");

    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_path.display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux launch");

    // Wait for home.
    let home_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last:\n---\n{last}\n---");
    }

    // Open PickRepo. Row 0 is the "recent" entry (no star marker yet).
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(5);
    let mut on_pick = poll_capture(&session, pick_deadline, |c| {
        c.contains("New Session") && c.contains("my-recent-repo")
    });
    if on_pick.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        on_pick = poll_capture(&session, retry_deadline, |c| {
            c.contains("New Session") && c.contains("my-recent-repo")
        });
    }
    let Some(pre_cap) = on_pick else {
        let last = capture(&session);
        kill_session(&session);
        panic!("PickRepo never opened; last:\n---\n{last}\n---");
    };

    // Negative pre-check: NO star next to my-recent-repo before ^F.
    // The recents row marker is ⌚ (U+231A); we accept either the row
    // line still containing ⌚ OR simply the absence of ★ on the row.
    let line_has_star_pre = pre_cap.lines().any(|line| {
        line.contains("my-recent-repo") && line.contains('\u{2605}')
    });
    assert!(
        !line_has_star_pre,
        "star marker present BEFORE ^F — fixture wrong:\n---\n{pre_cap}\n---"
    );

    // Press ^F to promote to favorite.
    send_key(&session, "C-f");

    // After ^F: star appears on the my-recent-repo line, favorites.yaml
    // has a new entry.
    let post_deadline = Instant::now() + Duration::from_secs(5);
    let post_cap = poll_capture(&session, post_deadline, |c| {
        c.lines()
            .any(|line| line.contains("my-recent-repo") && line.contains('\u{2605}'))
    });

    let favorites_path = home_path.join(".agents-in-a-box").join("favorites.yaml");
    let yaml_deadline = Instant::now() + Duration::from_secs(5);
    let mut yaml_with = String::new();
    while Instant::now() < yaml_deadline {
        if let Ok(s) = fs::read_to_string(&favorites_path) {
            if s.contains("my-recent-repo") && s.contains("alias") {
                yaml_with = s;
                break;
            }
        }
        thread::sleep(Duration::from_millis(150));
    }

    assert!(
        post_cap.is_some(),
        "star marker did NOT appear after ^F. Capture:\n---\n{}\n---",
        capture(&session)
    );
    assert!(
        yaml_with.contains("my-recent-repo"),
        "favorites.yaml not updated after ^F. YAML:\n{yaml_with:?}"
    );

    // Press ^F again — favorite removed. Star marker disappears AND the
    // entry is gone from yaml.
    send_key(&session, "C-f");

    let untoggle_deadline = Instant::now() + Duration::from_secs(5);
    let untoggle_cap = poll_capture(&session, untoggle_deadline, |c| {
        c.lines()
            .all(|line| !(line.contains("my-recent-repo") && line.contains('\u{2605}')))
    });

    let yaml_deadline2 = Instant::now() + Duration::from_secs(5);
    let mut yaml_after = String::new();
    while Instant::now() < yaml_deadline2 {
        if let Ok(s) = fs::read_to_string(&favorites_path) {
            // After untoggle: my-recent-repo NOT in favorites list.
            // YAML may still have empty `favorites: []`.
            if !s.contains("my-recent-repo") {
                yaml_after = s;
                break;
            }
        }
        thread::sleep(Duration::from_millis(150));
    }

    let final_cap = capture(&session);
    kill_session(&session);

    assert!(
        untoggle_cap.is_some(),
        "star marker still present after second ^F. Final:\n---\n{final_cap}\n---"
    );
    assert!(
        !yaml_after.contains("alias: my-recent-repo"),
        "favorites.yaml still has my-recent-repo after untoggle:\n{yaml_after:?}"
    );
}
