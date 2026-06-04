//! Tripwire (TDD plan §P6): the `learnings` Detail pane opens on `Enter` and
//! renders the selected record's body, entities, typed relationships, and
//! provenance; `Backspace` returns to the Browse list.
//!
//! PRIMARY user-visible gate for P6 (TUI-only — no CLI parity leg). Drives the
//! real `ainb` binary in a detached tmux session over the committed fixture KB
//! (`crates/ainb-plugin-learnings/tests/fixtures/kb`):
//!
//!   open learnings (`m`) → land on the first row (`lrn-audit-after-rebase`)
//!     → `Enter` opens Detail → assert a body token + a provenance token + an
//!     entity name → `Backspace` returns to the Browse list.
//!
//! CLOSE KEY = `Backspace`, NOT `Esc`. The host reserves `Esc` — it pops the
//! whole plugin screen back to home and never forwards Esc to the plugin (see
//! `ainb-core/src/app/screens/builtin.rs::is_host_reserved_key`). Burndown's
//! exemplar binds its in-plugin "pop" to `Backspace` for exactly this reason;
//! the Detail pane follows suit. A real Esc here would navigate to home, not
//! back to the list — so the close step presses `Backspace`.
//!
//! Setup mirrors `tripwire_learnings_browse.rs`:
//! - Skips if tmux isn't on PATH or `dist/plugins/learnings` isn't staged
//!   (re-signed for macOS AMFI — `scripts/build-plugins.sh`).
//! - Seeds an isolated HOME with `onboarding.toml` (skip first-run wizard), a
//!   dismissed notify-install prompt, and `config.toml`
//!   `[plugins.learnings].learnings_dir` → the fixture KB.
//! - Asserts EXACT unique tokens, never a substring-OR. Each token was locked
//!   by a deliberately-wrong run reading the real render first.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The fixture learning id the Browse list renders (sorted first by id), and
/// the record the Detail pane opens for. Re-asserted on return-to-list.
const FIXTURE_ID: &str = "lrn-audit-after-rebase";

/// The title token the learnings screen paints (P3 contract — re-asserted so a
/// P6 refactor dropping it is caught here too).
const TITLE_TOKEN: &str = "🧠 Learnings";

/// A body token from the opened record's markdown body. Only present once the
/// Detail pane renders `body_md`. Exact, unique — locked by a wrong-value run.
const DETAIL_BODY_TOKEN: &str = "## Solution";

/// A provenance token: the source_path basename. Extremely unique; present
/// only in the Detail provenance line.
const DETAIL_PROVENANCE_TOKEN: &str = "feedback_audit_after_rebase.md";

/// An entity name from the `.entities.yaml` sidecar — proves the Detail
/// entities list rendered. Not a substring of any learning id.
const DETAIL_ENTITY_TOKEN: &str = "git pull --rebase";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Absolute path to the committed learnings fixture KB.
fn fixture_kb_dir() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir
            .join("crates")
            .join("ainb-plugin-learnings")
            .join("tests")
            .join("fixtures")
            .join("kb");
        if candidate.join("lrn-audit-after-rebase.md").exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Walk up from the ainb binary looking for `dist/plugins/learnings/`.
