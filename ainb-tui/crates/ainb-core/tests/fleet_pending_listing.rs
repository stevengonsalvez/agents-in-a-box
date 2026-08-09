//! End-to-end: the pending-approve queue is inspectable.
//!
//! An operator who sees `approve broker ... 1 pending request` in
//! `ainb fleet daemons` must be able to run ONE command and learn which agent,
//! in which worktree, is asking for what, and for how long. Before this test
//! the listing carried only the provider's session uuid (a key that appears in
//! no other fleet listing) plus an unbounded blob, so the row was not
//! actionable.
//!
//! Everything below is real: a live `ainb_plugin_notifyd::broker::serve` accept
//! loop on an isolated `approve.sock`, a real parked `client_await` waiter, a
//! real notifyd sqlite `current_state` row supplying the cwd join, and the real
//! `ainb` binary rendering the listing. HOME lives under `/tmp` (not `TMPDIR`)
//! because `approve.sock` must stay inside `AF_UNIX`'s ~104-char path limit.
//!
//! NOT named `tripwire_*`, deliberately. The `Test` job runs
//! `cargo nextest run --workspace --lib --tests --all-features
//!  -E 'not binary(/^tripwire_/)'`, and no script globs
//! `crates/ainb-core/tests/tripwire_fleet_*`, so under the tripwire prefix this
//! binary was selected by NO CI command and the whole listing contract was
//! ungated (`ainb-tui/CLAUDE.md`: "A test package must be reachable by the CI
//! command that claims to run it"). The prefix is reserved for tests that drive
//! a real `ainb tui` under tmux against staged plugins, which the `Test` job
//! deliberately does not provision. This one needs neither: no tmux, no staged
//! plugin, no TUI. Only the `ainb` binary cargo builds for it anyway, a unix
//! socket inside its own private HOME, and a sqlite file beside it, so it runs
//! safely in the ordinary test lane.
//!
//! The pure half of the contract (the join, the JSON key set, the both-ends
//! worktree elision, `--full`) is asserted as unit tests in
//! `src/cli/fleet/approve.rs`, which the same job builds via `--lib`.

// This is a single end-to-end scenario driven start to finish; splitting it
// into helpers would hide the ordering the tripwire exists to pin.
#![allow(clippy::too_many_lines)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::Paths;
use ainb_plugin_notifyd::broker::{self, BrokerState, DecisionKind};
use ainb_plugin_notifyd::store::{StateRow, Store};

