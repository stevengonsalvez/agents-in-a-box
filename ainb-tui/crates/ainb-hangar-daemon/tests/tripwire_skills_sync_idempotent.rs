//! P6.2 — `skills_sync_from` importer tripwires.
//!
//! Five behavioural assertions over the toolkit-directory importer that backs
//! `ainb hangar skills sync`:
//!
//! 1. **imports a toolkit directory** — two `<name>/SKILL.md` dirs (one with a
//!    nested asset) produce two `skill` rows + three `skill_file` rows.
//! 2. **is idempotent** — re-running the same import leaves the row counts
//!    unchanged and updates the existing rows in place under their stable id
//!    (a changed `SKILL.md` body is reflected, never duplicated).
//! 3. **rejects malformed frontmatter** — a `SKILL.md` missing `name:` aborts
//!    the *whole* batch with `SyncError::Malformed { path }`; nothing is written
//!    (all-or-nothing).
//! 4. **skips non-skill shapes** — a top-level `README.md` (not
//!    `<name>/SKILL.md`) is ignored; only the skill dir imports.
//! 5. **walks nested assets** — `skill-a/references/x.md` and
//!    `skill-a/scripts/y.sh` are both captured as `skill_file` rows.
//!
//! Each test opens a fresh `Store` in a `tempfile::tempdir()` (so the suite is
//! parallel-safe — no `$HOME` or process-env reliance) and seeds one workspace
//! row the imports are scoped to.

use std::path::Path;

use ainb_hangar_core::ids::WorkspaceId;
use ainb_hangar_daemon::skills_sync::{SyncError, skills_sync_from};
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::skill::SkillRepo;
use sqlx::SqlitePool;

/// Open a fresh store in a tempdir, returning the store and the tempdir guard.
///
/// The guard MUST be kept alive for the test's duration — dropping it deletes
/// the directory out from under the open `SQLite` pool.
async fn fresh_store() -> (Store, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    (store, dir)
}

/// Seed one workspace row and return its typed id. The importer scopes every
/// write to this workspace.
async fn seed_workspace(pool: &SqlitePool) -> WorkspaceId {
    let id = "ws-test-0001";
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, 'test', 'Test', 0)")
        .bind(id)
        .execute(pool)
        .await
        .expect("seed workspace");
    WorkspaceId::from_str(id).expect("non-empty workspace id")
}

/// Write a `SKILL.md` with YAML frontmatter under `root/<name>/SKILL.md`.
fn write_skill_md(root: &Path, name: &str, body: &str) {
    let dir = root.join(name);
    std::fs::create_dir_all(&dir).expect("create skill dir");
    let content = format!("---\nname: {name}\ndescription: {name} skill\n---\n\n{body}\n");
    std::fs::write(dir.join("SKILL.md"), content).expect("write SKILL.md");
}

/// Write an arbitrary asset file under `root/<skill>/<rel>`.
fn write_asset(root: &Path, skill: &str, rel: &str, body: &str) {
    let path = root.join(skill).join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).expect("create asset dir");
    std::fs::write(path, body).expect("write asset");
}

/// Count rows in `skill` for one workspace.
async fn count_skills(pool: &SqlitePool, ws: &WorkspaceId) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM skill WHERE workspace_id = ?")
        .bind(ws.as_str())
        .fetch_one(pool)
        .await
        .expect("count skills")
}

/// Count all `skill_file` rows joined to a workspace.
async fn count_skill_files(pool: &SqlitePool, ws: &WorkspaceId) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM skill_file f \
         JOIN skill s ON s.id = f.skill_id WHERE s.workspace_id = ?",
    )
    .bind(ws.as_str())
    .fetch_one(pool)
    .await
    .expect("count skill files")
}

#[tokio::test]
async fn test_skills_sync_imports_toolkit_directory() {
    let (store, _store_dir) = fresh_store().await;
    let pool = store.pool();
    let ws = seed_workspace(pool).await;

    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    // Two skills; `skill-a` carries one nested asset.
    write_skill_md(root, "skill-a", "Alpha body.");
    write_asset(root, "skill-a", "assets/foo.md", "asset foo");
    write_skill_md(root, "skill-b", "Beta body.");

    let report = skills_sync_from(pool, &ws, root).await.expect("sync ok");

    assert_eq!(report.imported.len(), 2, "two skills imported");
    assert_eq!(count_skills(pool, &ws).await, 2, "two skill rows");
    // 2 SKILL.md (captured as the `SKILL.md` skill_file) + 1 nested asset = 3.
    assert_eq!(
        count_skill_files(pool, &ws).await,
        3,
        "three skill_file rows"
    );
}

