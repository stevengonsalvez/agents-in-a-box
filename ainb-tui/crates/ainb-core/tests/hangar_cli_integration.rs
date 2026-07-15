//! End-to-end integration tests for the `ainb hangar <verb>` CLI namespace.
//!
//! Drives the real `ainb` binary (located via `CARGO_BIN_EXE_ainb`) against an
//! isolated `$AINB_HANGAR_HOME` so each test owns an ephemeral `hangar.db`. The
//! store resolves `$AINB_HANGAR_HOME` as the directory that DIRECTLY holds the
//! database (no `.agents-in-a-box` segment), so a per-test tempdir keeps the real
//! `~/.agents-in-a-box/hangar.db` untouched.
//!
//! Covered:
//! * `ainb hangar --help` lists every wired noun group.
//! * `ainb hangar issue create` then `ainb hangar issue list` shows the issue
//!   (round-trip through a fresh, auto-bootstrapped workspace + migrated db).
//! * `ainb hangar issue show <id>` round-trips the created issue.
//! * `ainb hangar daemon status` reports the database reachable.
//! * `ainb hangar task list` on an empty db is a clean no-op (exit 0).

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Run `ainb <args>` with an isolated hangar home. Returns (success, stdout).
fn run(home: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        // Keep any HOME-scoped lookups off the real home too.
        .env("HOME", home)
        .output()
        .expect("spawn ainb");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}{stderr}"))
}

#[test]
fn hangar_help_lists_wired_verbs() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "--help"]);
    assert!(ok, "hangar --help should exit 0; out={out}");
    for verb in ["issue", "task", "beads", "daemon", "squad"] {
        assert!(out.contains(verb), "hangar --help missing '{verb}':\n{out}");
    }
}

#[test]
fn issue_create_then_list_shows_it() {
    let tmp = tempfile::tempdir().unwrap();

    let (ok, out) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Tripwire issue"],
    );
    assert!(ok, "issue create should exit 0; out={out}");
    assert!(out.contains("created issue"), "missing create ack:\n{out}");

    let (ok, out) = run(tmp.path(), &["hangar", "issue", "list"]);
    assert!(ok, "issue list should exit 0; out={out}");
    assert!(
        out.contains("Tripwire issue"),
        "created issue not in list:\n{out}"
    );
}

#[test]
fn issue_show_round_trips_created_issue() {
    let tmp = tempfile::tempdir().unwrap();

    let (ok, create_out) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Showable"],
    );
    assert!(ok, "create failed: {create_out}");
    // "created issue <id>" — pull the id token.
    let id = create_out
        .lines()
        .find_map(|l| l.strip_prefix("created issue "))
        .map(|s| s.trim().to_string())
        .expect("create output carries an id");

    let (ok, out) = run(tmp.path(), &["hangar", "issue", "show", &id]);
    assert!(ok, "issue show should exit 0; out={out}");
    assert!(out.contains(&id), "show output missing id:\n{out}");
    assert!(
        out.contains("Showable"),
        "show output missing title:\n{out}"
    );
}

#[test]
fn issue_list_json_format_emits_array() {
    let tmp = tempfile::tempdir().unwrap();
    run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Jsonable"],
    );
    let (ok, out) = run(tmp.path(), &["--format", "json", "hangar", "issue", "list"]);
    assert!(ok, "json issue list should exit 0; out={out}");
    assert!(
        out.trim_start().starts_with('['),
        "expected JSON array:\n{out}"
    );
    assert!(
        out.contains("\"title\":\"Jsonable\""),
        "json missing title:\n{out}"
    );
}

/// The user-visible proof for e38.8: `ainb hangar issue update` edits an
/// existing issue's state, priority, and due date through the daemon store, and
/// the change is observable via `issue show` (a real-binary round-trip).
#[test]
fn issue_update_edits_persist_through_show() {
    let tmp = tempfile::tempdir().unwrap();

    // Create a routine issue (priority 0, no due date).
    let (ok, create_out) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Editable issue"],
    );
    assert!(ok, "create failed: {create_out}");
    let id = create_out
        .lines()
        .find_map(|l| l.strip_prefix("created issue "))
        .map(|s| s.trim().to_string())
        .expect("create output carries an id");

    // Edit it: move it in_progress, bump priority to 3, set a due date.
    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "issue",
            "update",
            &id,
            "--state",
            "in_progress",
            "--priority",
            "3",
            "--due",
            "2026-01-15",
        ],
    );
    assert!(ok, "issue update should exit 0; out={out}");
    assert!(out.contains("updated issue"), "missing update ack:\n{out}");

    // The edit persisted: show the issue as JSON and assert the new fields.
    let (ok, shown) = run(
        tmp.path(),
        &["--format", "json", "hangar", "issue", "show", &id],
    );
    assert!(ok, "issue show should exit 0; out={shown}");
    assert!(
        shown.contains("\"state\":\"in_progress\""),
        "state edit not persisted:\n{shown}"
    );
    assert!(
        shown.contains("\"priority\":3"),
        "priority edit not persisted:\n{shown}"
    );
    // 2026-01-15 UTC midnight = 1768435200000 ms.
    assert!(
        shown.contains("\"due_date\":1768435200000"),
        "due_date edit not persisted:\n{shown}"
    );

    // A partial edit leaves untouched fields alone (priority stays 3).
    let (ok, _) = run(
        tmp.path(),
        &["hangar", "issue", "update", &id, "--state", "done"],
    );
    assert!(ok, "partial update should exit 0");
    let (_, shown) = run(
        tmp.path(),
        &["--format", "json", "hangar", "issue", "show", &id],
    );
    assert!(
        shown.contains("\"state\":\"done\"") && shown.contains("\"priority\":3"),
        "partial edit must change only state:\n{shown}"
    );

    // Clearing the due date removes the deadline.
    let (ok, _) = run(
        tmp.path(),
        &["hangar", "issue", "update", &id, "--clear-due"],
    );
    assert!(ok, "clear-due should exit 0");
    let (_, shown) = run(
        tmp.path(),
        &["--format", "json", "hangar", "issue", "show", &id],
    );
    assert!(
        shown.contains("\"due_date\":null"),
        "clear-due must null the deadline:\n{shown}"
    );
}

