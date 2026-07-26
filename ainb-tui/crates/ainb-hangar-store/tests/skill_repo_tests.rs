//! P6.1 — `SkillRepo` CRUD + workspace scoping.
//!
//! Proves the workspace-scoped skill repository: create writes a `skill` row
//! plus its ordered `skill_file` children, lookups are tenant-isolated, the
//! agent junction is idempotent, `(workspace_id, name)` is unique, and deleting
//! a skill cascades to its files in-app (the schema declares no
//! `ON DELETE CASCADE` on `skill_file`, so `SkillRepo::delete` removes children
//! explicitly inside a transaction — foreign keys themselves ARE enforced, which
//! is exactly why the children must be removed first).

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::skill::SkillFileInput;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::skill::SkillRepo;

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
    let stored = SkillRepo::get(store.pool(), &ws, &id).await.expect("get").expect("present");
    assert_eq!(stored.name.as_str(), "commit");

    // Two skill_file rows, ordered by path.
    let file_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_file WHERE skill_id = ?")
        .bind(id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count files");
    assert_eq!(file_rows, 2, "two skill_file rows");

    let read = SkillRepo::files_for(store.pool(), &id).await.expect("files_for");
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
    SkillRepo::attach_to_agent(store.pool(), &ws, &agent, &skill)
        .await
        .expect("attach 1");
    SkillRepo::attach_to_agent(store.pool(), &ws, &agent, &skill)
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
    let attached = SkillRepo::skills_for_agent(store.pool(), &ws, &agent)
        .await
        .expect("skills_for_agent");
    assert_eq!(attached.len(), 1);
    assert_eq!(attached[0].name.as_str(), "commit");

    // detach removes the junction.
    SkillRepo::detach_from_agent(store.pool(), &ws, &agent, &skill)
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
    let after = SkillRepo::get(store.pool(), &ws1, &id).await.unwrap().unwrap();
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

    SkillRepo::delete(store.pool(), &ws, &id).await.expect("delete");

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
    assert_eq!(
        files_after, 0,
        "child skill_file rows cascade-deleted in-app"
    );
}

// --- Cross-tenant IDOR scoping (agents-in-a-box-4pe) -----------------------
//
// Every by-id method on the typed API must be workspace-scoped: holding an id
// minted in workspace A must NOT let a caller read, delete, or attach against
// workspace B's rows.

#[tokio::test]
async fn get_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-a", "alpha").await;
    seed_workspace(&store, "ws-b", "beta").await;

    let ws_a = WorkspaceId::from_str("ws-a").unwrap();
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();

    let id = SkillRepo::create(store.pool(), &ws_a, "secret", None, Some("body"), vec![])
        .await
        .expect("create in ws-a");

    // Owning workspace sees the row.
    let mine = SkillRepo::get(store.pool(), &ws_a, &id).await.expect("get ws-a");
    assert!(mine.is_some(), "ws-a can read its own skill");

    // Foreign workspace must NOT, even holding the raw id.
    let theirs = SkillRepo::get(store.pool(), &ws_b, &id).await.expect("get ws-b");
    assert!(
        theirs.is_none(),
        "ws-b must not read ws-a's skill by id (IDOR)"
    );
}

#[tokio::test]
async fn delete_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-a", "alpha").await;
    seed_workspace(&store, "ws-b", "beta").await;

    let ws_a = WorkspaceId::from_str("ws-a").unwrap();
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();

    let id = SkillRepo::create(
        store.pool(),
        &ws_a,
        "secret",
        None,
        Some("body"),
        vec![SkillFileInput::new("a.md", "a")],
    )
    .await
    .expect("create in ws-a");

    // A cross-tenant delete must be a no-op against ws-a's data.
    SkillRepo::delete(store.pool(), &ws_b, &id)
        .await
        .expect("delete ws-b is not an error");

    let still_there = SkillRepo::get(store.pool(), &ws_a, &id).await.expect("get ws-a");
    assert!(
        still_there.is_some(),
        "ws-b's delete must not remove ws-a's skill (IDOR)"
    );
    // Child files must survive too (scoped cascade).
    let files: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM skill_file WHERE skill_id = ?")
        .bind(id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("count files");
    assert_eq!(files, 1, "cross-tenant delete must not touch child files");

    // The owning workspace can still delete it.
    SkillRepo::delete(store.pool(), &ws_a, &id).await.expect("delete ws-a");
    let gone = SkillRepo::get(store.pool(), &ws_a, &id).await.expect("get ws-a after delete");
    assert!(gone.is_none(), "owning workspace delete succeeds");
}

