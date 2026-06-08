//! Tripwire: the `learnings` Graph tab's radial ego MAP sub-mode renders and
//! responds over the real `ainb` binary in a detached tmux session.
//!
//! PRIMARY real-TUI gate for the radial map. It opens the learnings screen
//! (`m`), reaches the Graph tab (`g`), toggles the map (`v`), and asserts the
//! deterministic tokens the map paints over the fixture KB:
//!
//! - the `centre: audit-after-rebase` header + the boxed centre `[audit-after-rebase]`,
//! - a boxed neighbour `[stale plan execution]` + its typed edge label `solves`,
//! - `h` flips the node count (`nodes:1` → `nodes:2`) by widening the hop,
//! - `⏎` recentres on the selected neighbour (the centre token CHANGES to
//!   `centre: stale plan execution`),
//! - `Backspace` exits the map back to the neighbourhood (`--solves-->` returns).
//!
//! Toggle/edge keys (`v`/`h`/`⏎`/`Backspace`) are sent ONCE then polled (not
//! re-sent) — re-pressing a toggle would flip it back and never converge.
//!
//! MOUSE recentre, `o`→Detail, and the `[+N more]`/`e` overflow are exercised by
//! the in-process `tests/map.rs` suite (which round-trips the identical
//! handle_mouse / handle_key code through the real SDK `Server`); tmux can't
//! inject synthetic clicks reliably, so this gate covers the keyboard path.
//!
//! Setup mirrors `tripwire_learnings_graph.rs`: skips without tmux / staged
//! plugins / fixture KB; seeds an isolated HOME; asserts EXACT unique tokens.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

const FIXTURE_ID: &str = "lrn-audit-after-rebase";
const TITLE_TOKEN: &str = "🧠 Learnings";
/// The TYPED edge token of the text neighbourhood — present before `v`, NEVER in
/// the map (whose label is the bare `solves`). Used to confirm `g` reached the
/// neighbourhood (with the sorted-first `audit-after-rebase` selected).
const NEIGHBOURHOOD_EDGE: &str = "--solves-->";

/// The neighbourhood help-bar marker (`c communities`). Present in the text
/// views, ABSENT from the map's help bar — and independent of which entity is
/// selected, so it confirms "back to the neighbourhood" after `Backspace` even
/// though the list then lands on the recentred entity (whose own edges differ).
const NEIGHBOURHOOD_MARKER: &str = "communities";

// --- Map tokens (locked by a wrong-value run reading the real render first) ---
const CENTRE_HEADER: &str = "centre: audit-after-rebase";
const CENTRE_BOX: &str = "[audit-after-rebase]";
const NEIGHBOUR_BOX: &str = "[stale plan execution]";
const EDGE_LABEL: &str = "solves";
const HOP2_NODES: &str = "nodes:2";
const RECENTRED_HEADER: &str = "centre: stale plan execution";

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

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
        "[plugins.learnings]\nlearnings_dir = {dir:?}\ngraph_cache = {dir:?}\n",
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

fn poll_capture_resending<F>(session: &str, key: &str, deadline: Instant, mut ok: F) -> Option<String>
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

/// Send one key, then poll WITHOUT re-sending (correct for non-idempotent toggle
/// / edge keys whose repeat would undo themselves).
fn send_once_then_poll<F>(session: &str, key: &str, deadline: Instant, ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    send_key(session, key);
    poll_capture(session, deadline, ok)
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn learnings_radial_map_renders_and_responds() {
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

    let session = format!("tripwire-learnings-radial-map-{}", std::process::id());
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

    // HomeScreen.
    let home_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture(&session, home_deadline, |c| c.contains("Stats") && c.contains("[i]")).is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("HomeScreen never rendered; last:\n---\n{last}\n---");
    }

    // Open learnings (`m`) — idempotent, so re-send until the Browse row paints.
    let open_deadline = Instant::now() + Duration::from_secs(45);
    if poll_capture_resending(&session, "m", open_deadline, |c| {
        c.contains(TITLE_TOKEN) && c.contains(FIXTURE_ID)
    })
    .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("learnings Browse never painted {FIXTURE_ID:?}; last:\n---\n{last}\n---");
    }

    // `g` reaches the Graph tab + focuses the entity neighbourhood (idempotent).
    let graph_deadline = Instant::now() + Duration::from_secs(30);
    if poll_capture_resending(&session, "g", graph_deadline, |c| c.contains(NEIGHBOURHOOD_EDGE))
        .is_none()
    {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("`g` did not paint the neighbourhood ({NEIGHBOURHOOD_EDGE:?}); last:\n---\n{last}\n---");
    }

    // `v` toggles into the radial map — sent ONCE then polled (a re-send would
    // toggle it back). Assert the centre header, both boxes, and the typed label.
    let map_deadline = Instant::now() + Duration::from_secs(30);
    let map = send_once_then_poll(&session, "v", map_deadline, |c| {
        c.contains(CENTRE_HEADER)
            && c.contains(CENTRE_BOX)
            && c.contains(NEIGHBOUR_BOX)
            && c.contains(EDGE_LABEL)
    });
    let Some(map_cap) = map else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "`v` did not paint the radial map (centre {CENTRE_HEADER:?} + boxes {CENTRE_BOX:?} \
             / {NEIGHBOUR_BOX:?} + label {EDGE_LABEL:?}); last:\n---\n{last}\n---"
        );
    };
    assert!(map_cap.contains(CENTRE_BOX), "centre box:\n{map_cap}");
    assert!(map_cap.contains(NEIGHBOUR_BOX), "neighbour box:\n{map_cap}");

    // `h` widens to hop 2 — the node count flips 1 → 2 (sent once, polled).
    let hop_deadline = Instant::now() + Duration::from_secs(30);
    let hop2 = send_once_then_poll(&session, "h", hop_deadline, |c| c.contains(HOP2_NODES));
    if hop2.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("`h` did not widen the hop to {HOP2_NODES:?}; last:\n---\n{last}\n---");
    }

    // Back to hop 1 (`h`), select the ring neighbour (`Down`), then `⏎` recentres
    // — the centre token CHANGES. Each is sent once.
    send_key(&session, "h");
    thread::sleep(Duration::from_millis(300));
    send_key(&session, "Down");
    thread::sleep(Duration::from_millis(300));
    let recentre_deadline = Instant::now() + Duration::from_secs(30);
    let recentred =
        send_once_then_poll(&session, "Enter", recentre_deadline, |c| c.contains(RECENTRED_HEADER));
    if recentred.is_none() {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!("`⏎` did not recentre to {RECENTRED_HEADER:?}; last:\n---\n{last}\n---");
    }

    // `Backspace` exits the map back to the neighbourhood. Assert the
    // neighbourhood help-bar marker (map's help bar lacks it) rather than a
    // specific edge — the list now sits on the recentred entity, whose edges
    // differ from the original selection.
    let back_deadline = Instant::now() + Duration::from_secs(30);
    let back = send_once_then_poll(&session, "BSpace", back_deadline, |c| {
        c.contains(NEIGHBOURHOOD_MARKER) && !c.contains(RECENTRED_HEADER)
    });

    let final_cap = capture_pane(&session);
    kill_session(&session);

    assert!(
        back.is_some(),
        "`Backspace` did not exit the map back to the neighbourhood (marker \
         {NEIGHBOURHOOD_MARKER:?} present, map header gone); last:\n---\n{final_cap}\n---"
    );
}