/// `ainb hangar issue update` on an unknown id is a hard error (exit non-zero),
/// not a silent no-op — the mutating surface never lies about a write.
#[test]
fn issue_update_unknown_id_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    // Bootstrap a workspace so the failure is "no such issue", not "no db".
    run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Anchor"],
    );
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "issue", "update", "no-such-id", "--state", "done"],
    );
    assert!(!ok, "updating an unknown id must fail:\n{out}");
    assert!(
        out.contains("no issue with id"),
        "missing not-found message:\n{out}"
    );
}

/// `ainb hangar issue update` with no field flags is rejected (nothing to do).
#[test]
fn issue_update_with_no_fields_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, create_out) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Idle"],
    );
    let id = create_out
        .lines()
        .find_map(|l| l.strip_prefix("created issue "))
        .map(|s| s.trim().to_string())
        .expect("id");
    let (ok, out) = run(tmp.path(), &["hangar", "issue", "update", &id]);
    assert!(!ok, "an empty update must be rejected:\n{out}");
    assert!(out.contains("nothing to update"), "missing reason:\n{out}");
}

#[test]
fn daemon_status_reports_reachable() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "daemon", "status"]);
    assert!(ok, "daemon status should exit 0; out={out}");
    assert!(out.contains("reachable"), "status not reachable:\n{out}");
}

/// RED test 5: `ainb hangar config warnings reset --provider claude` wipes only
/// claude's per-session acks, so the next claude dispatch re-warns about
/// danger-full-access — while first-run + other providers stay acknowledged.
#[test]
fn reset_cli_wipes_acks() {
    let tmp = tempfile::tempdir().unwrap();
    // state.toml resolves to $AINB_HANGAR_HOME/hangar/state.toml.
    let state = tmp.path().join("hangar").join("state.toml");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    // Seed: a foreign workspace key + first_run + two claude sessions + a codex.
    std::fs::write(
        &state,
        "active_workspace = \"01ID\"\n\
         warnings_ack = [\"first_run\", \"provider:claude:session:s1\", \
         \"provider:claude:session:s2\", \"provider:codex:session:s1\"]\n",
    )
    .unwrap();

    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "config",
            "warnings",
            "reset",
            "--provider",
            "claude",
        ],
    );
    assert!(ok, "warnings reset should exit 0; out={out}");
    assert!(out.contains("re-warn"), "missing re-warn ack:\n{out}");

    let raw = std::fs::read_to_string(&state).unwrap();
    // Claude session acks gone → next claude dispatch re-warns.
    assert!(
        !raw.contains("provider:claude:session"),
        "claude acks not wiped:\n{raw}"
    );
    // first_run + codex + the foreign workspace key survive.
    assert!(raw.contains("first_run"), "first_run wrongly wiped:\n{raw}");
    assert!(
        raw.contains("provider:codex:session:s1"),
        "codex ack wrongly wiped:\n{raw}"
    );
    assert!(raw.contains("active_workspace"), "foreign key lost:\n{raw}");
}

/// `ainb hangar config warnings reset` (no flag) wipes every ack, including
/// first-run — the warnings show again from scratch.
#[test]
fn reset_cli_no_flag_wipes_all() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("hangar").join("state.toml");
    std::fs::create_dir_all(state.parent().unwrap()).unwrap();
    std::fs::write(
        &state,
        "warnings_ack = [\"first_run\", \"provider:claude:session:s1\"]\n",
    )
    .unwrap();

    let (ok, out) = run(tmp.path(), &["hangar", "config", "warnings", "reset"]);
    assert!(ok, "warnings reset should exit 0; out={out}");

    let raw = std::fs::read_to_string(&state).unwrap();
    assert!(!raw.contains("first_run"), "first_run not wiped:\n{raw}");
    assert!(
        !raw.contains("provider:claude"),
        "provider ack not wiped:\n{raw}"
    );
}

