//! Tripwire: drift glyph (⚠) renders in the Units panel of the live
//! ainb binary when the production GitLsRemoteBackend reports the
//! deployed SHA is behind the upstream tip.
//!
//! This is the live-binary equivalent of the MockBackend-driven
//! tripwire_core_skill_manager_drift_column.rs — that one proves the
//! glyph table is wired in isolation; this one proves the actual end-
//! to-end loop runs (background poll → GitLsRemoteBackend → real
//! `git ls-remote` → drift_cache update → re-render) against a real
//! bare local repo (file:// URI so we never touch the network).
//!
//! Catches: GitLsRemoteBackend env-propagation regression (the prior
//! GIT_TERMINAL_PROMPT freeze that landed in commit f34d851), drift
//! glyph table dropping the Outdated arm, detect_all source-lookup
//! regression on `git:` URIs, async-drift-poll → render race that
//! never repopulates drift_cache.
//!
//! Bead v12.1.T2 / agents-in-a-box-h6k. Pattern mirrors sibling live
//! tmux tripwires.
//!
//! Skips when `tmux` or `git` is unavailable on PATH.

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

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Seed an isolated `$HOME` so the first-run onboarding wizard does
/// not intercept our keystrokes.
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
    // Suppress the first-run ainb-hooks install nudge (intercepts the
    // first key on the home screen). See build_skill_manager_sandbox.
    fs::write(
        cfg.parent().unwrap().join("install.json"),
        "{\"agents\":[],\"hook_script\":\"\",\"prompt_dismissed\":true}\n",
    )
    .expect("seed install.json");
}

fn git(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "ainb-test")
        .env("GIT_AUTHOR_EMAIL", "ainb-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "ainb-test")
        .env("GIT_COMMITTER_EMAIL", "ainb-test@example.invalid")
        .output()
        .expect("git")
}

/// Build a real bare git repo with one commit on `main`. Returns the
/// path to the bare repo + the resolved tip SHA.
fn build_bare_repo_with_one_commit(scratch: &Path) -> (PathBuf, String) {
    let work = scratch.join("seed-work");
    let bare = scratch.join("seed-bare.git");

    fs::create_dir_all(&work).unwrap();
    git(&["init", "-b", "main"], &work);
    git(
        &["config", "user.email", "ainb-test@example.invalid"],
        &work,
    );
    git(&["config", "user.name", "ainb-test"], &work);
    fs::write(work.join("README.md"), "drift-seed\n").unwrap();
    git(&["add", "README.md"], &work);
    let out = git(&["commit", "-m", "drift-tripwire seed"], &work);
    assert!(out.status.success(), "seed commit failed: {out:?}");

    git(
        &[
            "clone",
            "--bare",
            work.to_str().unwrap(),
            bare.to_str().unwrap(),
        ],
        scratch,
    );

    let tip = git(&["rev-parse", "HEAD"], &work);
    let tip = String::from_utf8_lossy(&tip.stdout).trim().to_string();
    assert_eq!(tip.len(), 40, "tip SHA expected 40 chars, got `{tip}`");

    (bare, tip)
}

