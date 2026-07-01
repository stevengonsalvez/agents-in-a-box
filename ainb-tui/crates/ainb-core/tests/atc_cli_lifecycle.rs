//! End-to-end lifecycle test for `ainb fleet atc`.
//!
//! Drives the real `ainb` binary (via `CARGO_BIN_EXE_ainb`) against a
//! tempdir-backed `AINB_HOME`, using `--no-spawn --no-heartbeat` so the test
//! touches no real launchd/systemd timer and spawns no Claude session. It
//! proves the provisioning lifecycle and its idempotency:
//!
//! * `setup` writes CLAUDE.md + meta.json + seeded state/task-log.
//! * `status --format json` reports the instance.
//! * `list --format json` includes it.
//! * `setup` again is idempotent and does not clobber accumulated state.json.
//! * `teardown --purge` removes the instance dir.
//! * `teardown` again on an absent instance is a clean no-op.

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn run(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(ainb_bin())
        .env("AINB_HOME", home)
        // Point the heartbeat/spawn binary resolution back at this same binary.
        .env("AINB_BIN", ainb_bin())
        .args(args)
        .output()
        .expect("invoke ainb")
}

#[test]
fn atc_setup_status_list_teardown_lifecycle() {
    let home = std::env::temp_dir().join(format!("atc-cli-{}", std::process::id()));
    let atc_dir = home.join("atc").join("tower");

    // --- setup (no spawn, no heartbeat → no OS side effects) ---
    let out = run(
        &home,
        &[
            "--format",
            "json",
            "fleet",
            "atc",
            "setup",
            "tower",
            "--interval",
            "10",
            "--idle-pause",
            "45",
            "--no-spawn",
            "--no-heartbeat",
        ],
    );
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(atc_dir.join("CLAUDE.md").is_file(), "CLAUDE.md not written");
    assert!(atc_dir.join("meta.json").is_file(), "meta.json not written");
    assert!(
        atc_dir.join("state.json").is_file(),
        "state.json not seeded"
    );
    assert!(
        atc_dir.join("task-log.md").is_file(),
        "task-log.md not seeded"
    );

    // CLAUDE.md carries the custom cadence + the instance name.
    let policy = std::fs::read_to_string(atc_dir.join("CLAUDE.md")).unwrap();
    assert!(policy.contains("# ATC — Air Traffic Control (tower)"));
    assert!(policy.contains("every 10 minutes"));
    assert!(policy.contains("after 45 minutes"));

    // setup JSON reports the knobs and that no session/timer was created.
    let setup_json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(setup_json["heartbeat_interval_min"], 10);
    assert_eq!(setup_json["idle_pause_min"], 45);
    assert_eq!(setup_json["heartbeat_enabled"], false);
    assert_eq!(setup_json["session_spawned"], false);

    // --- mutate state.json, then re-setup → idempotent, state preserved ---
    std::fs::write(atc_dir.join("state.json"), r#"{"sentinel":true}"#).unwrap();
    let out = run(
        &home,
        &[
            "fleet",
            "atc",
            "setup",
            "tower",
            "--no-spawn",
            "--no-heartbeat",
        ],
    );
    assert!(out.status.success(), "re-setup failed");
    let state = std::fs::read_to_string(atc_dir.join("state.json")).unwrap();
    assert!(
        state.contains("sentinel"),
        "re-setup clobbered accumulated state.json"
    );

    // --- status ---
    let out = run(
        &home,
        &["--format", "json", "fleet", "atc", "status", "tower"],
    );
    assert!(out.status.success(), "status failed");
    let status_json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status_json["name"], "tower");
    assert_eq!(status_json["tmux_session"], "tmux_tower");

    // --- list ---
    let out = run(&home, &["--format", "json", "fleet", "atc", "list"]);
    assert!(out.status.success(), "list failed");
    let list_json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = list_json["instances"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"tower"), "list missing tower: {names:?}");

    // --- teardown --purge → dir gone ---
    let out = run(&home, &["fleet", "atc", "teardown", "tower", "--purge"]);
    assert!(out.status.success(), "teardown failed");
    assert!(!atc_dir.exists(), "teardown --purge left the dir behind");

    // --- teardown again → clean no-op (idempotent) ---
    let out = run(&home, &["fleet", "atc", "teardown", "tower", "--purge"]);
    assert!(
        out.status.success(),
        "second teardown should be a clean no-op"
    );

    // status on the now-absent instance fails cleanly.
    let out = run(&home, &["fleet", "atc", "status", "tower"]);
    assert!(
        !out.status.success(),
        "status on a torn-down instance should fail"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Teardown must kill the running ATC session **non-interactively**: it has to
/// pass `--force` (otherwise `ainb kill` hits a `[y/N]` prompt, reads EOF,
/// cancels, and leaves the session alive) and it must report the *real* kill
/// result rather than blindly setting `killed=true`.
///
/// We make this hermetic by:
///  * spawning a real, uniquely-named tmux session that matches the session
///    name teardown targets (`tmux_<name>`), so the kill branch actually runs,
///  * pointing `AINB_BIN` at a fake `ainb` that records its argv to a file and
///    really kills that one tmux session by exact name (simulating a real kill).
/// We then assert teardown invoked `kill --force <name>` and reported
/// `session_killed: true`. A second pass with a fake that *fails* proves
/// `killed` reflects reality (false), not an unconditional `true`.
#[cfg(unix)]
#[test]
fn atc_teardown_kills_with_force_and_reports_real_result() {
    use std::os::unix::fs::PermissionsExt;

    // tmux is required for this hermetic test; skip cleanly if absent.
    if Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("tmux unavailable — skipping teardown --force test");
        return;
    }

    let uniq = format!("atcforce{}", std::process::id());
    let home = std::env::temp_dir().join(format!("atc-force-{}", std::process::id()));
    let atc_dir = home.join("atc").join(&uniq);
    let session = format!("tmux_{uniq}"); // what teardown will target

    // Always tear down the real tmux session we create, even on assert failure.
    struct TmuxGuard(String);
    impl Drop for TmuxGuard {
        fn drop(&mut self) {
            let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).status();
        }
    }
    let _guard = TmuxGuard(session.clone());

    // Provision instance files (no real timer, no real spawn).
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args([
            "fleet",
            "atc",
            "setup",
            &uniq,
            "--no-spawn",
            "--no-heartbeat",
        ])
        .output()
        .expect("setup");
    assert!(
        out.status.success(),
        "setup failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Spawn the real session teardown will detect + try to kill.
    let started = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "sleep", "300"])
        .status()
        .expect("spawn tmux session");
    assert!(started.success(), "could not start fake tmux session");

    // --- Pass 1: fake `ainb kill --force` that SUCCEEDS (really kills it) -----
    let argv_log = home.join("kill-argv.txt");
    let fake = home.join("fake-ainb-ok.sh");
    std::fs::write(
        &fake,
        format!(
            "#!/bin/sh\nprintf '%s ' \"$@\" >> '{}'\nprintf '\\n' >> '{}'\n\
# Simulate a real, forced kill of exactly our session.\n\
tmux kill-session -t '{}' 2>/dev/null\nexit 0\n",
            argv_log.display(),
            argv_log.display(),
            session,
        ),
    )
    .unwrap();
    let mut perms = std::fs::metadata(&fake).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake, perms).unwrap();

    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_BIN", &fake)
        .args(["--format", "json", "fleet", "atc", "teardown", &uniq])
        .output()
        .expect("teardown");
    assert!(
        out.status.success(),
        "teardown failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The fake was invoked with `kill --force <name>` — proves --force is passed.
    let argv = std::fs::read_to_string(&argv_log).unwrap_or_default();
    assert!(
        argv.contains("kill --force ") && argv.contains(&uniq),
        "kill was not invoked with --force <name>: {argv:?}"
    );

    // And teardown reported the real result: the session was actually killed.
    let td: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        td["session_killed"], true,
        "killed should reflect a successful kill"
    );

    // --- Pass 2: fake `ainb kill` that FAILS → killed must be false ----------
    // Re-create the session so the kill branch runs again.
    let started = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, "sleep", "300"])
        .status()
        .expect("respawn tmux session");
    assert!(started.success());

    let fake_fail = home.join("fake-ainb-fail.sh");
    std::fs::write(&fake_fail, "#!/bin/sh\nexit 1\n").unwrap();
    let mut perms = std::fs::metadata(&fake_fail).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&fake_fail, perms).unwrap();

    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_BIN", &fake_fail)
        .args(["--format", "json", "fleet", "atc", "teardown", &uniq])
        .output()
        .expect("teardown 2");
    assert!(
        out.status.success(),
        "teardown should not abort on kill failure"
    );
    let td: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        td["session_killed"], false,
        "killed must reflect the failed kill, not an unconditional true"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// The heartbeat verb is poll-mode: it shells `ainb fleet needs`, builds the
