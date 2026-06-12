//! Tripwire (TDD plan §P8): the `learnings` Graph tab shows an entity
//! neighborhood with a TYPED edge, and `c` toggles to the community view.
//!
//! PRIMARY user-visible gate for the P8 graph UX (TUI-only — no CLI parity leg).
//! It drives the real `ainb` binary in a detached tmux session, opens the
//! learnings screen (`m`), presses `g` to reach the Graph tab + focus the entity
//! list, and asserts an EXACT real entity name plus a TYPED edge token render.
//! It then presses `c` and asserts a community-cluster title renders.
//!
//! GRAPH-SPECIFIC (load-bearing): the REAL nano_graphrag graphml edges are
//! UNTYPED — the relationship type lives in the `.entities.yaml` sidecars. (The
//! committed fixture's graphml happens to carry a `rel_type` key, but the
//! neighborhood deliberately does NOT read graphml edges — it reads the record
//! `relationships[]` — so that key is never consulted here.) So the entity
//! neighborhood is built from the AGGREGATED record relationships (typed:
//! `solves`/`caused_by`/…), and the community view reads
//! `kv_store_community_reports.json`. Because the Graph reads committed fixture
//! FILES (the record sidecars + the community json), this tripwire CAN assert
//! real graph content deterministically over the fixture KB.
//!
//! Setup mirrors `tripwire_learnings_search.rs` (P7):
//! - Skips if tmux isn't on PATH or `dist/plugins/learnings` isn't staged
//!   (re-signed for macOS AMFI — `scripts/build-plugins.sh`).
//! - Seeds an isolated HOME with `onboarding.toml` (skip first-run wizard) + a
//!   dismissed notify-install prompt + `[plugins.learnings]` with BOTH
//!   `learnings_dir` AND `graph_cache` → the fixture KB so the neighborhood and
//!   the community view paint deterministically.
//! - Asserts EXACT unique tokens, never a substring-OR. Each token was locked by
//!   a deliberately-wrong run reading the real render first.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The fixture learning id the Browse list renders (sorted first by id), used
/// only to confirm the learnings screen opened before pressing `g`.
const FIXTURE_ID: &str = "lrn-audit-after-rebase";

/// The title token the learnings screen paints (P3 contract — re-asserted so a
/// P8 refactor dropping it is caught here too).
const TITLE_TOKEN: &str = "🧠 Learnings";

/// An entity name from the aggregated `.entities.yaml` sidecars, sorted first
/// among the entity names → the initial Graph selection. Note this is NOT the
/// Browse row id `lrn-audit-after-rebase` (we assert the TYPED edge token below
/// to disambiguate the entity view from the Browse list).
const ENTITY_TOKEN: &str = "audit-after-rebase";

/// The TYPED edge token sourced from the aggregated record relationships
/// (`audit-after-rebase --solves--> stale plan execution`). This token never
/// appears on the Browse tab, so its presence proves the Graph entity
/// neighborhood painted.
const EDGE_TYPE_TOKEN: &str = "--solves-->";

/// A community-cluster title from `kv_store_community_reports.json` (sorted first
/// by id). Present only after `c` toggles to the community view.
const COMMUNITY_TOKEN: &str = "Rebase discipline";

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
/// point the learnings plugin's `learnings_dir` AND `graph_cache` at the fixture
/// KB (the fixture dir carries both the record sidecars and the community json).
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

    // Point BOTH learnings_dir (record scan → typed adjacency) and graph_cache
    // (community json) at the fixture dir so the Graph tab is deterministic.
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

/// Like [`poll_capture`], but re-sends `key` on each iteration until `ok` holds.
/// Re-pressing is safe for idempotent screen/focus keys (`m` once the screen is
/// open, `g` once the graph is focused — both are no-ops on repeat).
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
fn learnings_graph_shows_typed_neighborhood_and_community_toggle() {
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

    let session = format!("tripwire-learnings-graph-{}", std::process::id());
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

    // Press `g` to reach the Graph tab + focus the entity neighborhood,
    // re-pressing each poll until the TYPED edge token paints. `g` is idempotent
    // once focused (it just re-focuses + resets the selection to the first
    // entity, which is the row we assert on).
    let graph_deadline = Instant::now() + Duration::from_secs(30);
    let graph = poll_capture_resending(&session, "g", graph_deadline, |c| {
        c.contains(ENTITY_TOKEN) && c.contains(EDGE_TYPE_TOKEN)
    });
    let Some(graph_cap) = graph else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "`g` did not paint the Graph entity neighborhood (entity {ENTITY_TOKEN:?} + typed \
             edge {EDGE_TYPE_TOKEN:?}); last:\n---\n{last}\n---"
        );
    };

    // Press `c` to toggle to the community view, re-pressing each poll until a
    // community title paints. `c` toggles, so an even number of presses lands
    // back on entities — but the resend loop stops the instant the community
    // title appears (after the first odd press), so it converges on the
    // community view.
    let community_deadline = Instant::now() + Duration::from_secs(30);
    let community = poll_capture_resending(&session, "c", community_deadline, |c| {
        c.contains(COMMUNITY_TOKEN)
    });

    // Capture the final frame BEFORE killing the session so the diagnostic on
    // failure shows the real render (not an empty post-kill pane).
    let final_cap = capture_pane(&session);
    kill_session(&session);

    assert!(
        graph_cap.contains(ENTITY_TOKEN),
        "`g` did not render the Graph entity {ENTITY_TOKEN:?}:\n---\n{graph_cap}\n---"
    );
    assert!(
        graph_cap.contains(EDGE_TYPE_TOKEN),
        "`g` did not render the TYPED edge {EDGE_TYPE_TOKEN:?}:\n---\n{graph_cap}\n---"
    );
    assert!(
        community.is_some(),
        "`c` did not toggle to the community view listing {COMMUNITY_TOKEN:?}; \
         last capture:\n---\n{final_cap}\n---"
    );
}
