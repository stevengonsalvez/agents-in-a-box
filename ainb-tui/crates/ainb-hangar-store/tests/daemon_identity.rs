//! The daemon identity (migration 0100, spec D11, #1066), against a real
//! ephemeral `SQLite` database: minted once, kept across boots, and named on
//! every fleet row written after the mint.

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::idgen::FixedIdGen;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::daemon_identity::{DaemonIdentityRepo, UNMINTED_HOST_ID};
use ainb_hangar_store::repo::fleet::{
    FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};
use sqlx::SqlitePool;

const FIRST: &str = "01K5A0000000000000000FIRST";
const SECOND: &str = "01K5A000000000000000SECOND";
const NOW: i64 = 1_700_000_000_000;

fn event(event_id: &str, session_key: &str) -> NewFleetEvent {
    NewFleetEvent {
        event_id: event_id.to_string(),
        session_key: session_key.to_string(),
        observed_at: NOW,
        authority: ObservationAuthority::Authoritative,
        event_type: "SessionStart".to_string(),
        payload: "{}".to_string(),
        patch: FleetSessionPatch {
            provider: Some("claude".to_string()),
            ..FleetSessionPatch::default()
        },
    }
}

async fn host_of_session(pool: &SqlitePool, session_key: &str) -> String {
    sqlx::query_scalar("SELECT host_id FROM fleet_session WHERE session_key = ?")
        .bind(session_key)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn hosts_of_events(pool: &SqlitePool) -> Vec<String> {
    sqlx::query_scalar("SELECT host_id FROM fleet_event ORDER BY revision")
        .fetch_all(pool)
        .await
        .unwrap()
}

#[tokio::test]
async fn the_first_boot_mints_and_every_later_boot_reads_the_same_id() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    assert_eq!(DaemonIdentityRepo::read(pool).await.unwrap(), None);

    let first = DaemonIdentityRepo::mint_or_read(
        pool,
        &FixedIdGen::new(vec![FIRST.to_string()]),
        &FixedClock(NOW),
    )
    .await
    .unwrap();
    assert!(first.minted);
    assert_eq!(first.identity.host_id, FIRST);
    assert_eq!(first.identity.created_at, NOW);

    let again = DaemonIdentityRepo::mint_or_read(
        pool,
        &FixedIdGen::new(vec![SECOND.to_string()]),
        &FixedClock(NOW + 1),
    )
    .await
    .unwrap();
    assert!(!again.minted, "a second boot mints nothing");
    assert_eq!(again.identity, first.identity);
    assert_eq!(
        DaemonIdentityRepo::read(pool).await.unwrap(),
        Some(first.identity)
    );
}

#[tokio::test]
async fn the_identity_survives_reopening_the_database() {
    let dir = tempfile::tempdir().unwrap();
    {
        let store = Store::open_in(dir.path()).await.unwrap();
        DaemonIdentityRepo::mint_or_read(
            store.pool(),
            &FixedIdGen::new(vec![FIRST.to_string()]),
            &FixedClock(NOW),
        )
        .await
        .unwrap();
    }
    let store = Store::open_in(dir.path()).await.unwrap();
    let read = DaemonIdentityRepo::read(store.pool()).await.unwrap().unwrap();
    assert_eq!(read.host_id, FIRST);
}

#[tokio::test]
async fn rows_written_before_the_mint_are_adopted_and_rows_after_it_are_stamped() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();

    FleetRepo::apply_event(pool, &event("e-before", "claude:before")).await.unwrap();
    assert_eq!(
        host_of_session(pool, "claude:before").await,
        UNMINTED_HOST_ID
    );
    assert_eq!(hosts_of_events(pool).await, vec![UNMINTED_HOST_ID]);

    let outcome = DaemonIdentityRepo::mint_or_read(
        pool,
        &FixedIdGen::new(vec![FIRST.to_string()]),
        &FixedClock(NOW),
    )
    .await
    .unwrap();
    assert_eq!(outcome.adopted_sessions, 1);
    assert_eq!(host_of_session(pool, "claude:before").await, FIRST);

    // The event table is adopted in batches after boot, not in the mint.
    assert_eq!(hosts_of_events(pool).await, vec![UNMINTED_HOST_ID]);
    assert_eq!(
        DaemonIdentityRepo::adopt_local_events(pool, FIRST, 10).await.unwrap(),
        1
    );
    assert_eq!(
        DaemonIdentityRepo::adopt_local_events(pool, FIRST, 10).await.unwrap(),
        0
    );

    FleetRepo::apply_event(pool, &event("e-after", "claude:after")).await.unwrap();
    FleetRepo::apply_event(pool, &event("e-again", "claude:before")).await.unwrap();
    assert_eq!(host_of_session(pool, "claude:after").await, FIRST);
    assert_eq!(hosts_of_events(pool).await, vec![FIRST, FIRST, FIRST]);
}

#[tokio::test]
async fn event_adoption_moves_at_most_one_batch_per_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let pool = store.pool();
    for n in 0..5 {
        FleetRepo::apply_event(pool, &event(&format!("e-{n}"), &format!("claude:s{n}")))
            .await
            .unwrap();
    }

    assert_eq!(
        DaemonIdentityRepo::adopt_local_events(pool, FIRST, 2).await.unwrap(),
        2
    );
    assert_eq!(
        DaemonIdentityRepo::adopt_local_events(pool, FIRST, 2).await.unwrap(),
        2
    );
    assert_eq!(
        DaemonIdentityRepo::adopt_local_events(pool, FIRST, 2).await.unwrap(),
        1
    );
    assert_eq!(
        DaemonIdentityRepo::adopt_local_events(pool, FIRST, 2).await.unwrap(),
        0
    );
    assert!(hosts_of_events(pool).await.iter().all(|host| host == FIRST));
}
