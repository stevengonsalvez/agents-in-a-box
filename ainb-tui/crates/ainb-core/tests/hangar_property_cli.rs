//! ACCEPTANCE (CLI half): define a custom property, set it on an issue, it
//! persists and renders (multica parity #17).
//!
//! Drives the REAL `ainb` binary against an isolated `AINB_HANGAR_HOME`, with
//! no daemon and no network — nothing but sqlite + the CLI. The load-bearing
//! assertions are (a) `issue show` prints the property under its CURRENT
//! display name, (b) the stored bag is keyed by the DEFINITION ID rather than
//! the key or the name, so a rename touches zero issue rows, and (c) archiving
//! stops the render without deleting the value.

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

/// Run one invocation expected to FAIL, returning combined output.
fn ainb_fails(home: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .env("HOME", home)
        .output()
        .expect("run ainb");
    assert!(
        !out.status.success(),
        "`ainb {}` unexpectedly succeeded: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stdout)
    );
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The `created issue <ULID>` ack's id.
fn created_id(ack: &str) -> String {
    ack.split_whitespace()
        .last()
        .unwrap_or_else(|| panic!("no id in ack {ack:?}"))
        .to_string()
}

/// Read the issue's raw `properties` blob straight out of the sqlite file the
/// CLI just wrote, so the id-keying claim is checked against DISK.
fn stored_properties(
    home: &std::path::Path,
    issue_id: &str,
) -> serde_json::Map<String, serde_json::Value> {
    let db = home.join("hangar.db");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build a runtime for the raw read");
    let raw = rt.block_on(async {
        let opts = sqlx::sqlite::SqliteConnectOptions::new().filename(&db).create_if_missing(false);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .connect_with(opts)
            .await
            .expect("open the CLI's own database file");
        let raw: String = sqlx::query_scalar("SELECT properties FROM issue WHERE id = ?")
            .bind(issue_id)
            .fetch_one(&pool)
            .await
            .expect("read the stored bag");
        pool.close().await;
        raw
    });
    serde_json::from_str(&raw).expect("the bag is a JSON object")
}

#[test]
fn define_set_show_persists_by_definition_id_and_survives_a_rename() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    ainb(
        h,
        &[
            "hangar", "property", "define", "--key", "sprint", "--name", "Sprint", "--kind",
            "select", "--option", "S1", "--option", "S2",
        ],
    );
    let listed = ainb(h, &["hangar", "property", "list"]);
    assert!(listed.contains("sprint"), "the catalog lists it: {listed}");
    assert!(listed.contains("select"), "with its kind: {listed}");
    assert!(listed.contains("active"), "and its state: {listed}");

    let issue_id = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "Ship #17"],
    ));
    ainb(
        h,
        &[
            "hangar", "issue", "property", "set", &issue_id, "--key", "sprint", "--value", "S2",
        ],
    );

    // RENDERS.
    let shown = ainb(h, &["hangar", "issue", "show", &issue_id]);
    assert!(shown.contains("Properties:"), "the section: {shown}");
    assert!(shown.contains("Sprint: S2"), "the resolved value: {shown}");

    // PERSISTS, keyed by the DEFINITION ID — never by the key or the name.
    let bag = stored_properties(h, &issue_id);
    assert_eq!(bag.len(), 1, "one stored value: {bag:?}");
    let def_id = bag.keys().next().unwrap().clone();
    assert_ne!(def_id, "sprint", "not keyed by the slug: {bag:?}");
    assert_ne!(def_id, "Sprint", "not keyed by the name: {bag:?}");
    assert_eq!(bag[&def_id], serde_json::json!("S2"), "{bag:?}");

    // A RENAME is a catalog-only write: the blob is byte-identical.
    ainb(
        h,
        &[
            "hangar",
            "property",
            "define",
            "--key",
            "sprint",
            "--name",
            "Iteration",
        ],
    );
    assert_eq!(
        stored_properties(h, &issue_id),
        bag,
        "a rename must touch ZERO issue rows"
    );
    let shown = ainb(h, &["hangar", "issue", "show", &issue_id]);
    assert!(
        shown.contains("Iteration: S2"),
        "renders under the new label: {shown}"
    );
    assert!(
        !shown.contains("Sprint: S2"),
        "the old label is gone: {shown}"
    );

    // ARCHIVE stops the render but never deletes the value.
    ainb(h, &["hangar", "property", "archive", "--key", "sprint"]);
    let shown = ainb(h, &["hangar", "issue", "show", &issue_id]);
    assert!(
        !shown.contains("Iteration: S2"),
        "archived ⇒ not rendered: {shown}"
    );
    assert_eq!(
        stored_properties(h, &issue_id),
        bag,
        "archive is NEVER a delete"
    );
    ainb(
        h,
        &[
            "hangar",
            "property",
            "archive",
            "--key",
            "sprint",
            "--unarchive",
        ],
    );
    let shown = ainb(h, &["hangar", "issue", "show", &issue_id]);
    assert!(
        shown.contains("Iteration: S2"),
        "un-archive restores it: {shown}"
    );

    // `--format json` parses and carries the catalog fields.
    let json = ainb(h, &["hangar", "property", "list", "--format", "json"]);
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("catalog json");
    assert_eq!(rows.len(), 1, "{json}");
    assert_eq!(rows[0]["key"], "sprint", "{json}");
    assert_eq!(rows[0]["name"], "Iteration", "{json}");
    assert_eq!(rows[0]["archived"], false, "{json}");

    // A value outside the option catalog is refused, and nothing is written.
    let err = ainb_fails(
        h,
        &[
            "hangar", "issue", "property", "set", &issue_id, "--key", "sprint", "--value", "S9",
        ],
    );
    assert!(err.to_lowercase().contains("option"), "the reason: {err}");
    assert_eq!(
        stored_properties(h, &issue_id),
        bag,
        "a rejection writes nothing"
    );
}

