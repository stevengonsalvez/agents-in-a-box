//! Behavioural proof of the per-agent-env redaction contract (multica parity #30).
//!
//! An agent's `agent_env` values are secrets. This file asserts the four things
//! that must simultaneously hold at the STORE boundary:
//!
//! 1. reading a row back never renders the value (`Debug`),
//! 2. serialising the row's env never renders the value (`Serialize`),
//! 3. the exec seam still gets the PLAINTEXT (redaction must not be achieved by
//!    breaking dispatch),
//! 4. the stored column bytes are byte-identical to what a pre-change build
//!    wrote — which is why item 30 needs no migration and an old DB reads the
//!    same.

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};

/// The canary. Any appearance of this literal outside the exec seam is a leak.
const SECRET: &str = "sk-live-DEADBEEF01";

/// Seed the FK chain (workspace -> user -> `agent_runtime`) an `agent` row needs.
async fn seed_fk_chain(store: &Store) -> (String, String, String) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");

    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("a@example.com")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert user");

    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("rt-1")
    .bind("ws-1")
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(store.pool())
    .await
    .expect("insert agent_runtime");

    ("ws-1".to_string(), "rt-1".to_string(), "user-1".to_string())
}

/// Insert a secret-carrying agent and read it back through the typed repo.
async fn seeded_agent(store: &Store) -> Agent {
    let (workspace_id, runtime_id, owner_id) = seed_fk_chain(store).await;
    let agent = Agent {
        id: "agent-secret".to_string(),
        workspace_id,
        name: "secretive".to_string(),
        runtime_id,
        owner_id,
        agent_env: vec![("SECRET_TOKEN".to_string(), SECRET.to_string())].into(),
        ..Agent::default()
    };
    AgentRepo::insert(store.pool(), &agent).await.expect("insert agent");
    AgentRepo::get(store.pool(), "agent-secret")
        .await
        .expect("get agent")
        .expect("agent exists")
}

#[tokio::test]
async fn stored_env_never_appears_in_the_row_debug() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seeded_agent(&store).await;

    let rendered = format!("{agent:?}");
    assert!(
        !rendered.contains(SECRET),
        "row Debug leaked the env value: {rendered}"
    );
    assert!(
        rendered.contains("SECRET_TOKEN"),
        "row Debug should keep the key: {rendered}"
    );
    assert!(
        rendered.contains("****"),
        "row Debug should show the mask: {rendered}"
    );
}

#[tokio::test]
async fn serde_of_the_row_env_masks_values() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seeded_agent(&store).await;

    let json = serde_json::to_string(&agent.agent_env).expect("serialize env");
    assert!(!json.contains(SECRET), "serde leaked the env value: {json}");
    assert!(json.contains("****"), "serde should emit the mask: {json}");
    assert!(
        json.contains("SECRET_TOKEN"),
        "serde should keep the key: {json}"
    );
}

#[tokio::test]
async fn exec_seam_still_gets_the_plaintext() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let agent = seeded_agent(&store).await;

    assert_eq!(
        agent.agent_env.expose_for_child_env(),
        vec![("SECRET_TOKEN".to_string(), SECRET.to_string())],
        "the child env must still receive the real value — redaction must not break dispatch"
    );
}

#[tokio::test]
async fn db_column_bytes_are_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let _ = seeded_agent(&store).await;

    let raw: String = sqlx::query_scalar("SELECT agent_env FROM agent WHERE id = ?")
        .bind("agent-secret")
        .fetch_one(store.pool())
        .await
        .expect("read raw agent_env");

    assert_eq!(
        raw,
        format!(r#"{{"SECRET_TOKEN":"{SECRET}"}}"#),
        "the column still stores the pre-change JSON-object bytes — no migration needed"
    );
}
