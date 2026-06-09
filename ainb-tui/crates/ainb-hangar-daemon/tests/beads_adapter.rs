//! P2.2 adapter integration tests: shelling out to the `bd` CLI.
//!
//! Each test drives [`BdClient`] against a tiny shell-script stand-in for the
//! real `bd` binary, sourced from the crate's shared [`fake_bd`] fixtures (via
//! the `test-support` feature) so the integration suite and the in-crate unit
//! tests share one source of truth for the fake's wire shape. The harness never
//! invokes a real `bd` — the contract under test is the adapter's orchestration:
//! command construction (the exact argv that reaches `bd`, `--json`, `BEADS_DIR`
//! propagation), JSON-envelope parsing into [`BdIssue`], error classification,
//! missing-binary detection, and cross-process serialization via the `O_EXCL`
//! pidfile lock.
//!
//! Argv round-trip discipline: the happy-path verb tests use the
//! [`fake_bd::capturing`] fixture, which records its `$@` to a probe file, and
//! then assert that file contains the verb word and every flag/value the adapter
//! was supposed to emit. Without this, the adapter could drop `--assignee`, send
//! singular `--label`, or forget the verb entirely and every JSON-only assertion
//! would still pass green (the original masking bug). The shape parsing alone is
//! covered by the in-crate unit tests in `beads_adapter::tests`.
//!
//! The full daemon round-trip against a real `bd` lives in
//! `tripwire_beads_roundtrip.rs` (P2.6).

#![allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]

use std::fs;
use std::path::{Path, PathBuf};

use ainb_hangar_daemon::beads_adapter::{
    BdClient, BdCreateArgs, BdError, BdId, BdListFilter, BdStatus, fake_bd,
};
use tempfile::TempDir;

/// A new client bound to `bin` + a fresh `BEADS_DIR` tempdir.
fn client_with(bin: PathBuf, beads_dir: &Path) -> BdClient {
    BdClient::new(bin, beads_dir.to_path_buf()).expect("construct BdClient")
}

/// Read a capturing fixture's argv probe (one arg per line) into a vec.
fn read_argv(probe: &Path) -> Vec<String> {
    fs::read_to_string(probe)
        .expect("argv probe written")
        .lines()
        .map(str::to_string)
        .collect()
}

#[test]
fn test_bd_create_happy_path() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    // `bd create --json` emits a bare object, not an array — the fixture mirrors
    // that so this test would fail against any array-only parser.
    let bin = fake_bd::happy(tmp.path(), "abc-123", "ship it", "open");
    let client = client_with(bin, beads.path());

    let issue = client
        .create(BdCreateArgs {
            title: "ship it".into(),
            description: Some("d".into()),
            labels: vec!["foo".into(), "bar".into()],
            assignee: Some("stevie".into()),
        })
        .expect("create ok");

    assert_eq!(issue.id, BdId::from("abc-123"));
    assert_eq!(issue.title, "ship it");
    assert_eq!(issue.status, BdStatus::Open);
    assert_eq!(issue.assignee.as_deref(), Some("stevie"));
    // `bd create` does not echo labels back, so the parsed issue has none — the
    // caller's labels are verified to *reach* bd by the argv round-trip test
    // below, not by reading them back out of the create response.
    assert!(issue.labels.is_empty());
}

#[test]
fn test_bd_create_argv_round_trips_labels_and_assignee() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    let probe = tmp.path().join("argv.txt");
    let bin = fake_bd::capturing(tmp.path(), &probe, "abc-123", "ship it", "open");
    let client = client_with(bin, beads.path());

    client
        .create(BdCreateArgs {
            title: "ship it".into(),
            description: Some("d".into()),
            labels: vec!["foo".into(), "bar".into()],
            assignee: Some("stevie".into()),
        })
        .expect("create ok");

    let argv = read_argv(&probe);
    // Proves the adapter emits the create verb, --json, the title, -d, the
    // single comma-joined --labels flag (NOT repeated singular --label), and
    // --assignee — the exact wire shape `bd 0.49.0 create` accepts.
    assert_eq!(
        argv,
        vec![
            "--json",
            "create",
            "ship it",
            "-d",
            "d",
            "--labels",
            "foo,bar",
            "--assignee",
            "stevie",
        ]
    );
}