#[test]
fn issue_meta_round_trips_and_survives_an_unrelated_issue_update() {
    let home = tempfile::tempdir().expect("tempdir");
    let h = home.path();

    let issue_id = created_id(&ainb(
        h,
        &["hangar", "issue", "create", "--title", "Agent scratch"],
    ));
    ainb(
        h,
        &[
            "hangar",
            "issue",
            "meta",
            "set",
            &issue_id,
            "--key",
            "pr_number",
            "--value",
            "471",
            "--type",
            "number",
        ],
    );
    ainb(
        h,
        &[
            "hangar",
            "issue",
            "meta",
            "set",
            &issue_id,
            "--key",
            "pipeline_status",
            "--value",
            "running",
        ],
    );

    let listed = ainb(h, &["hangar", "issue", "meta", "list", &issue_id]);
    assert!(listed.contains("pr_number = 471"), "{listed}");
    assert!(listed.contains("pipeline_status = running"), "{listed}");

    // The anti-race rule, at the CLI boundary.
    ainb(
        h,
        &[
            "hangar",
            "issue",
            "update",
            &issue_id,
            "--state",
            "in_progress",
        ],
    );
    let got = ainb(
        h,
        &[
            "hangar",
            "issue",
            "meta",
            "get",
            &issue_id,
            "--key",
            "pr_number",
        ],
    );
    assert_eq!(
        got.trim(),
        "471",
        "an unrelated update must not clobber the bag"
    );

    // `issue show` surfaces the bag under its own section.
    let shown = ainb(h, &["hangar", "issue", "show", &issue_id]);
    assert!(shown.contains("Metadata:"), "the section: {shown}");
    assert!(shown.contains("pr_number = 471"), "the entry: {shown}");

    // Integer fidelity survives the store: 471, never 471.0.
    let json = ainb(
        h,
        &[
            "hangar", "issue", "meta", "list", &issue_id, "--format", "json",
        ],
    );
    let rows: Vec<serde_json::Value> = serde_json::from_str(&json).expect("metadata json");
    let pr = rows.iter().find(|r| r["key"] == "pr_number").expect("the entry");
    assert_eq!(pr["value_json"], "471", "{json}");

    ainb(
        h,
        &[
            "hangar",
            "issue",
            "meta",
            "delete",
            &issue_id,
            "--key",
            "pr_number",
        ],
    );
    let listed = ainb(h, &["hangar", "issue", "meta", "list", &issue_id]);
    assert!(!listed.contains("pr_number"), "the key went: {listed}");
    assert!(
        listed.contains("pipeline_status = running"),
        "the sibling survived a single-key delete: {listed}"
    );

    // A key that does not match the reference's regex is refused.
    let err = ainb_fails(
        h,
        &[
            "hangar", "issue", "meta", "set", &issue_id, "--key", "9lives", "--value", "x",
        ],
    );
    assert!(err.to_lowercase().contains("key"), "the reason: {err}");
}