#[test]
fn task_list_on_empty_db_is_clean_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "task", "list"]);
    assert!(ok, "task list on empty db should exit 0; out={out}");
    assert!(
        out.contains("no pending tasks"),
        "expected empty marker:\n{out}"
    );
}

#[test]
fn issue_list_all_four_formats_are_distinct() {
    let tmp = tempfile::tempdir().unwrap();
    // Title carries a comma so CSV quoting is exercised.
    let (ok, _) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "Wire, up payments"],
    );
    assert!(ok, "create should exit 0");

    let text = run(tmp.path(), &["hangar", "issue", "list", "--format", "text"]).1;
    let json = run(tmp.path(), &["hangar", "issue", "list", "--format", "json"]).1;
    let csv = run(tmp.path(), &["hangar", "issue", "list", "--format", "csv"]).1;
    let md = run(
        tmp.path(),
        &["hangar", "issue", "list", "--format", "markdown"],
    )
    .1;

    // Every format must produce DISTINCT output (the regression that let
    // csv/markdown silently alias text through a `_ =>` catch-all).
    let all = [&text, &json, &csv, &md];
    for (a, b) in [(0, 1), (0, 2), (0, 3), (1, 2), (1, 3), (2, 3)] {
        assert_ne!(
            all[a], all[b],
            "formats {a} and {b} produced identical output"
        );
    }

    // Format-specific markers.
    assert!(
        json.trim_start().starts_with('['),
        "json not an array:\n{json}"
    );
    assert!(
        csv.contains("id,state,title,description,created_at"),
        "csv header missing:\n{csv}"
    );
    assert!(
        csv.contains("\"Wire, up payments\""),
        "csv must quote the comma-bearing title:\n{csv}"
    );
    assert!(
        md.contains("| --- |"),
        "markdown separator row missing:\n{md}"
    );
    assert!(md.contains("| state |"), "markdown header missing:\n{md}");
}

/// Seed one runtime + agent (`agent-1`, `Builder`) into the bootstrapped default
/// workspace of the test's hangar home, so the `agent` CLI verbs have a real row
/// to edit. The binary resolves `$AINB_HANGAR_HOME/hangar.db`; this opens the
/// same file and inserts via the store repos. Must run AFTER a verb that
/// bootstraps the workspace (e.g. `issue create`).
fn seed_agent(home: &std::path::Path) {
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::agent::{Agent, AgentRepo};

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let store = Store::open_in(home).await.expect("open hangar db");
        let pool = store.pool();
        // The bootstrapped default workspace + owner already exist (issue create).
        let workspace_id: String = sqlx::query_scalar("SELECT id FROM workspace LIMIT 1")
            .fetch_one(pool)
            .await
            .expect("default workspace exists");
        let owner_id: String = sqlx::query_scalar("SELECT id FROM user LIMIT 1")
            .fetch_one(pool)
            .await
            .expect("default owner exists");
        sqlx::query(
            "INSERT INTO agent_runtime \
             (id, workspace_id, daemon_id, provider, runtime_mode, status) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind("rt-1")
        .bind(&workspace_id)
        .bind("daemon-1")
        .bind("claude")
        .bind("local")
        .bind("online")
        .execute(pool)
        .await
        .expect("insert runtime");
        AgentRepo::insert(
            pool,
            &Agent {
                id: "agent-1".into(),
                workspace_id,
                name: "Builder".into(),
                runtime_id: "rt-1".into(),
                instructions: None,
                visibility: "workspace".into(),
                owner_id,
                archived: false,
                model: None,
                cli_args: Vec::new(),
                mcp_config: None,
                thinking: None,
                agent_env: Vec::new(),
                provider: None,
            },
        )
        .await
        .expect("insert agent");
    });
}

