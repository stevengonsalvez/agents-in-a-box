//! Typed repository round-trips for `beads_mapping` rows (P2.1).
//!
//! `beads_mapping` correlates a Hangar entity (issue/task) with its mirrored
//! beads (`bd`) id, plus a `source` discriminant (`hangar` vs `swarm`) and a
//! `last_synced` wall-clock stamp the P2 sync loop maintains. These tests pin
//! the [`BeadsMappingRepo`] contract:
//!
//! - insert returns the row, `last_synced` round-trips at second precision
//! - lookup by either side (present + absent)
//! - `update_last_synced` bumps the timestamp
//! - `list_since` is a delta query for reconcile
//! - each id side is independently UNIQUE (a second row reusing either id errs)
//! - delete by hangar id

use chrono::{DateTime, TimeZone, Utc};

use ainb_hangar_store::repo::beads_mapping::{
    BeadsMappingRepo, BeadsMappingRow, MappingKind, MappingSource,
};
use ainb_hangar_store::Store;

/// A fixed wall-clock instant, truncated to whole seconds so equality holds
/// across the ISO-8601 TEXT round-trip (the column stores second precision).
fn t(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().expect("valid instant")
}

/// Build a canonical row with overridable ids and `last_synced`.
fn row(hangar_id: &str, bd_id: &str, last_synced: DateTime<Utc>) -> BeadsMappingRow {
    BeadsMappingRow {
        hangar_id: hangar_id.to_string(),
        bd_id: bd_id.to_string(),
        hangar_kind: MappingKind::Issue,
        bd_kind: MappingKind::Issue,
        source: MappingSource::Hangar,
        last_synced,
    }
}

#[tokio::test]
async fn test_insert_returns_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    let t0 = t(1_700_000_000);
    let input = row("iss_01HX", "abc-123", t0);

    let returned = repo.insert(&input).await.expect("insert returns row");

    assert_eq!(returned.hangar_id, "iss_01HX");
    assert_eq!(returned.bd_id, "abc-123");
    assert_eq!(returned.hangar_kind, MappingKind::Issue);
    assert_eq!(returned.bd_kind, MappingKind::Issue);
    assert_eq!(returned.source, MappingSource::Hangar);
    assert_eq!(
        returned.last_synced, t0,
        "last_synced must round-trip at second precision"
    );
}

#[tokio::test]
async fn test_lookup_by_hangar_id_present_and_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    repo.insert(&row("iss_01HX", "abc-123", t(1_700_000_000)))
        .await
        .expect("insert");

    let present = repo
        .find_by_hangar("iss_01HX")
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(present.bd_id, "abc-123");

    let absent = repo.find_by_hangar("iss_missing").await.expect("query");
    assert!(absent.is_none(), "absent hangar id yields None");
}

#[tokio::test]
async fn test_lookup_by_bd_id_present_and_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    repo.insert(&row("iss_01HX", "abc-123", t(1_700_000_000)))
        .await
        .expect("insert");

    let present = repo
        .find_by_bd("abc-123")
        .await
        .expect("query")
        .expect("row present");
    assert_eq!(present.hangar_id, "iss_01HX");

    let absent = repo.find_by_bd("zzz-999").await.expect("query");
    assert!(absent.is_none(), "absent bd id yields None");
}

#[tokio::test]
async fn test_update_last_synced_bumps_timestamp() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    let t0 = t(1_700_000_000);
    let t1 = t(1_700_000_500);
    repo.insert(&row("iss_01HX", "abc-123", t0))
        .await
        .expect("insert");

    repo.update_last_synced("iss_01HX", t1)
        .await
        .expect("update last_synced");

    let got = repo
        .find_by_hangar("iss_01HX")
        .await
        .expect("query")
        .expect("present");
    assert_eq!(got.last_synced, t1, "last_synced must reflect the update");
}

#[tokio::test]
async fn test_list_since_returns_rows_at_or_after_t() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    let early = t(1_700_000_000);
    let cutoff = t(1_700_000_500);
    let late = t(1_700_001_000);

    repo.insert(&row("iss_early", "bd-early", early))
        .await
        .expect("insert early");
    repo.insert(&row("iss_cutoff", "bd-cutoff", cutoff))
        .await
        .expect("insert cutoff");
    repo.insert(&row("iss_late", "bd-late", late))
        .await
        .expect("insert late");

    let since = repo.list_since(cutoff).await.expect("list_since");
    let ids: Vec<&str> = since.iter().map(|r| r.hangar_id.as_str()).collect();

    assert!(
        ids.contains(&"iss_cutoff"),
        "row at exactly the cutoff is inclusive"
    );
    assert!(ids.contains(&"iss_late"), "row after the cutoff is included");
    assert!(
        !ids.contains(&"iss_early"),
        "row before the cutoff is excluded"
    );
}

#[tokio::test]
async fn test_unique_constraint_hangar_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    repo.insert(&row("iss_01HX", "abc-123", t(1_700_000_000)))
        .await
        .expect("first insert");

    // Same hangar_id, different bd_id => UNIQUE violation on the hangar side.
    let err = repo
        .insert(&row("iss_01HX", "different-bd", t(1_700_000_001)))
        .await
        .expect_err("second insert reusing hangar_id must violate UNIQUE");

    let db_err = err
        .as_database_error()
        .expect("expected a database constraint error");
    let msg = db_err.message().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("constraint"),
        "expected a UNIQUE constraint failure, got: {err}"
    );
}

#[tokio::test]
async fn test_unique_constraint_bd_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    repo.insert(&row("iss_01HX", "abc-123", t(1_700_000_000)))
        .await
        .expect("first insert");

    // Different hangar_id, same bd_id => UNIQUE violation on the bd side.
    let err = repo
        .insert(&row("iss_other", "abc-123", t(1_700_000_001)))
        .await
        .expect_err("second insert reusing bd_id must violate UNIQUE");

    let db_err = err
        .as_database_error()
        .expect("expected a database constraint error");
    let msg = db_err.message().to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("constraint"),
        "expected a UNIQUE constraint failure, got: {err}"
    );
}

#[tokio::test]
async fn test_delete_by_hangar_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    repo.insert(&row("iss_01HX", "abc-123", t(1_700_000_000)))
        .await
        .expect("insert");

    repo.delete_by_hangar("iss_01HX")
        .await
        .expect("delete by hangar id");

    let gone = repo.find_by_hangar("iss_01HX").await.expect("query");
    assert!(gone.is_none(), "row must be gone after delete");
}

#[tokio::test]
async fn test_swarm_source_round_trips() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let repo = BeadsMappingRepo::new(store.pool());

    let mut input = row("iss_swarm", "bd-swarm", t(1_700_000_000));
    input.source = MappingSource::Swarm;
    input.hangar_kind = MappingKind::Task;
    input.bd_kind = MappingKind::Task;

    repo.insert(&input).await.expect("insert");

    let got = repo
        .find_by_hangar("iss_swarm")
        .await
        .expect("query")
        .expect("present");
    assert_eq!(got.source, MappingSource::Swarm);
    assert_eq!(got.hangar_kind, MappingKind::Task);
    assert_eq!(got.bd_kind, MappingKind::Task);
}
