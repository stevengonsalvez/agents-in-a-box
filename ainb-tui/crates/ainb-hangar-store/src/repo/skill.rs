//! Typed repository wrappers over the skill tables.
//!
//! Covers `skill`, `skill_file`, and `agent_skill`.
//!
//! A [`Skill`] is a reusable instruction bundle owned by a workspace. It owns
//! zero or more [`SkillFile`] rows (composite PK `(skill_id, path)`) and is
//! attached to agents through the `agent_skill` join (composite PK
//! `(agent_id, skill_id)`). All three are thin, stateless sqlx wrappers sharing
//! the single [`crate::Store`] pool.

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
    /// Insert one [`Skill`] row.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on failure (e.g. `workspace_id` FK violation
    /// or duplicate primary key).
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

    /// Fetch one [`Skill`] by primary key, or `None` if absent.
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] if the query itself fails.
    pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<Skill>, sqlx::Error> {
        sqlx::query_as::<_, Skill>(
            "SELECT id, workspace_id, name, description, content FROM skill WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    }

    /// List all skills in a workspace, ordered by `name`.
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

    /// List all [`SkillFile`] rows for a skill, ordered by `path`.
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

    /// Attach a skill to an agent (insert into the `agent_skill` join).
    ///
    /// # Errors
    ///
    /// Returns a [`sqlx::Error`] on failure (FK violation on either side, or a
    /// duplicate `(agent_id, skill_id)` link).
    pub async fn attach_to_agent(
        pool: &SqlitePool,
        agent_id: &str,
        skill_id: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO agent_skill (agent_id, skill_id) VALUES (?, ?)")
            .bind(agent_id)
            .bind(skill_id)
            .execute(pool)
            .await?;
        Ok(())
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
        sqlx::query_scalar(
            "SELECT skill_id FROM agent_skill WHERE agent_id = ? ORDER BY skill_id",
        )
        .bind(agent_id)
        .fetch_all(pool)
        .await
    }
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