/// The provider session uuid the hook forwards. Deliberately opaque: proving
/// the join means proving this alone is never what the operator has to act on.
const SESSION_ID: &str = "a1b2c3d4-0000-4000-8000-pendinglisting";
const WORKTREE: &str = "agents-in-a-box--pending-listing-probe--deadbeef";
/// The exact bytes the hook parks. Asserted verbatim, not by substring.
const TOOL_INPUT: &str =
    r#"{"command":"rm -rf build/ && cargo build --release","description":"probe"}"#;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn run_ainb(ainb_home: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", ainb_home)
        .env("AINB_HOME", ainb_home)
        // `HOME` too, following `tripwire_fleet_enrich` and friends.
        // `AppConfig::get_user_config_dir` resolves through `dirs::home_dir`,
        // NOT through `AINB_HOME`, so without this the child still reads the
        // developer's real `~/.agents-in-a-box/config/config.toml` and the test
        // outcome depends on whatever that file happens to say.
        .env("HOME", ainb_home)
        .env("AINB_DISABLE_PLUGINS", "1")
        .output()
        .expect("run ainb");
    assert!(
        out.status.success(),
        "`ainb {}` failed: {}\n{}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn fleet_approve_listing_names_the_worktree_tool_input_and_age() {
    // Short-path HOME: AF_UNIX socket paths cap at ~104 chars.
    let home = PathBuf::from(format!("/tmp/ainb-pendlist-{}", std::process::id()));
    let _ = fs::remove_dir_all(&home);
    fs::create_dir_all(&home).expect("create short-path home");
    let paths = Paths::under(&home);
    paths.ensure_base().expect("ensure base");

    // The cwd join source: notifyd's materialised state table, written by the
    // daemon in production and seeded here through the same public API.
    let cwd = format!("/tmp/worktrees/by-name/{WORKTREE}");
    {
        let store = Store::open(&paths.db).expect("open notifyd db");
        store
            .upsert_current_state(&StateRow {
                session_id: SESSION_ID.to_string(),
                cwd: cwd.clone(),
                kind: "ASK".to_string(),
                context: None,
                parent: None,
                last_event_ts: 4_000_000_000_100,
                source: "hook".to_string(),
            })
            .expect("seed current_state");
    }

    // Real broker on the isolated approve.sock.
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let state = BrokerState::new();
    {
        let sock = paths.approve_socket.clone();
        rt.spawn(async move {
            let listener = tokio::net::UnixListener::bind(&sock).expect("bind approve.sock");
            broker::serve(listener, state).await;
        });
    }

    // Park a REAL waiter: the same blocking call a PermissionRequest hook makes.
    let waiter = {
        let sock = paths.approve_socket.clone();
        thread::spawn(move || {
            broker::client_await(
                &sock,
                SESSION_ID,
                "Bash",
                TOOL_INPUT,
                Duration::from_mins(1),
            )
        })
    };

    // Wait for the AWAIT to register, and long enough that the age column is a
    // non-zero number of seconds (an always-"0s" column proves nothing).
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let pending = broker::client_list(&paths.approve_socket).unwrap_or_default();
        if pending.first().map_or(0, |p| p.waiting_ms) >= 1_200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "waiter never registered on the broker"
        );
        thread::sleep(Duration::from_millis(200));
    }

    let json = run_ainb(&home, &["fleet", "approve", "--format", "json"]);
    let rows: Vec<serde_json::Value> =
        serde_json::from_str(&json).unwrap_or_else(|e| panic!("listing is not JSON ({e}): {json}"));
    assert_eq!(rows.len(), 1, "exactly one waiter is parked: {json}");
    let row = &rows[0];
    assert_eq!(row["session_id"], SESSION_ID, "row: {row}");
    // The join, the whole point: the uuid alone is unactionable.
    assert_eq!(row["workspace"], WORKTREE, "row: {row}");
    assert_eq!(row["cwd"], cwd, "row: {row}");
    assert_eq!(row["tool"], "Bash", "row: {row}");
    // Verbatim bytes: JSON is the scripting surface and must never truncate.
    assert_eq!(row["tool_input"], TOOL_INPUT, "row: {row}");
    assert!(
        row["waiting_secs"].as_u64().unwrap_or(0) >= 1,
        "age must be reported in whole seconds: {row}"
    );

    let text = run_ainb(&home, &["fleet", "approve"]);
    // The compact row squeezes the middle out of an over-long worktree name but
    // MUST keep both ends: the repo at the head and the disambiguating hash at
    // the tail. A head-only truncation would render sibling worktrees identical.
    for needle in [
        "WORKSPACE",
        "WAITING",
        "agents-in-a-box--…-probe--deadbeef",
        "Bash",
        "rm -rf build/ && cargo build --release",
    ] {
        assert!(
            text.contains(needle),
            "text listing is missing {needle:?}:\n{text}"
        );
    }
    // A live age, not a hardcoded placeholder: some token reads `<n>s`, n >= 1.
    assert!(
        text.split_whitespace().any(|t| {
            t.strip_suffix('s').and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n >= 1)
        }),
        "text listing has no non-zero wait age:\n{text}"
    );

    // `--full` shortens nothing: the whole worktree name plus the absolute cwd.
    let full = run_ainb(&home, &["fleet", "approve", "--full"]);
    assert!(
        full.contains(WORKTREE),
        "--full elides the worktree name:\n{full}"
    );
    assert!(
        full.contains(&format!("cwd: {cwd}")),
        "--full omits the cwd:\n{full}"
    );

    // Release the parked waiter so the test never rides the 60s deny fallback.
    let decided = broker::client_decide(
        &paths.approve_socket,
        SESSION_ID,
        DecisionKind::Deny,
        "tripwire cleanup",
    )
    .expect("decide reaches the broker");
    assert!(decided, "the parked waiter must receive the decision");
    let _ = waiter.join();
    let _ = fs::remove_dir_all(&home);
}
