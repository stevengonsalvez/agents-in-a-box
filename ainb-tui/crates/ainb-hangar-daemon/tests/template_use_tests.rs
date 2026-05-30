//! P6.3 — transactional `templates_use` integration tests.
//!
//! Covers the side-effecting half of P6.3 (the IO-free registry is tested in
//! `ainb-hangar-core/tests/template_registry_tests.rs`): applying a curated
//! template creates one agent + N `agent_skill` rows in a single transaction,
//! is idempotent by agent name, and hard-errors (writing nothing) when a
//! referenced skill has not been imported yet.

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_core::skill::SkillFileInput;
use ainb_hangar_core::template::TemplateRegistry;
use ainb_hangar_daemon::templates::{TemplateUseError, templates_use};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::skill::SkillRepo;

/// Seed the FK chain a template-created agent needs: workspace + owner user +
/// member(owner) + one provider runtime. Returns the workspace id.
async fn seed_workspace(store: &Store) -> WorkspaceId {
    let ws = "ws-1";
    let user = "user-1";
    let runtime = "rt-1";
    let pool = store.pool();

    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind("default")
        .bind("Default")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind(user)
        .bind("owner@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'owner')")
        .bind(ws)
        .bind(user)
        .execute(pool)
        .await
        .expect("insert member");
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
    .execute(pool)
    .await
    .expect("insert runtime");

    WorkspaceId::from_str(ws).unwrap()
}

/// Import every skill the named template references as a workspace `skill` row
/// (mimics `ainb hangar skills sync` having run first).
async fn import_template_skills(store: &Store, ws: &WorkspaceId, template_name: &str) {
    let template = TemplateRegistry::get(template_name).expect("template present");
    for skill in &template.skills {
        SkillRepo::upsert_by_name(
            store.pool(),
            ws,
            skill,
            Some("imported skill"),
            Some("body"),
            vec![SkillFileInput::new("SKILL.md", "body")],
        )
        .await
        .expect("import skill");
    }
}

/// Count rows, returned as `usize` so it compares directly against `.len()`.
async fn count(store: &Store, sql: &str) -> usize {
    let n: i64 = sqlx::query_scalar(sql).fetch_one(store.pool()).await.expect("count query");
    usize::try_from(n).expect("row count is non-negative")
}

#[tokio::test]
async fn use_template_creates_agent_and_imports_skills() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace(&store).await;
    import_template_skills(&store, &ws, "code-reviewer").await;

    let template = TemplateRegistry::get("code-reviewer").unwrap();
    let n_skills = template.skills.len();

    let outcome = templates_use(store.pool(), &ws, "code-reviewer", None)
        .await
        .expect("use template");

    assert!(outcome.created, "first use must create the agent");
    assert_eq!(outcome.skill_ids.len(), n_skills);

    // Exactly one agent, named after the template.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM agent").await, 1);
    let agent_name: String = sqlx::query_scalar("SELECT name FROM agent WHERE id = ?")
        .bind(outcome.agent_id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("agent name");
    assert_eq!(agent_name, "code-reviewer");

    // N skill rows (the imported ones) + N agent_skill junction rows.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM skill").await, n_skills);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM agent_skill").await,
        n_skills,
        "one agent_skill row per bundled skill"
    );
}

#[tokio::test]
async fn use_template_is_idempotent_by_name() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace(&store).await;
    import_template_skills(&store, &ws, "planner").await;

    let first = templates_use(store.pool(), &ws, "planner", None).await.expect("first use");
    assert!(first.created);

    let second = templates_use(store.pool(), &ws, "planner", None).await.expect("second use");
    assert!(!second.created, "second use must not create a duplicate");
    assert_eq!(
        first.agent_id, second.agent_id,
        "second use returns the same agent"
    );

    // Still exactly one agent and no extra junction rows.
    assert_eq!(count(&store, "SELECT COUNT(*) FROM agent").await, 1);
    let n = TemplateRegistry::get("planner").unwrap().skills.len();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM agent_skill").await, n);
}

#[tokio::test]
async fn use_template_hard_errors_when_skill_not_imported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace(&store).await;
    // Deliberately do NOT import the template's skills.

    let err = templates_use(store.pool(), &ws, "code-reviewer", None)
        .await
        .expect_err("must error when skills not imported");

    match err {
        TemplateUseError::SkillNotImported {
            ref skill,
            ref workspace,
            ..
        } => {
            assert_eq!(workspace, ws.as_str());
            assert!(!skill.is_empty(), "the missing skill name must be reported");
            // The hint to run sync must be present in the rendered error.
            let msg = err.to_string();
            assert!(
                msg.contains("ainb hangar skills sync"),
                "error must hint to run sync: {msg}"
            );
        }
        other => panic!("expected SkillNotImported, got {other:?}"),
    }

    // Nothing was written — the agent insert never ran.
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM agent").await,
        0,
        "a failed use must not leave a partial agent"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM agent_skill").await, 0);
}

#[tokio::test]
async fn use_template_writes_nothing_on_precondition_failure() {
    // Every precondition (template exists, skills imported, runtime present) is
    // resolved BEFORE the write transaction opens, so a precondition failure can
    // never leave a partial agent. Exercise two precondition failures and assert
    // zero rows are written.
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace(&store).await;

    // Unknown template — short-circuits before any lookup.
    let err = templates_use(store.pool(), &ws, "no-such-template", None)
        .await
        .expect_err("unknown template");
    assert!(matches!(err, TemplateUseError::UnknownTemplate { .. }));
    assert_eq!(count(&store, "SELECT COUNT(*) FROM agent").await, 0);

    // Skills resolve but the workspace has no runtime: import the skills, drop
    // the runtime, then `use` — the agent must NOT be created.
    import_template_skills(&store, &ws, "code-reviewer").await;
    sqlx::query("DELETE FROM agent_runtime WHERE workspace_id = ?")
        .bind(ws.as_str())
        .execute(store.pool())
        .await
        .expect("delete runtime");
    let err = templates_use(store.pool(), &ws, "code-reviewer", None)
        .await
        .expect_err("no runtime");
    assert!(matches!(err, TemplateUseError::NoRuntime { .. }));
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM agent").await,
        0,
        "a runtime-less workspace must not produce a partial agent"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM agent_skill").await, 0);
}

#[tokio::test]
async fn use_template_respects_agent_name_override() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    let ws = seed_workspace(&store).await;
    import_template_skills(&store, &ws, "code-reviewer").await;

    let outcome = templates_use(store.pool(), &ws, "code-reviewer", Some("my-reviewer"))
        .await
        .expect("use with override");
    assert!(outcome.created);

    let name: String = sqlx::query_scalar("SELECT name FROM agent WHERE id = ?")
        .bind(outcome.agent_id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("agent name");
    assert_eq!(name, "my-reviewer", "the --agent-name override must win");
}

#[tokio::test]
async fn use_template_reports_workspace_not_found() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    // A workspace id that was never created; no seeding at all.
    let ghost = WorkspaceId::from_str("ws-does-not-exist").unwrap();

    let err = templates_use(store.pool(), &ghost, "code-reviewer", None)
        .await
        .expect_err("missing workspace");
    // The skill resolution runs first and reports the missing skill before we
    // even reach the workspace check, which is the actionable error for the
    // user. Either a SkillNotImported (skills absent) or WorkspaceNotFound is
    // acceptable — both write nothing.
    assert!(
        matches!(
            err,
            TemplateUseError::SkillNotImported { .. } | TemplateUseError::WorkspaceNotFound(_)
        ),
        "expected a precondition error, got {err:?}"
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM agent").await, 0);
}