/// nudge, and (when the fleet is quiet long enough) idle-pauses. Here we point
/// `AINB_BIN` at a fake that returns an empty needs list, pre-age the
/// `last_active_ms` past the idle-pause window, and assert the heartbeat
/// reports `idle_paused: true` and stamps `last_heartbeat_ms`. No tmux/session
/// is involved (the session is not live → no send), so this is hermetic.
#[test]
fn atc_heartbeat_idle_pauses_when_fleet_quiet() {
    let home = std::env::temp_dir().join(format!("atc-hb-{}", std::process::id()));
    let atc_dir = home.join("atc").join("ctl");
    std::fs::create_dir_all(&atc_dir).unwrap();

    // A fake `ainb` that returns an empty needs JSON array for the
    // `--format json fleet needs --no-enrich` call the heartbeat makes.
    let fake = home.join("fake-ainb.sh");
    std::fs::write(&fake, "#!/bin/sh\necho '[]'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake, perms).unwrap();
    }

    // Provision files only (no real timer, no spawn), then pre-age state so the
    // idle-pause window (default 60m) has elapsed.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args([
            "fleet",
            "atc",
            "setup",
            "ctl",
            "--no-spawn",
            "--no-heartbeat",
        ])
        .output()
        .expect("setup");
    assert!(out.status.success(), "setup failed");

    // Idle-pause bookkeeping now lives in the heartbeat-owned file (separate
    // writer from the session's state.json). Pre-age last_active_ms there.
    let long_ago = 0i64; // epoch → guaranteed older than any idle-pause window
    std::fs::write(
        atc_dir.join("heartbeat-state.json"),
        format!(r#"{{"last_active_ms":{long_ago}}}"#),
    )
    .unwrap();

    // Fire the heartbeat with the fake ainb as the needs source.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_BIN", &fake)
        .args(["--format", "json", "fleet", "atc", "heartbeat", "ctl"])
        .output()
        .expect("heartbeat");
    assert!(
        out.status.success(),
        "heartbeat failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hb: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(hb["needs_count"], 0, "fake returned empty needs");
    assert_eq!(hb["idle_paused"], true, "should idle-pause on quiet fleet");
    assert_eq!(hb["delivered"], false, "paused → nothing delivered");

    // The heartbeat-owned file was updated with a fresh last_heartbeat_ms.
    let hb_state: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(atc_dir.join("heartbeat-state.json")).unwrap(),
    )
    .unwrap();
    assert!(
        hb_state["last_heartbeat_ms"].as_i64().unwrap() > 0,
        "heartbeat did not stamp last_heartbeat_ms"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// EVENT-DRIVEN PATH (success criterion 2): a child completion committed to the
/// parent's durable inbox is drained exactly-once and turned into a
/// `{"decision":"block",...}` decision, and the parent ATC heartbeat picks the
/// completion up via the inbox (not the poll-mode `fleet needs` scan).
///
/// All through the real `ainb` binary against a tempdir `AINB_HOME`:
///   1. `inbox commit` a child completion to parent "tower".
///   2. `inbox peek` shows it undrained.
///   3. The ATC heartbeat (fake empty `fleet needs`) still surfaces the
///      completion — proving it arrived event-driven, not from the poll scan.
///   4. A direct `inbox drain` returns the block decision exactly once; a second
///      drain returns nothing (exactly-once / no re-delivery).
#[test]
fn atc_event_driven_inbox_drains_exactly_once() {
    let home = std::env::temp_dir().join(format!("atc-evt-{}", std::process::id()));
    let atc_dir = home.join("atc").join("tower");
    std::fs::create_dir_all(&atc_dir).unwrap();

    // Fake `ainb` for the heartbeat's `fleet needs` shell-out → empty fleet, so
    // any completion the heartbeat surfaces MUST have come from the inbox.
    let fake = home.join("fake-ainb.sh");
    std::fs::write(&fake, "#!/bin/sh\necho '[]'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&fake).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&fake, perms).unwrap();
    }

    // Provision the ATC instance (no spawn / timer / settings hooks).
    let out = run(
        &home,
        &[
            "fleet",
            "atc",
            "setup",
            "tower",
            "--no-spawn",
            "--no-heartbeat",
            "--no-hooks",
        ],
    );
    assert!(out.status.success(), "setup failed");

    // 1. A child finishes → commit its completion to tower's inbox.
    let out = run(
        &home,
        &[
            "--format",
            "json",
            "fleet",
            "atc",
            "inbox",
            "commit",
            "tower",
            "--child",
            "worker-7",
            "--summary",
            "merged the PR",
        ],
    );
    assert!(out.status.success(), "inbox commit failed");
    let committed: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        committed["routed"], true,
        "should route to live parent inbox"
    );

    // 2. peek shows it undrained (no consume).
    let out = run(
        &home,
        &["--format", "json", "fleet", "atc", "inbox", "peek", "tower"],
    );
    assert!(out.status.success());
    let peeked: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(peeked.as_array().unwrap().len(), 1);
    assert_eq!(peeked[0]["child_id"], "worker-7");

    // 3. The ATC heartbeat surfaces the completion via the inbox (NOT the poll
    //    scan — fake `fleet needs` returns []). Pre-age idle so a poll-only run
    //    would idle-pause; the completion must override that.
    std::fs::write(
        atc_dir.join("heartbeat-state.json"),
        r#"{"last_active_ms":0}"#,
    )
    .unwrap();
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_BIN", &fake)
        .args(["--format", "json", "fleet", "atc", "heartbeat", "tower"])
        .output()
        .expect("heartbeat");
    assert!(
        out.status.success(),
        "heartbeat failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let hb: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    // Poll scan was empty, yet the heartbeat did not idle-pause: the inbox
    // completion woke it. Delivery is false because there is no live tmux
    // session.
    assert_eq!(hb["needs_count"], 0, "poll scan empty");
    assert_eq!(
        hb["idle_paused"], false,
        "inbox completion must override idle-pause"
    );
    assert_eq!(hb["delivered"], false, "no live session → not delivered");

    // C-A1 (data-loss regression): because the session was NOT live, the
    // heartbeat must NOT have drained the inbox. Draining before a confirmed
    // delivery would consume + mark the completion's fingerprint and lose it
    // forever. The completion MUST still be pending for a later, deliverable
    // firing. (The pre-fix code drained unconditionally here — exactly the bug.)
    let out = run(
        &home,
        &["--format", "json", "fleet", "atc", "inbox", "peek", "tower"],
    );
    let peeked: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        peeked.as_array().unwrap().len(),
        1,
        "dead-session heartbeat must RETAIN the completion, not drain it: {peeked}"
    );
    assert_eq!(peeked[0]["child_id"], "worker-7");

    // Explicitly drain worker-7 via the CLI so the rest of the flow starts clean.
    let out = run(
        &home,
        &[
            "--format", "json", "fleet", "atc", "inbox", "drain", "tower",
        ],
    );
    assert!(out.status.success(), "explicit drain of worker-7 failed");

    // 4. Commit a fresh completion and drain it directly → block decision once.
    let out = run(
        &home,
        &[
            "fleet",
            "atc",
            "inbox",
            "commit",
            "tower",
            "--child",
            "worker-9",
            "--summary",
            "ran the tests",
        ],
    );
    assert!(out.status.success());
    let out = run(
        &home,
        &[
            "--format", "json", "fleet", "atc", "inbox", "drain", "tower",
        ],
    );
    assert!(out.status.success(), "drain failed");
    let drained: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(drained["drained"].as_array().unwrap().len(), 1);
    assert_eq!(drained["decision"]["decision"], "block");
    assert!(
        drained["decision"]["reason"].as_str().unwrap().contains("worker-9"),
        "block reason must carry the completion: {}",
        drained["decision"]["reason"]
    );

    // Second drain → nothing (exactly-once: no re-delivery).
    let out = run(
        &home,
        &[
            "--format", "json", "fleet", "atc", "inbox", "drain", "tower",
        ],
    );
    let drained2: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        drained2["drained"].as_array().unwrap().is_empty(),
        "completion re-delivered on second drain: {drained2}"
    );
    assert!(drained2["decision"].is_null(), "empty inbox must not block");

    let _ = std::fs::remove_dir_all(&home);
}

