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
        permission_mode: "private".to_string(),
        owner_id,
        ..Agent::default()
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
        permission_mode: "private".to_string(),
        owner_id,
        ..Agent::default()
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
        agent_env: Some(vec![("FOO".to_string(), "bar".to_string())].into()),
        token_budget: Some(Some(750_000)),
        ..AgentConfigUpdate::default()
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
    assert_eq!(
        got.agent_env.expose_for_child_env(),
        vec![("FOO".to_string(), "bar".to_string())]
    );
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
        permission_mode: "private".to_string(),
        owner_id,
        ..Agent::default()
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
        permission_mode: "private".to_string(),
        owner_id,
        ..Agent::default()
    };
    AgentRepo::insert(store.pool(), &agent).await.expect("insert agent");

    // Active list shows the agent before archiving.
    let active = AgentRepo::list_by_workspace(store.pool(), &workspace_id).await.unwrap();
    assert_eq!(active.len(), 1, "active list shows the un-archived agent");

    // Archive it (workspace-scoped).
    let touched = AgentRepo::set_archived(store.pool(), &workspace_id, "agent-1", true, None, 0)
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
    AgentRepo::set_archived(store.pool(), &workspace_id, "agent-1", false, None, 0)
        .await
        .expect("unarchive");
    let active = AgentRepo::list_by_workspace(store.pool(), &workspace_id).await.unwrap();
    assert_eq!(
        active.len(),
        1,
        "un-archived agent returns to the active list"
    );

    // A foreign workspace archives nothing (workspace-scoped).
    let touched = AgentRepo::set_archived(store.pool(), "other-ws", "agent-1", true, None, 0)
        .await
        .expect("cross-tenant archive");
    assert!(!touched, "a foreign workspace must archive no row");
}

// ──────────────────────────────────────────────────────────────────────────
// Migration 0050: agent metadata + name uniqueness + the system-agent kind,
// exercised through the REPO / BOOTSTRAP API rather than raw SQL.
// ──────────────────────────────────────────────────────────────────────────

/// A description supplied at create round-trips through BOTH read paths, and the
/// created agent is never avatar-less (hangar mints an emoji token).
#[tokio::test]
async fn create_agent_from_round_trips_description_and_mints_an_avatar() {
    use ainb_hangar_store::bootstrap::{AgentDraft, create_agent_from, ensure_default_workspace};

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = ensure_default_workspace(store.pool()).await.expect("bootstrap workspace");

    let created = create_agent_from(
        store.pool(),
        &ws,
        AgentDraft {
            name: "builder".into(),
            provider: "claude".into(),
            description: "  ships the backend  ".into(),
            ..AgentDraft::default()
        },
    )
    .await
    .expect("create agent");
    assert_eq!(
        created.description, "ships the backend",
        "the description is trimmed on the way in"
    );
    let avatar = created.avatar_url.clone().expect("an agent is never avatar-less");
    assert!(
        avatar.starts_with("emoji:"),
        "the minted avatar is an emoji token, got {avatar}"
    );
    assert_eq!(created.kind, "user", "a plain create is user-kind");

    // Both read paths carry the metadata, not just the create return value.
    let got = AgentRepo::get(store.pool(), &created.id)
        .await
        .unwrap()
        .expect("agent persisted");
    assert_eq!(got.description, "ships the backend");
    assert_eq!(got.avatar_url, created.avatar_url);
    let listed = AgentRepo::list_by_workspace(store.pool(), &ws).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].description, "ships the backend");
}

