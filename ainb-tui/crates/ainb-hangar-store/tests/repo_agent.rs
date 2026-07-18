//! Typed repository round-trip for `agent` rows.
//!
//! Proves the `AgentRepo` sqlx wrapper inserts an [`Agent`] and reads it back
//! identically, with the `agent.runtime_id` FK (required by the reference pattern)
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
        archived: false,
        model: None,
        cli_args: Vec::new(),
        mcp_config: None,
        thinking: None,
        agent_env: Vec::new(),
        provider: None,
        token_budget: None,
    };

    AgentRepo::insert(store.pool(), &agent).await.expect("insert agent");

    let got = AgentRepo::get(store.pool(), "agent-1")
        .await
        .expect("get agent")
        .expect("agent present");

    assert_eq!(got, agent, "round-tripped agent must equal inserted agent");
}

/// A config edit persists the model / args / MCP / thinking / env knobs, and a
/// partial edit leaves untouched fields alone (e38.15).
#[tokio::test]
async fn update_config_persists_and_is_partial() {
    use ainb_hangar_store::repo::agent::AgentConfigUpdate;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (workspace_id, runtime_id, owner_id) = seed_fk_chain(&store).await;

    let agent = Agent {
        id: "agent-1".to_string(),
        workspace_id: workspace_id.clone(),
        name: "Builder".to_string(),
        runtime_id,
        instructions: None,
        visibility: "workspace".to_string(),
        owner_id,
        archived: false,
        model: None,
        cli_args: Vec::new(),
        mcp_config: None,
        thinking: None,
        agent_env: Vec::new(),
        provider: None,
        token_budget: None,
    };
    AgentRepo::insert(store.pool(), &agent).await.expect("insert agent");

    // Edit the model, args, MCP, thinking, env, and rename in one call.
    let update = AgentConfigUpdate {
        name: Some("Builder Pro".to_string()),
        instructions: Some(Some("Be precise.".to_string())),
        model: Some(Some("claude-opus-4".to_string())),
        cli_args: Some(vec!["--verbose".to_string()]),
        mcp_config: Some(Some(r#"{"servers":{}}"#.to_string())),
        thinking: Some(Some("high".to_string())),
        agent_env: Some(vec![("FOO".to_string(), "bar".to_string())]),
        token_budget: Some(Some(750_000)),
    };
    let touched = AgentRepo::update_config(store.pool(), &workspace_id, "agent-1", &update)
        .await
        .expect("update config");
    assert!(touched, "a real edit touches one row");

    let got = AgentRepo::get(store.pool(), "agent-1").await.unwrap().unwrap();
    assert_eq!(got.name, "Builder Pro");
    assert_eq!(got.instructions.as_deref(), Some("Be precise."));
    assert_eq!(got.model.as_deref(), Some("claude-opus-4"));
    assert_eq!(got.cli_args, vec!["--verbose".to_string()]);
    assert_eq!(got.mcp_config.as_deref(), Some(r#"{"servers":{}}"#));
    assert_eq!(got.thinking.as_deref(), Some("high"));
    assert_eq!(got.agent_env, vec![("FOO".to_string(), "bar".to_string())]);
    assert_eq!(
        got.token_budget,
        Some(750_000),
        "budget set via config edit (0042)"
    );
    assert!(!got.archived, "a config edit does not archive");

    // A partial edit (only the model) leaves the other fields alone.
    let only_model = AgentConfigUpdate {
        model: Some(Some("claude-sonnet-4".to_string())),
        ..Default::default()
    };
    AgentRepo::update_config(store.pool(), &workspace_id, "agent-1", &only_model)
        .await
        .expect("partial update");
    let got = AgentRepo::get(store.pool(), "agent-1").await.unwrap().unwrap();
    assert_eq!(
        got.model.as_deref(),
        Some("claude-sonnet-4"),
        "model changed"
    );
    assert_eq!(got.name, "Builder Pro", "name untouched by partial edit");
    assert_eq!(got.thinking.as_deref(), Some("high"), "thinking untouched");
}

/// A config edit is workspace-scoped: an agent id in another tenant touches no
/// row (no cross-tenant edit), and an empty edit is a deliberate no-op (e38.15).
#[tokio::test]
async fn update_config_is_workspace_scoped_and_empty_is_noop() {
    use ainb_hangar_store::repo::agent::AgentConfigUpdate;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (workspace_id, runtime_id, owner_id) = seed_fk_chain(&store).await;

    let agent = Agent {
        id: "agent-1".to_string(),
        workspace_id,
        name: "Builder".to_string(),
        runtime_id,
        instructions: None,
        visibility: "workspace".to_string(),
        owner_id,
        archived: false,
        model: None,
        cli_args: Vec::new(),
        mcp_config: None,
        thinking: None,
        agent_env: Vec::new(),
        provider: None,
        token_budget: None,
    };
    AgentRepo::insert(store.pool(), &agent).await.expect("insert agent");

    // A foreign workspace matches no (id, workspace) pair → no row touched.
    let update = AgentConfigUpdate {
        model: Some(Some("claude-opus-4".to_string())),
        ..Default::default()
    };
    let touched = AgentRepo::update_config(store.pool(), "other-ws", "agent-1", &update)
        .await
        .expect("cross-tenant update");
    assert!(!touched, "a foreign workspace must touch no row");
    let got = AgentRepo::get(store.pool(), "agent-1").await.unwrap().unwrap();
    assert_eq!(
        got.model, None,
        "cross-tenant edit must not change the model"
    );

    // An empty edit is a deliberate no-op (no SET clause), still false.
    let empty = AgentConfigUpdate::default();
    let touched = AgentRepo::update_config(store.pool(), "ws-1", "agent-1", &empty)
        .await
        .expect("empty update");
    assert!(!touched, "an empty edit touches no row");
}

/// Archiving flips the flag and excludes the agent from the active list, while
/// the include-archived list still returns it (e38.15).
#[tokio::test]
async fn archive_flips_flag_and_hides_from_active_list() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (workspace_id, runtime_id, owner_id) = seed_fk_chain(&store).await;

    let agent = Agent {
        id: "agent-1".to_string(),
        workspace_id: workspace_id.clone(),
        name: "Builder".to_string(),
        runtime_id,
        instructions: None,
        visibility: "workspace".to_string(),
        owner_id,
        archived: false,
        model: None,
        cli_args: Vec::new(),
        mcp_config: None,
        thinking: None,
        agent_env: Vec::new(),
        provider: None,
        token_budget: None,
    };
    AgentRepo::insert(store.pool(), &agent).await.expect("insert agent");

    // Active list shows the agent before archiving.
    let active = AgentRepo::list_by_workspace(store.pool(), &workspace_id).await.unwrap();
    assert_eq!(active.len(), 1, "active list shows the un-archived agent");

    // Archive it (workspace-scoped).
    let touched = AgentRepo::set_archived(store.pool(), &workspace_id, "agent-1", true)
        .await
        .expect("archive");
    assert!(touched, "archive touches one row");
    let got = AgentRepo::get(store.pool(), "agent-1").await.unwrap().unwrap();
    assert!(got.archived, "the flag flipped to archived");

    // Active list now excludes it; the include-archived list still returns it.
    let active = AgentRepo::list_by_workspace(store.pool(), &workspace_id).await.unwrap();
    assert!(
        active.is_empty(),
        "archived agent excluded from the active list"
    );
    let all = AgentRepo::list_by_workspace_including_archived(store.pool(), &workspace_id)
        .await
        .unwrap();
    assert_eq!(all.len(), 1, "include-archived list still returns it");

    // Un-archive restores it to the active list.
    AgentRepo::set_archived(store.pool(), &workspace_id, "agent-1", false)
        .await
        .expect("unarchive");
    let active = AgentRepo::list_by_workspace(store.pool(), &workspace_id).await.unwrap();
    assert_eq!(
        active.len(),
        1,
        "un-archived agent returns to the active list"
    );

    // A foreign workspace archives nothing (workspace-scoped).
    let touched = AgentRepo::set_archived(store.pool(), "other-ws", "agent-1", true)
        .await
        .expect("cross-tenant archive");
    assert!(!touched, "a foreign workspace must archive no row");
}