#[test]
fn test_bd_create_propagates_beads_dir() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    let probe = tmp.path().join("probe.txt");
    // Fake bd writes whatever BEADS_DIR it received to a probe file, then emits
    // a valid create object so parsing succeeds.
    let bin = fake_bd::write_script(
        tmp.path(),
        "fake-bd.sh",
        &format!(
            "printf '%s' \"$BEADS_DIR\" > '{}'\ncat <<'JSON'\n{}\nJSON\nexit 0",
            probe.display(),
            fake_bd::one_issue_object("id-1", "t", "open")
        ),
    );
    let client = client_with(bin, beads.path());

    client
        .create(BdCreateArgs {
            title: "t".into(),
            description: None,
            labels: vec![],
            assignee: None,
        })
        .expect("create ok");

    let seen = fs::read_to_string(&probe).expect("probe written");
    assert_eq!(seen, beads.path().display().to_string());
}

#[test]
fn test_bd_close_returns_closed_status() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    // `bd close --json` returns an array (single element), unlike create.
    let bin = fake_bd::write_script(
        tmp.path(),
        "fake-bd.sh",
        &format!(
            "cat <<'JSON'\n{}\nJSON\nexit 0",
            fake_bd::one_issue_array("abc-123", "ship it", "closed")
        ),
    );
    let client = client_with(bin, beads.path());

    let issue = client.close(&BdId::from("abc-123"), Some("done")).expect("close ok");

    assert_eq!(issue.status, BdStatus::Closed);
}

#[test]
fn test_bd_close_argv_round_trips_reason() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    let probe = tmp.path().join("argv.txt");
    // Capturing fixture writes argv then emits a closed-issue *array* (close
    // shape), so both the argv assertion and the parse succeed.
    let bin = fake_bd::write_script(
        tmp.path(),
        "fake-bd.sh",
        &format!(
            "printf '%s\\n' \"$@\" > '{probe}'\ncat <<'JSON'\n{json}\nJSON\nexit 0",
            probe = probe.display(),
            json = fake_bd::one_issue_array("abc-123", "ship it", "closed")
        ),
    );
    let client = client_with(bin, beads.path());

    client.close(&BdId::from("abc-123"), Some("done")).expect("close ok");

    let argv = read_argv(&probe);
    assert_eq!(argv, vec!["--json", "close", "abc-123", "--reason", "done"]);
}

#[test]
fn test_bd_list_with_label_filter_argv_and_parse() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    let probe = tmp.path().join("argv.txt");
    let since = chrono::DateTime::parse_from_rfc3339("2026-05-29T00:00:00Z")
        .expect("parse since")
        .with_timezone(&chrono::Utc);
    // Two issues in the array; also capture argv to prove -l is repeated per
    // label (AND filter) and --since carries the rfc3339 instant.
    let body = format!(
        "printf '%s\\n' \"$@\" > '{probe}'\ncat <<'JSON'\n[{a},{b}]\nJSON\nexit 0",
        probe = probe.display(),
        a = r#"{"id":"a","title":"one","status":"open","assignee":null,"owner":"x@y","labels":["hangar-v1"],"updated_at":"2026-05-29T12:00:00Z"}"#,
        b = r#"{"id":"b","title":"two","status":"in_progress","assignee":"stevie","owner":"x@y","labels":["hangar-v1","foo"],"updated_at":"2026-05-29T12:01:00Z"}"#,
    );
    let bin = fake_bd::write_script(tmp.path(), "fake-bd.sh", &body);
    let client = client_with(bin, beads.path());

    let issues = client
        .list(BdListFilter {
            labels: vec!["hangar-v1".into(), "foo".into()],
            since: Some(since),
            include_closed: false,
        })
        .expect("list ok");

    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].id, BdId::from("a"));
    assert_eq!(issues[1].status, BdStatus::InProgress);
    assert_eq!(issues[1].assignee.as_deref(), Some("stevie"));

    let argv = read_argv(&probe);
    assert_eq!(
        argv,
        vec![
            "--json",
            "list",
            "-l",
            "hangar-v1",
            "-l",
            "foo",
            "--since",
            &since.to_rfc3339(),
        ]
    );
}