#[tokio::test]
async fn skills_for_agent_is_workspace_scoped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-a", "alpha").await;
    seed_workspace(&store, "ws-b", "beta").await;
    let agent_id = seed_agent(&store, "ws-a", "a").await;

    let ws_a = WorkspaceId::from_str("ws-a").unwrap();
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();
    let agent = ainb_hangar_core::ids::AgentId::from_str(&agent_id).unwrap();

    let skill = SkillRepo::create(store.pool(), &ws_a, "commit", None, None, vec![])
        .await
        .expect("create skill");
    SkillRepo::attach_to_agent(store.pool(), &ws_a, &agent, &skill)
        .await
        .expect("attach");

    // Owning workspace sees the attached skill.
    let mine = SkillRepo::skills_for_agent(store.pool(), &ws_a, &agent)
        .await
        .expect("skills_for_agent ws-a");
    assert_eq!(mine.len(), 1, "ws-a sees its agent's skill");

    // A different workspace querying the same agent id sees nothing — the agent
    // does not belong to ws-b.
    let theirs = SkillRepo::skills_for_agent(store.pool(), &ws_b, &agent)
        .await
        .expect("skills_for_agent ws-b");
    assert!(
        theirs.is_empty(),
        "ws-b must not read ws-a's agent skills (IDOR)"
    );
}

#[tokio::test]
async fn attach_rejects_cross_workspace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-a", "alpha").await;
    seed_workspace(&store, "ws-b", "beta").await;
    let agent_id = seed_agent(&store, "ws-a", "a").await;

    let ws_a = WorkspaceId::from_str("ws-a").unwrap();
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();
    let agent = ainb_hangar_core::ids::AgentId::from_str(&agent_id).unwrap();

    // Skill lives in ws-b; agent lives in ws-a.
    let foreign_skill = SkillRepo::create(store.pool(), &ws_b, "secret", None, None, vec![])
        .await
        .expect("create skill in ws-b");

    // Attaching a ws-b skill to a ws-a agent (scoped to ws-a) must error and
    // create no junction row.
    let res = SkillRepo::attach_to_agent(store.pool(), &ws_a, &agent, &foreign_skill).await;
    assert!(
        res.is_err(),
        "attach across workspaces must be rejected (IDOR)"
    );
    let junction: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_skill WHERE agent_id = ? AND skill_id = ?")
            .bind(agent.as_str())
            .bind(foreign_skill.as_str())
            .fetch_one(store.pool())
            .await
            .expect("count junction");
    assert_eq!(junction, 0, "rejected attach writes no junction row");
}

// ---- Per-agent skill enablement (migration 0051, parity #24) ----------------
//
// Attachment and enablement are two orthogonal levers: detach removes the link,
// disable keeps the link and suppresses only its effect. These tests pin that
// split — in particular that `skills_for_agent` (the materialisation read) hides
// a disabled link while `agent_skill_links` (the attachment read) still shows it.

/// Seed a workspace + agent + two named skills, attach both, and return the
/// typed ids the enablement tests drive.
async fn seed_two_attached_skills(
    store: &Store,
) -> (
    WorkspaceId,
    ainb_hangar_core::ids::AgentId,
    ainb_hangar_core::ids::SkillId,
    ainb_hangar_core::ids::SkillId,
) {
    seed_workspace(store, "ws-1", "alpha").await;
    let agent_id = seed_agent(store, "ws-1", "1").await;
    let ws = WorkspaceId::from_str("ws-1").unwrap();
    let agent = ainb_hangar_core::ids::AgentId::from_str(&agent_id).unwrap();
    let commit = SkillRepo::create(store.pool(), &ws, "commit", None, Some("# commit"), vec![])
        .await
        .expect("create commit");
    let review = SkillRepo::create(store.pool(), &ws, "review", None, Some("# review"), vec![])
        .await
        .expect("create review");
    for skill in [&commit, &review] {
        SkillRepo::attach_to_agent(store.pool(), &ws, &agent, skill)
            .await
            .expect("attach");
    }
    (ws, agent, commit, review)
}

