//! Typed repository wrappers over the skill tables.
//!
//! Covers `skill`, `skill_file`, and `agent_skill`.
//!
//! A [`Skill`] is a reusable instruction bundle owned by a workspace. It owns
//! zero or more [`SkillFile`] rows (composite PK `(skill_id, path)`) and is
//! attached to agents through the `agent_skill` join (composite PK
//! `(agent_id, skill_id)`). All three are thin, stateless sqlx wrappers sharing
//! the single [`crate::Store`] pool.
//!
//! # Two layers
//!
//! - **Low-level row methods** ([`SkillRepo::insert`], [`SkillRepo::insert_file`],
//!   [`SkillRepo::list_by_workspace`], …) mirror the columns one-to-one and take
//!   raw `&str` ids. The seed/snapshot paths in the daemon use these.
//! - **P6.1 typed API** ([`SkillRepo::create`], [`SkillRepo::upsert_by_name`],
//!   [`SkillRepo::skills_for_agent`], [`SkillRepo::delete`], …) takes the
//!   strongly-typed [`WorkspaceId`]/[`AgentId`]/[`SkillId`] newtypes, normalises
//!   the skill name to kebab-case, and returns the [`SkillWithFiles`] aggregate.
//!   This is the surface the importer (P6.2), templates (P6.3), and dispatch
//!   materialiser (P6.4) build on.
//!
//! # Workspace scoping
//!
//! **Every method in the typed API takes a [`WorkspaceId`] and enforces it in
//! SQL — there is no by-id read or mutation that trusts a bare ULID.** This is
//! the tenant-isolation boundary: a caller holding a [`SkillId`] or [`AgentId`]
//! minted in workspace A can never read, delete, or attach against workspace B's
//! rows.
//!
//! - by-name / list reads ([`SkillRepo::list`], [`SkillRepo::upsert_by_name`])
//!   filter `WHERE workspace_id = ?`;
//! - by-id reads/deletes ([`SkillRepo::get`], [`SkillRepo::delete`]) filter
//!   `WHERE id = ? AND workspace_id = ?` (the delete cascade to `skill_file` is
//!   itself scoped, so a cross-tenant delete touches no child rows);
//! - the agent-scoped read ([`SkillRepo::skills_for_agent`]) joins through the
//!   `agent` row and filters `WHERE a.workspace_id = ?`, so a foreign agent id
//!   yields an empty set;
//! - the junction mutations ([`SkillRepo::attach_to_agent`],
//!   [`SkillRepo::detach_from_agent`]) verify that **both** the agent and the
//!   skill belong to the supplied workspace before touching `agent_skill`, and
//!   return [`SkillRepoError::CrossWorkspace`] otherwise.
//!
//! The `(workspace_id, name)` unique index (`idx_skill_workspace_name`,
//! migration 0008) is the authoritative guard that a skill name resolves to
//! exactly one row per tenant. The schema declares no `ON DELETE CASCADE` on
//! `skill_file`, so the cascade-on-delete is performed explicitly in application
//! code (see [`SkillRepo::delete`]) rather than by the engine.

use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AgentId, SkillId, WorkspaceId};
use ainb_hangar_core::skill::{SkillFileInput, SkillName, SkillNameError, SkillWithFiles};
use sqlx::SqlitePool;

/// A reusable instruction bundle (Anthropic-style skill).
///
/// Fields track the `skill` columns one-to-one. `content` is the top-level
/// skill body (e.g. a `SKILL.md`); `None` when the skill is file-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// Primary key (ULID string).
    pub id: String,
    /// Owning workspace (`workspace.id`).
    pub workspace_id: String,
    /// Human-readable skill name.
    pub name: String,
    /// Short description; `None` when unset.
    pub description: Option<String>,
    /// Top-level skill body; `None` when file-only.
    pub content: Option<String>,
}

/// A single file belonging to a [`Skill`], keyed by `(skill_id, path)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    /// Owning skill (`skill.id`).
    pub skill_id: String,
    /// Relative path of the file within the skill bundle.
    pub path: String,
    /// File contents; `None` for an empty placeholder.
    pub content: Option<String>,
}

/// Stateless typed wrapper over the skill tables.
pub struct SkillRepo;

impl SkillRepo {
    // ---- Low-level row methods (raw `&str` ids) -------------------------------