/// The user-visible proof for e38.15 (CLI leg): `ainb hangar agent edit` persists
/// the config knobs, observable via `agent list --format json`; `agent archive`
/// hides the agent from the active `agent list`, and `unarchive` restores it.
#[test]
fn agent_edit_and_archive_round_trip() {
    let tmp = tempfile::tempdir().unwrap();
    // Bootstrap the default workspace, then seed an agent into its db.
    let (ok, _) = run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "anchor"],
    );
    assert!(ok, "bootstrap create failed");
    seed_agent(tmp.path());

    // The agent shows in the active list before any edit.
    let (ok, out) = run(tmp.path(), &["hangar", "agent", "list"]);
    assert!(ok, "agent list should exit 0; out={out}");
    assert!(out.contains("agent-1"), "seeded agent not listed:\n{out}");

    // Edit: rename + set model + a CLI arg + an env var.
    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "agent",
            "edit",
            "agent-1",
            "--name",
            "Builder Pro",
            "--model",
            "claude-opus-4",
            // A flag-looking arg value must use the `=` form so clap reads it as a
            // value, not a new flag.
            "--arg=--verbose",
            "--env",
            "FOO=bar",
            "--thinking",
            "high",
        ],
    );
    assert!(ok, "agent edit should exit 0; out={out}");
    assert!(out.contains("updated agent"), "missing edit ack:\n{out}");

    // The edit persisted: list as JSON and assert the new fields.
    let (ok, shown) = run(tmp.path(), &["--format", "json", "hangar", "agent", "list"]);
    assert!(ok, "json agent list should exit 0; out={shown}");
    assert!(
        shown.contains("\"name\":\"Builder Pro\""),
        "rename not persisted:\n{shown}"
    );
    assert!(
        shown.contains("\"model\":\"claude-opus-4\""),
        "model not persisted:\n{shown}"
    );
    assert!(
        shown.contains("\"thinking\":\"high\""),
        "thinking not persisted:\n{shown}"
    );
    assert!(
        shown.contains("\"FOO\":\"bar\""),
        "env not persisted:\n{shown}"
    );

    // Archive it: the active list no longer shows it.
    let (ok, out) = run(tmp.path(), &["hangar", "agent", "archive", "agent-1"]);
    assert!(ok, "archive should exit 0; out={out}");
    assert!(
        out.contains("archived agent"),
        "missing archive ack:\n{out}"
    );
    let (_, active) = run(tmp.path(), &["hangar", "agent", "list"]);
    assert!(
        !active.contains("agent-1"),
        "archived agent must be hidden from the active list:\n{active}"
    );
    // `--all` still shows it (with the archived badge).
    let (_, all) = run(tmp.path(), &["hangar", "agent", "list", "--all"]);
    assert!(
        all.contains("agent-1") && all.contains("[archived]"),
        "--all must show the archived agent:\n{all}"
    );

    // Un-archive restores it to the active list.
    let (ok, _) = run(tmp.path(), &["hangar", "agent", "unarchive", "agent-1"]);
    assert!(ok, "unarchive should exit 0");
    let (_, restored) = run(tmp.path(), &["hangar", "agent", "list"]);
    assert!(
        restored.contains("agent-1"),
        "un-archived agent returns to the active list:\n{restored}"
    );

    // Clearing the model removes it (the explicit clear flag).
    let (ok, _) = run(
        tmp.path(),
        &["hangar", "agent", "edit", "agent-1", "--clear-model"],
    );
    assert!(ok, "clear-model should exit 0");
    let (_, shown) = run(tmp.path(), &["--format", "json", "hangar", "agent", "list"]);
    assert!(
        shown.contains("\"model\":null"),
        "clear-model must null the model:\n{shown}"
    );
}

/// `ainb hangar agent edit` on an unknown id is a hard error (exit non-zero),
/// not a silent no-op; an edit with no field flags is rejected too.
#[test]
fn agent_edit_unknown_id_and_empty_edit_are_errors() {
    let tmp = tempfile::tempdir().unwrap();
    run(
        tmp.path(),
        &["hangar", "issue", "create", "--title", "anchor"],
    );
    seed_agent(tmp.path());

    // Unknown id → not-found error.
    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "agent",
            "edit",
            "no-such-agent",
            "--name",
            "ghost",
        ],
    );
    assert!(!ok, "editing an unknown id must fail:\n{out}");
    assert!(
        out.contains("no agent with id"),
        "missing not-found message:\n{out}"
    );

    // No field flags → "nothing to update".
    let (ok, out) = run(tmp.path(), &["hangar", "agent", "edit", "agent-1"]);
    assert!(!ok, "an empty edit must be rejected:\n{out}");
    assert!(out.contains("nothing to update"), "missing reason:\n{out}");
}

/// `ainb hangar agent list` on an empty workspace is a clean no-op (exit 0).
#[test]
fn agent_list_on_empty_db_is_clean_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "agent", "list"]);
    assert!(ok, "agent list on empty db should exit 0; out={out}");
    assert!(out.contains("no agents"), "expected empty marker:\n{out}");
}

/// `ainb hangar agent create --name` on a fresh home lays down the default
/// workspace/runtime/owner behind the scenes, inserts the agent, and prints its
/// NAME (never an id). A follow-up `agent list` shows it, and an unsupported
/// provider is a clean error.
#[test]
fn agent_create_on_fresh_home_inserts_and_lists() {
    let tmp = tempfile::tempdir().unwrap();

    let (ok, out) = run(
        tmp.path(),
        &["hangar", "agent", "create", "--name", "reviewer", "--provider", "codex"],
    );
    assert!(ok, "agent create should exit 0 on a fresh home; out={out}");
    assert!(out.contains("created agent reviewer"), "missing create ack (by name):\n{out}");

    let (ok, out) = run(tmp.path(), &["hangar", "agent", "list"]);
    assert!(ok, "agent list should exit 0; out={out}");
    assert!(out.contains("reviewer"), "created agent not in list:\n{out}");

    // An unsupported provider is rejected, not silently coerced.
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "agent", "create", "--name", "x", "--provider", "gpt5"],
    );
    assert!(!ok, "an unsupported provider must fail; out={out}");
    assert!(out.contains("unsupported provider"), "missing provider error:\n{out}");
}

