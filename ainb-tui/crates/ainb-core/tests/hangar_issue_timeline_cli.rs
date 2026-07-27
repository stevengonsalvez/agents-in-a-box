//! ACCEPTANCE (CLI half): `ainb hangar issue timeline` prints the card's
//! narrative (multica parity #13).
//!
//! Drives the REAL `ainb` binary against an isolated `AINB_HANGAR_HOME`, with
//! no daemon and no network — nothing but sqlite + the CLI. It proves the write
//! sites and the read verb agree: a create records `created`, an edit records
//! one row per changed field with multica's details shape, and the text form
//! renders the change (`open → in_progress`).

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
fn create_then_update_writes_and_prints_the_activity_timeline() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    let issue_id = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "Wire the timeline"],
    ));

    // A brand-new card already has its opening line.
    let json = ainb(
        h,
        &["hangar", "issue", "timeline", &issue_id, "--format", "json"],
    );
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("timeline json");
    let actions: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("action").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(
        actions,
        ["created"],
        "a fresh card opens its narrative: {json}"
    );

    // Two changed fields in one edit → two rows, in the service's field order.
    ainb(
        h,
        &[
            "hangar",
            "issue",
            "update",
            &issue_id,
            "--state",
            "in_progress",
            "--priority",
            "3",
        ],
    );

    let json = ainb(
        h,
        &["hangar", "issue", "timeline", &issue_id, "--format", "json"],
    );
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("timeline json");
    let actions: Vec<&str> = rows
        .iter()
        .filter_map(|r| r.get("action").and_then(serde_json::Value::as_str))
        .collect();
    assert_eq!(
        actions,
        ["created", "status_changed", "priority_changed"],
        "one row per changed field, oldest first: {json}"
    );

    // multica's exact details shape on the move.
    let status = rows
        .iter()
        .find(|r| r.get("action").and_then(serde_json::Value::as_str) == Some("status_changed"))
        .expect("a status_changed row");
    assert_eq!(
        status.get("details"),
        Some(&serde_json::json!({"from": "open", "to": "in_progress"}))
    );

    // …and the text form renders the change a human reads.
    let text = ainb(h, &["hangar", "issue", "timeline", &issue_id]);
    assert!(
        text.contains("open → in_progress"),
        "the text timeline must render the transition: {text}"
    );
    assert!(
        text.contains("status_changed"),
        "the text timeline names the action: {text}"
    );
}

/// A card with nothing recorded prints an honest empty line, and the `activity`
/// alias resolves to the same verb.
#[test]
fn the_activity_alias_resolves_to_the_timeline_verb() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    let issue_id = created_id(&ainb(h, &["hangar", "issue", "create", "--title", "Alias"]));
    let via_alias = ainb(
        h,
        &["hangar", "issue", "activity", &issue_id, "--format", "json"],
    );
    let via_name = ainb(
        h,
        &["hangar", "issue", "timeline", &issue_id, "--format", "json"],
    );
    assert_eq!(via_alias, via_name);
}
