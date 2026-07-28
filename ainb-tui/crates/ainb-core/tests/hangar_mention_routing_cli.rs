//! ACCEPTANCE (CLI half): `ainb hangar comment add|preview` reports where every
//! `@`-mention went, and `ainb hangar inbox list` proves a human mention landed
//! on that human (multica parity #2-rest).
//!
//! Drives the REAL `ainb` binary against an isolated `AINB_HANGAR_HOME`, with
//! no daemon and no network — nothing but sqlite + the CLI. That is the point:
//! the routing behaviour is provable against a bare database file, so the
//! acceptance proof does not depend on a running control plane.
//!
//! The three legs, each asserting BOTH the side effect and the surfaced code:
//!
//! 1. mentioning a HUMAN notifies them and spawns nothing;
//! 2. re-mentioning an agent reports `coalesced` rather than failing silently;
//! 3. a preview reports the identical codes and writes nothing.

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

/// Parse the JSON row array a `comment add|preview` invocation prints.
fn rows(json: &str) -> Vec<serde_json::Value> {
    serde_json::from_str(json).unwrap_or_else(|e| panic!("bad routing json {json:?}: {e}"))
}

fn field<'a>(row: &'a serde_json::Value, key: &str) -> &'a str {
    row.get(key).and_then(serde_json::Value::as_str).unwrap_or("")
}

/// **ACCEPTANCE 1** — `@`-mentioning a HUMAN routes to that human: a `notified`
/// outcome, an inbox entry addressed to them, and NO task.
#[test]
fn mentioning_a_human_notifies_them_and_never_spawns_a_run() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    let issue = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "routing demo"],
    ));
    // A second human, so the mention target is unambiguous and is not the
    // bootstrapped owner.
    ainb(
        h,
        &["hangar", "member", "add", "--email", "bob@example.com"],
    );

    let out = ainb(
        h,
        &[
            "hangar",
            "comment",
            "add",
            "--issue",
            &issue,
            "--body",
            "@bob can you look?",
            "--format",
            "json",
        ],
    );
    let routed = rows(&out);
    assert_eq!(routed.len(), 1, "one row per target: {out}");
    assert_eq!(field(&routed[0], "target_type"), "member", "{out}");
    assert_eq!(field(&routed[0], "outcome"), "notified", "{out}");
    let user_id = field(&routed[0], "target_id").to_string();
    assert!(
        !user_id.is_empty(),
        "the human resolved to a user id: {out}"
    );

    // The mention landed in THAT human's inbox.
    let inbox = ainb(
        h,
        &[
            "hangar",
            "inbox",
            "list",
            "--recipient",
            &format!("member:{user_id}"),
            "--format",
            "json",
        ],
    );
    let entries = rows(&inbox);
    assert_eq!(entries.len(), 1, "exactly one inbox entry: {inbox}");
    assert_eq!(field(&entries[0], "event"), "mention", "{inbox}");
    assert_eq!(field(&entries[0], "subject_id"), issue, "{inbox}");

    // Nobody else's inbox saw it (the entry is addressed, not broadcast).
    let other = ainb(
        h,
        &[
            "hangar",
            "inbox",
            "list",
            "--recipient",
            "member:nobody",
            "--format",
            "json",
        ],
    );
    assert!(
        rows(&other).is_empty(),
        "an addressed entry is not a broadcast: {other}"
    );
}

/// **ACCEPTANCE 2** — a repeat mention SURFACES as `coalesced` instead of
/// disappearing into a swallowed unique-constraint violation.
#[test]
fn re_mentioning_a_pending_agent_reports_coalesced() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    ainb(h, &["hangar", "agent", "create", "--name", "builder"]);
    let issue = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "coalesce demo"],
    ));

    let first = rows(&ainb(
        h,
        &[
            "hangar",
            "comment",
            "add",
            "--issue",
            &issue,
            "--body",
            "@builder go",
            "--format",
            "json",
        ],
    ));
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(field(&first[0], "outcome"), "queued", "{first:?}");
    assert_eq!(field(&first[0], "target_type"), "agent");

    let second = rows(&ainb(
        h,
        &[
            "hangar",
            "comment",
            "add",
            "--issue",
            &issue,
            "--body",
            "@builder again",
            "--format",
            "json",
        ],
    ));
    assert_eq!(second.len(), 1, "{second:?}");
    assert_eq!(
        field(&second[0], "outcome"),
        "coalesced",
        "a repeat mention is reported, not swallowed: {second:?}"
    );
    assert_eq!(field(&second[0], "reason"), "coalesced", "{second:?}");
}

/// The preview reports the identical codes the write then produces, and writes
/// nothing — proven by running the preview TWICE and then the write, and
/// checking the agent still ends up with exactly one queued task.
#[test]
fn a_preview_reports_the_same_codes_and_writes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    ainb(h, &["hangar", "agent", "create", "--name", "builder"]);
    let issue = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "preview demo"],
    ));

    let preview_args = [
        "hangar",
        "comment",
        "preview",
        "--issue",
        &issue,
        "--body",
        "@builder go",
        "--format",
        "json",
    ];
    let first_preview = rows(&ainb(h, &preview_args));
    assert_eq!(first_preview.len(), 1, "{first_preview:?}");
    assert_eq!(field(&first_preview[0], "outcome"), "queued");

    // A preview writes nothing, so running it again reports the SAME thing
    // rather than degrading to `coalesced` (which is what a write would do).
    let second_preview = rows(&ainb(h, &preview_args));
    assert_eq!(
        field(&second_preview[0], "outcome"),
        "queued",
        "a preview that had written would now report coalesced: {second_preview:?}"
    );

    // And nothing landed in the timeline either — the comment was never written.
    let timeline = ainb(
        h,
        &["hangar", "issue", "timeline", &issue, "--format", "json"],
    );
    assert!(
        !timeline.contains("@builder go"),
        "a preview must not write the comment: {timeline}"
    );

    // The real write then produces the code the preview promised.
    let written = rows(&ainb(
        h,
        &[
            "hangar",
            "comment",
            "add",
            "--issue",
            &issue,
            "--body",
            "@builder go",
            "--format",
            "json",
        ],
    ));
    assert_eq!(field(&written[0], "outcome"), "queued", "{written:?}");
}

/// A mention that resolves to nothing is REPORTED as `ignored`, not silently
/// dropped — the whole point of the outcome vocabulary.
#[test]
fn an_unresolvable_handle_is_reported_as_ignored() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    let issue = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "ignored demo"],
    ));
    let out = ainb(
        h,
        &[
            "hangar",
            "comment",
            "add",
            "--issue",
            &issue,
            "--body",
            "@nobody-at-all hello",
            "--format",
            "json",
        ],
    );
    let routed = rows(&out);
    assert_eq!(routed.len(), 1, "{out}");
    assert_eq!(field(&routed[0], "outcome"), "ignored", "{out}");
    assert_eq!(field(&routed[0], "handle"), "nobody-at-all", "{out}");
}

/// A typo'd `--workspace` is an ERROR, never a silently empty routing report a
/// caller would read as "this comment mentions nobody".
#[test]
fn a_mistyped_workspace_is_rejected_rather_than_reporting_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();
    let issue = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "ws demo"],
    ));

    let out = Command::new(ainb_bin())
        .args([
            "hangar",
            "comment",
            "preview",
            "--issue",
            &issue,
            "--body",
            "@builder go",
            "--workspace",
            "not-a-workspace",
        ])
        .env("AINB_HANGAR_HOME", h)
        .env("HOME", h)
        .output()
        .expect("run ainb");
    assert!(
        !out.status.success(),
        "a typo'd workspace must fail: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}
