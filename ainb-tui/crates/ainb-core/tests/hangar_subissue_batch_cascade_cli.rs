//! The end-to-end acceptance for multica parity #3-rest: **two sub-issues of the
//! same stage completing together produce ONE aggregated roll-up comment on the
//! parent.**
//!
//! Drives the REAL `ainb` binary against an isolated `$AINB_HANGAR_HOME`, so the
//! whole chain runs end to end: `issue create --parent … --stage 1` (a staged
//! sibling set could not even be AUTHORED from the CLI before this change) →
//! `issue batch-state --state done <A> <B>` → the parent's timeline.
//!
//! Decoy discipline: every assertion here is an EXACT COUNT. "A roll-up comment
//! exists" stays green under the broken one-comment-per-child behaviour, which is
//! precisely the vacuous-green this test exists to rule out — so the test counts
//! occurrences and demands exactly 1.

use std::path::PathBuf;
use std::process::Command;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

/// Run `ainb <args>` against an isolated hangar home. Returns (ok, stdout+stderr).
fn run(home: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(ainb_bin())
        .args(args)
        .env("AINB_HANGAR_HOME", home)
        .env("HOME", home)
        .output()
        .expect("spawn ainb");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out.status.success(), format!("{stdout}{stderr}"))
}

/// Create an issue and return its id, parsed from the `created issue <id>` ack.
fn create_issue(home: &std::path::Path, args: &[&str]) -> String {
    let mut argv = vec!["hangar", "issue", "create"];
    argv.extend_from_slice(args);
    let (ok, out) = run(home, &argv);
    assert!(ok, "issue create must exit 0; out={out}");
    out.lines()
        .find_map(|l| l.strip_prefix("created issue ").map(str::trim))
        .unwrap_or_else(|| panic!("no `created issue <id>` ack:\n{out}"))
        .to_string()
}

/// The whole journey, ordered because each step depends on the previous one's
/// persisted state.
#[test]
fn batch_state_posts_one_aggregated_rollup_on_the_parent() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    let parent = create_issue(home, &["--title", "Parent"]);
    let a = create_issue(home, &["--title", "A", "--parent", &parent, "--stage", "1"]);
    let b = create_issue(home, &["--title", "B", "--parent", &parent, "--stage", "1"]);
    assert_ne!(a, b, "two distinct children");

    // Both stage-1 children complete in ONE batch → the barrier closes once.
    let (ok, out) = run(
        home,
        &["hangar", "issue", "batch-state", "--state", "done", &a, &b],
    );
    assert!(ok, "batch-state must exit 0; out={out}");
    assert!(
        out.contains("updated 2 of 2 issue(s) to done"),
        "both children must transition:\n{out}"
    );
    let rollups = out
        .lines()
        .filter(|l| l.contains("posted sub-issue roll-up on parent"))
        .count();
    assert_eq!(
        rollups, 1,
        "EXACTLY one roll-up line — one per child is the bug:\n{out}"
    );
    assert!(
        out.contains(&format!(
            "posted sub-issue roll-up on parent {parent} (2/2) covering 2 sub-issues"
        )),
        "the roll-up must report both children:\n{out}"
    );

    // The parent's narrative carries exactly ONE cascade comment, naming both.
    let (ok, timeline) = run(home, &["hangar", "issue", "timeline", &parent]);
    assert!(ok, "timeline must exit 0; out={timeline}");
    let cascade_comments = timeline
        .lines()
        .filter(|l| l.contains("sub-issues complete."))
        .count();
    assert_eq!(
        cascade_comments, 1,
        "EXACTLY one cascade comment on the parent, never one per child:\n{timeline}"
    );
    let line = timeline
        .lines()
        .find(|l| l.contains("sub-issues complete."))
        .expect("the cascade comment");
    assert!(
        line.contains(&a) && line.contains(&b),
        "the ONE comment must name BOTH children:\n{line}"
    );
    assert!(
        line.contains("Sub-issues "),
        "plural form for an aggregated comment:\n{line}"
    );
    assert!(
        line.contains("Closed stage 1."),
        "the comment names the barrier it closed:\n{line}"
    );
}

/// A stage below 1 is refused by the CLI parser — the barrier ordinal is 1-based,
/// so `--stage 0` must fail loudly rather than persist an inert value.
#[test]
fn stage_zero_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let parent = create_issue(home, &["--title", "Parent"]);
    let (ok, out) = run(
        home,
        &[
            "hangar", "issue", "create", "--title", "bad", "--parent", &parent, "--stage", "0",
        ],
    );
    assert!(!ok, "--stage 0 must be refused; out={out}");
}
