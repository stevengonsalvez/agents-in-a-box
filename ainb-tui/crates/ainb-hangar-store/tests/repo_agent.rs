//! Typed repository round-trip for `agent` rows.
//!
//! Proves the `AgentRepo` sqlx wrapper inserts an [`Agent`] and reads it back
//! identically, with the `agent.runtime_id` FK (required by the Multica pattern)
//! satisfied by a real `agent_runtime` row.

use ainb_hangar_store::Store;
use ainb_hangar_store::repo::agent::{Agent, AgentRepo};

/// Seed the FK chain (workspace -> user -> `agent_runtime`) that an `agent` row
/// requires, returning `(workspace_id, runtime_id, owner_id)`.
async fn seed_fk_chain(store: &Store) -> (String, String, String) {
    let ws = "ws-1";
    let user = "user-1";
    let runtime = "rt-1";

    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");

    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(user)
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
    .bind(runtime)
    .bind(ws)
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(store.pool())
    .await
    .expect("insert agent_runtime");

    (ws.to_string(), runtime.to_string(), user.to_string())
}

#[tokio::test]
async fn insert_and_get_agent_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (workspace_id, runtime_id, owner_id) = seed_fk_chain(&store).await;

    let agent = Agent {
        id: "agent-1".to_string(),
        workspace_id,
        name: "Builder".to_string(),
        runtime_id,
        instructions: Some("Build things carefully.".to_string()),
        visibility: "workspace".to_string(),
        owner_id,
    };

    AgentRepo::insert(store.pool(), &agent).await.expect("insert agent");

    let got = AgentRepo::get(store.pool(), "agent-1")
        .await
        .expect("get agent")
        .expect("agent present");

    assert_eq!(got, agent, "round-tripped agent must equal inserted agent");
}
