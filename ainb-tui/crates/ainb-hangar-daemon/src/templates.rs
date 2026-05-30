//! P6.3 — `ainb hangar templates use <name>` transactional materialisation.
//!
//! Turns one embedded curated [`AgentTemplate`] into a live `agent` row plus its
//! skill attachments, atomically. The embedded registry itself
//! ([`ainb_hangar_core::template::TemplateRegistry`]) is IO-free and lives in
//! `ainb-hangar-core`; this module owns the side-effecting half — resolving the
//! workspace's imported skills and writing the agent + `agent_skill` rows in a
//! single transaction.
//!
//! # Skills must be pre-imported
//!
//! A template references skills by *name*. Those names must already exist as
//! `skill` rows in the target workspace (imported via `ainb hangar skills sync`,
//! P6.2). If any referenced skill is absent, [`templates_use`] returns
//! [`TemplateUseError::SkillNotImported`] — which carries a hint to run sync —
//! and writes nothing. We deliberately do **not** create skill rows here: skills
//! are workspace-owned, curated content, and silently minting an empty skill row
//! would hide the real "you forgot to sync" error.
//!
//! # Atomicity + idempotency
//!
//! The agent insert and every `agent_skill` insert run in one transaction: any
//! failure (a missing skill surfaces *before* the transaction opens; an SQL
//! error inside rolls back) leaves zero rows. Re-running `use <name>` is
//! idempotent by agent *name* within the workspace: the second call finds the
//! existing agent and returns it without inserting a duplicate (and without
//! re-attaching, since attach itself is `ON CONFLICT DO NOTHING`).

use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::{AgentId, SkillId, WorkspaceId};
use ainb_hangar_core::skill::SkillName;
use ainb_hangar_core::template::{AgentTemplate, TemplateRegistry};
use sqlx::{Row, SqlitePool};

/// Outcome of a [`templates_use`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateUseOutcome {
    /// The agent that now exists (freshly created, or the pre-existing one on a
    /// repeated `use`).
    pub agent_id: AgentId,
    /// The skill ids attached to the agent (resolved from the template's skill
    /// names). Empty only if the template bundled no skills (the embedded set
    /// never does).
    pub skill_ids: Vec<SkillId>,
    /// `true` when this call created a new agent; `false` when it returned a
    /// pre-existing agent unchanged (the idempotent path).
    pub created: bool,
}

/// Failure surface for `templates use`.
#[derive(Debug, thiserror::Error)]
pub enum TemplateUseError {
    /// No curated template carries the requested name.
    #[error("no curated template named `{name}` (run `ainb hangar templates list` to see them)")]
    UnknownTemplate {
        /// The name the caller asked for.
        name: String,
    },
    /// The target workspace does not exist.
    #[error("workspace `{0}` not found")]
    WorkspaceNotFound(String),
    /// The workspace has no runtime to bind the new agent to. An agent's
    /// `runtime_id` is required (Multica pattern), so a runtime must be
    /// registered first.
    #[error(
        "workspace `{workspace}` has no agent runtime registered; \
         register a provider runtime before creating agents from templates"
    )]
    NoRuntime {
        /// The workspace that lacked a runtime.
        workspace: String,
    },
    /// A skill the template references is not imported into the workspace yet.
    /// Carries the actionable hint: run the importer.
    #[error(
        "template `{template}` needs skill `{skill}`, which is not imported into \
         workspace `{workspace}`. Run `ainb hangar skills sync` first to import the \
         curated toolkit skills."
    )]
    SkillNotImported {
        /// The template being applied.
        template: String,
        /// The missing skill name (normalised).
        skill: String,
        /// The workspace it was looked up in.
        workspace: String,
    },
    /// An underlying store failure.
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// Apply a curated template to a workspace: create (or reuse) the agent and
/// attach every referenced skill, in one transaction.
///
/// `agent_name_override` lets a caller name the agent something other than the
/// template name (the `--agent-name` flag); when `None`, the agent is named
/// after the template. Idempotency is keyed on the *resolved* agent name within
/// the workspace.
///
/// # Errors
///
/// - [`TemplateUseError::UnknownTemplate`] — no template by that name.
/// - [`TemplateUseError::WorkspaceNotFound`] — the workspace id does not exist.
/// - [`TemplateUseError::NoRuntime`] — the workspace has no runtime to bind to.
/// - [`TemplateUseError::SkillNotImported`] — a referenced skill is not yet
///   imported (run `ainb hangar skills sync`); nothing is written.
/// - [`TemplateUseError::Db`] — a store error; the transaction rolls back.
pub async fn templates_use(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    template_name: &str,
    agent_name_override: Option<&str>,
) -> Result<TemplateUseOutcome, TemplateUseError> {
    let template =
        TemplateRegistry::get(template_name).ok_or_else(|| TemplateUseError::UnknownTemplate {
            name: template_name.to_string(),
        })?;

    // The agent's display name (idempotency key within the workspace).
    let agent_name = agent_name_override.unwrap_or(&template.name);

    // Resolve every referenced skill to an existing row BEFORE opening the
    // transaction. A missing skill is a hard error (no silent creation) — and
    // resolving up front means we never start a partial write.
    let skill_ids = resolve_skill_ids(pool, workspace, &template).await?;

    // Idempotency: if an agent with this name already exists in the workspace,
    // return it unchanged (no duplicate, no re-attach churn).
    if let Some(existing) = find_agent_by_name(pool, workspace, agent_name).await? {
        return Ok(TemplateUseOutcome {
            agent_id: existing,
            skill_ids,
            created: false,
        });
    }

    // Resolve the owner first: it also validates the workspace exists, so a bad
    // workspace id surfaces as `WorkspaceNotFound` rather than the more confusing
    // `NoRuntime`. A runtime is then required to bind the agent (NOT NULL FK);
    // templates do not register runtimes themselves.
    let owner_id = workspace_owner(pool, workspace).await?;
    let runtime_id = first_runtime(pool, workspace).await?;

    let agent_id = SystemIdGen.new_ulid();

    // The agent row and every junction insert share one transaction: any failure
    // rolls back all of them, so a partially-built agent can never be observed.
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO agent \
         (id, workspace_id, name, runtime_id, instructions, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, 'workspace', ?)",
    )
    .bind(&agent_id)
    .bind(workspace.as_str())
    .bind(agent_name)
    .bind(&runtime_id)
    .bind(&template.instructions)
    .bind(&owner_id)
    .execute(&mut *tx)
    .await?;

    for skill_id in &skill_ids {
        sqlx::query(
            "INSERT INTO agent_skill (agent_id, skill_id) VALUES (?, ?) \
             ON CONFLICT (agent_id, skill_id) DO NOTHING",
        )
        .bind(&agent_id)
        .bind(skill_id.as_str())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;

    let agent_id = AgentId::from_str(agent_id).expect("freshly minted ulid is non-empty");
    Ok(TemplateUseOutcome {
        agent_id,
        skill_ids,
        created: true,
    })
}

