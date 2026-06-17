//! Tripwire (TDD plan §P7): the `learnings` Search tab opens a query input box
//! on `/` and echoes the typed query.
//!
//! PRIMARY user-visible gate for the P7 search UX SHELL (TUI-only — no CLI
//! parity leg). It drives the real `ainb` binary in a detached tmux session,
//! opens the learnings screen (`m`), presses `/` to focus the query box, types
//! a query, and asserts the EXACT query-box prompt token renders and the typed
//! query echoes.
//!
//! SCOPE NOTE: this tripwire asserts the search UX SHELL only — the prompt box
//! + typed-query echo. It deliberately does NOT assert any specific qmd hit:
//! the real `/` path shells the user's live qmd index (`~/.cache/qmd`), whose
//! output is not deterministic in CI and is empty under the isolated test HOME.
//! Ranked-RESULT rendering is asserted deterministically by the in-proc unit
//! tests with an INJECTED fake `QmdSearch` runner (`tests/search.rs`), per the
//! search-tripwire discipline.
//!
//! Setup mirrors `tripwire_learnings_browse.rs` (P5):
//! - Skips if tmux isn't on PATH or `dist/plugins/learnings` isn't staged
//!   (re-signed for macOS AMFI — `scripts/build-plugins.sh`).
//! - Seeds an isolated HOME with `onboarding.toml` (skip first-run wizard) + a
//!   dismissed notify-install prompt + `[plugins.learnings].learnings_dir` →
//!   the fixture KB so the Browse list paints deterministically.
//! - Asserts EXACT unique tokens, never a substring-OR. Each token was locked
//!   by a deliberately-wrong run reading the real render first.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

/// The fixture learning id the Browse list renders (sorted first by id), used
/// only to confirm the learnings screen opened before pressing `/`.
const FIXTURE_ID: &str = "lrn-audit-after-rebase";

/// The title token the learnings screen paints (P3 contract — re-asserted so a
/// P7 refactor dropping it is caught here too).
const TITLE_TOKEN: &str = "🧠 Learnings";

/// The query-box prompt token. Unique to the Search input box. Locked by a
/// wrong-value-first run reading the real render.
const PROMPT_TOKEN: &str = "search:";

/// The query string typed into the box. Echoed back verbatim — its presence in
/// the capture proves the typed characters reached the query input.
const TYPED_QUERY: &str = "rebase";

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
/// idempotent once the screen is open (a no-op nav).
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

/// Send `text` as a literal string (`-l`) so tmux echoes the characters
/// verbatim into the focused box rather than interpreting them as key names.
fn send_literal(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("tmux send literal");
}

/// Poll until the full `query` has echoed into the box, re-sending any
/// not-yet-echoed tail on each iteration.
///
/// The query echoes character-by-character; under cold/tmux-contention load the
/// last char often hasn't painted before a short single-window poll deadline,
/// which made this assertion flake (~4/6). This mirrors the idempotent
/// [`poll_capture_resending`] pattern used for the open key: each iteration
/// inspects the capture, and if the full `query` substring isn't present yet it
/// re-sends only the missing suffix (the chars after the longest already-echoed
/// prefix). Re-sending only the tail keeps the box from accumulating duplicate
/// characters, so the assertion converges on the exact typed query.
fn poll_query_echo_resending(session: &str, query: &str, deadline: Instant) -> Option<String> {
    // Prime the box with the full query, then top up the tail each poll.
    send_literal(session, query);
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if cap.contains(query) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(400));
        // Re-send only the suffix that hasn't echoed yet: find the longest
        // prefix of `query` already present in the capture and re-type the rest.
        // This is idempotent — if the whole prefix already echoed we send only
        // the missing chars, never duplicating already-painted ones.
        let remaining = unechoed_suffix(&cap, query);
        if !remaining.is_empty() {
            send_literal(session, remaining);
        }
    }
    None
}

/// The suffix of `query` that has NOT yet echoed into `cap`: the chars after
/// the longest prefix of `query` that appears as a substring in `cap`. Returns
/// the whole query if not even the first char echoed, and `""` once the full
/// query is present. Char-boundary safe (operates on byte indices of `query`'s
/// own `char_indices`, never a raw slice of an arbitrary offset).
fn unechoed_suffix<'a>(cap: &str, query: &'a str) -> &'a str {
    // Longest prefix of `query` present in `cap`. Walk char boundaries so a
    // multibyte query can't slice mid-char.
    let mut longest_echoed_end = 0;
    for (idx, ch) in query.char_indices() {
        let end = idx + ch.len_utf8();
        if cap.contains(&query[..end]) {
            longest_echoed_end = end;
        } else {
            break;
        }
    }
    &query[longest_echoed_end..]
}

fn kill_session(session: &str) {
    let _ = Command::new("tmux").args(["kill-session", "-t", session]).status();
}

#[test]
fn learnings_search_opens_query_box_and_echoes_typed_query() {
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

    let session = format!("tripwire-learnings-search-{}", std::process::id());
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

    // Press `/` to focus the Search query box, re-pressing each poll until the
    // prompt token paints. `/` is idempotent once the box is focused (a stray
    // `/` while focused is just another query char that the next step's echo
    // assertion is robust to — we type a distinct query AFTER the box appears).
    let prompt_deadline = Instant::now() + Duration::from_secs(20);
    let prompt =
        poll_capture_resending(&session, "/", prompt_deadline, |c| c.contains(PROMPT_TOKEN));
    let Some(prompt_cap) = prompt else {
        let last = capture_pane(&session);
        kill_session(&session);
        panic!(
            "`/` did not open the Search query box with prompt {PROMPT_TOKEN:?}; \
             last:\n---\n{last}\n---"
        );
    };

    // Type the query into the focused box — each char echoes. Poll until the
    // FULL query has painted, re-sending any not-yet-echoed tail on each
    // iteration (the open step's idempotent resend pattern, applied to literal
    // characters). The deadline is generous (40s) because the last char of the
    // echo frequently lags under cold/tmux-contention load; a single short
    // window raced that last char and flaked ~4/6.
    let echo_deadline = Instant::now() + Duration::from_secs(40);
    let echoed = poll_query_echo_resending(&session, TYPED_QUERY, echo_deadline);

    // Capture the final frame BEFORE killing the session so the diagnostic on
    // failure shows the real render (not an empty post-kill pane).
    let final_cap = capture_pane(&session);
    kill_session(&session);

    assert!(
        prompt_cap.contains(PROMPT_TOKEN),
        "`/` did not render the query-box prompt {PROMPT_TOKEN:?}:\n---\n{prompt_cap}\n---"
    );
    assert!(
        echoed.is_some(),
        "the typed query {TYPED_QUERY:?} did not echo into the query box; \
         last capture:\n---\n{final_cap}\n---"
    );
}