/// Create an issue via the CLI and return its id (pulled from the create ack).
fn create_issue(home: &std::path::Path, title: &str) -> String {
    let (ok, out) = run(home, &["hangar", "issue", "create", "--title", title]);
    assert!(ok, "issue create should exit 0; out={out}");
    out.lines()
        .find_map(|l| l.strip_prefix("created issue "))
        .map(|s| s.trim().to_string())
        .expect("create output carries an id")
}

/// The user-visible proof for e38.10: `ainb hangar issue label attach` adds a
/// label to an issue and `... detach` removes it, both observable via
/// `issue show --format json` (a real-binary round-trip through `LabelRepo`).
#[test]
fn issue_label_attach_then_detach_persist_through_show() {
    let tmp = tempfile::tempdir().unwrap();
    let id = create_issue(tmp.path(), "Labelable");

    // Attach `bug`: the ack reports it and a json show carries the chip.
    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar", "issue", "label", "attach", &id, "bug", "--color", "#ff0000",
        ],
    );
    assert!(ok, "label attach should exit 0; out={out}");
    assert!(out.contains("attached `bug`"), "missing attach ack:\n{out}");

    let (ok, out) = run(
        tmp.path(),
        &["--format", "json", "hangar", "issue", "show", &id],
    );
    assert!(ok, "json show should exit 0; out={out}");
    assert!(
        out.contains("\"labels\":[\"bug\"]"),
        "attached label not in json show:\n{out}"
    );

    // Detach `bug`: the ack reports `none` and the json show drops the chip.
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "issue", "label", "detach", &id, "bug"],
    );
    assert!(ok, "label detach should exit 0; out={out}");
    assert!(out.contains("detached `bug`"), "missing detach ack:\n{out}");
    assert!(out.contains("labels: none"), "detach left a label:\n{out}");

    let (ok, out) = run(
        tmp.path(),
        &["--format", "json", "hangar", "issue", "show", &id],
    );
    assert!(ok, "json show should exit 0; out={out}");
    assert!(
        out.contains("\"labels\":[]"),
        "detach did not clear the label in json show:\n{out}"
    );
}

/// `ainb hangar issue label attach` against an unknown issue id is an error
/// (exit non-zero), never a silent no-op.
#[test]
fn issue_label_attach_unknown_id_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    // Bootstrap the default workspace so the failure is "no issue", not "no
    // workspace".
    let _ = create_issue(tmp.path(), "Bootstrap");

    let (ok, out) = run(
        tmp.path(),
        &["hangar", "issue", "label", "attach", "no-such-issue", "bug"],
    );
    assert!(!ok, "labelling an unknown id must fail:\n{out}");
    assert!(
        out.contains("no issue with id"),
        "missing not-found message:\n{out}"
    );
}

/// Insert one comment on an issue by opening the same hangar db the binary uses,
/// so the search CLI can be exercised against a comment-body hit (there is no
/// `hangar comment` CLI verb to seed it through). Mirrors `seed_agent`'s
/// open-the-same-file pattern.
fn seed_comment(home: &std::path::Path, issue_id: &str, body: &str) {
    use ainb_hangar_store::Store;

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let store = Store::open_in(home).await.expect("open hangar db");
        sqlx::query(
            "INSERT INTO comment (id, issue_id, author_type, author_id, body, created_at) \
             VALUES (?, ?, 'member', 'user-1', ?, 1700000000000)",
        )
        .bind(format!("c-{issue_id}"))
        .bind(issue_id)
        .bind(body)
        .execute(store.pool())
        .await
        .expect("insert comment");
    });
}

/// The user-visible proof for e38.12: `ainb hangar issue search <query>` finds
/// issues by a substring of their title, description, OR comment body — reaching
/// beyond the plugin's client-side title-only filter — and ranks title hits
/// above description hits above comment-only hits, excluding non-matching issues.
#[test]
fn issue_search_ranks_title_desc_comment_and_excludes_nonmatch() {
    let tmp = tempfile::tempdir().unwrap();

    // A title hit.
    create_issue(tmp.path(), "Improve telemetry pipeline");
    // A description-only hit.
    let (ok, _) = run(
        tmp.path(),
        &[
            "hangar",
            "issue",
            "create",
            "--title",
            "Routine cleanup",
            "--description",
            "the telemetry sink needs a flush",
        ],
    );
    assert!(ok, "desc-issue create should exit 0");
    // A comment-only hit (seeded directly — no comment CLI verb).
    let cmt_id = create_issue(tmp.path(), "Backlog grooming");
    seed_comment(tmp.path(), &cmt_id, "we lost telemetry yesterday");
    // A non-matching issue.
    create_issue(tmp.path(), "Quiet unrelated work");

    // Text format: the three matching titles appear in rank order, the
    // non-match is absent.
    let (ok, out) = run(tmp.path(), &["hangar", "issue", "search", "telemetry"]);
    assert!(ok, "issue search should exit 0; out={out}");
    let title_pos = out
        .find("Improve telemetry pipeline")
        .unwrap_or_else(|| panic!("title hit missing from search:\n{out}"));
    let desc_pos = out
        .find("Routine cleanup")
        .unwrap_or_else(|| panic!("description hit missing from search:\n{out}"));
    let cmt_pos = out
        .find("Backlog grooming")
        .unwrap_or_else(|| panic!("comment hit missing from search:\n{out}"));
    assert!(
        title_pos < desc_pos && desc_pos < cmt_pos,
        "ranking must be title < description < comment in output order:\n{out}"
    );
    assert!(
        !out.contains("Quiet unrelated work"),
        "a non-matching issue must be excluded from search:\n{out}"
    );
}

