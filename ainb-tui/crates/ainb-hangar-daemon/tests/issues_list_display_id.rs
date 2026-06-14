//! 63l.3 — daemon `hangar/issues_list` surfaces the human-facing display id.
//!
//! The `issues_list` snapshot mapper threads each issue's `HGR-<n>` display id
//! (its per-workspace creation ordinal, prefixed with the workspace's
//! `issue_prefix` or the `HGR` default) onto the wire [`IssueRow`] so the TUI's
//! issue list and the CLI listing can show it. These tests prove:
//!
//! - a workspace with NO configured prefix reads its issues under the `HGR`
//!   default, numbered by creation order (`HGR-1`, `HGR-2`, `HGR-3`);
//! - an explicitly-configured prefix is honoured verbatim before the `-<seq>`.

use ainb_hangar_daemon::rpc::snapshots::issues_list;
use ainb_hangar_daemon::seed::{WS_ID, seed_p4_fixture};
use ainb_hangar_store::Store;

#[tokio::test]
async fn issues_list_surfaces_hgr_display_id_in_creation_order() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();

    let issues = issues_list(store.pool(), WS_ID).await.unwrap();

    // The fixture seeds the workspace with NO issue_prefix (NULL), so every issue
    // reads under the HGR default, numbered by creation order: issue-1 is the
    // oldest (HGR-1), issue-2 next (HGR-2), issue-3 last (HGR-3).
    let issue_1 = issues.iter().find(|i| i.id.as_str() == "issue-1").expect("issue-1 present");
    assert_eq!(
        issue_1.display_id.as_deref(),
        Some("HGR-1"),
        "the oldest issue reads HGR-1 under the default prefix"
    );

    let issue_2 = issues.iter().find(|i| i.id.as_str() == "issue-2").expect("issue-2 present");
    assert_eq!(
        issue_2.display_id.as_deref(),
        Some("HGR-2"),
        "the second issue reads HGR-2"
    );

    let issue_3 = issues.iter().find(|i| i.id.as_str() == "issue-3").expect("issue-3 present");
    assert_eq!(
        issue_3.display_id.as_deref(),
        Some("HGR-3"),
        "the third issue reads HGR-3"
    );
}

#[tokio::test]
async fn issues_list_honours_a_configured_issue_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed_p4_fixture(store.pool()).await.unwrap();

    // Configure the workspace's issue_prefix to `OPS-` (an operator-set prefix
    // that already carries its own separator). The display id then reads
    // `OPS--1`? No — the join trims trailing whitespace only, not a hyphen, so
    // `OPS` (no trailing separator) reads `OPS-1`.
    sqlx::query("UPDATE workspace SET issue_prefix = 'OPS' WHERE id = ?")
        .bind(WS_ID)
        .execute(store.pool())
        .await
        .unwrap();

    let issues = issues_list(store.pool(), WS_ID).await.unwrap();

    let issue_1 = issues.iter().find(|i| i.id.as_str() == "issue-1").expect("issue-1 present");
    assert_eq!(
        issue_1.display_id.as_deref(),
        Some("OPS-1"),
        "a configured prefix is honoured before the -<seq>"
    );
}
