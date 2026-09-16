//! The daemon's minted host (spec D11, #1066) at its boot seams: the receipt
//! sweep resolves claims under both `local` and the minted id, and the event
//! adoption task leaves no `fleet_event` row under `local`.

use std::time::Duration;

use ainb_hangar_core::clock::SystemClock;
use ainb_hangar_core::idgen::SystemIdGen;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::daemon_identity::DaemonIdentityRepo;
use ainb_hangar_store::repo::fleet::{
    FleetRepo, FleetSessionPatch, NewFleetEvent, ObservationAuthority,
};
use ainb_hangar_store::repo::mutation_ledger::{
    LOCAL_PRINCIPAL, LedgerKey, MutationLedgerRepo, TIER_DEDUPE,
};

async fn mint(store: &Store) -> String {
    DaemonIdentityRepo::mint_or_read(store.pool(), &SystemIdGen, &SystemClock)
        .await
        .unwrap()
        .identity
        .host_id
}

#[tokio::test]
async fn the_boot_sweep_resolves_claims_under_local_and_the_minted_host() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let host_id = mint(&store).await;

    let keys = [
        LedgerKey::local("op-left-by-the-old-binary"),
        LedgerKey {
            host_id,
            principal: LOCAL_PRINCIPAL.to_string(),
            op_id: "op-left-by-this-binary".to_string(),
        },
    ];
    for key in &keys {
        MutationLedgerRepo::claim(
            store.pool(),
            key,
            "hangar/issue_update",
            "fp",
            TIER_DEDUPE,
            1_000,
        )
        .await
        .unwrap();
    }

    let report = ainb_hangar_daemon::receipt_sweep::run(store.pool()).await.unwrap();
    assert_eq!(report.ambiguous, 2, "{report:?}");
    for key in &keys {
        let row = MutationLedgerRepo::get(store.pool(), key).await.unwrap().unwrap();
        assert_eq!(row.status, "unknown", "{key:?}");
    }
}

#[tokio::test]
async fn event_adoption_leaves_no_fleet_event_under_local() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    for n in 0..3 {
        let event = NewFleetEvent {
            event_id: format!("e-{n}"),
            session_key: format!("claude:s{n}"),
            observed_at: 1_000,
            authority: ObservationAuthority::Authoritative,
            event_type: "SessionStart".to_string(),
            payload: "{}".to_string(),
            patch: FleetSessionPatch {
                provider: Some("claude".to_string()),
                ..FleetSessionPatch::default()
            },
        };
        FleetRepo::apply_event(store.pool(), &event).await.unwrap();
    }
    let host_id = mint(&store).await;

    tokio::time::timeout(
        Duration::from_secs(10),
        ainb_hangar_daemon::host_identity::spawn_event_adoption(
            store.pool().clone(),
            host_id.clone(),
        ),
    )
    .await
    .expect("adoption finishes")
    .unwrap();

    let hosts: Vec<String> = sqlx::query_scalar("SELECT host_id FROM fleet_event")
        .fetch_all(store.pool())
        .await
        .unwrap();
    assert_eq!(hosts, vec![host_id; 3]);
}
