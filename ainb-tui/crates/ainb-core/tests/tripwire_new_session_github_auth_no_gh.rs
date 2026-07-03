//! Tripwire: the GitHub auth pre-check prompt appears (and exits gracefully)
//! when `gh` is logged out — the credential prompt never hangs the TUI.
//!
//! Regression guard for PR #155. Before the pre-check, selecting a remote
//! GitHub repo with no `gh` auth handed straight to `git clone`, whose
//! interactive `Username for 'https://github.com':` prompt blocked on stdin
//! that crossterm owns in raw mode — the whole TUI froze with no way out.
//!
//! This drives the real binary with a stubbed, logged-out `gh` (a fake `gh`
//! on PATH that exits non-zero, exactly like `gh auth status` when signed
//! out) and asserts:
//!   1. pressing Enter on the GitHub favorite row surfaces the inline amber
//!      "GitHub auth required" prompt with the `gh auth login` remediation,
//!   2. the pane NEVER shows a git credential prompt (`Username for` /
//!      `Password for`) — i.e. it did not fall through to `git clone`,
//!   3. pressing Esc dismisses the prompt and returns to the picker
//!      (graceful exit, per the return-path rule in the tmux-ui-tripwire
//!      skill).

#[allow(dead_code)]
mod tripwire_new_session_common;
use tripwire_new_session_common::*;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Write a fake `gh` onto an isolated bin dir that behaves like a logged-out
/// CLI: `gh auth status …` exits 1 (and prints the real CLI's "not logged
/// into" line to stderr, which the app discards). Returns the bin dir to
/// prepend onto PATH.
fn seed_logged_out_gh(home: &Path) -> std::path::PathBuf {
    let bin = home.join("fakebin");
    fs::create_dir_all(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(
        &gh,
        "#!/bin/sh\n\
         # Stub: simulates `gh` installed but signed out.\n\
         echo 'You are not logged into any GitHub hosts. To log in, run: gh auth login' 1>&2\n\
         exit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// Suppress the first-run "install ainb-hooks?" nudge that otherwise opens a
/// modal on the Home screen and swallows the `n` keystroke. Mirrors what
/// `notifyd::dismiss_prompt` writes: an empty install record with
/// `prompt_dismissed = true` at `~/.agents-in-a-box/install.json`.
fn seed_dismiss_notify_prompt(home: &Path) {
    let root = home.join(".agents-in-a-box");
    fs::create_dir_all(&root).unwrap();
    let install_json = r#"{
  "agents": [],
  "hook_script": "",
  "claude_plugin_dir": null,
  "codex_hooks_json": null,
  "plugin_version": null,
  "prompt_dismissed": true
}
"#;
    fs::write(root.join("install.json"), install_json).unwrap();
}

#[test]
fn github_auth_prompt_shows_and_exits_without_credential_hang() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    // Seeds an `ainb-tui` GithubShorthand favorite — the row we Enter on.
    seed_new_session_fixtures(home_tmp.path());
    // Skip the first-run notify-install modal that would eat the `n` key.
    seed_dismiss_notify_prompt(home_tmp.path());
    let fakebin = seed_logged_out_gh(home_tmp.path());

    let session = format!("tripwire-gh-auth-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "180", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success());

    // Prepend the stub bin so `gh` resolves to our logged-out fake. `exec`
    // replaces the shell so keystrokes hit ainb directly; `$PATH` expands in
    // the launch shell before exec.
    let cmd = format!(
        "HOME={home} PATH={fake}:$PATH AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home_tmp.path().display(),
        fake = fakebin.display(),
        bin = ainb.display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux launch");

    // Wait for HomeScreen.
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

    // Open PickRepo (retry once if `n` is dropped under load).
    send_key(&session, "n");
    let pick_deadline = Instant::now() + Duration::from_secs(5);
    let mut on_pick = poll_capture(&session, pick_deadline, |c| {
        c.contains("New Session") && c.contains("Enter=Select")
    });
    if on_pick.is_none() {
        send_key(&session, "n");
        let retry_deadline = Instant::now() + Duration::from_secs(8);
        on_pick = poll_capture(&session, retry_deadline, |c| {
            c.contains("New Session") && c.contains("Enter=Select")
        });
    }
    let pre = on_pick.unwrap_or_else(|| {
        let last = capture(&session);
        kill_session(&session);
        panic!("PickRepo never opened after `n`; last:\n---\n{last}\n---");
    });
    // The favorite row is present and we are NOT yet showing an auth prompt.
    assert!(
        pre.contains("ainb-tui"),
        "favorite row missing on PickRepo:\n---\n{pre}\n---"
    );
    assert!(
        !pre.contains("auth required"),
        "auth prompt shown before Enter:\n---\n{pre}\n---"
    );

    // Enter on the highlighted GithubShorthand favorite → StartClone →
    // CheckGitAuth → (stub gh exits 1) → NotAuthenticated prompt.
    send_key(&session, "Enter");

    let prompt_deadline = Instant::now() + Duration::from_secs(20);
    let prompted = poll_capture(&session, prompt_deadline, |c| {
        c.contains("auth required") && c.contains("gh auth login")
    });
    let prompt_cap = prompted.unwrap_or_else(|| {
        let last = capture(&session);
        kill_session(&session);
        panic!(
            "inline GitHub auth prompt never rendered after Enter on the \
             GitHub favorite with a logged-out gh; last:\n---\n{last}\n---"
        );
    });

    // THE point of the PR: it must NOT have fallen through to `git clone`'s
    // interactive credential prompt. Those strings would only appear if git
    // were blocking on stdin — exactly the hang this feature prevents.
    assert!(
        !prompt_cap.contains("Username for") && !prompt_cap.contains("Password for"),
        "git credential prompt leaked into the TUI — the pre-check did not \
         intercept the clone:\n---\n{prompt_cap}\n---"
    );
    // The remediation + key hints are visible so the user knows how to recover.
    assert!(
        prompt_cap.contains("Retry")
            && prompt_cap.contains("Skip")
            && prompt_cap.contains("Dismiss"),
        "auth prompt key hints (Retry/Skip/Dismiss) missing:\n---\n{prompt_cap}\n---"
    );

    // Return path: Esc dismisses the prompt and lands back on the picker.
    send_key(&session, "Escape");
    let back_deadline = Instant::now() + Duration::from_secs(10);
    let back = poll_capture(&session, back_deadline, |c| {
        c.contains("Enter=Select") && !c.contains("auth required")
    });

    let final_cap = back.clone().unwrap_or_else(|| capture(&session));
    kill_session(&session);

    assert!(
        back.is_some(),
        "Esc did not return to the picker (graceful exit) within 10s:\n---\n{final_cap}\n---"
    );
    assert!(
        !final_cap.contains("auth required"),
        "auth prompt still visible after Esc:\n---\n{final_cap}\n---"
    );
}