#[tokio::test]
async fn set_enabled_false_hides_link_from_skills_for_agent() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (ws, agent, _commit, review) = seed_two_attached_skills(&store).await;

    let toggled = SkillRepo::set_enabled(store.pool(), &ws, &agent, &review, false)
        .await
        .expect("disable review");
    assert!(toggled, "disabling an attached link reports a row change");

    // The materialisation read sees only the enabled link…
    let live = SkillRepo::skills_for_agent(store.pool(), &ws, &agent)
        .await
        .expect("skills_for_agent");
    assert_eq!(
        live.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["commit"],
        "a disabled link must not reach the materialiser"
    );

    // …while the attachment read still lists BOTH, flagged.
    let links = SkillRepo::agent_skill_links(store.pool(), &ws, &agent)
        .await
        .expect("agent_skill_links");
    assert_eq!(
        links
            .iter()
            .map(|l| (l.name.as_str().to_string(), l.enabled))
            .collect::<Vec<_>>(),
        vec![("commit".to_string(), true), ("review".to_string(), false)],
        "a disabled link is still ATTACHED, just not live"
    );
}

#[tokio::test]
async fn set_enabled_true_restores_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (ws, agent, _commit, review) = seed_two_attached_skills(&store).await;

    SkillRepo::set_enabled(store.pool(), &ws, &agent, &review, false)
        .await
        .expect("disable");
    SkillRepo::set_enabled(store.pool(), &ws, &agent, &review, true)
        .await
        .expect("re-enable");

    let live = SkillRepo::skills_for_agent(store.pool(), &ws, &agent)
        .await
        .expect("skills_for_agent");
    assert_eq!(
        live.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["commit", "review"],
        "re-enabling restores the link to the materialisation set"
    );
}

/// D2 — the regression guard. Seed/`templates use` re-attach on every re-run; if
/// attach reset `enabled` those idempotent paths would silently undo a disable.
#[tokio::test]
async fn attach_does_not_re_enable_a_disabled_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (ws, agent, _commit, review) = seed_two_attached_skills(&store).await;

    SkillRepo::set_enabled(store.pool(), &ws, &agent, &review, false)
        .await
        .expect("disable");
    SkillRepo::attach_to_agent(store.pool(), &ws, &agent, &review)
        .await
        .expect("re-attach (idempotent)");

    let links = SkillRepo::agent_skill_links(store.pool(), &ws, &agent)
        .await
        .expect("agent_skill_links");
    let review_link = links.iter().find(|l| l.name.as_str() == "review").expect("review link");
    assert!(
        !review_link.enabled,
        "re-attaching must NOT resurrect a deliberately disabled link"
    );
}

/// Detach removes the row outright, so a later attach mints a NEW row — which
/// starts at the column default, enabled.
#[tokio::test]
async fn detach_then_attach_starts_enabled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (ws, agent, _commit, review) = seed_two_attached_skills(&store).await;

    SkillRepo::set_enabled(store.pool(), &ws, &agent, &review, false)
        .await
        .expect("disable");
    SkillRepo::detach_from_agent(store.pool(), &ws, &agent, &review)
        .await
        .expect("detach");
    SkillRepo::attach_to_agent(store.pool(), &ws, &agent, &review)
        .await
        .expect("re-attach");

    let links = SkillRepo::agent_skill_links(store.pool(), &ws, &agent)
        .await
        .expect("agent_skill_links");
    let review_link = links.iter().find(|l| l.name.as_str() == "review").expect("review link");
    assert!(
        review_link.enabled,
        "a freshly-created link starts enabled (the column default)"
    );
}