fn plugins_staged() -> Option<PathBuf> {
    let bin = ainb_bin();
    let mut dir = bin.parent()?;
    for _ in 0..6 {
        let candidate = dir.join("dist").join("plugins");
        if candidate.join("learnings").join("learnings").exists() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    None
}

/// Seed an isolated HOME so the first keystroke lands on the HomeScreen, and
/// point the learnings plugin's `learnings_dir` at the fixture KB.
fn seed_isolated_home(home: &Path, kb_dir: &Path) {
    let cfg = home.join(".agents-in-a-box").join("config");
    fs::create_dir_all(&cfg).expect("create config dir");

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

    let config_toml = format!(
        "[plugins.learnings]\nlearnings_dir = {dir:?}\n",
        dir = kb_dir.display().to_string(),
    );
    fs::write(cfg.join("config.toml"), config_toml).expect("seed config.toml");

    let paths = ainb_plugin_notifyd::Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create agents-in-a-box base");
    ainb_plugin_notifyd::dismiss_prompt(&paths).expect("seed dismissed notify prompt");
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

/// Like [`poll_capture`], but re-sends `key` on each iteration until `ok`
/// holds. Re-pressing is safe for the screen-open step because `m` is
/// idempotent once the screen is open (a no-op nav). See
/// `tripwire_learnings_browse.rs` for the full rationale.
fn poll_capture_resending<F>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    send_key(session, key);
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(400));
        send_key(session, key);
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
fn learnings_detail_opens_on_enter_and_backspace_returns() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let Some(plugin_root) = plugins_staged() else {
        eprintln!("SKIP: dist/plugins/learnings not staged — run `scripts/build-plugins.sh` first");
        return;
    };
    let Some(kb_dir) = fixture_kb_dir() else {
        eprintln!("SKIP: fixture KB not found");
        return;
    };

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    seed_isolated_home(home_tmp.path(), &kb_dir);

    let session = format!("tripwire-learnings-detail-{}", std::process::id());
    let ainb = ainb_bin();

    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} AINB_PLUGIN_ROOT={} exec {} tui",
        home_tmp.path().display(),
        plugin_root.display(),
        ainb.display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");

    // Wait for HomeScreen.
    let home_deadline = Instant::now() + Duration::from_secs(45);
    let pre_home = poll_capture(&session, home_deadline, |c| {
        c.contains("Stats") && c.contains("[i]")
    });
    if pre_home.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // Open learnings (`m`), re-pressing each poll until the fixture id row
    // paints (idempotent once open).
    let open_deadline = Instant::now() + Duration::from_secs(45);
    let opened = poll_capture_resending(&session, "m", open_deadline, |c| {
        c.contains(TITLE_TOKEN) && c.contains(FIXTURE_ID)
    });
    if opened.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "learnings Browse never painted the fixture id {FIXTURE_ID:?}; last:\n---\n{last}\n---"
        );
    }

    // Press Enter on the selected (first) row → open the Detail pane. Poll for
    // ALL three exact tokens (body + provenance + entity) so a partial render
    // can't pass.
    send_key(&session, "Enter");
    let detail_deadline = Instant::now() + Duration::from_secs(20);
    let detail = poll_capture(&session, detail_deadline, |c| {
        c.contains(DETAIL_BODY_TOKEN)
            && c.contains(DETAIL_PROVENANCE_TOKEN)
            && c.contains(DETAIL_ENTITY_TOKEN)
    });
    let Some(detail_cap) = detail else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "Detail pane did not render all of body {DETAIL_BODY_TOKEN:?} / provenance \
             {DETAIL_PROVENANCE_TOKEN:?} / entity {DETAIL_ENTITY_TOKEN:?} after Enter; \
             last:\n---\n{last}\n---"
        );
    };

    // Backspace (NOT Esc — host-reserved) closes Detail → back to the Browse
    // list: the body token is gone and the fixture id row is back.
    send_key(&session, "BSpace");
    let back_deadline = Instant::now() + Duration::from_secs(15);
    let back = poll_capture(&session, back_deadline, |c| {
        !c.contains(DETAIL_BODY_TOKEN) && c.contains(FIXTURE_ID) && c.contains(TITLE_TOKEN)
    });

    kill_session(&session);

    // Assertions on the captured frames (exact, unique tokens).
    assert!(
        detail_cap.contains(DETAIL_BODY_TOKEN),
        "Detail did not render the body token {DETAIL_BODY_TOKEN:?}:\n---\n{detail_cap}\n---"
    );
    assert!(
        detail_cap.contains(DETAIL_PROVENANCE_TOKEN),
        "Detail did not render the provenance token {DETAIL_PROVENANCE_TOKEN:?}:\n---\n{detail_cap}\n---"
    );
    assert!(
        detail_cap.contains(DETAIL_ENTITY_TOKEN),
        "Detail did not render the entity token {DETAIL_ENTITY_TOKEN:?}:\n---\n{detail_cap}\n---"
    );
    assert!(
        back.is_some(),
        "Backspace from Detail did not return to the Browse list (body token swallowed / \
         list not restored); last capture:\n---\n{}\n---",
        capture_pane(&session)
    );
}