#[tokio::test]
async fn test_skills_sync_is_idempotent() {
    let (store, _store_dir) = fresh_store().await;
    let pool = store.pool();
    let ws = seed_workspace(pool).await;

    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    write_skill_md(root, "skill-a", "Alpha body.");
    write_asset(root, "skill-a", "assets/foo.md", "asset foo");
    write_skill_md(root, "skill-b", "Beta body.");

    // First import.
    skills_sync_from(pool, &ws, root).await.expect("first sync");
    let skills_after_first = count_skills(pool, &ws).await;
    let files_after_first = count_skill_files(pool, &ws).await;

    // Capture the stable id of `skill-a` before the second run; an idempotent
    // re-import must preserve it (no fork to a fresh ULID).
    let id_a = {
        let rows = SkillRepo::list(pool, &ws).await.expect("list");
        rows.into_iter()
            .find(|s| s.name.as_str() == "skill-a")
            .expect("skill-a present")
            .id
    };

    // Mutate the on-disk body, then re-import: idempotent by (workspace, name)
    // means the SAME row is updated in place (content reflects the new body),
    // never duplicated.
    write_skill_md(root, "skill-a", "Alpha body REVISED.");
    skills_sync_from(pool, &ws, root).await.expect("second sync");

    assert_eq!(
        count_skills(pool, &ws).await,
        skills_after_first,
        "skill row count unchanged on re-import"
    );
    assert_eq!(
        count_skill_files(pool, &ws).await,
        files_after_first,
        "skill_file row count unchanged on re-import"
    );

    let after = SkillRepo::get(pool, &id_a)
        .await
        .expect("get")
        .expect("skill-a still present under its original id");
    assert_eq!(
        after.id, id_a,
        "re-import updates the same row id (no duplicate fork)"
    );
    let body = after.files.iter().find(|f| f.path == "SKILL.md").expect("SKILL.md file");
    assert!(
        body.content.contains("REVISED"),
        "re-import updated the SKILL.md body in place: {}",
        body.content
    );
}

#[tokio::test]
async fn test_skills_sync_handles_malformed_frontmatter() {
    let (store, _store_dir) = fresh_store().await;
    let pool = store.pool();
    let ws = seed_workspace(pool).await;

    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    // One valid skill.
    write_skill_md(root, "skill-a", "Alpha body.");
    // One malformed skill: SKILL.md with frontmatter but NO `name:` key.
    let bad_dir = root.join("skill-bad");
    std::fs::create_dir_all(&bad_dir).unwrap();
    std::fs::write(
        bad_dir.join("SKILL.md"),
        "---\ndescription: missing name\n---\n\nbody\n",
    )
    .unwrap();

    let err = skills_sync_from(pool, &ws, root)
        .await
        .expect_err("malformed frontmatter must abort the batch");
    match err {
        SyncError::Malformed { path } => {
            assert!(
                path.ends_with("skill-bad/SKILL.md"),
                "error points at the offending SKILL.md: {}",
                path.display()
            );
        }
        other => panic!("expected SyncError::Malformed, got {other:?}"),
    }

    // All-or-nothing: the valid `skill-a` must NOT have been written.
    assert_eq!(
        count_skills(pool, &ws).await,
        0,
        "no partial import — zero rows after a malformed batch"
    );
    assert_eq!(count_skill_files(pool, &ws).await, 0, "no partial files");
}

#[tokio::test]
async fn test_skills_sync_skips_non_skill_dirs() {
    let (store, _store_dir) = fresh_store().await;
    let pool = store.pool();
    let ws = seed_workspace(pool).await;

    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    // A top-level README (not a `<name>/SKILL.md` shape) must be ignored.
    std::fs::write(root.join("README.md"), "# toolkit skills\n").unwrap();
    // A loose top-level file too.
    std::fs::write(root.join("index.json"), "{}").unwrap();
    // One real skill.
    write_skill_md(root, "skill-a", "Alpha body.");

    let report = skills_sync_from(pool, &ws, root).await.expect("sync ok");

    assert_eq!(report.imported.len(), 1, "only the real skill imports");
    assert_eq!(count_skills(pool, &ws).await, 1, "one skill row");
    let names: Vec<String> = SkillRepo::list(pool, &ws)
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.name.as_str().to_string())
        .collect();
    assert_eq!(names, vec!["skill-a".to_string()], "README/index ignored");
}

#[tokio::test]
async fn test_skills_sync_walks_nested_assets() {
    let (store, _store_dir) = fresh_store().await;
    let pool = store.pool();
    let ws = seed_workspace(pool).await;

    let src = tempfile::tempdir().unwrap();
    let root = src.path();
    write_skill_md(root, "skill-a", "Alpha body.");
    write_asset(root, "skill-a", "references/x.md", "reference x");
    write_asset(root, "skill-a", "scripts/y.sh", "#!/bin/sh\necho y\n");

    skills_sync_from(pool, &ws, root).await.expect("sync ok");

    let skill = SkillRepo::list(pool, &ws)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("skill-a present");
    let paths: Vec<&str> = skill.files.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"references/x.md"),
        "nested reference captured: {paths:?}"
    );
    assert!(
        paths.contains(&"scripts/y.sh"),
        "nested script captured: {paths:?}"
    );
    assert!(
        paths.contains(&"SKILL.md"),
        "the SKILL.md body file captured: {paths:?}"
    );
}
