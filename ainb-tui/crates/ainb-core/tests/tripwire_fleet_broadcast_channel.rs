//! Tripwire: a broadcast channel, made and used the way an operator makes and
//! uses one.
//!
//! Part 2 Phase C's daemon half (minting `channel:<ulid>`, recording its
//! membership, refusing a stranger) landed with tests on the wire. None of them
//! touched the surface an operator actually drives, which is the exact gap that
//! shipped an ACP provider rendering as UNKNOWN. This drives the REAL `ainb`
//! binary in tmux against a REAL daemon on an isolated socket: `f`, `5`, `N`,
//! name, members, message, each key pressed ONCE after waiting for the screen
//! that receives it.
//!
//! What is proven against the real daemon here:
//!
//! * `N` opens the channel form, the name is typed into it, and the roster is
//!   the checklist (`e` widens it, `Space` ticks a row);
//! * the channel the daemon MINTS is what opens: the header carries a
//!   `channel:<ulid>` scope this client could not have composed, and the store
//!   holds a row for it with the membership the operator ticked;
//! * one send addresses the WHOLE membership: the message is filed once, under
//!   the channel's scope, with one durable delivery leg per member;
//! * the receipts block renders ONE ROW PER RECIPIENT, and a member whose
//!   session died between the create and the send is legible as such, by name,
//!   with the daemon's own reason on the same row.
//!
//! What is NOT proven here, and why:
//!
//! * a DELIVERED leg. A verified tmux send needs a live agent pane, and there
//!   is no agent in this test. The mixed delivered/rejected RENDERING is pinned
//!   where it can be driven deterministically: `fleet_chat.rs`'s
//!   `a_partial_fan_out_renders_one_receipt_per_recipient_with_its_reason` for
//!   the screen, and `fleet_msg_cli.rs`'s channel round trip for the CLI. What
//!   this file proves is that the block is per-recipient at all, and that the
//!   reason survives the whole path from the daemon to the pane.
//! * an agent's reply into the channel. No adapter process runs here.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;

use fleet_hangar::{ExactTmuxSession, FleetHangar};

/// The two sessions the channel is created over. `LIVE_SESSION` keeps its
/// row for the whole test; `DOOMED_SESSION` is retired between the create and
/// the send, which is the ordinary way a fan-out ends up partial.
const LIVE_SESSION: &str = "claude:one";
const DOOMED_SESSION: &str = "claude:gone";

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

/// Seed one tmux-era Fleet session so the channel form has a row to tick.
fn seed_fleet_session(hangar: &FleetHangar, session_key: &str, name: &str, cwd: &Path) {
    let cwd = cwd.display().to_string();
    hangar.block_on(async {
        sqlx::query(
            "INSERT INTO fleet_session \
             (session_key, provider, cwd, display_name, lifecycle_state, management_state, \
              capabilities, discovered_at, last_observed_at, version) \
             VALUES (?, 'claude', ?, ?, 'IDLE', 'MANAGED', \
                     '{\"send_prompt\":true}', 1, 1, 1)",
        )
        .bind(session_key)
        .bind(&cwd)
        .bind(name)
        .execute(hangar.pool())
        .await
        .expect("seed a channel member");
    });
}

/// Retire one member the way a real session dies: EXITED, still in the store,
/// still a member of the channel.
///
/// This is what makes the fan-out PARTIAL, and it happens AFTER the channel is
/// created on purpose: membership is decided once, delivery is decided per
/// send, and the whole point of the receipts block is that the second can
/// disagree with the first.
fn retire_session(hangar: &FleetHangar, session_key: &str) {
    hangar.block_on(async {
        sqlx::query(
            "UPDATE fleet_session SET lifecycle_state = 'EXITED', version = version + 1 \
             WHERE session_key = ?",
        )
        .bind(session_key)
        .execute(hangar.pool())
        .await
        .expect("retire the doomed member");
    });
}

