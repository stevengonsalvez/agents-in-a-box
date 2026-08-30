//! Tripwire: the registry-driven Settings screen, end to end in real tmux.
//!
//! The settings rows now come from `CONFIG_REGISTRY` rather than a hand-written
//! list, which is only worth anything if a user can actually reach a leaf and
//! change it. This drives the real `ainb` binary and exercises the whole loop:
//!
//!   `o` → Configuration screen (row count in the title proves registry rows,
//!         not the old ~24 hand-written ones)
//!     → `j` down the tree to Container Templates
//!     → `Space` expands it; per-template child nodes appear that were NOT
//!       there before (the tree mirrors the TOML sections)
//!     → `/` opens the filter; typing a dotted key finds a leaf in a section
//!       the tree is not even on
//!     → `Enter` edits the match, the new value is typed and confirmed
//!     → the row re-renders with the new value
//!     → `Esc` `Esc` returns to the HomeScreen (the return path, per the
//!       tmux-ui-tripwire skill's forward+return rule)
//!     → the seeded config.toml carries the NEW value and no longer the old one
//!
//! Per the tmux-ui-tripwire skill:
//! - Skips gracefully if tmux isn't on PATH.
//! - Runs with `AINB_DISABLE_PLUGINS=1`: this is the host's own code path, and
//!   it must not depend on a staged plugin tree to be testable.
//! - Seeds an isolated HOME with `onboarding.toml` (skip the first-run wizard)
//!   and a dismissed notify-install prompt, so neither modal eats a keystroke.
//! - Navigates by POLLING FOR THE PANE TITLE rather than counting `j` presses,
//!   so adding a category cannot silently retarget the test at another section.
//! - Asserts the exact rendered row (`<key> : <value>`), never a substring-OR:
//!   `capture.contains("450")` alone would pass off any digit anywhere.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The row the test edits, and its seeded / edited values. `idle_grace_secs`
/// is a `Number` row with a declared range, so the edit also exercises the
/// registry's validation on the way to disk.
const ROW_KEY: &str = "mcp_pool.idle_grace_secs";
const SEED_SECS: &str = "300";
const EDITED_SECS: &str = "450";

/// The Configuration screen title token (host `config_screen.rs`).
const CONFIG_TITLE: &str = "Configuration";

/// The category whose tree node must expand into per-template children.
const TREE_CATEGORY: &str = "Container Templates";

/// A child node the expansion must reveal: the `claude-dev` template's node,
/// title-cased by the tree. Distinct from the lower-case `claude-dev` that
/// appears as a *value* in the right-hand pane, so this token can only come
/// from the tree.
const TREE_CHILD: &str = "Claude-dev";

/// A HomeScreen marker (mirrors the other tripwires).
const HOME_MARKER: &str = "Stats";

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