#[test]
fn test_bd_show_not_found_maps_to_not_found_error() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    let probe = tmp.path().join("argv.txt");
    // `bd show` of a missing id returns an empty array (exit 0); also capture
    // argv to prove the verb + id reach bd.
    let bin = fake_bd::write_script(
        tmp.path(),
        "fake-bd.sh",
        &format!(
            "printf '%s\\n' \"$@\" > '{}'\necho '[]'\nexit 0",
            probe.display()
        ),
    );
    let client = client_with(bin, beads.path());

    let err = client.show(&BdId::from("missing")).expect_err("expected NotFound");
    match err {
        BdError::NotFound(id) => assert_eq!(id, BdId::from("missing")),
        other => panic!("expected NotFound(missing), got {other:?}"),
    }

    assert_eq!(read_argv(&probe), vec!["--json", "show", "missing"]);
}

#[test]
fn test_bd_create_empty_object_array_maps_to_empty_result() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    // A degenerate create that returns an empty array surfaces as EmptyResult
    // (a malformed write response), NOT a misleading blank "not found".
    let bin = fake_bd::write_script(tmp.path(), "fake-bd.sh", "echo '[]'\nexit 0");
    let client = client_with(bin, beads.path());

    let err = client
        .create(BdCreateArgs {
            title: "t".into(),
            description: None,
            labels: vec![],
            assignee: None,
        })
        .expect_err("expected EmptyResult");
    match err {
        BdError::EmptyResult(verb) => assert_eq!(verb, "create"),
        other => panic!("expected EmptyResult(create), got {other:?}"),
    }
}

#[test]
fn test_bd_failure_nonzero_exit_returns_adapter_error() {
    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    let bin = fake_bd::write_script(
        tmp.path(),
        "fake-bd.sh",
        "echo 'boom: something broke' 1>&2\nexit 3",
    );
    let client = client_with(bin, beads.path());

    let err = client.list(BdListFilter::default()).expect_err("expected Cli error");
    match err {
        BdError::Cli { exit, stderr } => {
            assert_eq!(exit, Some(3));
            assert!(stderr.contains("boom"), "stderr={stderr:?}");
        }
        other => panic!("expected Cli error, got {other:?}"),
    }
}

#[test]
fn test_bd_bin_missing_returns_bd_not_installed() {
    let beads = TempDir::new().expect("beads");
    let missing = PathBuf::from("/nonexistent/definitely/not/a/bd/binary");

    let err =
        BdClient::new(missing, beads.path().to_path_buf()).expect_err("expected BdNotInstalled");
    assert!(matches!(err, BdError::BdNotInstalled(_)), "got {err:?}");
}

#[test]
fn test_concurrent_invocations_serialize_per_beads_dir() {
    use std::sync::Arc;
    use std::thread;

    let tmp = TempDir::new().expect("tmp");
    let beads = TempDir::new().expect("beads");
    // Fake bd that appends its PID to a shared sentinel file under an exclusive
    // window: it records "start", sleeps briefly, records "end". If two
    // invocations overlap, the file shows interleaved start/start; if the lock
    // serializes them, every start is immediately followed by its own end.
    let sentinel = beads.path().join("order.log");
    let bin = fake_bd::write_script(
        tmp.path(),
        "fake-bd.sh",
        &format!(
            "echo \"start $$\" >> '{s}'\nsleep 0.2\necho \"end $$\" >> '{s}'\ncat <<'JSON'\n{j}\nJSON\nexit 0",
            s = sentinel.display(),
            j = fake_bd::one_issue_object("id-x", "t", "open")
        ),
    );
    let client = Arc::new(client_with(bin, beads.path()));

    let handles: Vec<_> = (0..2)
        .map(|_| {
            let c = Arc::clone(&client);
            thread::spawn(move || {
                c.create(BdCreateArgs {
                    title: "t".into(),
                    description: None,
                    labels: vec![],
                    assignee: None,
                })
                .expect("create ok");
            })
        })
        .collect();
    for h in handles {
        h.join().expect("thread join");
    }

    let log = fs::read_to_string(&sentinel).expect("sentinel written");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 4, "expected 2 start + 2 end lines: {log:?}");
    // Serialized => each start is paired with the matching end before the next
    // start: lines 0,1 are one invocation (start/end), lines 2,3 the other.
    assert!(lines[0].starts_with("start"), "line0={:?}", lines[0]);
    assert!(lines[1].starts_with("end"), "line1={:?}", lines[1]);
    assert!(lines[2].starts_with("start"), "line2={:?}", lines[2]);
    assert!(lines[3].starts_with("end"), "line3={:?}", lines[3]);
}