/// A second create with the SAME name in the SAME workspace is refused, and the
/// refusal is classified by `is_duplicate_name` (so callers can answer multica's
/// 409 instead of an opaque store fault). The same name in ANOTHER workspace is
/// fine.
#[tokio::test]
async fn duplicate_agent_name_is_refused_per_workspace() {
    use ainb_hangar_store::bootstrap::{AgentDraft, create_agent_from, ensure_default_workspace};
    use ainb_hangar_store::repo::agent::is_duplicate_name;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = ensure_default_workspace(store.pool()).await.expect("bootstrap workspace");

    let draft = |name: &str| AgentDraft {
        name: name.to_string(),
        provider: "claude".into(),
        ..AgentDraft::default()
    };
    create_agent_from(store.pool(), &ws, draft("builder"))
        .await
        .expect("first create");
    let err = create_agent_from(store.pool(), &ws, draft("builder"))
        .await
        .expect_err("a duplicate name must be refused");
    assert!(
        is_duplicate_name(&err),
        "the refusal must classify as a duplicate name, got: {err}"
    );
    assert_eq!(
        AgentRepo::list_by_workspace(store.pool(), &ws).await.unwrap().len(),
        1,
        "the refused create wrote no second row"
    );

    // A second workspace may hold its own `builder` — the constraint is scoped
    // to (workspace_id, name), not global. Inserted through the repo directly
    // because `create_agent_from` always binds the DEFAULT workspace's runtime.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES ('ws-2','other','O',1)")
        .execute(store.pool())
        .await
        .expect("insert second workspace");
    let first = AgentRepo::list_by_workspace(store.pool(), &ws).await.unwrap().remove(0);
    AgentRepo::insert(
        store.pool(),
        &Agent {
            id: "ws2-builder".into(),
            workspace_id: "ws-2".into(),
            name: "builder".into(),
            runtime_id: first.runtime_id.clone(),
            owner_id: first.owner_id.clone(),
            ..Agent::default()
        },
    )
    .await
    .expect("the same name in another workspace is fine");
}

/// A `system`-kind agent is INVISIBLE to every roster read but reachable through
/// `find_system` — the shape the agent-builder carrier needs (gap #9-rest).
#[tokio::test]
async fn system_agents_are_hidden_from_rosters_but_found_by_key() {
    use ainb_hangar_store::bootstrap::{AgentDraft, create_agent_from, ensure_default_workspace};

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = ensure_default_workspace(store.pool()).await.expect("bootstrap workspace");

    let user = create_agent_from(
        store.pool(),
        &ws,
        AgentDraft {
            name: "visible".into(),
            provider: "claude".into(),
            ..AgentDraft::default()
        },
    )
    .await
    .expect("create user agent");
    let hidden = create_agent_from(
        store.pool(),
        &ws,
        AgentDraft {
            name: "carrier".into(),
            provider: "claude".into(),
            kind: Some("system".into()),
            system_key: Some("agent_builder".into()),
            ..AgentDraft::default()
        },
    )
    .await
    .expect("create system agent");

    let names = |rows: Vec<ainb_hangar_store::repo::agent::Agent>| {
        rows.into_iter().map(|a| a.name).collect::<Vec<_>>()
    };
    assert_eq!(
        names(AgentRepo::list_by_workspace(store.pool(), &ws).await.unwrap()),
        ["visible"],
        "the active roster hides the system agent"
    );
    assert_eq!(
        names(
            AgentRepo::list_by_workspace_including_archived(store.pool(), &ws)
                .await
                .unwrap()
        ),
        ["visible"],
        "even the include-archived roster hides it"
    );
    let ids = AgentRepo::list_ids_by_runtime(store.pool(), &user.runtime_id).await.unwrap();
    assert_eq!(
        ids,
        [user.id.clone()],
        "the presence fan-out never addresses a system agent"
    );
    // The command palette must not surface it either.
    let hits = ainb_hangar_store::repo::search::cross_entity_search(store.pool(), &ws, "carrier")
        .await
        .unwrap();
    assert!(
        hits.is_empty(),
        "a hidden system agent must not surface in search, got {hits:?}"
    );

    // But the by-key lookup and the kind-blind by-id get both reach it.
    let found = AgentRepo::find_system(store.pool(), &ws, "agent_builder")
        .await
        .unwrap()
        .expect("find_system reaches the carrier");
    assert_eq!(found.id, hidden.id);
    assert!(
        AgentRepo::get(store.pool(), &hidden.id).await.unwrap().is_some(),
        "the internal by-id get stays kind-blind"
    );
    assert!(
        AgentRepo::find_system(store.pool(), &ws, "no-such-key")
            .await
            .unwrap()
            .is_none()
    );
}

