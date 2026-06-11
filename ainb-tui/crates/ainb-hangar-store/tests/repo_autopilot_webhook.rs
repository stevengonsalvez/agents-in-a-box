//! Integration coverage for the webhook repo + 0600 secret store (e38.18).
//!
//! Proves the persistence contract behind webhook-triggered autopilots:
//! `set_config` / `get_config` round-trip the three columns, deliveries record +
//! list latest-first, both reads are workspace-scoped (a foreign id sees
//! nothing), and the 0600 secret file round-trips a recoverable secret with
//! owner-only permissions while the database holds only the digest.

use ainb_hangar_core::ids::{AutopilotId, WorkspaceId};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::autopilot_webhook::{
    AutopilotWebhookRepo, DeliveryOutcome, NewDelivery, WebhookConfig, WebhookSecretStore,
};
use sqlx::SqlitePool;

/// Seed a workspace + user + runtime + agent + one autopilot, returning the
/// `(workspace_id, autopilot_id)` pair. A second workspace + autopilot is seeded
/// too so the scoping assertions have a foreign tenant to probe.
async fn seed(pool: &SqlitePool) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-1','a','A',0)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','b','B',0)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1','a@b.c',0)")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1','u-1','owner')")
        .execute(pool)
        .await
        .unwrap();
    for (rt, ws) in [("rt-1", "ws-1"), ("rt-2", "ws-2")] {
        sqlx::query(
            "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
             VALUES (?, ?, 'd', 'claude', 'local', 'online')",
        )
        .bind(rt)
        .bind(ws)
        .execute(pool)
        .await
        .unwrap();
    }
    for (ag, ws, rt) in [("ag-1", "ws-1", "rt-1"), ("ag-2", "ws-2", "rt-2")] {
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES (?, ?, 'A', ?, 'workspace', 'u-1')",
        )
        .bind(ag)
        .bind(ws)
        .bind(rt)
        .execute(pool)
        .await
        .unwrap();
    }
    for (ap, ws, ag) in [("ap-1", "ws-1", "ag-1"), ("ap-2", "ws-2", "ag-2")] {
        sqlx::query(
            "INSERT INTO autopilot \
             (id, workspace_id, agent_id, name, cron_expr, max_concurrent_runs, enabled, created_at) \
             VALUES (?, ?, ?, 'n', '0 0 * * *', 1, 1, 0)",
        )
        .bind(ap)
        .bind(ws)
        .bind(ag)
        .execute(pool)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn set_and_get_config_round_trip_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed(store.pool()).await;
    let ws1 = WorkspaceId::from_str("ws-1").unwrap();
    let ws2 = WorkspaceId::from_str("ws-2").unwrap();
    let ap1 = AutopilotId::from_str("ap-1").unwrap();

    // Default: webhook OFF, no secret, no filter.
    let cfg = AutopilotWebhookRepo::get_config(store.pool(), &ws1, &ap1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cfg, WebhookConfig::default());

    // Enable + set a digest + a filter.
    let want = WebhookConfig {
        enabled: true,
        secret_sha256: Some("deadbeef".repeat(8)),
        event_filter: Some("push".to_string()),
    };
    AutopilotWebhookRepo::set_config(store.pool(), &ws1, &ap1, &want).await.unwrap();
    let got = AutopilotWebhookRepo::get_config(store.pool(), &ws1, &ap1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(got, want, "config round-trips through the three columns");

    // A foreign workspace cannot read ap-1's config (tenant isolation).
    assert!(
        AutopilotWebhookRepo::get_config(store.pool(), &ws2, &ap1)
            .await
            .unwrap()
            .is_none(),
        "ap-1 is invisible from ws-2"
    );
    // And a foreign-workspace set touches no row: ap-1's config from ws-1 is
    // unchanged.
    AutopilotWebhookRepo::set_config(store.pool(), &ws2, &ap1, &WebhookConfig::default())
        .await
        .unwrap();
    let still = AutopilotWebhookRepo::get_config(store.pool(), &ws1, &ap1)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(still, want, "a ws-2 set must not clear ws-1's ap-1 config");
}

#[tokio::test]
async fn deliveries_record_and_list_latest_first_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    seed(store.pool()).await;
    let ws1 = WorkspaceId::from_str("ws-1").unwrap();
    let ws2 = WorkspaceId::from_str("ws-2").unwrap();
    let ap1 = AutopilotId::from_str("ap-1").unwrap();

    for (at, outcome, run) in [
        (10, DeliveryOutcome::Rejected, None),
        (20, DeliveryOutcome::Filtered, None),
        (30, DeliveryOutcome::Fired, Some("run-x".to_string())),
    ] {
        AutopilotWebhookRepo::record_delivery(
            store.pool(),
            &NewDelivery {
                autopilot_id: ap1.clone(),
                received_at: at,
                outcome,
                event: Some("push".to_string()),
                http_status: if matches!(outcome, DeliveryOutcome::Rejected) {
                    401
                } else {
                    200
                },
                run_id: run,
                detail: None,
            },
        )
        .await
        .unwrap();
    }

    let rows = AutopilotWebhookRepo::list_deliveries(store.pool(), &ws1, &ap1, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].received_at, 30, "latest-first ordering");
    assert_eq!(rows[0].outcome, "fired");
    assert_eq!(rows[0].run_id.as_deref(), Some("run-x"));
    assert_eq!(rows[2].outcome, "rejected");
    assert_eq!(rows[2].http_status, 401);

    // A foreign workspace sees no deliveries for ap-1.
    let foreign = AutopilotWebhookRepo::list_deliveries(store.pool(), &ws2, &ap1, 10)
        .await
        .unwrap();
    assert!(foreign.is_empty(), "ap-1 deliveries invisible from ws-2");
}

#[cfg(unix)]
#[tokio::test]
async fn secret_store_round_trips_with_0600_perms() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().unwrap();
    let secrets = WebhookSecretStore::in_home(dir.path());

    // Missing file: no secret yet.
    assert!(secrets.get("ap-1").unwrap().is_none());

    secrets.set("ap-1", "s3cr3t-plaintext").unwrap();
    secrets.set("ap-2", "another").unwrap();
    assert_eq!(
        secrets.get("ap-1").unwrap().as_deref(),
        Some("s3cr3t-plaintext")
    );
    assert_eq!(secrets.get("ap-2").unwrap().as_deref(), Some("another"));

    // 0600: owner-only.
    let mode = std::fs::metadata(secrets.path()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "secret file must be owner-only");

    // Remove one, the other survives.
    secrets.remove("ap-1").unwrap();
    assert!(secrets.get("ap-1").unwrap().is_none());
    assert_eq!(secrets.get("ap-2").unwrap().as_deref(), Some("another"));
}