/// Seed manifest + lockfile so that:
///   - the source URI is `git:file:///<bare>` (passes verbatim through
///     `source_to_remote_url`; `git ls-remote` resolves the local file://
///     remote without hitting the network)
///   - the unit's `sha` is something OTHER than `tip` so detect_drift
///     returns Outdated → Units panel renders ⚠
fn seed_manifest_and_lockfile(home: &Path, bare: &Path, tip_sha: &str) {
    let ainb_home = home.join(".agents-in-a-box");
    fs::create_dir_all(&ainb_home).expect("create ainb_home");

    let bare_url = format!("file://{}", bare.display());
    let source_uri = format!("git:{bare_url}");
    let manifest_yaml = format!(
        r#"schema_version: 1
sources:
  - name: stevie-drift
    type: git
    uri: {source_uri}
    ref: main
    enabled: true
units:
  - uri: {source_uri}@main/skills/commit
    targets: [claude]
"#
    );
    fs::write(ainb_home.join("manifest.yaml"), manifest_yaml).expect("seed manifest.yaml");

    // Deployed SHA = a known-different value. We never pin it to the
    // real tip because then drift would be `InSync` and the test
    // wouldn't observe Outdated.
    let stale = "0000000000000000000000000000000000000000";
    assert_ne!(stale, tip_sha, "stale must differ from tip");

    let lock_yaml = format!(
        r#"schema_version: 1
generated_at: "2026-05-26T00:00:00+00:00"
sources: []
units:
  - uri: {source_uri}@{stale}/skills/commit
    declared_uri: {source_uri}@main/skills/commit
    kind: skill
    sha: {stale}
    deployed:
      claude:
        status: deployed
        path: /tmp/tripwire-drift-deployed/claude/skills/commit
        file_hashes:
          SKILL.md: deadbeef
"#
    );
    fs::write(ainb_home.join("lock.yaml"), lock_yaml).expect("seed lock.yaml");
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

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn pressing_m_renders_drift_warning_glyph_against_real_local_bare() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not on PATH");
        return;
    }
    if !git_available() {
        eprintln!("SKIP: git not on PATH");
        return;
    }

    let scratch = tempfile::tempdir().expect("scratch tempdir");
    let (bare, tip_sha) = build_bare_repo_with_one_commit(scratch.path());

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path());
    seed_manifest_and_lockfile(home_tmp.path(), &bare, &tip_sha);

    let session = format!("tripwire-sm-drift-{}", std::process::id());
    let bin = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    // Pin `AINB_HOME` explicitly alongside `HOME` rather than relying
    // on `default_ainb_home()`'s `HOME`-only fallback — every sibling
    // live tripwire (`SandboxLayout::env_vars()` in
    // ainb-skill-core::fixtures) does the same for exactly this
    // reason: `default_ainb_home()` prefers `$AINB_HOME`, then
    // `$XDG_CONFIG_HOME/agents-in-a-box`, and only falls back to
    // `$HOME/.agents-in-a-box` last. On a CI runner where either of
    // those higher-precedence vars is set in the job's ambient
    // environment, a bare `HOME=...` override silently loses the race
    // and the app reads an empty/real ainb home instead of the one we
    // just seeded — Sources/Units render empty and the ⚠ glyph never
    // appears, which is exactly the failure this test is guarding
    // against, just misattributed to the product instead of the
    // launch line. Also set `GIT_TERMINAL_PROMPT`/`GIT_ASKPASS`
    // belt-and-braces (matches `SandboxLayout::env_vars()`) so a
    // credential prompt can never freeze the pane.
    let ainb_home = home_tmp.path().join(".agents-in-a-box");
    let cmd = format!(
        "HOME={home} AINB_HOME={ainb_home} GIT_TERMINAL_PROMPT=0 GIT_ASKPASS=/bin/true exec {bin} 2>&1",
        home = home_tmp.path().display(),
        ainb_home = ainb_home.display(),
        bin = bin.display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send-keys launch");

    let home_render = poll_capture(&session, Instant::now() + Duration::from_secs(120), |c| {
        c.contains("Agents") && c.contains("Catalog") && c.contains("Skills (manager)")
    });
    if home_render.is_none() {
        let dump = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered. last capture:\n{dump}");
    }

    thread::sleep(Duration::from_millis(200));
    send_key(&session, "z");

    // Wait for the SkillManager screen to surface + the async drift
    // poll to update the Units panel with the ⚠ glyph. The async
    // pipeline is: GoToSkillManager → tokio::spawn(spawn_blocking
    // detect_all over Arc<dyn DriftBackend>) → mpsc::send → tick
    // drains channel → drift_cache populated → next render shows ⚠.
    //
    // The same screen also shows "Sources" + "Units" + "Detail" + the
    // source name — we wait on the full picture so a partial render
    // before the glyph paints doesn't false-pass.
    let post = poll_capture(&session, Instant::now() + Duration::from_secs(90), |c| {
        c.contains("Sources")
            && c.contains("Units")
            && c.contains("Detail")
            && c.contains("stevie-drift")
            && c.contains('⚠')
    });
    let post = match post {
        Some(p) => p,
        None => {
            let dump = capture_pane(&session);
            kill_session(&session);
            panic!(
                "SkillManager Units panel never rendered the ⚠ drift glyph against \
                 the real local bare repo (file://{bare}, tip={tip}). \n\
                 last capture:\n{dump}",
                bare = bare.display(),
                tip = tip_sha,
            );
        }
    };
    kill_session(&session);

    // Chrome — the screen is alive.
    assert!(post.contains("Sources"), "missing Sources panel: {post}");
    assert!(post.contains("Units"), "missing Units panel: {post}");
    // Drift glyph — the load-bearing assertion.
    assert!(
        post.contains('⚠'),
        "Units panel did not render the ⚠ Outdated drift glyph: {post}"
    );
    // The "in-sync" glyph (✓) must NOT appear in our single-unit
    // setup, otherwise the backend incorrectly reported InSync.
    // (Note: ✓ glyph could appear elsewhere in chrome — we don't
    // negative-assert it here because future cosmetic changes might
    // legitimately put a ✓ somewhere unrelated.)
}