#[tokio::test]
async fn set_enabled_cross_workspace_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (ws_a, agent, commit, _review) = seed_two_attached_skills(&store).await;
    seed_workspace(&store, "ws-b", "beta").await;
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();

    // Scoping the agent's own link to a FOREIGN workspace must be refused.
    let res = SkillRepo::set_enabled(store.pool(), &ws_b, &agent, &commit, false).await;
    assert!(
        matches!(
            res,
            Err(ainb_hangar_store::repo::skill::SkillRepoError::CrossWorkspace)
        ),
        "cross-workspace toggle must be rejected (IDOR), got {res:?}"
    );

    // …and the real link is untouched.
    let live = SkillRepo::skills_for_agent(store.pool(), &ws_a, &agent)
        .await
        .expect("skills_for_agent");
    assert_eq!(
        live.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        vec!["commit", "review"],
        "a rejected toggle must not change any flag"
    );
}

#[tokio::test]
async fn set_enabled_on_unattached_pair_returns_false() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_workspace(&store, "ws-1", "alpha").await;
    let agent_id = seed_agent(&store, "ws-1", "1").await;
    let ws = WorkspaceId::from_str("ws-1").unwrap();
    let agent = ainb_hangar_core::ids::AgentId::from_str(&agent_id).unwrap();
    let orphan = SkillRepo::create(store.pool(), &ws, "orphan", None, None, vec![])
        .await
        .expect("create skill");

    let toggled = SkillRepo::set_enabled(store.pool(), &ws, &agent, &orphan, false)
        .await
        .expect("no error for an unattached pair");
    assert!(
        !toggled,
        "toggling an unattached pair reports no row change (not an error)"
    );
}

#[tokio::test]
async fn agent_skill_links_foreign_agent_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (_ws_a, agent, _commit, _review) = seed_two_attached_skills(&store).await;
    seed_workspace(&store, "ws-b", "beta").await;
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();

    let links = SkillRepo::agent_skill_links(store.pool(), &ws_b, &agent)
        .await
        .expect("agent_skill_links");
    assert!(
        links.is_empty(),
        "a foreign agent id must never leak another tenant's attachments"
    );
}

// ---- The roster shape (parity `7-rest`) -------------------------------------
//
// `enabled_skill_names_for_agent` is what the squad-leader briefing renders: the
// same enabled+workspace filter as `skills_for_agent`, name-shaped, no file
// hydration.

#[tokio::test]
async fn enabled_skill_names_for_agent_returns_name_ordered_enabled_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (ws, agent, _commit, _review) = seed_two_attached_skills(&store).await;
    // A third skill sorting BEFORE both of the seeded ones pins ORDER BY name
    // rather than insertion order.
    let audit = SkillRepo::create(store.pool(), &ws, "audit", None, Some("# audit"), vec![])
        .await
        .expect("create audit");
    SkillRepo::attach_to_agent(store.pool(), &ws, &agent, &audit)
        .await
        .expect("attach audit");

    let names = SkillRepo::enabled_skill_names_for_agent(store.pool(), &ws, &agent)
        .await
        .expect("enabled_skill_names_for_agent");
    assert_eq!(
        names.iter().map(|n| n.as_str()).collect::<Vec<_>>(),
        vec!["audit", "commit", "review"],
        "every enabled attachment, ordered by name"
    );
}

#[tokio::test]
async fn enabled_skill_names_for_agent_excludes_a_disabled_link() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (ws, agent, _commit, review) = seed_two_attached_skills(&store).await;

    SkillRepo::set_enabled(store.pool(), &ws, &agent, &review, false)
        .await
        .expect("disable review");

    let names = SkillRepo::enabled_skill_names_for_agent(store.pool(), &ws, &agent)
        .await
        .expect("enabled_skill_names_for_agent");
    assert_eq!(
        names.iter().map(|n| n.as_str()).collect::<Vec<_>>(),
        vec!["commit"],
        "a disabled link must never be advertised on the roster"
    );
}

#[tokio::test]
async fn enabled_skill_names_for_agent_foreign_agent_is_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let (_ws_a, agent, _commit, _review) = seed_two_attached_skills(&store).await;
    seed_workspace(&store, "ws-b", "beta").await;
    let ws_b = WorkspaceId::from_str("ws-b").unwrap();

    let names = SkillRepo::enabled_skill_names_for_agent(store.pool(), &ws_b, &agent)
        .await
        .expect("enabled_skill_names_for_agent");
    assert!(
        names.is_empty(),
        "the cross-tenant guard holds for the roster read too"
    );
}