/// Resolve each of the template's skill *names* to an existing `skill` row id in
/// the workspace, erroring (with the sync hint) on the first one that is absent.
async fn resolve_skill_ids(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    template: &AgentTemplate,
) -> Result<Vec<SkillId>, TemplateUseError> {
    let mut ids = Vec::with_capacity(template.skills.len());
    for raw in &template.skills {
        // Normalise the name the same way the importer does so the lookup key
        // matches the stored `(workspace_id, name)` row.
        let name = SkillName::new(raw).map_or_else(|_| raw.clone(), |n| n.as_str().to_string());
        let row: Option<String> =
            sqlx::query_scalar("SELECT id FROM skill WHERE workspace_id = ? AND name = ?")
                .bind(workspace.as_str())
                .bind(&name)
                .fetch_optional(pool)
                .await?;
        let id = row.ok_or_else(|| TemplateUseError::SkillNotImported {
            template: template.name.clone(),
            skill: name.clone(),
            workspace: workspace.as_str().to_string(),
        })?;
        ids.push(SkillId::from_str(id).expect("stored skill id is non-empty"));
    }
    Ok(ids)
}

/// Find an existing agent by `(workspace_id, name)`, returning its id.
async fn find_agent_by_name(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
    name: &str,
) -> Result<Option<AgentId>, TemplateUseError> {
    let row: Option<String> =
        sqlx::query_scalar("SELECT id FROM agent WHERE workspace_id = ? AND name = ?")
            .bind(workspace.as_str())
            .bind(name)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|id| AgentId::from_str(id).expect("stored agent id is non-empty")))
}

/// The workspace's first registered runtime id (oldest by insertion order), or a
/// [`TemplateUseError::NoRuntime`] when none is registered.
async fn first_runtime(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
) -> Result<String, TemplateUseError> {
    let row = sqlx::query("SELECT id FROM agent_runtime WHERE workspace_id = ? LIMIT 1")
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
    row.map_or_else(
        || {
            Err(TemplateUseError::NoRuntime {
                workspace: workspace.as_str().to_string(),
            })
        },
        |r| Ok(r.get::<String, _>("id")),
    )
}

/// The owner user id for the workspace: the workspace's `owner`-role member,
/// else the oldest user (standalone-CLI bootstrap leaves exactly one).
///
/// Validates the workspace exists first (so a bad workspace id surfaces as
/// [`TemplateUseError::WorkspaceNotFound`] rather than a confusing missing-owner
/// error).
async fn workspace_owner(
    pool: &SqlitePool,
    workspace: &WorkspaceId,
) -> Result<String, TemplateUseError> {
    let ws_exists: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE id = ?")
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
    if ws_exists.is_none() {
        return Err(TemplateUseError::WorkspaceNotFound(
            workspace.as_str().to_string(),
        ));
    }

    let owner: Option<String> = sqlx::query_scalar(
        "SELECT user_id FROM member WHERE workspace_id = ? AND role = 'owner' \
         ORDER BY user_id LIMIT 1",
    )
    .bind(workspace.as_str())
    .fetch_optional(pool)
    .await?;
    if let Some(owner) = owner {
        return Ok(owner);
    }

    // Fallback: the oldest user (the standalone-CLI bootstrap creates one).
    let any_user: Option<String> =
        sqlx::query_scalar("SELECT id FROM user ORDER BY created_at LIMIT 1")
            .fetch_optional(pool)
            .await?;
    any_user.ok_or(TemplateUseError::WorkspaceNotFound(
        workspace.as_str().to_string(),
    ))
}