/// `ainb hangar issue search` with a blank query matches nothing — it must not
/// dump the whole board.
#[test]
fn issue_search_blank_query_matches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    create_issue(tmp.path(), "Some issue title");

    let (ok, out) = run(tmp.path(), &["hangar", "issue", "search", "   "]);
    assert!(ok, "blank search should exit 0; out={out}");
    assert!(
        out.contains("no issues") && !out.contains("Some issue title"),
        "a blank query must match nothing, not dump the board:\n{out}"
    );
}

/// `ainb hangar member --help` lists the three e38.11 verbs.
#[test]
fn member_help_lists_verbs() {
    let tmp = tempfile::tempdir().unwrap();
    let (ok, out) = run(tmp.path(), &["hangar", "member", "--help"]);
    assert!(ok, "member --help should exit 0; out={out}");
    for verb in ["list", "set-role", "remove"] {
        assert!(out.contains(verb), "member --help missing '{verb}':\n{out}");
    }
}

/// Pull the bootstrapped owner's user id from `member list --format json`.
fn owner_user_id(home: &std::path::Path) -> String {
    let (ok, out) = run(home, &["--format", "json", "hangar", "member", "list"]);
    assert!(ok, "member list should exit 0; out={out}");
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("member list is JSON");
    v.as_array()
        .and_then(|a| a.first())
        .and_then(|m| m["user_id"].as_str())
        .map(ToString::to_string)
        .unwrap_or_else(|| panic!("member list carries a user id:\n{out}"))
}

/// `ainb hangar member list` shows the lazily-bootstrapped owner with their
/// email + role (a real-binary round-trip through `MemberRepo`).
#[test]
fn member_list_shows_bootstrapped_owner() {
    let tmp = tempfile::tempdir().unwrap();
    // `issue create` lazily bootstraps the default workspace + owner member.
    create_issue(tmp.path(), "Bootstrap the workspace");

    let (ok, out) = run(tmp.path(), &["hangar", "member", "list"]);
    assert!(ok, "member list should exit 0; out={out}");
    assert!(
        out.contains("stevie@local") && out.contains("role=owner"),
        "member list must show the bootstrapped owner:\n{out}"
    );
}

/// The last-owner guard over the CLI: demoting the sole owner is rejected (a
/// workspace must always keep an owner).
#[test]
fn member_set_role_rejects_demoting_the_only_owner() {
    let tmp = tempfile::tempdir().unwrap();
    create_issue(tmp.path(), "Bootstrap the workspace");
    let owner = owner_user_id(tmp.path());

    let (ok, out) = run(
        tmp.path(),
        &["hangar", "member", "set-role", &owner, "admin"],
    );
    assert!(!ok, "demoting the only owner must fail; out={out}");
    assert!(
        out.contains("at least one owner"),
        "the last-owner guard message must surface:\n{out}"
    );

    // The owner is untouched.
    let (_ok, list) = run(tmp.path(), &["hangar", "member", "list"]);
    assert!(
        list.contains("role=owner"),
        "rejected demotion must leave the owner role:\n{list}"
    );
}

/// The last-owner guard over the CLI: removing the sole owner is rejected.
#[test]
fn member_remove_rejects_removing_the_only_owner() {
    let tmp = tempfile::tempdir().unwrap();
    create_issue(tmp.path(), "Bootstrap the workspace");
    let owner = owner_user_id(tmp.path());

    let (ok, out) = run(tmp.path(), &["hangar", "member", "remove", &owner]);
    assert!(!ok, "removing the only owner must fail; out={out}");
    assert!(
        out.contains("at least one owner"),
        "the last-owner guard message must surface:\n{out}"
    );

    // The owner is still listed.
    let (_ok, list) = run(tmp.path(), &["hangar", "member", "list"]);
    assert!(
        list.contains("stevie@local"),
        "rejected removal must leave the owner:\n{list}"
    );
}