#[test]
fn a_broadcast_channel_is_created_addressed_and_receipted_per_recipient() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-chan-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let hangar_home = home_tmp.path().join("hangar-home");
    seed_isolated_home(home_tmp.path(), &hangar_home);
    let hangar = FleetHangar::start(&hangar_home);
    // A NAMED working directory, not the temp home: the panel titles a row with
    // its repository label (the cwd's basename), and the temp home's basename
    // is random per run, so a test that waits for a stable name never sees one.
    let cwd = home_tmp.path().join("channel-demo");
    fs::create_dir_all(&cwd).expect("create the members' working directory");
    seed_fleet_session(&hangar, LIVE_SESSION, "chan-live", &cwd);
    seed_fleet_session(&hangar, DOOMED_SESSION, "chan-doomed", &cwd);

    let name = format!("ainb-chan-{}", std::process::id());
    let tmux = ExactTmuxSession::create(name, "180", "50");
    let session = tmux.name();
    let command = format!(
        "HOME={home} AINB_HOME={home}/.agents-in-a-box AINB_HANGAR_HOME={hangar} \
         AINB_FLEET_DISABLE_TMUX_DISCOVERY=1 AINB_DISABLE_PLUGINS=1 \
         CLAUDE_PEERS_DB={peers} AINB_FLEET_JOBS_DIR={jobs} exec {bin} tui",
        home = home_tmp.path().display(),
        hangar = hangar_home.display(),
        peers = home_tmp.path().join("peers.db").display(),
        jobs = home_tmp.path().join("jobs").display(),
        bin = ainb_bin().display(),
    );
    Command::new("tmux")
        .args(["send-keys", "-t", session, &command, "C-m"])
        .status()
        .expect("launch the TUI");

    // Each key ONCE, after waiting for the screen that receives it. A retry
    // loop is how a modal that swallows the first press stays invisible.
    assert!(
        wait_for(session, "A I N B", 60),
        "the home screen never painted:\n{}",
        capture_pane(session)
    );
    thread::sleep(Duration::from_millis(500));
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        send_key(session, "f");
        if wait_for(session, "Fleet ·", 2) {
            break;
        }
        thread::sleep(Duration::from_millis(500));
    }
    assert!(
        wait_for(session, "Fleet ·", 5),
        "the Fleet panel did not open:\n{}",
        capture_pane(session)
    );
    // The panel lands on the action queue, which shows only what needs the
    // operator; an IDLE session is not on it. `5` is the All view.
    send_key(session, "5");
    // The ROSTER titles a row with its repository label (the cwd's basename),
    // not with `display_name`, so this waits for the directory both members
    // share. Their session KEYS are what the member checklist shows, and that
    // is asserted below, where it is the fact that matters.
    assert!(
        wait_for(session, "channel-demo", 30),
        "the seeded members never reached the roster:\n{}",
        capture_pane(session)
    );

    // THE PICKER FIRST. `N` opens the channels that already exist, with a
    // trailing row that falls through to the create form. There are none yet,
    // so the `+ new channel` row is where the cursor lands.
    send_key(session, "N");
    assert!(
        wait_for(session, "+ new channel", 20),
        "`N` did not open the channel picker:\n{}",
        capture_pane(session)
    );
    send_key(session, "Enter");
    assert!(
        wait_for(session, "Name the channel", 20),
        "the picker's `+ new channel` row did not reach the create form:\n{}",
        capture_pane(session)
    );
    type_text(session, "ops");
    send_key(session, "Enter");
    assert!(
        wait_for(session, "Space toggle", 20),
        "the form did not reach the member checklist:\n{}",
        capture_pane(session)
    );
    // `e` widens the checklist to the whole roster, then both members are
    // ticked one at a time. Space, Down, Space: exactly what a human does.
    send_key(session, "e");
    assert!(
        wait_for(session, LIVE_SESSION, 20),
        "the roster never reached the checklist:\n{}",
        capture_pane(session)
    );
    send_key(session, "Space");
    send_key(session, "Down");
    send_key(session, "Space");
    assert!(
        wait_for(session, "Enter creates (2 member(s))", 20),
        "both members were not ticked:\n{}",
        capture_pane(session)
    );
    send_key(session, "Enter");

    // THE SCREEN FIRST. Everything below reads rows off the channel view, and
    // reading them off the form underneath is how an assertion about a channel
    // passes against a modal.
    let header = wait_for_row(
        session,
        |row| row.contains("Fleet channel · ops") && row.contains("channel:"),
        30,
    );
    let header = header.unwrap_or_else(|| {
        panic!(
            "the minted channel never opened as a conversation:\n{}",
            capture_pane(session)
        )
    });

    // The scope is the DAEMON's. Read it back out of the store rather than out
    // of the pane, so this cannot pass against a client that composed its own
    // `channel:<something>` and never asked anybody.
    let channels = hangar.block_on(async {
        ainb_hangar_store::repo::fleet_chat::FleetChannelRepo::list(hangar.pool())
            .await
            .expect("read the channel back")
    });
    let channel = channels
        .iter()
        .find(|channel| channel.name == "ops")
        .unwrap_or_else(|| panic!("the daemon minted no channel called ops: {channels:#?}"));
    assert_eq!(channel.kind, "broadcast", "the form minted the wrong kind");
    // Compared as a SET: the checklist is a `BTreeSet`, so the order the
    // operator ticked in is not the order the daemon stores.
    let mut members = channel.recipients.clone();
    members.sort();
    let mut ticked = vec![LIVE_SESSION.to_string(), DOOMED_SESSION.to_string()];
    ticked.sort();
    assert_eq!(
        members, ticked,
        "the channel's membership is not the two members that were ticked"
    );
    assert!(
        header.contains(&channel.scope_key),
        "the channel header names a scope the daemon did not mint: {header} vs {}",
        channel.scope_key
    );

    // ONE MEMBER DIES between the create and the send. This is what makes the
    // fan-out partial, and it is the only interesting case on this screen.
    retire_session(&hangar, DOOMED_SESSION);

    // THE SEND. One message, addressed to the whole membership.
    type_text(session, "run the tests");
    send_key(session, "Enter");
    assert!(
        wait_for(session, "DELIVERY RECEIPTS", 30),
        "no receipts block appeared after the send:\n{}",
        capture_pane(session)
    );

    // PER RECIPIENT, ROW-ANCHORED. The refused member's row carries its name,
    // its state and the daemon's own reason, all three on the line an operator
    // reads.
    let refused = wait_for_row(
        session,
        |row| row.contains(DOOMED_SESSION) && row.contains("REJECTED"),
        30,
    );
    let refused = refused.unwrap_or_else(|| {
        panic!(
            "the member that died is not legible as refused:\n{}",
            capture_pane(session)
        )
    });
    assert!(
        refused.contains("target_not_running"),
        "the refused member's row does not say WHY: {refused}"
    );
    let pane = capture_pane(session);
    let live_row = pane
        .lines()
        .find(|row| row.contains(LIVE_SESSION) && !row.contains("run the tests"))
        .unwrap_or_else(|| panic!("the live member has no receipt row:\n{pane}"));
    assert!(
        !live_row.contains("target_not_running"),
        "both members were painted with the dead one's reason, so the block is \
         not per-recipient: {live_row}"
    );
    eprintln!("--- the channel, after one fan-out ---\n{pane}");

    // AND THE DURABLE HALF. One message, filed under the channel's scope, with
    // one leg per member: a fan-out is ONE send with N legs, never N sends.
    let stored = hangar.block_on(async {
        ainb_hangar_store::repo::fleet_message::FleetMessageRepo::list_by_scope(
            hangar.pool(),
            &channel.scope_key,
            0,
            50,
        )
        .await
        .expect("read the channel's timeline")
    });
    let composed = stored
        .iter()
        .find(|row| row.sender == "operator" && row.body == "run the tests")
        .unwrap_or_else(|| panic!("the daemon filed the message elsewhere: {stored:#?}"));
    let legs = hangar.block_on(async {
        ainb_hangar_store::repo::fleet_message::FleetMessageRepo::deliveries_for_message(
            hangar.pool(),
            &composed.id,
        )
        .await
        .expect("read the delivery legs")
    });
    assert_eq!(
        legs.len(),
        2,
        "one send did not produce one leg per member: {legs:#?}"
    );
    let doomed_leg = legs
        .iter()
        .find(|leg| leg.session_key == DOOMED_SESSION)
        .expect("the dead member has a durable leg");
    assert_eq!(doomed_leg.state, "REJECTED", "{doomed_leg:#?}");
    assert_eq!(
        doomed_leg.detail.as_deref(),
        Some("target_not_running"),
        "the durable leg and the painted row must carry the SAME reason: {doomed_leg:#?}"
    );

    // And the channel is a chat surface, not a modal trap: one Esc returns.
    send_key(session, "Escape");
    assert!(
        wait_for(session, "ACTION QUEUE", 20),
        "Esc did not return the channel to the Fleet panel:\n{}",
        capture_pane(session)
    );

    // THE WAY BACK IN. Without this the channel is write-once: minted in one
    // keystroke, unreachable the moment the operator presses Esc, and pressing
    // `N` again would mint a SECOND `ops` with the conversation split between
    // them. The picker lists the one the daemon has, and Enter reopens THAT
    // conversation, with the message already in it.
    send_key(session, "N");
    assert!(
        wait_for(session, "ops · 2 member(s)", 20),
        "the channel that exists is not offered by the picker:\n{}",
        capture_pane(session)
    );
    send_key(session, "Enter");
    let reopened = wait_for_row(
        session,
        |row| row.contains("Fleet channel · ops") && row.contains(&channel.scope_key),
        30,
    );
    assert!(
        reopened.is_some(),
        "the picker did not reopen the daemon's own channel scope:\n{}",
        capture_pane(session)
    );
    assert!(
        wait_for(session, "run the tests", 30),
        "the reopened channel is empty, so the picker opened a different scope:\n{}",
        capture_pane(session)
    );
}
