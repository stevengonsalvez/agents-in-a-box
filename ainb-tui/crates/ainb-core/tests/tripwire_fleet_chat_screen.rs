//! Tripwire: the copilot chat surface, driven the way an operator drives it.
//!
//! Part 1 shipped a daemon and a CLI and called itself end-to-end proven. It was
//! not: nothing ever opened the TUI, and the Fleet panel was one unmapped token
//! away from rendering every ACP session as `UNKNOWN`. So this drives the REAL
//! `ainb` binary in tmux against a REAL daemon on an isolated socket, opens the
//! chat the way a user does (`f` then `m`, each pressed ONCE), and reads the
//! pane.
//!
//! What is proven against the real daemon here:
//!
//! * the chat surface opens on ONE `m`, resolves its channel through
//!   `fleet/channel_list`, and creates the copilot channel when there is none;
//! * a copilot-authored row and an operator-authored row are DISTINGUISHABLE on
//!   screen, asserted on the row rather than a substring anywhere in the pane;
//! * an open confirm card renders as answerable and `y` resolves it through
//!   `fleet/confirm_answer`, with the store's own row as the receipt.
//!
//! What is NOT proven here, and why:
//!
//! * a card in a state this build cannot decode. `fleet/confirm_list` only
//!   returns `state = 'open'` rows, so an undecodable card cannot be produced
//!   through the daemon at all. That rule is unit-tested where it lives
//!   (`ainb_plugin_hangar::screen::fleet_chat`, `a_card_in_an_unknown_state_*`).
//! * a copilot REPLY. The chat surface addresses a real ACP session, but no
//!   adapter process runs in this test, so
//!   [`the_composer_puts_the_operators_message_in_the_conversation`] proves the
//!   operator's half of that journey: composed, delivered to the channel's own
//!   session, durable and attributed by the daemon.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_hangar_store::repo::fleet_chat::{FleetChannelRepo, FleetConfirmRepo, FleetConfirmRow};
use ainb_hangar_store::repo::fleet_message::{FleetMessageRepo, NewFleetMessage};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{ExactTmuxSession, FleetHangar};

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux").arg("-V").output().is_ok_and(|out| out.status.success())
}