/// HOOK + DEAD-LETTER (success criterion 2): the `ainb fleet atc hook` verb —
/// the Rust side of the installed hook script — appends a canonical event line
/// to `events.jsonl` on every (fleet-member) event, routes a child's Stop
/// completion to its parent's inbox, and dead-letters a Stop completion whose
/// parent is unresolvable.
#[test]
fn atc_hook_writes_status_and_routes_completion() {
    let home = std::env::temp_dir().join(format!("atc-hook-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    // Read every canonical line from <home>/events.jsonl (empty when absent).
    let read_events = |home: &std::path::Path| -> Vec<serde_json::Value> {
        std::fs::read_to_string(home.join("events.jsonl"))
            .ok()
            .map(|s| {
                s.lines()
                    .filter(|l| !l.trim().is_empty())
                    .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                    .collect()
            })
            .unwrap_or_default()
    };

    // A genuine user turn appends a UserPromptSubmit event line. The append is
    // now GATED on fleet membership (H1), so we pass the child's parent env —
    // exactly as `ainb run --parent tower` seeds it — which makes `child-a` a
    // resolvable fleet member and a legitimate appender. (An UNRELATED session
    // with no parent / no inbox appends nothing; asserted separately below.)
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_PARENT_SESSION", "tower")
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "UserPromptSubmit",
            "--session-id",
            "child-a",
        ])
        .output()
        .expect("hook");
    assert!(out.status.success(), "hook (UserPromptSubmit) failed");
    let events = read_events(&home);
    assert_eq!(
        events.len(),
        1,
        "UserPromptSubmit must append one event line"
    );
    assert_eq!(events[0]["event_type"], "UserPromptSubmit");
    assert_eq!(events[0]["session_id"], "child-a");
    assert_eq!(events[0]["parent"], "tower");

    // A Stop with a parent (via AINB_PARENT_SESSION) routes a completion to the
    // parent's inbox, and appends a Stop event line.
    let payload = r#"{"hook_event_name":"Stop","last_assistant_message":"finished task","transcript_path":"/t/child-a.jsonl"}"#;
    let mut child = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_PARENT_SESSION", "tower")
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "Stop",
            "--session-id",
            "child-a",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn hook");
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().expect("hook stop");
    assert!(out.status.success(), "hook (Stop) failed");
    let events = read_events(&home);
    assert_eq!(events.len(), 2, "Stop must append a second event line");
    let stop = events.iter().find(|e| e["event_type"] == "Stop").expect("Stop line present");
    assert_eq!(stop["session_id"], "child-a");
    assert_eq!(stop["transcript_path"], "/t/child-a.jsonl");

    // The completion landed in tower's inbox (drain it via the CLI).
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args(["--format", "json", "fleet", "atc", "inbox", "peek", "tower"])
        .output()
        .expect("peek");
    let peeked: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        peeked.as_array().unwrap().len(),
        1,
        "completion not routed to parent"
    );
    assert_eq!(peeked[0]["child_id"], "child-a");

    // A Stop with NO resolvable parent does ZERO durable routing writes (H3):
    // the hooks are installed host-wide, so an unrelated session's Stop must not
    // grow the dead-letter sink. No commit, no dead-letter.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "Stop",
            "--session-id",
            "orphan-b",
        ])
        .output()
        .expect("hook orphan");
    assert!(out.status.success());
    let dl = home.join("inbox").join("dead-letter.jsonl");
    assert!(
        !dl.exists(),
        "orphan Stop with no resolvable parent must not dead-letter (host-wide growth)"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Regression (H3): the lifecycle hooks live in the GLOBAL settings.json, so
/// every Claude session on the host runs `ainb fleet atc hook`. A Stop for a
/// session with no resolvable parent and no inbox must do ZERO durable routing
/// writes — no commit, no dead-letter — otherwise the dead-letter sink grows
/// unbounded on every unrelated host session. A Stop WITH a resolvable parent
/// still routes the completion to that parent's inbox.
#[test]
fn atc_hook_unlinked_stop_writes_no_dead_letter_but_linked_routes() {
    let home = std::env::temp_dir().join(format!("atc-h3-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let dl = home.join("inbox").join("dead-letter.jsonl");

    // (1) Unlinked Stop (no AINB_PARENT_SESSION, no durable map entry) →
    //     no dead-letter, no commit.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "Stop",
            "--session-id",
            "unrelated-host-session",
        ])
        .output()
        .expect("hook unlinked stop");
    assert!(out.status.success(), "unlinked Stop should still exit 0");
    assert!(
        !dl.exists(),
        "unlinked Stop must not create a dead-letter file"
    );

    // (2) Linked Stop (resolvable parent via env) → routes to the parent inbox.
    let payload = r#"{"hook_event_name":"Stop","last_assistant_message":"linked work done"}"#;
    let mut child = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_PARENT_SESSION", "tower")
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "Stop",
            "--session-id",
            "linked-child",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn linked hook");
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().expect("linked hook stop");
    assert!(out.status.success(), "linked Stop failed");

    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args(["--format", "json", "fleet", "atc", "inbox", "peek", "tower"])
        .output()
        .expect("peek");
    let peeked: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        peeked.as_array().unwrap().len(),
        1,
        "linked Stop must route the completion to the parent inbox"
    );
    assert_eq!(peeked[0]["child_id"], "linked-child");
    // The linked Stop still must not dead-letter.
    assert!(
        !dl.exists(),
        "a routed completion must not also dead-letter"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Regression (H2): the durable child→parent map must be keyed by the id the
/// HOOK reports (the Claude-minted session id), not ainb's spawn-time Uuid which
/// the hook never sees. The hook self-registers from AINB_PARENT_SESSION, so
/// after one hook invocation the map resolves the hook-observed child id to its
/// parent — and a later lookup with the env CLEARED (restart / resume) still
/// resolves via that durable map.
#[test]
fn atc_hook_self_registers_durable_parent_map_under_hook_observed_id() {
    use ainb::fleet::plumbing::parent::load_map_in;
    use ainb::fleet::plumbing::resolve_parent_in;

    let home = std::env::temp_dir().join(format!("atc-h2-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    // Run the hook once with the live env parent + a hook-observed child id.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_PARENT_SESSION", "tower")
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "SessionStart",
            "--session-id",
            "claude-minted-id",
        ])
        .output()
        .expect("hook session start");
    assert!(out.status.success(), "hook (SessionStart) failed");

    // The durable map is now keyed by the id the hook actually reported.
    let map = load_map_in(&home);
    assert_eq!(
        map.get("claude-minted-id").map(String::as_str),
        Some("tower"),
        "hook must self-register the durable map under the hook-observed id: {map:?}"
    );

    // And with the env CLEARED (None), resolution still succeeds via the map —
    // proving the restart-safe fallback now actually works.
    assert_eq!(
        resolve_parent_in(&home, "claude-minted-id", None),
        Some("tower".to_string()),
        "durable fallback must resolve after env loss"
    );

    let _ = std::fs::remove_dir_all(&home);
}

/// Phase 5 (conservative exactly-once): the event store OBSERVES completions but
/// is NOT the authoritative delivery channel. This proves two independent facts
/// from a single child completion:
///
///   1. The proven inbox/drain exactly-once is UNCHANGED — drain #1 delivers the
///      `{"decision":"block"}` completion, drain #2 returns nothing (no
///      re-delivery). This is the authoritative parent/child completion path.
///   2. The SAME completion is also visible in the event-sourced `current_state`
///      table as `DONE` — derived by ingesting the hook's `events.jsonl` (the
///      durable record) and running the notifyd materializer — WITHOUT the event
///      store touching, gating, or otherwise affecting the inbox delivery in (1).
///
/// The two surfaces are decoupled: the inbox file is the delivery ledger; the
/// event store is read-only observability. Migrating readers onto `current_state`
/// (Wave 4) did not weaken the exactly-once guarantee.
#[test]
fn exactly_once_drain_unchanged_and_completion_observed_in_current_state() {
    use ainb_plugin_notifyd::{Store, ingest, transition};

    let home = std::env::temp_dir().join(format!("atc-p5-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    // A child finishes: fire its Stop hook with a resolvable parent ("tower").
    // This (a) commits a last-wins completion to tower's inbox and (b) appends a
    // Stop line to events.jsonl — the two writes the rest of the test inspects.
    let stop_payload = r#"{"hook_event_name":"Stop","last_assistant_message":"shipped the feature","transcript_path":"/t/worker-9.jsonl","cwd":"/repo/worker-9"}"#;
    let mut child = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_PARENT_SESSION", "tower")
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "Stop",
            "--session-id",
            "worker-9",
            "--cwd",
            "/repo/worker-9",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn Stop hook");
    {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(stop_payload.as_bytes()).unwrap();
    }
    assert!(child.wait_with_output().expect("Stop hook").status.success());

    // The session then ENDS — fire SessionEnd so the materialized lifecycle is
    // the terminal DONE the readers/TUI surface for a finished session.
    let end_payload = r#"{"hook_event_name":"SessionEnd","reason":"clear","cwd":"/repo/worker-9"}"#;
    let mut ender = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .env("AINB_PARENT_SESSION", "tower")
        .args([
            "fleet",
            "atc",
            "hook",
            "--event",
            "SessionEnd",
            "--session-id",
            "worker-9",
            "--cwd",
            "/repo/worker-9",
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("spawn SessionEnd hook");
    {
        use std::io::Write;
        ender.stdin.take().unwrap().write_all(end_payload.as_bytes()).unwrap();
    }
    assert!(ender.wait().expect("SessionEnd hook").success());

    // --- (2) The event store OBSERVES the completion as DONE --------------------
    // Ingest the hook's durable events.jsonl into a fresh notifyd store and run
    // the materializer — exactly what the notifyd daemon does in production. This
    // is a pure READ of the same events.jsonl; it does not touch the inbox.
    let events_jsonl = home.join("events.jsonl");
    assert!(
        events_jsonl.exists(),
        "hook must have appended events.jsonl"
    );
    let db = home.join("notifications.db");
    let store = Store::open(&db).expect("open notifyd store");
    let summary = ingest::ingest_once(&store, &events_jsonl).expect("ingest events.jsonl");
    assert!(
        summary.events_ingested >= 2,
        "Stop + SessionEnd must be ingested, got {summary:?}"
    );
    let now_ms = chrono::Utc::now().timestamp_millis();
    transition::materialize_at(&store, now_ms, 5).expect("materialize current_state");
    let state = store
        .get_current_state("worker-9", "/repo/worker-9")
        .expect("query current_state")
        .expect("worker-9 has a current_state row");
    assert_eq!(
        state.kind, "DONE",
        "a completed session must materialize as DONE in current_state"
    );
    assert_eq!(state.source, "hook");

    // --- (1) The inbox exactly-once delivery is UNCHANGED -----------------------
    // drain #1 delivers the completion as the block decision.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args([
            "--format", "json", "fleet", "atc", "inbox", "drain", "tower",
        ])
        .output()
        .expect("drain #1");
    assert!(out.status.success(), "drain #1 failed");
    let drained1: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        drained1["drained"].as_array().unwrap().len(),
        1,
        "drain #1 must deliver the single completion: {drained1}"
    );
    assert_eq!(drained1["decision"]["decision"], "block");
    assert!(
        drained1["decision"]["reason"].as_str().unwrap().contains("worker-9"),
        "block reason must reference the finished child: {}",
        drained1["decision"]["reason"]
    );

    // drain #2 returns nothing — exactly-once, no re-delivery. The event store
    // showing DONE above did NOT cause a second delivery.
    let out = Command::new(ainb_bin())
        .env("AINB_HOME", &home)
        .args([
            "--format", "json", "fleet", "atc", "inbox", "drain", "tower",
        ])
        .output()
        .expect("drain #2");
    let drained2: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        drained2["drained"].as_array().unwrap().is_empty(),
        "completion re-delivered on second drain — exactly-once regressed: {drained2}"
    );
    assert!(
        drained2["decision"].is_null(),
        "an empty inbox must not block"
    );

    // current_state is read-only observability: re-querying after both drains
    // still shows DONE, and draining never mutated it.
    let state_after = store
        .get_current_state("worker-9", "/repo/worker-9")
        .expect("query current_state after drains")
        .expect("row still present");
    assert_eq!(
        state_after.kind, "DONE",
        "the event store is independent of the inbox: DONE survives both drains"
    );

    let _ = std::fs::remove_dir_all(&home);
}
