//! P6.1 — `SkillRepo` CRUD + workspace scoping.
//!
//! Proves the workspace-scoped skill repository: create writes a `skill` row
//! plus its ordered `skill_file` children, lookups are tenant-isolated, the
//! agent junction is idempotent, `(workspace_id, name)` is unique, and deleting
//! a skill cascades to its files in-app (FK cascade is unavailable — the crate
//! runs with `PRAGMA foreign_keys` off, so `SkillRepo::delete` removes children
//! explicitly inside a transaction).

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::skill::SkillFileInput;
use ainb_hangar_store::repo::skill::SkillRepo;
use ainb_hangar_store::Store;

/// Insert a workspace row (the FK target every skill needs).
async fn seed_workspace(store: &Store, id: &str, slug: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(id)
        .bind(slug)
        .bind(slug)
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
}

/// Seed the workspace -> user -> runtime -> agent FK chain an `agent_skill` row
/// needs, returning the agent id.
async fn seed_agent(store: &Store, ws: &str, suffix: &str) -> String {
    let user = format!("user-{suffix}");
    let runtime = format!("rt-{suffix}");
    let agent = format!("agent-{suffix}");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(&user)
        .bind(format!("{suffix}@example.com"))
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime \
         (id, workspace_id, daemon_id, provider, runtime_mode, status) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&runtime)
    .bind(ws)
    .bind(format!("daemon-{suffix}"))
    .bind("claude")
    .bind("local")
    .bind("online")
    .execute(store.pool())
    .await
    .expect("insert agent_runtime");
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&agent)
    .bind(ws)
    .bind(format!("Agent {suffix}"))
    .bind(&runtime)
    .bind(Option::<String>::None)
    .bind("workspace")
    .bind(&user)
    .execute(store.pool())
    .await
    .expect("insert agent");
    agent
}

#[tokio::test]
async fn test_insert_skill_writes_row_and_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-1", "alpha").await;

    let ws = WorkspaceId::from_str("ws-1").unwrap();
    let files = vec![
        SkillFileInput::new("references/x.md", "ref body"),
        SkillFileInput::new("scripts/y.sh", "#!/bin/sh\necho hi"),
    ];
    let id = SkillRepo::create(
        store.pool(),
        &ws,
        "Commit",
        Some("Commit helper"),
        Some("# Commit\nbody"),
        files.clone(),
    )
    .await
    .expect("create skill");

    // One skill row.
    let skill_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill WHERE id = ?")
        .bind(id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count skill");
    assert_eq!(skill_rows, 1, "exactly one skill row");

    // Name was normalised to kebab-case on write.
    let stored = SkillRepo::get(store.pool(), &id)
        .await
        .expect("get")
        .expect("present");
    assert_eq!(stored.name.as_str(), "commit");

    // Two skill_file rows, ordered by path.
    let file_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_file WHERE skill_id = ?")
        .bind(id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count files");
    assert_eq!(file_rows, 2, "two skill_file rows");

    let read = SkillRepo::files_for(store.pool(), &id)
        .await
        .expect("files_for");
    assert_eq!(read, files, "files round-trip in path order");
}

#[tokio::test]
async fn test_skill_lookup_by_workspace_is_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-a", "alpha").await;
    seed_workspace(&store, "ws-b", "beta").await;

    let ws_a = WorkspaceId::from_str("ws-a").unwrap();
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();

    SkillRepo::create(store.pool(), &ws_a, "alpha-skill", None, None, vec![])
        .await
        .expect("create a");
    SkillRepo::create(store.pool(), &ws_b, "beta-skill", None, None, vec![])
        .await
        .expect("create b");

    let listed = SkillRepo::list(store.pool(), &ws_a).await.expect("list a");
    assert_eq!(listed.len(), 1, "ws-a sees only its own skill");
    assert_eq!(listed[0].name.as_str(), "alpha-skill");
    assert!(
        listed.iter().all(|s| s.workspace_id == "ws-a"),
        "no cross-tenant leak"
    );
}