/// A HOME with onboarding complete and the notify prompt dismissed.
///
/// The record is seeded under BOTH homes on purpose: `notifyd::Paths` resolves
/// `AINB_HANGAR_HOME` BEFORE `AINB_HOME`, this test sets both, and a missing
/// record fires an install modal whose first act is to eat the keypress this
/// test is about to send.
///
/// The onboarding version is read from the BINARY's own version. A literal
/// parses as major 0 and re-runs the wizard, which eats the same keypress.
fn seed_isolated_home(home: &Path, hangar_home: &Path) {
    let base = home.join(".agents-in-a-box");
    let config = base.join("config");
    fs::create_dir_all(&config).expect("create isolated config dir");
    fs::write(
        config.join("onboarding.toml"),
        format!(
            r#"completed = true
completed_at = "2026-08-08T00:00:00+00:00"
version = "{version}"
skipped_dependencies = []
git_directories = []
"#,
            version = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
    seed_notify_dismissed(&base);
    seed_notify_dismissed(hangar_home);
}

fn seed_notify_dismissed(base: &Path) {
    fs::create_dir_all(base).expect("create notify record dir");
    fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("seed install.json");
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn send_key(session: &str, key: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
}

fn type_text(session: &str, text: &str) {
    Command::new("tmux")
        .args(["send-keys", "-t", session, "-l", text])
        .status()
        .expect("tmux send-keys -l");
}

fn wait_for(session: &str, needle: &str, secs: u64) -> bool {
    wait_for_row(session, |row| row.contains(needle), secs).is_some()
}

/// Wait for a ROW matching `matches`, and return it.
///
/// Row-anchored on purpose: a bare pane-wide `contains` passes on any
/// incidental occurrence, which is how an assertion that proves nothing stays
/// green for a release.
fn wait_for_row<F>(session: &str, mut matches: F, secs: u64) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let capture = capture_pane(session);
        if let Some(row) = capture.lines().find(|row| matches(row)) {
            return Some(row.to_string());
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(400));
    }
}

/// Open the sessions screen and walk the tab strip to `copilot`.
///
/// The Fleet panel this used to open is deleted; the conversation it hosted is
/// now a tab on the sessions screen. `s` opens the screen, then `Tab` walks the
/// strip — re-checking between presses, because the copilot pane dials the
/// daemon when it opens and pressing again inside that window walks past it.
fn open_chat_surface(session: &str) -> bool {
    if !wait_for(session, "Enter select | Tab content", 60) {
        return false;
    }
    thread::sleep(Duration::from_millis(500));
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        send_key(session, "s");
        if wait_for(session, "Workspaces (", 2) {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    if !wait_for(session, "Workspaces (", 5) {
        return false;
    }
    let deadline_m = Instant::now() + Duration::from_secs(45);
    while Instant::now() < deadline_m {
        send_key(session, "Tab");
        for _ in 0..4 {
            if wait_for(session, "Fleet chat · #copilot", 1) {
                return true;
            }
        }
    }
    wait_for(session, "Fleet chat · #copilot", 5)
}

/// Everything a chat journey needs: an isolated home, a real daemon, a real
/// `ainb` in tmux, and the chat surface open on it.
///
/// Returns the tmux session guard and the channel scope the SCREEN resolved,
/// read back out of the store so the scope is known to have come from the
/// daemon rather than from a constant in the client.
fn chat_journey(prefix: &str) -> (tempfile::TempDir, FleetHangar, ExactTmuxSession, String) {
    let home_tmp = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let hangar_home = home_tmp.path().join("hangar-home");
    seed_isolated_home(home_tmp.path(), &hangar_home);
    let hangar = FleetHangar::start(&hangar_home);

    let name = format!("{prefix}{}", std::process::id());
    let tmux = ExactTmuxSession::create(name, "180", "50");
    let command = format!(
        "HOME={home} AINB_HOME={hangar} AINB_HANGAR_HOME={hangar} \
         AINB_FLEET_DISABLE_TMUX_DISCOVERY=1 AINB_DISABLE_PLUGINS=1 \
         CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home_tmp.path().display(),
        hangar = hangar_home.display(),
        peers = home_tmp.path().join("peers.db").display(),
        jobs = home_tmp.path().join("jobs").display(),
        bin = ainb_bin().display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", tmux.name(), &command, "C-m"])
        .status()
        .expect("launch the TUI");
    assert!(
        open_chat_surface(tmux.name()),
        "the copilot chat did not open on one `f` and one `m`:\n{}",
        capture_pane(tmux.name())
    );

    // The screen resolves its channel through `fleet/channel_list` and mints
    // one when the daemon has none. Reading the store back is how we know the
    // scope came from the DAEMON: a hardcoded `channel:copilot` was the first
    // version of this screen, and it read an empty timeline forever while
    // every one of its unit tests stayed green.
    let deadline = Instant::now() + Duration::from_secs(30);
    let scope = loop {
        let channel = hangar.block_on(async {
            FleetChannelRepo::newest_of_kind(hangar.pool(), "copilot")
                .await
                .expect("read the copilot channel")
        });
        if let Some(channel) = channel {
            break channel.scope_key;
        }
        assert!(
            Instant::now() < deadline,
            "the chat surface never created its copilot channel:\n{}",
            capture_pane(tmux.name())
        );
        thread::sleep(Duration::from_millis(300));
    };
    assert!(
        scope.starts_with("channel:") && scope != "channel:copilot",
        "the daemon mints channel:<ulid>; got {scope:?}"
    );
    (home_tmp, hangar, tmux, scope)
}

#[test]
fn the_copilot_chat_opens_attributes_and_answers_a_confirm_card() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let (_home_tmp, hangar, tmux, scope) = chat_journey("ainb-chat-");
    let session = tmux.name();
    assert!(
        wait_for(session, &scope, 20),
        "the chat did not render the scope the daemon minted ({scope}):\n{}",
        capture_pane(session)
    );

    // Two rows, one from each author. Seeded through the daemon's own writer
    // for the copilot line (`copilot::post_channel_message`, the exact function
    // the copilot service posts with, so `sender` is set the way the daemon
    // sets it) and through the store for the operator line.
    hangar.block_on(async {
        ainb_hangar_daemon::copilot::post_channel_message(
            hangar.pool(),
            hangar.events(),
            &scope,
            "session one is waiting on an approval",
        )
        .await
        .expect("seed the copilot line");
        FleetMessageRepo::insert_message(
            hangar.pool(),
            &NewFleetMessage {
                id: "01J0CHATOPERATOR".to_string(),
                request_id: None,
                request_fingerprint: None,
                scope_key: scope.clone(),
                origin_message_id: None,
                sender: "operator".to_string(),
                kind: "user".to_string(),
                body: "what is blocked right now".to_string(),
                created_at: 1_700_000_000_000,
            },
        )
        .await
        .expect("seed the operator line");
    });

    // ATTRIBUTION. The wire carries the actor precisely so a copilot write
    // cannot masquerade as a human's, and that guarantee dies at the last inch
    // if the panel paints both rows the same. Anchored on the attribution
    // COLUMN, which is fixed width and followed by `│`.
    let operator_row = wait_for_row(
        session,
        |row| row.contains("YOU") && row.contains("│ what is blocked right now"),
        25,
    );
    let copilot_row = wait_for_row(
        session,
        |row| row.contains("COPILOT") && row.contains("│ session one is waiting on an approval"),
        25,
    );
    let pane = capture_pane(session);
    assert!(
        operator_row.is_some(),
        "the operator's message is not attributed to the operator:\n{pane}"
    );
    assert!(
        copilot_row.is_some(),
        "the copilot's message is not attributed to the copilot:\n{pane}"
    );
    assert_ne!(
        operator_row, copilot_row,
        "the two authors rendered as the same row"
    );

    // CONFIRM CARD. Seeded open, rendered answerable, answered with `y`,
    // resolved by the daemon through `fleet/confirm_answer`.
    hangar.block_on(async {
        FleetConfirmRepo::insert(
            hangar.pool(),
            &FleetConfirmRow {
                confirm_id: "01J0CHATCARD".to_string(),
                scope_key: scope.clone(),
                tool: "kill".to_string(),
                arguments: r#"{"session":"claude:one"}"#.to_string(),
                target_session_key: Some("claude:one".to_string()),
                state: "open".to_string(),
                edited_arguments: None,
                created_at: 1_700_000_000_000,
                expires_at: 4_000_000_000_000,
                answered_at: None,
            },
        )
        .await
        .expect("seed one open confirm card");
    });
    let card_row = wait_for_row(
        session,
        |row| row.contains("[OPEN]") && row.contains("kill") && row.contains("y approve"),
        25,
    );
    assert!(
        card_row.is_some(),
        "the open confirm card is not on screen as answerable:\n{}",
        capture_pane(session)
    );

    // Tab focuses the card block; it does NOT arm a card. Nothing is selected
    // until the operator picks one with the arrow keys, which is what the
    // surface itself says when a key arrives with no selection ("pick a card
    // with ↑↓ before answering").
    //
    // That is deliberate. `apply_snapshot` used to adopt the first card
    // whenever nothing was selected, so a background poll returning cards armed
    // `y` over a destructive call the operator had not read. Removing it means
    // approving takes two keys, and this test presses both rather than
    // asserting the pre-armed behaviour it was written against.
    // `Shift+Tab` focuses the card block. `Tab` belongs to the sessions
    // screen's tab strip now, so the conversation's own focus toggle moved to
    // the reverse key — the strip wraps, so nothing is lost.
    send_key(session, "BTab");
    send_key(session, "Down");
    send_key(session, "y");
    let resolved = {
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let card = hangar.block_on(async {
                FleetConfirmRepo::get(hangar.pool(), "01J0CHATCARD")
                    .await
                    .expect("read the confirm card back")
                    .expect("the card still exists")
            });
            if card.state != "open" {
                break card.state;
            }
            assert!(
                Instant::now() < deadline,
                "`y` never reached fleet/confirm_answer:\n{}",
                capture_pane(session)
            );
            thread::sleep(Duration::from_millis(300));
        }
    };
    assert_eq!(
        resolved, "approved",
        "the operator approved, so the card must be approved"
    );
    assert!(
        wait_for(session, "CONFIRM CARDS · none open", 25),
        "the answered card is still on screen:\n{}",
        capture_pane(session)
    );

    // Esc is deliberately two-step from the cards: the first returns focus to
    // the composer, the second leaves. An operator part-way through answering a
    // destructive card must not lose the screen to one stray keypress.
    send_key(session, "Escape");
    assert!(
        wait_for(session, "Enter sends · Tab confirm cards", 10),
        "the first Esc left the card focus without returning to the composer:\n{}",
        capture_pane(session)
    );
    eprintln!("--- the chat surface, live ---\n{}", capture_pane(session));
    send_key(session, "Escape");
    // The second Esc leaves the conversation for `preview`, the one tab that is
    // never disabled. It used to return to the Fleet panel; the panel is gone
    // and the conversation is a tab on the sessions screen now.
    assert!(
        wait_for(session, "Workspaces (", 20),
        "the second Esc did not leave the conversation for the sessions screen:\n{}",
        capture_pane(session)
    );
    assert!(
        !capture_pane(session).contains("Fleet chat \u{b7} #copilot"),
        "and the conversation must actually be closed:\n{}",
        capture_pane(session)
    );
}

/// The journey the operator is actually here for: type a message to the
/// copilot, and see it in the conversation.
///
/// This was `#[ignore]`d against a named daemon bug, and the ignore is gone
/// with the bug. Two landed rules contradicted each other:
///
///   * `fleet/channel_create` REFUSES a recipient list for a copilot channel
///     ("create its ACP session against the minted scope_key"), so the channel
///     row is stored with NO members;
///   * `fleet/message_send` required every target of a `channel:` scope to be
///     IN that channel's `recipients`.
///
/// The session that IS the channel's only true member was therefore a stranger
/// to the membership check, and every operator message was refused. Each rule
/// is defensible alone, which is why the daemon's own tests were green and only
/// a test that drives the screen could see it. `message_send` now resolves a
/// copilot channel's membership through `FleetAcpSessionRepo::get_live_by_scope`.
#[test]
fn the_composer_puts_the_operators_message_in_the_conversation() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }
    let (_home_tmp, hangar, tmux, scope) = chat_journey("ainb-chatsend-");
    let session = tmux.name();

    type_text(session, "what is blocked?");
    send_key(session, "Enter");

    // On screen, attributed to the operator, in the row the operator reads.
    let sent = wait_for_row(
        session,
        |row| row.contains("YOU") && row.contains("│ what is blocked?"),
        30,
    );
    assert!(
        sent.is_some(),
        "the operator's message never reached the conversation:\n{}",
        capture_pane(session)
    );

    // And durable, in the channel's own scope, attributed by the DAEMON.
    let stored = hangar.block_on(async {
        FleetMessageRepo::list_by_scope(hangar.pool(), &scope, 0, 50)
            .await
            .expect("read the channel timeline back")
    });
    assert!(
        stored
            .iter()
            .any(|row| row.sender == "operator" && row.body == "what is blocked?"),
        "the daemon stored no operator row for the composed message: {stored:#?}"
    );
}