    /// Insert one [`Skill`] row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on failure (e.g. `workspace_id` FK violation,
    /// duplicate primary key, or a `(workspace_id, name)` uniqueness conflict).
    pub async fn insert(pool: &SqlitePool, skill: &Skill) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO skill (id, workspace_id, name, description, content) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&skill.id)
        .bind(&skill.workspace_id)
        .bind(&skill.name)
        .bind(&skill.description)
        .bind(&skill.content)
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Fetch one raw [`Skill`] row by primary key, or `None` if absent.
    ///
    /// Prefer [`SkillRepo::get`] in new code — it returns the typed aggregate.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query itself fails.
    pub async fn get_row(pool: &SqlitePool, id: &str) -> Result<Option<Skill>, sqlx::Error> {
        sqlx::query_as::<_, Skill>(
            "SELECT id, workspace_id, name, description, content FROM skill WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    }

    /// List all raw skill rows in a workspace, ordered by `name`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_by_workspace(
        pool: &SqlitePool,
        workspace_id: &str,
    ) -> Result<Vec<Skill>, sqlx::Error> {
        sqlx::query_as::<_, Skill>(
            "SELECT id, workspace_id, name, description, content \
             FROM skill WHERE workspace_id = ? ORDER BY name",
        )
        .bind(workspace_id)
        .fetch_all(pool)
        .await
    }

    /// Insert one [`SkillFile`] row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on failure (e.g. `skill_id` FK violation or a
    /// duplicate `(skill_id, path)` composite key).
    pub async fn insert_file(pool: &SqlitePool, file: &SkillFile) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO skill_file (skill_id, path, content) VALUES (?, ?, ?)")
            .bind(&file.skill_id)
            .bind(&file.path)
            .bind(&file.content)
            .execute(pool)
            .await?;
        Ok(())
    }

    /// List all raw [`SkillFile`] rows for a skill, ordered by `path`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_files(
        pool: &SqlitePool,
        skill_id: &str,
    ) -> Result<Vec<SkillFile>, sqlx::Error> {
        sqlx::query_as::<_, SkillFile>(
            "SELECT skill_id, path, content FROM skill_file WHERE skill_id = ? ORDER BY path",
        )
        .bind(skill_id)
        .fetch_all(pool)
        .await
    }

    /// List the skill ids attached to an agent, ordered by `skill_id`.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn list_for_agent(
        pool: &SqlitePool,
        agent_id: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT skill_id FROM agent_skill WHERE agent_id = ? ORDER BY skill_id")
            .bind(agent_id)
            .fetch_all(pool)
            .await
    }

    // ---- P6.1 typed API -------------------------------------------------------

    /// Create a skill (and its ordered files) in one transaction.
    ///
    /// The `name` is normalised to lowercase kebab-case via [`SkillName`] before
    /// the row is written, so `"Commit"`, `"commit"`, and `"COMMIT"` collapse to
    /// the same `(workspace_id, name)` key. Files are written in the order given
    /// (the `Vec<SkillFileInput>` is never re-ordered) so import output is
    /// byte-stable.
    ///
    /// The whole insert runs in a transaction: a uniqueness conflict on the
    /// skill row, or any file failure, rolls back everything — there is never a
    /// partially-imported skill.
    ///
    /// # Errors
    ///
    /// Returns [`SkillRepoError::Name`] if `name` normalises to empty, or
    /// [`SkillRepoError::Db`] on any SQL failure — most notably a
    /// `(workspace_id, name)` uniqueness conflict (the duplicate case).
    pub async fn create(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        name: &str,
        description: Option<&str>,
        content: Option<&str>,
        files: Vec<SkillFileInput>,
    ) -> Result<SkillId, SkillRepoError> {
        let name = SkillName::new(name)?;
        let id = SystemIdGen.new_ulid();
        Self::write_new(pool, &id, workspace, &name, description, content, &files).await?;
        SkillId::from_str(id).map_err(|_| SkillRepoError::EmptyId)
    }

    /// Fetch one skill as a [`SkillWithFiles`] aggregate, or `None` if absent.
    ///
    /// Workspace-scoped: the row is fetched with `WHERE id = ? AND workspace_id
    /// = ?`, so a [`SkillId`] minted in another workspace resolves to `None`
    /// (never another tenant's row).
    ///
    /// # Errors
    ///
    /// Returns [`SkillRepoError::Db`] on a SQL failure, or
    /// [`SkillRepoError::Name`] if a stored name fails normalisation (a
    /// corrupt-row guard that should be impossible given [`create`] normalises
    /// on write).
    pub async fn get(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &SkillId,
    ) -> Result<Option<SkillWithFiles>, SkillRepoError> {
        let row = sqlx::query_as::<_, Skill>(
            "SELECT id, workspace_id, name, description, content \
             FROM skill WHERE id = ? AND workspace_id = ?",
        )
        .bind(id.as_str())
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(Self::hydrate(pool, row).await?))
    }

    /// List every skill in a workspace as [`SkillWithFiles`], ordered by name.
    ///
    /// Workspace-scoped: a skill owned by another workspace can never appear.
    ///
    /// # Errors
    ///
    /// Returns [`SkillRepoError::Db`] on a SQL failure or
    /// [`SkillRepoError::Name`] on a corrupt stored name.
    pub async fn list(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Vec<SkillWithFiles>, SkillRepoError> {
        let rows = Self::list_by_workspace(pool, workspace.as_str()).await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(Self::hydrate(pool, row).await?);
        }
        Ok(out)
    }

    /// Insert-or-update a skill keyed by `(workspace_id, name)` — the importer
    /// hot path.
    ///
    /// Normalises `name`, then `INSERT ... ON CONFLICT (workspace_id, name) DO
    /// UPDATE` so re-running an import updates the existing row in place (never
    /// forking a duplicate). The file set is replaced wholesale: existing
    /// `skill_file` rows for the skill are deleted and the supplied `files` are
    /// re-inserted in order. Returns the (stable across re-runs) [`SkillId`].
    ///
    /// All of this runs in a single transaction.
    ///
    /// # Errors
    ///
    /// Returns [`SkillRepoError::Name`] if `name` normalises to empty, or
    /// [`SkillRepoError::Db`] on any SQL failure.
    pub async fn upsert_by_name(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        name: &str,
        description: Option<&str>,
        content: Option<&str>,
        files: Vec<SkillFileInput>,
    ) -> Result<SkillId, SkillRepoError> {
        let name = SkillName::new(name)?;
        let new_id = SystemIdGen.new_ulid();

        let mut tx = pool.begin().await?;

        // Upsert the parent row. On conflict the existing `id` is preserved
        // (RETURNING gives us whichever id won), so the SkillId is stable across
        // re-imports.
        let id: String = sqlx::query_scalar(
            "INSERT INTO skill (id, workspace_id, name, description, content) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (workspace_id, name) DO UPDATE SET \
                 description = excluded.description, \
                 content = excluded.content \
             RETURNING id",
        )
        .bind(&new_id)
        .bind(workspace.as_str())
        .bind(name.as_str())
        .bind(description)
        .bind(content)
        .fetch_one(&mut *tx)
        .await?;

        // Replace the file set wholesale, in path-stable insertion order.
        sqlx::query("DELETE FROM skill_file WHERE skill_id = ?")
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        for file in &files {
            sqlx::query("INSERT INTO skill_file (skill_id, path, content) VALUES (?, ?, ?)")
                .bind(&id)
                .bind(&file.path)
                .bind(&file.content)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        SkillId::from_str(id).map_err(|_| SkillRepoError::EmptyId)
    }

    /// Attach a skill to an agent (idempotent), within a single workspace.
    ///
    /// Both the `agent` and the `skill` must belong to `workspace`; otherwise
    /// this returns [`SkillRepoError::CrossWorkspace`] and writes nothing — a
    /// caller holding an id from another tenant can never forge a junction row.
    /// When both checks pass it runs `INSERT ... ON CONFLICT (agent_id,
    /// skill_id) DO NOTHING`, so calling it twice leaves exactly one row.
    ///
    /// # Errors
    ///
    /// Returns [`SkillRepoError::CrossWorkspace`] if either the agent or the
    /// skill is not in `workspace`, or [`SkillRepoError::Db`] on a SQL failure.
    pub async fn attach_to_agent(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        agent: &AgentId,
        skill: &SkillId,
    ) -> Result<(), SkillRepoError> {
        Self::ensure_same_workspace(pool, workspace, agent, skill).await?;
        sqlx::query(
            "INSERT INTO agent_skill (agent_id, skill_id) VALUES (?, ?) \
             ON CONFLICT (agent_id, skill_id) DO NOTHING",
        )
        .bind(agent.as_str())
        .bind(skill.as_str())
        .execute(pool)
        .await?;
        Ok(())
    }

    /// Detach a skill from an agent, within a single workspace. Removing an
    /// absent link affects zero rows (not an error), so detach is idempotent.
    ///
    /// Both the `agent` and the `skill` must belong to `workspace`; otherwise
    /// this returns [`SkillRepoError::CrossWorkspace`] and deletes nothing.
    ///
    /// # Errors
    ///
    /// Returns [`SkillRepoError::CrossWorkspace`] if either the agent or the
    /// skill is not in `workspace`, or [`SkillRepoError::Db`] on a SQL failure.
    pub async fn detach_from_agent(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        agent: &AgentId,
        skill: &SkillId,
    ) -> Result<(), SkillRepoError> {
        Self::ensure_same_workspace(pool, workspace, agent, skill).await?;
        sqlx::query("DELETE FROM agent_skill WHERE agent_id = ? AND skill_id = ?")
            .bind(agent.as_str())
            .bind(skill.as_str())
            .execute(pool)
            .await?;
        Ok(())
    }

    /// Return the ordered file inputs for a skill (by `path`).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query fails.
    pub async fn files_for(
        pool: &SqlitePool,
        id: &SkillId,
    ) -> Result<Vec<SkillFileInput>, sqlx::Error> {
        let rows = Self::list_files(pool, id.as_str()).await?;
        Ok(rows
            .into_iter()
            .map(|f| SkillFileInput {
                path: f.path,
                content: f.content.unwrap_or_default(),
            })
            .collect())
    }

    /// List the skills attached to an agent as [`SkillWithFiles`], ordered by
    /// skill name.
    ///
    /// Joins `agent_skill` → `skill` and additionally joins the `agent` row,
    /// filtering `WHERE ag.workspace_id = ?`: a foreign agent id (one that does
    /// not belong to `workspace`) yields an empty set rather than leaking
    /// another tenant's skills. The result carries each skill's full file set
    /// (the shape the dispatch materialiser, P6.4, writes to disk).
    ///
    /// # Errors
    ///
    /// Returns [`SkillRepoError::Db`] on a SQL failure or
    /// [`SkillRepoError::Name`] on a corrupt stored name.
    pub async fn skills_for_agent(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        agent: &AgentId,
    ) -> Result<Vec<SkillWithFiles>, SkillRepoError> {
        let rows = sqlx::query_as::<_, Skill>(
            "SELECT s.id, s.workspace_id, s.name, s.description, s.content \
             FROM skill s \
             JOIN agent_skill a ON a.skill_id = s.id \
             JOIN agent ag ON ag.id = a.agent_id \
             WHERE a.agent_id = ? AND ag.workspace_id = ? \
             ORDER BY s.name",
        )
        .bind(agent.as_str())
        .bind(workspace.as_str())
        .fetch_all(pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(Self::hydrate(pool, row).await?);
        }
        Ok(out)
    }

    /// Delete a skill and its child `skill_file` rows in one transaction.
    ///
    /// Workspace-scoped: the delete only fires when the skill belongs to
    /// `workspace`. The child `skill_file` cascade is scoped through the same
    /// guard (a subquery confirming the parent is in `workspace`), so a
    /// cross-tenant call is a complete no-op — it removes neither the parent nor
    /// any child file.
    ///
    /// The schema declares no `ON DELETE CASCADE` on `skill_file`, so the cascade
    /// is performed explicitly: child files are deleted first, then the parent,
    /// atomically. (Foreign keys ARE enforced — sqlx enables `PRAGMA
    /// foreign_keys` — which is exactly why the children must go first.)
    /// Any `agent_skill` links are left to the caller (deleting a skill that is
    /// still attached is a service-layer decision, not a storage one).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if either delete fails.
    pub async fn delete(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        id: &SkillId,
    ) -> Result<(), sqlx::Error> {
        let mut tx = pool.begin().await?;
        // Scope the child cascade through the parent's workspace: only delete
        // files whose skill is owned by `workspace`.
        sqlx::query(
            "DELETE FROM skill_file WHERE skill_id IN \
             (SELECT id FROM skill WHERE id = ? AND workspace_id = ?)",
        )
        .bind(id.as_str())
        .bind(workspace.as_str())
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM skill WHERE id = ? AND workspace_id = ?")
            .bind(id.as_str())
            .bind(workspace.as_str())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    // ---- private helpers ------------------------------------------------------

    /// Verify that both `agent` and `skill` belong to `workspace` before a
    /// junction mutation. Returns [`SkillRepoError::CrossWorkspace`] if either is
    /// absent from the workspace (which also covers a non-existent id).
    async fn ensure_same_workspace(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        agent: &AgentId,
        skill: &SkillId,
    ) -> Result<(), SkillRepoError> {
        let agent_ok: Option<String> =
            sqlx::query_scalar("SELECT id FROM agent WHERE id = ? AND workspace_id = ?")
                .bind(agent.as_str())
                .bind(workspace.as_str())
                .fetch_optional(pool)
                .await?;
        if agent_ok.is_none() {
            return Err(SkillRepoError::CrossWorkspace);
        }
        let skill_ok: Option<String> =
            sqlx::query_scalar("SELECT id FROM skill WHERE id = ? AND workspace_id = ?")
                .bind(skill.as_str())
                .bind(workspace.as_str())
                .fetch_optional(pool)
                .await?;
        if skill_ok.is_none() {
            return Err(SkillRepoError::CrossWorkspace);
        }
        Ok(())
    }

    /// Write a new skill row plus its files in one transaction. Shared by
    /// [`create`]; uniqueness conflicts surface from the parent insert.
    async fn write_new(
        pool: &SqlitePool,
        id: &str,
        workspace: &WorkspaceId,
        name: &SkillName,
        description: Option<&str>,
        content: Option<&str>,
        files: &[SkillFileInput],
    ) -> Result<(), sqlx::Error> {
        let mut tx = pool.begin().await?;
        sqlx::query(
            "INSERT INTO skill (id, workspace_id, name, description, content) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(workspace.as_str())
        .bind(name.as_str())
        .bind(description)
        .bind(content)
        .execute(&mut *tx)
        .await?;
        for file in files {
            sqlx::query("INSERT INTO skill_file (skill_id, path, content) VALUES (?, ?, ?)")
                .bind(id)
                .bind(&file.path)
                .bind(&file.content)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Turn a raw [`Skill`] row plus its files into a [`SkillWithFiles`].
    async fn hydrate(pool: &SqlitePool, row: Skill) -> Result<SkillWithFiles, SkillRepoError> {
        let files = Self::list_files(pool, &row.id).await?;
        let id = SkillId::from_str(row.id).map_err(|_| SkillRepoError::EmptyId)?;
        let name = SkillName::new(&row.name)?;
        Ok(SkillWithFiles {
            id,
            workspace_id: row.workspace_id,
            name,
            description: row.description,
            content: row.content,
            files: files
                .into_iter()
                .map(|f| SkillFileInput {
                    path: f.path,
                    content: f.content.unwrap_or_default(),
                })
                .collect(),
        })
    }
}

/// Error surface for the P6.1 typed [`SkillRepo`] API.
///
/// Splits a name-normalisation failure (caller passed an empty/separator-only
/// name) from a database failure (uniqueness conflict, FK violation, IO).
#[derive(Debug, thiserror::Error)]
pub enum SkillRepoError {
    /// The skill name normalised to the empty string.
    #[error(transparent)]
    Name(#[from] SkillNameError),
    /// An underlying `sqlx` failure (uniqueness conflict, FK violation, IO).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// A stored primary key was empty — an invariant violation that should be
    /// unreachable (PKs are minted as 26-char ULIDs). Surfaced rather than
    /// `unwrap`'d so a corrupt row fails loudly instead of panicking the daemon.
    #[error("corrupt skill row: empty primary key")]
    EmptyId,
    /// A junction mutation ([`SkillRepo::attach_to_agent`] /
    /// [`SkillRepo::detach_from_agent`]) referenced an agent and/or skill that
    /// do not both belong to the supplied workspace. The mutation is rejected
    /// (nothing written) — this is the tenant-isolation guard against IDOR.
    #[error("agent and skill must belong to the same workspace")]
    CrossWorkspace,
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for Skill {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            id: row.try_get("id")?,
            workspace_id: row.try_get("workspace_id")?,
            name: row.try_get("name")?,
            description: row.try_get("description")?,
            content: row.try_get("content")?,
        })
    }
}

impl sqlx::FromRow<'_, sqlx::sqlite::SqliteRow> for SkillFile {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Self, sqlx::Error> {
        use sqlx::Row;
        Ok(Self {
            skill_id: row.try_get("skill_id")?,
            path: row.try_get("path")?,
            content: row.try_get("content")?,
        })
    }
}