#[tokio::test]
async fn test_attach_skill_to_agent_creates_junction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-1", "alpha").await;
    let agent_id = seed_agent(&store, "ws-1", "1").await;

    let ws = WorkspaceId::from_str("ws-1").unwrap();
    let agent = ainb_hangar_core::ids::AgentId::from_str(&agent_id).unwrap();
    let skill = SkillRepo::create(store.pool(), &ws, "commit", None, None, vec![])
        .await
        .expect("create skill");

    // Attaching twice must be idempotent — one junction row.
    SkillRepo::attach_to_agent(store.pool(), &agent, &skill)
        .await
        .expect("attach 1");
    SkillRepo::attach_to_agent(store.pool(), &agent, &skill)
        .await
        .expect("attach 2 (idempotent)");

    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_skill WHERE agent_id = ? AND skill_id = ?")
            .bind(agent.as_str())
            .bind(skill.as_str())
            .fetch_one(store.pool())
            .await
            .expect("count junction");
    assert_eq!(count, 1, "attach is idempotent");

    // skills_for_agent returns the attached skill with its files.
    let attached = SkillRepo::skills_for_agent(store.pool(), &agent)
        .await
        .expect("skills_for_agent");
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0].name.as_str(), "commit");

    // detach removes the junction.
    SkillRepo::detach_from_agent(store.pool(), &agent, &skill)
        .await
        .expect("detach");
    let after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_skill WHERE agent_id = ?")
        .bind(agent.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count after detach");
    assert_eq!(after, 0, "detach removes the junction");
}

#[tokio::test]
async fn test_skill_name_unique_per_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-1", "alpha").await;
    seed_workspace(&store, "ws-2", "beta").await;

    let ws1 = WorkspaceId::from_str("ws-1").unwrap();
    let ws2 = WorkspaceId::from_str("ws-2").unwrap();

    SkillRepo::create(store.pool(), &ws1, "commit", None, None, vec![])
        .await
        .expect("first create");

    // Same name in same workspace -> conflict error. Differing only in case /
    // separators still conflicts because the name normalises identically.
    let dup = SkillRepo::create(store.pool(), &ws1, "Commit", None, None, vec![]).await;
    assert!(dup.is_err(), "duplicate name in same workspace must error");

    // Same name in a DIFFERENT workspace is fine.
    SkillRepo::create(store.pool(), &ws2, "commit", None, None, vec![])
        .await
        .expect("same name, different workspace");

    // upsert_by_name on the existing one updates rather than conflicting.
    let id = SkillRepo::upsert_by_name(
        store.pool(),
        &ws1,
        "commit",
        Some("updated desc"),
        Some("updated body"),
        vec![SkillFileInput::new("a.md", "a")],
    )
    .await
    .expect("upsert existing");
    let after = SkillRepo::get(store.pool(), &id).await.unwrap().unwrap();
    assert_eq!(after.description.as_deref(), Some("updated desc"));
    assert_eq!(after.content.as_deref(), Some("updated body"));
    // Still exactly one skill named commit in ws-1.
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM skill WHERE workspace_id = ? AND name = ?")
            .bind("ws-1")
            .bind("commit")
            .fetch_one(store.pool())
            .await
            .expect("count");
    assert_eq!(count, 1, "upsert updates in place, no duplicate");
    // Files were replaced wholesale by the upsert.
    let files = SkillRepo::files_for(store.pool(), &id).await.unwrap();
    assert_eq!(files, vec![SkillFileInput::new("a.md", "a")]);
}

#[tokio::test]
async fn test_skill_files_cascade_delete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-1", "alpha").await;

    let ws = WorkspaceId::from_str("ws-1").unwrap();
    let id = SkillRepo::create(
        store.pool(),
        &ws,
        "commit",
        None,
        Some("body"),
        vec![
            SkillFileInput::new("a.md", "a"),
            SkillFileInput::new("b.md", "b"),
        ],
    )
    .await
    .expect("create");

    // Sanity: children present.
    let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_file WHERE skill_id = ?")
        .bind(id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count before");
    assert_eq!(before, 2);

    SkillRepo::delete(store.pool(), &id).await.expect("delete");

    let skill_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill WHERE id = ?")
        .bind(id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count skill after");
    assert_eq!(skill_after, 0, "skill row removed");

    let files_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_file WHERE skill_id = ?")
        .bind(id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count files after");
    assert_eq!(files_after, 0, "child skill_file rows cascade-deleted in-app");
}