/// `update_config` writes, rewrites and CLEARS the three new metadata fields.
#[tokio::test]
async fn update_config_sets_and_clears_agent_metadata() {
    use ainb_hangar_store::bootstrap::{AgentDraft, create_agent_from, ensure_default_workspace};
    use ainb_hangar_store::repo::agent::AgentConfigUpdate;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = ensure_default_workspace(store.pool()).await.expect("bootstrap workspace");
    let agent = create_agent_from(
        store.pool(),
        &ws,
        AgentDraft {
            name: "builder".into(),
            provider: "claude".into(),
            description: "first".into(),
            ..AgentDraft::default()
        },
    )
    .await
    .expect("create agent");

    // Set: rewrite the description, set an avatar and a service tier.
    let set = AgentConfigUpdate {
        description: Some("second".into()),
        avatar_url: Some(Some("emoji:🦊".into())),
        service_tier: Some(Some("priority".into())),
        ..AgentConfigUpdate::default()
    };
    assert!(AgentRepo::update_config(store.pool(), &ws, &agent.id, &set).await.unwrap());
    let got = AgentRepo::get(store.pool(), &agent.id).await.unwrap().unwrap();
    assert_eq!(got.description, "second");
    assert_eq!(got.avatar_url.as_deref(), Some("emoji:🦊"));
    assert_eq!(got.service_tier.as_deref(), Some("priority"));

    // Clear: the two nullable fields go back to NULL, the description untouched.
    let clear = AgentConfigUpdate {
        avatar_url: Some(None),
        service_tier: Some(None),
        ..AgentConfigUpdate::default()
    };
    assert!(AgentRepo::update_config(store.pool(), &ws, &agent.id, &clear).await.unwrap());
    let got = AgentRepo::get(store.pool(), &agent.id).await.unwrap().unwrap();
    assert_eq!(got.avatar_url, None, "the avatar cleared to NULL");
    assert_eq!(got.service_tier, None, "the service tier cleared to NULL");
    assert_eq!(
        got.description, "second",
        "an untouched field is left alone by a partial edit"
    );
}

/// RENAMING an agent onto a name a SIBLING already holds is refused by the same
/// unique index, and the loser keeps its old name (no partial write).
#[tokio::test]
async fn renaming_onto_a_taken_name_is_refused() {
    use ainb_hangar_store::bootstrap::{AgentDraft, create_agent_from, ensure_default_workspace};
    use ainb_hangar_store::repo::agent::{AgentConfigUpdate, is_duplicate_name};

    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = ensure_default_workspace(store.pool()).await.expect("bootstrap workspace");
    let draft = |name: &str| AgentDraft {
        name: name.to_string(),
        provider: "claude".into(),
        ..AgentDraft::default()
    };
    create_agent_from(store.pool(), &ws, draft("alpha"))
        .await
        .expect("create alpha");
    let beta = create_agent_from(store.pool(), &ws, draft("beta")).await.expect("create beta");

    let rename = AgentConfigUpdate {
        name: Some("alpha".into()),
        ..AgentConfigUpdate::default()
    };
    let err = AgentRepo::update_config(store.pool(), &ws, &beta.id, &rename)
        .await
        .expect_err("renaming onto a taken name must be refused");
    assert!(
        is_duplicate_name(&err),
        "classified as a duplicate name: {err}"
    );
    assert_eq!(
        AgentRepo::get(store.pool(), &beta.id).await.unwrap().unwrap().name,
        "beta",
        "the refused rename left the row untouched"
    );
}
