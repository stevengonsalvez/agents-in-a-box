//! ACCEPTANCE (CLI half): `ainb hangar issue why` explains a non-dispatch
//! (multica parity #12).
//!
//! Drives the REAL `ainb` binary against an isolated `AINB_HANGAR_HOME`, with
//! no daemon and no network — nothing but sqlite + the CLI. It reproduces the
//! exact silent hole item 12 closes:
//!
//!   `enqueue_assigned_task` returned a bare `Ok(None)` when a card had no repo
//!   pinned, so `issue update --assign <agent>` printed "updated issue" and
//!   nothing ran, with no record and no message anywhere.
//!
//! Now the same flow records `target_unavailable`, `issue why` prints it, and
//! `issue show` states it. Delete the `record_cli_dispatch_attempt` call from
//! the no-repo branch and this test goes RED.

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Run one `ainb hangar …` invocation inside the isolated home, returning stdout.
fn ainb(home: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .env("HOME", home)
        .output()
        .expect("run ainb");
    assert!(
        out.status.success(),
        "`ainb {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The `created issue <ULID>` ack's id.
fn created_id(ack: &str) -> String {
    ack.split_whitespace()
        .last()
        .unwrap_or_else(|| panic!("no id in ack {ack:?}"))
        .to_string()
}

#[test]
fn assigning_an_agent_to_a_repoless_card_records_and_explains_the_non_dispatch() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    // A card with NO repo pinned — the case that used to vanish into `Ok(None)`.
    let issue_id = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "RepolessCard"],
    ));
    ainb(h, &["hangar", "agent", "create", "--name", "bot"]);

    // Before the assign there is nothing to explain.
    let empty = ainb(
        h,
        &["hangar", "issue", "why", &issue_id, "--format", "json"],
    );
    assert_eq!(empty.trim(), "[]", "no attempts before any dispatch");

    // Resolve the agent id from the CLI's own listing (the create ack prints the
    // name, not the id) — still no daemon, still the real binary.
    let agents = ainb(h, &["hangar", "agent", "list", "--format", "json"]);
    let agents: serde_json::Value = serde_json::from_str(&agents).expect("agent list json");
    let agent_id = agents
        .as_array()
        .and_then(|a| a.first())
        .and_then(|a| a.get("id"))
        .and_then(serde_json::Value::as_str)
        .expect("one agent listed")
        .to_string();

    // The assign SUCCEEDS (the edit is committed) but dispatches nothing.
    let updated = ainb(
        h,
        &[
            "hangar", "issue", "update", &issue_id, "--assign", &agent_id,
        ],
    );
    assert!(updated.contains("updated issue"), "{updated}");

    // …and that is now RECORDED, with the stable code.
    let json = ainb(
        h,
        &["hangar", "issue", "why", &issue_id, "--format", "json"],
    );
    let rows: serde_json::Value = serde_json::from_str(&json).expect("why json");
    let rows = rows.as_array().expect("array");
    assert_eq!(rows.len(), 1, "exactly one attempt: {json}");
    let row = &rows[0];
    assert_eq!(
        row.get("reason").and_then(serde_json::Value::as_str),
        Some("target_unavailable")
    );
    assert_eq!(
        row.get("detail").and_then(serde_json::Value::as_str),
        Some("no repo pinned on this card")
    );
    assert_eq!(
        row.get("source").and_then(serde_json::Value::as_str),
        Some("assign"),
        "the trigger surface is recorded, not just the code"
    );
    assert!(
        row.get("task_id").is_some_and(serde_json::Value::is_null),
        "no task was enqueued: {json}"
    );
    assert_eq!(
        row.get("issue_id").and_then(serde_json::Value::as_str),
        Some(issue_id.as_str())
    );

    // The text surface says the same thing.
    let text = ainb(h, &["hangar", "issue", "why", &issue_id]);
    assert!(text.contains("target_unavailable"), "{text}");
    assert!(text.contains("no repo pinned on this card"), "{text}");
    assert!(text.contains("assign"), "{text}");

    // And `issue show` states it inline, so the operator does not need to know
    // `why` exists to find out.
    let show = ainb(h, &["hangar", "issue", "show", &issue_id]);
    assert!(
        show.contains("Not dispatched: target_unavailable — no repo pinned on this card"),
        "issue show must state the decline: {show}"
    );

    // `--alias dispatch-log` reaches the same verb.
    let aliased = ainb(h, &["hangar", "issue", "dispatch-log", &issue_id]);
    assert!(aliased.contains("target_unavailable"), "{aliased}");
}

/// A healthy card says nothing extra: `issue show` is byte-unchanged from
/// pre-#12 for a card that never had a declined dispatch. The negative twin, so
/// the line above is proven conditional rather than always-on.
#[test]
fn a_card_with_no_declined_dispatch_shows_no_extra_line() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();
    let issue_id = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "QuietCard"],
    ));

    let show = ainb(h, &["hangar", "issue", "show", &issue_id]);
    assert!(
        !show.contains("Not dispatched"),
        "an untouched card shows no dispatch line: {show}"
    );
    let why = ainb(h, &["hangar", "issue", "why", &issue_id]);
    assert!(
        why.contains("no dispatch attempts recorded"),
        "an untouched card explains itself as having no history: {why}"
    );
}