/// Seed an isolated HOME: skip the first-run wizard, dismiss the notify prompt,
/// and pre-write the row this test edits so the pre-edit render is
/// deterministic AND the file the final assertion inspects already has it.
fn seed_isolated_home(home: &Path) -> PathBuf {
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

    let config_path = cfg.join("config.toml");
    fs::write(
        &config_path,
        format!("[mcp_pool]\nenabled = true\nidle_grace_secs = {SEED_SECS}\n"),
    )
    .expect("seed config.toml");

    let paths = ainb_plugin_notifyd::Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create agents-in-a-box base");
    ainb_plugin_notifyd::dismiss_prompt(&paths).expect("seed dismissed notify prompt");

    config_path
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

/// Re-sends `key` on each iteration until `ok` holds. Only safe for idempotent
/// keys — used here for `o` (a no-op once the screen is open).
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

fn type_literal(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("tmux type literal");
}

fn kill(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

/// The rendered row for `key`, with the padding collapsed: `"<key> : <value>"`.
/// Returns `None` when no line mentions the key.
///
/// Asserting on this rather than on `capture.contains(value)` is the difference
/// between "the edit landed on this row" and "the number 450 appears somewhere
/// on a 200-column screen".
fn rendered_row(capture: &str, key: &str) -> Option<String> {
    let line = capture.lines().find(|line| line.contains(key))?;
    // Start AT the key: the same terminal row also carries the left pane's tree
    // and both panes' borders, none of which belong to this assertion.
    let start = line.find(key)?;
    let segment = line[start..].trim_end_matches(['│', ' ']);
    Some(segment.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Step the tree down until the right-hand pane's title is `label`, which is
/// how the screen names the SELECTED node. Index-free, so inserting a category
/// cannot silently point this test at a different section.
fn step_to_node(session: &str, label: &str) -> Option<String> {
    let wanted = format!("╭ {label} ");
    for _ in 0..30 {
        let cap = capture_pane(session);
        if cap.contains(&wanted) {
            return Some(cap);
        }
        send_key(session, "j");
        thread::sleep(Duration::from_millis(150));
    }
    None
}

#[test]
fn settings_tree_search_and_edit_reach_config_toml() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let config_path = seed_isolated_home(home_tmp.path());

    let session = format!("tripwire-config-registry-{}", std::process::id());
    let status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "-x", "200", "-y", "50"])
        .status()
        .expect("tmux new-session");
    assert!(status.success(), "tmux new-session failed");

    let cmd = format!(
        "HOME={} AINB_DISABLE_PLUGINS=1 exec {} tui",
        home_tmp.path().display(),
        ainb_bin().display()
    );
    Command::new("tmux")
        .args(["send-keys", "-t", &session, &cmd, "Enter"])
        .status()
        .expect("tmux send launch cmd");

    let home_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture(&session, home_deadline, |c| {
        c.contains(HOME_MARKER) && c.contains("[i]")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill(&session);
        panic!("HomeScreen never rendered; last capture:\n---\n{last}\n---");
    }

    // 1. Open Settings. The title carries the row count, which is the cheapest
    //    proof the rows came from the registry: the old hand-written list could
    //    not reach three figures.
    let open_deadline = Instant::now() + Duration::from_secs(30);
    let Some(opened) =
        poll_capture_resending(&session, "o", open_deadline, |c| c.contains(CONFIG_TITLE))
    else {
        let last = capture_pane(&session);
        kill(&session);
        panic!("Settings never opened; last:\n---\n{last}\n---");
    };
    let row_count = settings_count(&opened).unwrap_or_else(|| {
        kill(&session);
        panic!("title did not report a row count:\n---\n{opened}\n---");
    });
    assert!(
        row_count > 100,
        "expected the registry's ~150 rows, got {row_count}; the screen is still on a \
         hand-written list:\n---\n{opened}\n---"
    );

    // 2. The tree expands. Before pressing Space the child nodes must NOT be on
    //    screen, or "expanding worked" would be unfalsifiable.
    let Some(before_expand) = step_to_node(&session, TREE_CATEGORY) else {
        let last = capture_pane(&session);
        kill(&session);
        panic!("never reached the {TREE_CATEGORY} node; last:\n---\n{last}\n---");
    };
    assert!(
        !before_expand.contains(TREE_CHILD),
        "the tree was already expanded, so this test cannot prove Space expands \
         it:\n---\n{before_expand}\n---"
    );

    send_key(&session, "Space");
    let expand_deadline = Instant::now() + Duration::from_secs(15);
    let expanded = poll_capture(&session, expand_deadline, |c| c.contains(TREE_CHILD));
    if expanded.is_none() {
        let last = capture_pane(&session);
        kill(&session);
        panic!("Space did not expand {TREE_CATEGORY} into {TREE_CHILD}; last:\n---\n{last}\n---");
    }

    // 3. `/` finds a leaf in a section the tree is not on. The filter is the
    //    whole reason a 150-row tree is usable, so it has to reach across.
    send_key(&session, "/");
    thread::sleep(Duration::from_millis(300));
    type_literal(&session, "idle_grace_secs");
    let search_deadline = Instant::now() + Duration::from_secs(15);
    let Some(matched) = poll_capture(&session, search_deadline, |c| c.contains(ROW_KEY)) else {
        let last = capture_pane(&session);
        kill(&session);
        panic!("`/` search did not surface {ROW_KEY}; last:\n---\n{last}\n---");
    };
    assert_eq!(
        rendered_row(&matched, ROW_KEY).as_deref(),
        Some(format!("{ROW_KEY} : {SEED_SECS}").as_str()),
        "the filtered row did not render its seeded value:\n---\n{matched}\n---"
    );

    // 4. Edit it. The popup pre-fills with the current value; clear and retype.
    send_key(&session, "Enter");
    let popup_deadline = Instant::now() + Duration::from_secs(15);
    if poll_capture(&session, popup_deadline, |c| {
        c.contains("Enter save | Esc cancel")
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill(&session);
        panic!("the edit popup never mounted; last:\n---\n{last}\n---");
    }
    for _ in 0..(SEED_SECS.len() + 4) {
        send_key(&session, "BSpace");
    }
    type_literal(&session, EDITED_SECS);
    thread::sleep(Duration::from_millis(200));
    send_key(&session, "Enter");

    // 5. The row re-renders with the new value — exact `key : value`, so a
    //    stray "450" elsewhere on screen cannot pass this.
    let edited_deadline = Instant::now() + Duration::from_secs(15);
    let edited = poll_capture(&session, edited_deadline, |c| {
        rendered_row(c, ROW_KEY).as_deref() == Some(format!("{ROW_KEY} : {EDITED_SECS}").as_str())
    });
    if edited.is_none() {
        let last = capture_pane(&session);
        kill(&session);
        panic!(
            "the edited row did not re-render as '{ROW_KEY} : {EDITED_SECS}'; \
             last:\n---\n{last}\n---"
        );
    }

    // 6. Return path: Esc clears the filter, Esc leaves Settings. A forward-only
    //    test passes while the way back is broken.
    send_key(&session, "Escape");
    thread::sleep(Duration::from_millis(400));
    send_key(&session, "Escape");
    let back_deadline = Instant::now() + Duration::from_secs(15);
    let back = poll_capture(&session, back_deadline, |c| {
        c.contains(HOME_MARKER) && !c.contains(CONFIG_TITLE)
    });
    let last = capture_pane(&session);
    kill(&session);
    assert!(
        back.is_some(),
        "Esc did not return to the HomeScreen; last:\n---\n{last}\n---"
    );

    // 7. The edit is on disk, and the old value is gone — a save that appended
    //    rather than replaced would leave both.
    let on_disk = fs::read_to_string(&config_path).expect("read seeded config.toml");
    assert!(
        on_disk.contains(&format!("idle_grace_secs = {EDITED_SECS}")),
        "config.toml did not persist the edit; contents:\n---\n{on_disk}\n---"
    );
    assert!(
        !on_disk.contains(&format!("idle_grace_secs = {SEED_SECS}")),
        "config.toml still carries the old value; contents:\n---\n{on_disk}\n---"
    );
}

/// Parse the `(N settings)` badge out of the Configuration title bar.
fn settings_count(capture: &str) -> Option<usize> {
    let line = capture.lines().find(|line| line.contains(CONFIG_TITLE))?;
    let start = line.find('(')? + 1;
    let rest = &line[start..];
    let end = rest.find(" settings")?;
    rest[..end].trim().parse().ok()
}