/// The user-visible proof for e38.17: `ainb hangar squad create` + `... add-member`
/// build a squad with a leader + members, and `ainb hangar squad list` renders the
/// status view (squad name, leader, members) — a real-binary round-trip through
/// `SquadRepo`.
#[test]
fn squad_create_add_member_then_list_shows_status() {
    let tmp = tempfile::tempdir().unwrap();
    // Bootstrap the default workspace so the squad has somewhere to live.
    create_issue(tmp.path(), "Bootstrap the workspace");

    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "squad",
            "create",
            "shippers",
            "--leader",
            "agent:lead-1",
        ],
    );
    assert!(ok, "squad create should exit 0; out={out}");
    assert!(
        out.contains("created squad shippers"),
        "missing create ack:\n{out}"
    );
    let squad_id = out
        .lines()
        .find_map(|l| l.split('(').nth(1).and_then(|s| s.split(')').next()))
        .expect("create output carries the squad id")
        .to_string();

    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "squad",
            "add-member",
            &squad_id,
            "--member",
            "member:user-1",
        ],
    );
    assert!(ok, "add-member should exit 0; out={out}");

    // The status view shows the squad with its leader + member.
    let (ok, list) = run(tmp.path(), &["hangar", "squad", "list"]);
    assert!(ok, "squad list should exit 0; out={list}");
    assert!(list.contains("shippers"), "squad name not in list:\n{list}");
    assert!(
        list.contains("leader=agent:lead-1"),
        "squad leader not in status view:\n{list}"
    );
    assert!(
        list.contains("member:user-1"),
        "squad member not in status view:\n{list}"
    );

    // The JSON surface carries the leader + members array (machine-readable).
    let (ok, json) = run(tmp.path(), &["--format", "json", "hangar", "squad", "list"]);
    assert!(ok, "squad list --format json should exit 0; out={json}");
    assert!(
        json.contains("\"leader\":\"agent:lead-1\"")
            && json.contains("\"members\":[\"member:user-1\"]"),
        "json status view missing leader/members:\n{json}"
    );
}

/// A duplicate squad name in the workspace is rejected (resolve-or-reject guard).
#[test]
fn squad_create_rejects_a_duplicate_name() {
    let tmp = tempfile::tempdir().unwrap();
    create_issue(tmp.path(), "Bootstrap the workspace");

    let (ok, _out) = run(
        tmp.path(),
        &[
            "hangar",
            "squad",
            "create",
            "alpha",
            "--leader",
            "agent:lead-1",
        ],
    );
    assert!(ok, "first create should succeed");

    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "squad",
            "create",
            "alpha",
            "--leader",
            "member:user-1",
        ],
    );
    assert!(!ok, "a duplicate squad name must fail; out={out}");
    assert!(
        out.contains("already exists"),
        "the duplicate-name guard message must surface:\n{out}"
    );
}

/// Seed a real agent (`assign-agent` on `assign-runtime`) into the bootstrapped
/// db at `$AINB_HANGAR_HOME` so a squad led by it has a leader to route work to.
/// Runs synchronously on a fresh tokio runtime (the binary is invoked separately,
/// so there is no concurrent writer on the WAL).
fn seed_assignable_agent(home: &std::path::Path) {
    use ainb_hangar_store::Store;
    use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
    use ainb_hangar_store::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let store = Store::open_in(home).await.unwrap();
        let pool = store.pool();
        // The default workspace the CLI bootstrap created.
        let ws_id: String =
            sqlx::query_scalar("SELECT id FROM workspace WHERE slug = 'default' LIMIT 1")
                .fetch_one(pool)
                .await
                .expect("default workspace exists after bootstrap");
        // `agent.owner_id` FKs to `user(id)`; seed the owner.
        sqlx::query("INSERT OR IGNORE INTO user (id, email, created_at) VALUES (?, ?, ?)")
            .bind("assign-owner")
            .bind("owner@example.com")
            .bind(0_i64)
            .execute(pool)
            .await
            .unwrap();
        AgentRuntimeRepo::insert(
            pool,
            &AgentRuntime {
                id: "assign-runtime".into(),
                workspace_id: ws_id.clone(),
                daemon_id: "daemon-cli".into(),
                provider: "codex".into(),
                runtime_mode: "local".into(),
                last_seen_at: Some(1),
                status: "online".into(),
            },
        )
        .await
        .unwrap();
        AgentRepo::insert(
            pool,
            &Agent {
                id: "assign-agent".into(),
                workspace_id: ws_id,
                name: "lead".into(),
                runtime_id: "assign-runtime".into(),
                instructions: None,
                visibility: "workspace".into(),
                owner_id: "assign-owner".into(),
                archived: false,
                model: None,
                cli_args: Vec::new(),
                mcp_config: None,
                thinking: None,
                agent_env: Vec::new(),
                provider: None,
            },
        )
        .await
        .unwrap();
    });
}

