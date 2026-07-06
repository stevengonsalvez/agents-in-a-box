//! CLI surface test for `ainb hangar autopilot webhook|deliveries` (e38.18).
//!
//! Drives the real `ainb` binary against an isolated `$AINB_HANGAR_HOME` tempdir
//! (mirroring `hangar_autopilot_cli.rs`). This is the cheap CLI leg of the
//! e38.18 acceptance: it proves the operator-facing flow — create an autopilot →
//! enable its webhook (mints + prints the secret + URL once) → verify the digest
//! is persisted (not the plaintext) → the 0600 secret file holds the recoverable
//! plaintext → rotate the secret → list deliveries (empty) — without any HTTP or
//! tmux. The HTTP-level fire/reject security proof lives in the daemon crate's
//! `webhook_ingress_http.rs`.

use std::path::PathBuf;
use std::process::Command;

use ainb_hangar_store::Store;
use sqlx::SqlitePool;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

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

async fn seed_agent(pool: &SqlitePool) {
    sqlx::query(
        "INSERT INTO workspace (id, slug, name, created_at) \
         VALUES ('ws-1', 'default', 'Default Workspace', 0)",
    )
    .execute(pool)
    .await
    .expect("seed workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES ('u-1', 'a@b.c', 0)")
        .execute(pool)
        .await
        .expect("seed user");
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES ('ws-1', 'u-1', 'owner')")
        .execute(pool)
        .await
        .expect("seed member");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES ('rt-1', 'ws-1', 'd-1', 'claude', 'local', 'online')",
    )
    .execute(pool)
    .await
    .expect("seed runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES ('ag-1', 'ws-1', 'Tester', 'rt-1', 'workspace', 'u-1')",
    )
    .execute(pool)
    .await
    .expect("seed agent");
}

#[tokio::test]
async fn cli_autopilot_webhook_enable_rotate_deliveries() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let store = Store::open_in(tmp.path()).await.expect("open store");
        seed_agent(store.pool()).await;
    }

    // Create an autopilot to attach the webhook to.
    let (ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "autopilot",
            "create",
            "--name",
            "hooked",
            "--cron",
            "0 9 * * *",
            "--agent",
            "ag-1",
        ],
    );
    assert!(ok, "create should exit 0; out={out}");
    let id = out
        .split_whitespace()
        .nth(2)
        .map(str::to_string)
        .expect("create output carries id");

    // Enable the webhook: mints + prints the URL + secret once, sets an event
    // filter.
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "autopilot", "webhook", &id, "--event", "push"],
    );
    assert!(ok, "webhook enable should exit 0; out={out}");
    assert!(
        out.contains("enabled webhook"),
        "missing enable ack:\n{out}"
    );
    assert!(
        out.contains(&format!("/hangar/webhook/{id}")),
        "missing webhook URL:\n{out}"
    );
    assert!(
        out.contains("secret:"),
        "missing one-time secret line:\n{out}"
    );
    assert!(
        out.contains("event filter: push"),
        "missing event filter:\n{out}"
    );

    // Pull the printed secret and confirm the DB stores its DIGEST, not the
    // plaintext, and the 0600 file holds the recoverable plaintext.
    let secret = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("secret: "))
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .expect("secret line present");
    assert_eq!(secret.len(), 64, "secret is 64 hex chars: {secret}");

    let store = Store::open_in(tmp.path()).await.expect("reopen store");
    let digest: Option<String> =
        sqlx::query_scalar("SELECT webhook_secret_sha256 FROM autopilot WHERE id = ?")
            .bind(&id)
            .fetch_one(store.pool())
            .await
            .expect("read digest");
    let digest = digest.expect("a secret digest is persisted");
    assert_eq!(
        digest,
        ainb_hangar_core::token::sha256_hex(&secret),
        "the DB stores the digest of the printed secret"
    );
    assert_ne!(digest, secret, "the DB must NOT store the plaintext secret");
    let enabled: i64 = sqlx::query_scalar("SELECT webhook_enabled FROM autopilot WHERE id = ?")
        .bind(&id)
        .fetch_one(store.pool())
        .await
        .expect("read enabled");
    assert_eq!(enabled, 1, "webhook is enabled");

    // The 0600 secret file holds the recoverable plaintext.
    let secret_file = tmp.path().join("hangar").join("webhook_secrets.json");
    let contents = std::fs::read_to_string(&secret_file).expect("secret file exists");
    assert!(
        contents.contains(&secret),
        "secret file holds the recoverable plaintext"
    );

    // Rotate: a new secret is printed and the digest changes.
    let (ok, out) = run(
        tmp.path(),
        &["hangar", "autopilot", "webhook", &id, "--rotate"],
    );
    assert!(ok, "webhook rotate should exit 0; out={out}");
    let secret2 = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("secret: "))
        .map(|rest| rest.split_whitespace().next().unwrap_or("").to_string())
        .expect("rotate prints a new secret");
    assert_ne!(secret2, secret, "rotate mints a fresh secret");
    let digest2: Option<String> =
        sqlx::query_scalar("SELECT webhook_secret_sha256 FROM autopilot WHERE id = ?")
            .bind(&id)
            .fetch_one(store.pool())
            .await
            .expect("read digest after rotate");
    assert_eq!(
        digest2.unwrap(),
        ainb_hangar_core::token::sha256_hex(&secret2),
        "rotate persists the new digest"
    );

    // deliveries: a fresh autopilot has no deliveries yet.
    let (ok, out) = run(tmp.path(), &["hangar", "autopilot", "deliveries", &id]);
    assert!(ok, "deliveries should exit 0; out={out}");
    assert!(
        out.contains("no webhook deliveries"),
        "empty deliveries list:\n{out}"
    );
}

#[tokio::test]
async fn cli_autopilot_webhook_disable_clears_secret() {
    let tmp = tempfile::tempdir().unwrap();
    {
        let store = Store::open_in(tmp.path()).await.expect("open store");
        seed_agent(store.pool()).await;
    }
    let (_ok, out) = run(
        tmp.path(),
        &[
            "hangar",
            "autopilot",
            "create",
            "--name",
            "h",
            "--cron",
            "0 9 * * *",
            "--agent",
            "ag-1",
        ],
    );
    let id = out.split_whitespace().nth(2).map(str::to_string).expect("id");
    run(tmp.path(), &["hangar", "autopilot", "webhook", &id]);

    let (ok, out) = run(
        tmp.path(),
        &["hangar", "autopilot", "webhook", &id, "--disable"],
    );
    assert!(ok, "disable should exit 0; out={out}");
    assert!(
        out.contains("disabled webhook"),
        "missing disable ack:\n{out}"
    );

    let store = Store::open_in(tmp.path()).await.expect("reopen store");
    let digest: Option<String> =
        sqlx::query_scalar("SELECT webhook_secret_sha256 FROM autopilot WHERE id = ?")
            .bind(&id)
            .fetch_one(store.pool())
            .await
            .expect("read digest");
    assert!(digest.is_none(), "disable clears the secret digest");
    let enabled: i64 = sqlx::query_scalar("SELECT webhook_enabled FROM autopilot WHERE id = ?")
        .bind(&id)
        .fetch_one(store.pool())
        .await
        .expect("read enabled");
    assert_eq!(enabled, 0, "disable turns the webhook off");
}