/// The user-visible proof for e38.17 leader routing: `ainb hangar squad assign`
/// routes a task to the squad's LEADER through the real binary. The squad is led
/// by `assign-agent` (a real agent on `assign-runtime`); `squad assign` reports
/// the leader agent + the runtime it DERIVED (never supplied on the CLI).
#[test]
fn squad_assign_routes_a_task_to_the_leader() {
    let tmp = tempfile::tempdir().unwrap();
    // Bootstrap the workspace, then seed the leader agent so it is routable.
    create_issue(tmp.path(), "Bootstrap the workspace");
    seed_assignable_agent(tmp.path());

    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "squad",
            "create",
            "shippers",
            "--leader",
            "agent:assign-agent",
        ],
    );
    assert!(ok, "squad create should exit 0; out={out}");
    let squad_id = out
        .lines()
        .find_map(|l| l.split('(').nth(1).and_then(|s| s.split(')').next()))
        .expect("create output carries the squad id")
        .to_string();

    // Assign to the squad — naming ONLY the squad, never the agent/runtime.
    let (ok, out) = run(tmp.path(), &["hangar", "squad", "assign", &squad_id]);
    assert!(ok, "squad assign should exit 0; out={out}");
    assert!(
        out.contains("leader assign-agent"),
        "assign ack must name the leader agent:\n{out}"
    );
    assert!(
        out.contains("runtime assign-runtime"),
        "assign ack must name the DERIVED leader runtime:\n{out}"
    );
}

/// `ainb hangar squad assign` on a squad led by a HUMAN member is rejected: there
/// is no agent to dispatch to, so the routing guard fires through the real binary.
#[test]
fn squad_assign_rejects_a_human_leader() {
    let tmp = tempfile::tempdir().unwrap();
    create_issue(tmp.path(), "Bootstrap the workspace");

    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "squad",
            "create",
            "humans",
            "--leader",
            "member:user-1",
        ],
    );
    assert!(ok, "squad create should exit 0; out={out}");
    let squad_id = out
        .lines()
        .find_map(|l| l.split('(').nth(1).and_then(|s| s.split(')').next()))
        .expect("create output carries the squad id")
        .to_string();

    let (ok, out) = run(tmp.path(), &["hangar", "squad", "assign", &squad_id]);
    assert!(!ok, "assigning to a human-led squad must fail; out={out}");
    assert!(
        out.contains("no agent leader"),
        "the human-leader guard message must surface:\n{out}"
    );
}

/// The user-visible proof for e38.21: `ainb hangar workspace config` sets the
/// per-workspace config knobs, `... workspace show` round-trips them, and the
/// `issue_prefix` actually TAKES EFFECT — an issue created afterward carries the
/// prefix in its title (a real-binary round-trip through `WorkspaceRepo` + the
/// CLI issue-create path).
#[test]
fn workspace_config_sets_knobs_and_issue_prefix_takes_effect() {
    let tmp = tempfile::tempdir().unwrap();

    // Configure the default workspace (bootstraps it on first touch).
    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "workspace",
            "config",
            "--context-prompt",
            "Always run cargo fmt.",
            "--issue-prefix",
            "[OPS] ",
            "--repo-whitelist",
            "org/api,org/web",
        ],
    );
    assert!(ok, "workspace config should exit 0; out={out}");
    assert!(
        out.contains("updated workspace config"),
        "missing config ack:\n{out}"
    );

    // `workspace show` reflects every stored knob.
    let (ok, out) = run(tmp.path(), &["hangar", "workspace", "show"]);
    assert!(ok, "workspace show should exit 0; out={out}");
    assert!(
        out.contains("Always run cargo fmt."),
        "context prompt not shown:\n{out}"
    );
    assert!(out.contains("[OPS] "), "issue prefix not shown:\n{out}");
    assert!(
        out.contains("org/api") && out.contains("org/web"),
        "repo whitelist not shown:\n{out}"
    );

    // The takes-effect proof: an issue created in this configured workspace
    // carries the prefix in its persisted title (observed via `issue show`).
    let id = create_issue(tmp.path(), "fix the build");
    let (ok, out) = run(tmp.path(), &["hangar", "issue", "show", &id]);
    assert!(ok, "issue show should exit 0; out={out}");
    assert!(
        out.contains("[OPS] fix the build"),
        "the created issue's title must carry the workspace prefix:\n{out}"
    );
}

/// `--clear-…` flags unset a knob (back to the v1 "not configured" behaviour):
/// an issue created after clearing the prefix uses the bare title verbatim.
#[test]
fn workspace_config_clear_flags_unset_knobs() {
    let tmp = tempfile::tempdir().unwrap();

    // Set, then clear, the issue prefix.
    let (ok, _) = run(
        tmp.path(),
        &["hangar", "workspace", "config", "--issue-prefix", "[OPS] "],
    );
    assert!(ok);
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "workspace", "config", "--clear-issue-prefix"],
    );
    assert!(ok, "clear should exit 0; out={out}");

    let (ok, out) = run(tmp.path(), &["hangar", "workspace", "show"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("issue_prefix:   (not set)"),
        "cleared prefix should read (not set):\n{out}"
    );

    // A subsequently-created issue uses the bare title (no prefix).
    let id = create_issue(tmp.path(), "no prefix here");
    let (ok, out) = run(tmp.path(), &["hangar", "issue", "show", &id]);
    assert!(ok, "{out}");
    assert!(
        out.contains("no prefix here") && !out.contains("[OPS] no prefix here"),
        "a cleared prefix must leave the title verbatim:\n{out}"
    );
}
