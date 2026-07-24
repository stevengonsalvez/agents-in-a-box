//! Typed repository wrapper over the `workspace` table's per-workspace config
//! columns (e38.21).
//!
//! Migration 0001's `workspace` was identity-only (`id` / `slug` / `name` /
//! `created_at`). Migration 0020 added three nullable config columns that hold
//! the per-workspace agent-run configuration the reference's workspaces carry:
//!
//! - **`context_prompt`** — free-text agent context. When set, dispatch writes
//!   it into the per-task execenv as a `CLAUDE.md` so the agent run actually
//!   sees it (the daemon's [`crate`]-external `execenv` layer). NULL = no
//!   per-workspace context (the v1 behaviour).
//! - **`repo_whitelist`** — the repositories a workspace task may check out, as a
//!   JSON array of `"owner/name"` strings. A repo-checkout flow does not exist
//!   yet, so this repo PERSISTS + VALIDATES + EXPOSES the whitelist; the gate
//!   point is [`WorkspaceConfig::repo_allowed`], to be called from a checkout
//!   flow once it lands. NULL = "no whitelist configured" (no gate).
//! - **`issue_prefix`** — a short prefix prepended to a newly-created issue's
//!   title in this workspace (e.g. `[OPS] `). Applied at issue-create time via
//!   [`apply_issue_prefix`]. NULL = no prefix (the v1 title verbatim).
//!
//! # Workspace scoping
//!
//! Every method takes a [`WorkspaceId`] and enforces it in SQL: a foreign /
//! unknown workspace id resolves to no row, so [`WorkspaceRepo::get_config`]
//! yields `None` and [`WorkspaceRepo::set_config`] touches nothing (a
//! [`WorkspaceRepoError::NotFound`], never a cross-tenant write).

use ainb_hangar_core::clock::{HangarClock, SystemClock};
use ainb_hangar_core::idgen::{IdGen, SystemIdGen};
use ainb_hangar_core::ids::WorkspaceId;
use sqlx::SqlitePool;

use crate::bootstrap::default_owner_id;

/// The per-workspace config read from / written to the `workspace` table's
/// migration-0020 columns.
///
/// Every field is optional (NULL in the column = "not configured"). The
/// `repo_whitelist` is held in its decoded form (a list of `"owner/name"`
/// repository slugs); the store serialises it to a JSON array on write and
/// parses it on read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceConfig {
    /// Free-text agent context injected into a task's execenv as `CLAUDE.md`.
    /// `None` = no per-workspace context.
    pub context_prompt: Option<String>,
    /// The repositories a workspace task may check out (`owner/name` slugs).
    /// `None` = no whitelist configured (no gate). `Some(vec)` is the allowed
    /// set; an empty vec is a configured-but-empty whitelist (gates everything).
    pub repo_whitelist: Option<Vec<String>>,
    /// A prefix prepended to a newly-created issue's title (e.g. `[OPS] `).
    /// `None` = no prefix.
    pub issue_prefix: Option<String>,
}

impl WorkspaceConfig {
    /// Whether `repo` (an `"owner/name"` slug) is permitted by this workspace's
    /// whitelist.
    ///
    /// The gate seam for a future repo-checkout flow: a checkout must call this
    /// before cloning. `None` whitelist = no gate (every repo allowed, the v1
    /// behaviour); `Some(list)` = `repo` must appear in `list` (an empty list
    /// allows nothing).
    #[must_use]
    pub fn repo_allowed(&self, repo: &str) -> bool {
        self.repo_whitelist.as_ref().is_none_or(|list| list.iter().any(|r| r == repo))
    }
}

/// A workspace's identity row (`id` / `slug` / `name`), as returned by
/// [`WorkspaceRepo::create`]. The stable ULID `id` is what `state.toml` and every
/// daemon RPC key on; `slug`/`name` are display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    /// Stable ULID workspace id.
    pub id: String,
    /// Short display handle (e.g. `acme`).
    pub slug: String,
    /// Human-readable display name.
    pub name: String,
}

/// Stateless typed wrapper over the `workspace` table's config columns.
pub struct WorkspaceRepo;

impl WorkspaceRepo {
    /// Create a workspace + owner `member` row in one transaction (multica
    /// `CreateWorkspace` parity).
    ///
    /// Mints a fresh ULID `id`, inserts the `workspace` row, and links the
    /// bootstrap owner user (resolved via [`default_owner_id`]) with an `owner`
    /// `member` row — mirroring multica's "insert workspace, then insert an owner
    /// member" transaction. Hangar is single-operator, so the owner user already
    /// exists (the bootstrap seed); no new user, no auth.
    ///
    /// `slug` is assumed already validated by [`validate_slug`] (the caller owns
    /// format checking); the DB's `slug` UNIQUE index is the last line of defence,
    /// surfacing a duplicate as [`WorkspaceRepoError::SlugTaken`] rather than a raw
    /// `sqlx` error.
    ///
    /// `issue_prefix` is stored verbatim (upper-cased) when `Some`; when `None` the
    /// column is left NULL — the SAME deliberate choice
    /// [`crate::bootstrap::ensure_default_workspace`] makes, because the
    /// `issue_prefix` column is overloaded as the TITLE prefix
    /// [`apply_issue_prefix`] prepends, so a defaulted prefix would mangle every new
    /// issue's title (`ACMfix the build`). The display-id prefix defaults to `HGR`
    /// at the render layer ([`issue_display_id`]) regardless. [`generate_issue_prefix`]
    /// is provided for a caller that wants an explicit derived prefix.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceRepoError::SlugTaken`] on the slug UNIQUE violation,
    /// [`WorkspaceRepoError::NotFound`] when no owner user exists yet (an
    /// un-bootstrapped DB), or [`WorkspaceRepoError::Db`] on any other store fault.
    pub async fn create(
        pool: &SqlitePool,
        slug: &str,
        name: &str,
        issue_prefix: Option<&str>,
    ) -> Result<WorkspaceRow, WorkspaceRepoError> {
        let owner_id = default_owner_id(pool).await?.ok_or(WorkspaceRepoError::NotFound)?;
        let id = SystemIdGen.new_ulid();
        let now = SystemClock.now_ms();
        let stored_prefix = issue_prefix.map(str::to_ascii_uppercase);

        let mut tx = pool.begin().await?;
        let insert_ws = sqlx::query(
            "INSERT INTO workspace (id, slug, name, created_at, issue_prefix) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(slug)
        .bind(name)
        .bind(now)
        .bind(stored_prefix.as_deref())
        .execute(&mut *tx)
        .await;
        if let Err(e) = insert_ws {
            let taken = e
                .as_database_error()
                .is_some_and(sqlx::error::DatabaseError::is_unique_violation);
            if taken {
                return Err(WorkspaceRepoError::SlugTaken);
            }
            return Err(e.into());
        }
        sqlx::query("INSERT INTO member (workspace_id, user_id, role) VALUES (?, ?, 'owner')")
            .bind(&id)
            .bind(&owner_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(WorkspaceRow {
            id,
            slug: slug.to_string(),
            name: name.to_string(),
        })
    }

    /// Delete a workspace and every workspace-scoped child row in one transaction
    /// (multica `DeleteWorkspace` parity).
    ///
    /// Multica leans on Postgres `ON DELETE CASCADE`; sqlite's workspace child FKs
    /// are bare `REFERENCES workspace(id)` (no cascade, and an applied migration
    /// cannot be `ALTER`ed to add one), so this performs an EXPLICIT teardown of
    /// every table that references the workspace (directly or through a
    /// workspace-scoped parent) inside a single transaction. `PRAGMA
    /// defer_foreign_keys = ON` defers FK enforcement to commit time, so the
    /// per-statement order cannot trip an immediate-FK check (e.g. the
    /// `agent_task_queue` self-reference) — only the final committed state must be
    /// consistent, which it is because every referencing row is removed.
    ///
    /// The shared owner `user` is NOT deleted: it is the bootstrap seed linked to
    /// the surviving default workspace via its own `member` row. Only THIS
    /// workspace's `member` rows go.
    ///
    /// Refuses to delete the LAST workspace ([`WorkspaceRepoError::LastWorkspace`])
    /// — the host must always have a tenant to stand in — and an unknown id
    /// ([`WorkspaceRepoError::NotFound`]).
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceRepoError::LastWorkspace`] when only one workspace
    /// exists, [`WorkspaceRepoError::NotFound`] for an unknown id, or
    /// [`WorkspaceRepoError::Db`] on a store fault.
    pub async fn delete(pool: &SqlitePool, id: &WorkspaceId) -> Result<(), WorkspaceRepoError> {
        let ws = id.as_str();
        let total: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM workspace").fetch_one(pool).await?;
        if total <= 1 {
            return Err(WorkspaceRepoError::LastWorkspace);
        }
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM workspace WHERE id = ?")
            .bind(ws)
            .fetch_optional(pool)
            .await?;
        if exists.is_none() {
            return Err(WorkspaceRepoError::NotFound);
        }

        let mut tx = pool.begin().await?;
        // Defer FK checks to COMMIT so intra-transaction statement order is
        // irrelevant; the committed state is consistent because every referencing
        // row below is deleted. `defer_foreign_keys` resets to OFF at commit.
        sqlx::query("PRAGMA defer_foreign_keys = ON").execute(&mut *tx).await?;
        // Children (and grandchildren) first, then the workspace. Every `?` binds
        // the same workspace id (see the per-statement bind loop below); the final
        // statement keys on `id` rather than `workspace_id`, but the bound value is
        // identical.
        for stmt in WORKSPACE_TEARDOWN {
            let mut q = sqlx::query(stmt);
            for _ in 0..stmt.matches('?').count() {
                q = q.bind(ws);
            }
            q.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Read `workspace`'s per-workspace config, or `None` if no such workspace
    /// exists (an unknown / foreign-tenant id).
    ///
    /// The stored `repo_whitelist` JSON array is parsed into a `Vec<String>`; a
    /// malformed JSON value (which the validating [`set_config`](Self::set_config)
    /// never writes) surfaces as a [`WorkspaceRepoError::BadWhitelist`].
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceRepoError::Db`] on a store fault, or
    /// [`WorkspaceRepoError::BadWhitelist`] if a stored whitelist value is not a
    /// JSON array of strings.
    pub async fn get_config(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
    ) -> Result<Option<WorkspaceConfig>, WorkspaceRepoError> {
        use sqlx::Row;
        let row = sqlx::query(
            "SELECT context_prompt, repo_whitelist, issue_prefix \
             FROM workspace WHERE id = ?",
        )
        .bind(workspace.as_str())
        .fetch_optional(pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let repo_whitelist = match row.try_get::<Option<String>, _>("repo_whitelist")? {
            Some(json) => Some(parse_whitelist(&json)?),
            None => None,
        };
        Ok(Some(WorkspaceConfig {
            context_prompt: row.try_get("context_prompt")?,
            repo_whitelist,
            issue_prefix: row.try_get("issue_prefix")?,
        }))
    }

    /// Overwrite `workspace`'s per-workspace config with `config`.
    ///
    /// Workspace-scoped: an unknown / foreign-tenant id matches no row and is
    /// rejected with [`WorkspaceRepoError::NotFound`] (never a cross-tenant
    /// write). The `repo_whitelist` is validated and serialised to a JSON array
    /// of strings; a `None` field writes SQL `NULL` ("not configured").
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceRepoError::NotFound`] when the workspace does not
    /// exist, [`WorkspaceRepoError::BadWhitelist`] when a whitelist entry is
    /// invalid, or [`WorkspaceRepoError::Db`] on a store fault.
    pub async fn set_config(
        pool: &SqlitePool,
        workspace: &WorkspaceId,
        config: &WorkspaceConfig,
    ) -> Result<(), WorkspaceRepoError> {
        let whitelist_json = match &config.repo_whitelist {
            Some(list) => Some(serialise_whitelist(list)?),
            None => None,
        };
        let affected = sqlx::query(
            "UPDATE workspace \
             SET context_prompt = ?, repo_whitelist = ?, issue_prefix = ? \
             WHERE id = ?",
        )
        .bind(config.context_prompt.as_deref())
        .bind(whitelist_json.as_deref())
        .bind(config.issue_prefix.as_deref())
        .bind(workspace.as_str())
        .execute(pool)
        .await?
        .rows_affected();
        if affected == 0 {
            return Err(WorkspaceRepoError::NotFound);
        }
        Ok(())
    }
}

/// The default issue-id prefix a workspace reads its issues under when it has
/// not configured an explicit one (63l.3): `HGR`, so a fresh workspace's issues
/// read `HGR-1`, `HGR-2`, ….
///
/// This default lives at the DISPLAY-ID layer ([`issue_display_id`]), NOT in the
/// stored `issue_prefix` column, and NOT as a value the workspace bootstrap
/// binds. The reasoning is the e38.21 column overload: `issue_prefix` is ALSO
/// the operator-set TITLE prefix that [`apply_issue_prefix`] prepends to a new
/// issue's title (`[OPS] fix the build`). Were the column defaulted to `HGR`,
/// every fresh issue's stored TITLE would become `HGRfix the build` — a bug, not
/// a display id. Keeping the column NULL-by-default leaves the title verbatim
/// (the e38.21 behaviour) while the display id still reads `HGR-<n>`: a NULL
/// column means "no explicit prefix", which [`issue_display_id`] renders with
/// this `HGR` fallback. An operator who configures `[OPS] ` gets both that title
/// prefix AND an `[OPS] -<n>` display id; one who clears it keeps a bare-ordinal
/// display id. This is the "None-default is cleaner" path 63l.3 invites.
pub const DEFAULT_ISSUE_PREFIX: &str = "HGR";

/// Prepend a workspace's `issue_prefix` to a newly-created issue `title`.
///
/// The single place the prefix is applied so the CLI issue-create
/// (`run_issue_create`) and the RPC issue-create (`snapshots::issue_create`)
/// agree byte-for-byte. `None` prefix returns the title verbatim; a present
/// prefix is prepended as-is (a trailing space, if wanted, is part of the
/// stored prefix — the prefix is used literally so the operator controls the
/// separator). Deliberately NOT defaulted to [`DEFAULT_ISSUE_PREFIX`]: that
/// default belongs to the display id ([`issue_display_id`]), not the title.
#[must_use]
pub fn apply_issue_prefix(prefix: Option<&str>, title: &str) -> String {
    match prefix {
        Some(p) if !p.is_empty() => format!("{p}{title}"),
        _ => title.to_string(),
    }
}

/// Format an issue's human-facing display id from a workspace `prefix` and the
/// issue's 1-based per-workspace `seq` number (63l.3).
///
/// A workspace with no explicit prefix (the NULL-column default) reads its
/// issues under [`DEFAULT_ISSUE_PREFIX`] (`HGR`), so its first issue is `HGR-1`,
/// its second `HGR-2`, and so on. An explicitly-configured prefix is used
/// verbatim before the `-<seq>` (`OPS-1`). The single place the display id is
/// assembled so the CLI, the RPC snapshot, and the plugin agree byte-for-byte.
///
/// `seq` is the issue's 1-based creation ordinal within its workspace (the
/// caller supplies it; the store derives it via [`IssueRepo::workspace_seq`]).
/// The `prefix` is trimmed of trailing whitespace before the join so a TITLE
/// prefix that carries a trailing separator (`[OPS] `) does not leak a stray
/// space into the display id (`[OPS]-1`, not `[OPS] -1`); the `HGR` default has
/// none to trim.
#[must_use]
pub fn issue_display_id(prefix: Option<&str>, seq: i64) -> String {
    let resolved = match prefix {
        Some(p) if !p.trim().is_empty() => p.trim_end(),
        // A NULL / blank column means "no explicit prefix" → the HGR default.
        _ => DEFAULT_ISSUE_PREFIX,
    };
    format!("{resolved}-{seq}")
}

/// Every table that references a workspace (directly via `workspace_id`, or
/// through a workspace-scoped parent), in child-before-parent teardown order.
///
/// Enumerated from the 17 `REFERENCES workspace` migrations plus their child
/// tables. `PRAGMA defer_foreign_keys = ON` (set in [`WorkspaceRepo::delete`])
/// makes the order non-load-bearing for correctness, but children-first keeps the
/// intent legible. Each `?` binds the workspace id. Grandchild tables that already
/// declare `ON DELETE CASCADE` off a workspace-scoped parent (`comment` → `issue`)
/// are torn down explicitly here too — belt and suspenders, and harmless.
const WORKSPACE_TEARDOWN: &[&str] = &[
    "DELETE FROM agent_skill WHERE agent_id IN (SELECT id FROM agent WHERE workspace_id = ?) \
       OR skill_id IN (SELECT id FROM skill WHERE workspace_id = ?)",
    "DELETE FROM skill_file WHERE skill_id IN (SELECT id FROM skill WHERE workspace_id = ?)",
    "DELETE FROM issue_label WHERE issue_id IN (SELECT id FROM issue WHERE workspace_id = ?) \
       OR label_id IN (SELECT id FROM label WHERE workspace_id = ?)",
    "DELETE FROM squad_member WHERE squad_id IN (SELECT id FROM squad WHERE workspace_id = ?)",
    "DELETE FROM comment WHERE issue_id IN (SELECT id FROM issue WHERE workspace_id = ?)",
    "DELETE FROM board_card WHERE board_id IN (SELECT id FROM board WHERE workspace_id = ?)",
    "DELETE FROM board_column WHERE board_id IN (SELECT id FROM board WHERE workspace_id = ?)",
    "DELETE FROM autopilot_webhook_delivery \
       WHERE autopilot_id IN (SELECT id FROM autopilot WHERE workspace_id = ?)",
    "DELETE FROM task_usage WHERE workspace_id = ?",
    "DELETE FROM run_history WHERE workspace_id = ?",
    "DELETE FROM agent_task_queue WHERE workspace_id = ?",
    "DELETE FROM autopilot_run WHERE autopilot_id IN (SELECT id FROM autopilot WHERE workspace_id = ?)",
    "DELETE FROM autopilot WHERE workspace_id = ?",
    "DELETE FROM daemon_token \
       WHERE runtime_id IN (SELECT id FROM agent_runtime WHERE workspace_id = ?)",
    "DELETE FROM issue WHERE workspace_id = ?",
    "DELETE FROM label WHERE workspace_id = ?",
    "DELETE FROM squad WHERE workspace_id = ?",
    "DELETE FROM board WHERE workspace_id = ?",
    "DELETE FROM card_dependency WHERE workspace_id = ?",
    "DELETE FROM inbox_entry WHERE workspace_id = ?",
    "DELETE FROM attention WHERE workspace_id = ?",
    "DELETE FROM event_log WHERE workspace_id = ?",
    "DELETE FROM notify_rule WHERE workspace_id = ?",
    "DELETE FROM skill WHERE workspace_id = ?",
    "DELETE FROM agent WHERE workspace_id = ?",
    "DELETE FROM agent_runtime WHERE workspace_id = ?",
    "DELETE FROM member WHERE workspace_id = ?",
    "DELETE FROM workspace WHERE id = ?",
];

/// Validate + normalise a caller-supplied workspace slug against multica's
/// `^[a-z0-9]+(-[a-z0-9]+)*$` rule (`workspace.go` slug guard).
///
/// Trims surrounding whitespace, then accepts only lowercase ASCII letters,
/// digits, and internal single hyphens (no leading/trailing/doubled hyphen).
/// Upper-case input is REJECTED (not silently down-cased) so a slug is exactly
/// what the operator typed — the multica frontend routes on `/{slug}/...`, so a
/// surprising auto-mangle would point at the wrong route. Returns the trimmed
/// slug on success.
///
/// # Errors
///
/// Returns [`WorkspaceRepoError::BadSlug`] for an empty slug or one containing any
/// character outside `[a-z0-9-]`, or with a leading/trailing/doubled hyphen.
pub fn validate_slug(raw: &str) -> Result<String, WorkspaceRepoError> {
    let s = raw.trim();
    let bad = |detail: &str| WorkspaceRepoError::BadSlug {
        detail: detail.to_string(),
    };
    if s.is_empty() {
        return Err(bad("slug must not be empty"));
    }
    if s.starts_with('-') || s.ends_with('-') || s.contains("--") {
        return Err(bad(
            "slug must not begin or end with a hyphen or contain a doubled hyphen",
        ));
    }
    if !s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(bad(
            "slug must contain only lowercase letters, numbers, and hyphens",
        ));
    }
    Ok(s.to_string())
}

/// Derive a default issue prefix from a workspace name (multica `generateIssuePrefix`).
///
/// Strips non-alphabetic characters, upper-cases, takes the first three, falling
/// back to `"WS"` when nothing alphabetic remains. Examples: `"My Team"` → `MYT`,
/// `"AB"` → `AB`, `"123"` → `WS`.
#[must_use]
pub fn generate_issue_prefix(name: &str) -> String {
    let alpha: String = name.chars().filter(char::is_ascii_alphabetic).collect();
    if alpha.is_empty() {
        return "WS".to_string();
    }
    alpha.to_ascii_uppercase().chars().take(3).collect()
}

/// Parse a stored `repo_whitelist` JSON value into a `Vec<String>`, rejecting
/// anything that is not a JSON array of strings.
fn parse_whitelist(json: &str) -> Result<Vec<String>, WorkspaceRepoError> {
    serde_json::from_str::<Vec<String>>(json).map_err(|e| WorkspaceRepoError::BadWhitelist {
        detail: format!("repo_whitelist must be a JSON array of strings: {e}"),
    })
}

/// Validate + serialise a whitelist to a JSON array of strings.
///
/// Rejects a blank repo slug (a configured-but-empty entry is a config error,
/// not an allow-everything sentinel — that is `None`).
fn serialise_whitelist(list: &[String]) -> Result<String, WorkspaceRepoError> {
    if list.iter().any(|r| r.trim().is_empty()) {
        return Err(WorkspaceRepoError::BadWhitelist {
            detail: "repo_whitelist entries must be non-empty".to_string(),
        });
    }
    serde_json::to_string(list).map_err(|e| WorkspaceRepoError::BadWhitelist {
        detail: format!("serialise repo_whitelist: {e}"),
    })
}

/// Error surface for [`WorkspaceRepo`].
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceRepoError {
    /// The target workspace does not exist (an unknown / foreign-tenant id). The
    /// mutation is rejected, nothing written.
    #[error("workspace not found")]
    NotFound,
    /// A `repo_whitelist` value is not a valid JSON array of non-empty strings.
    #[error("invalid repo whitelist: {detail}")]
    BadWhitelist {
        /// The validation failure detail.
        detail: String,
    },
    /// A create slug failed the `^[a-z0-9]+(-[a-z0-9]+)*$` format guard.
    #[error("invalid slug: {detail}")]
    BadSlug {
        /// The validation failure detail.
        detail: String,
    },
    /// A create hit the `workspace.slug` UNIQUE index — the slug is already taken.
    #[error("a workspace with that slug already exists")]
    SlugTaken,
    /// A delete would remove the last remaining workspace, which is refused (the
    /// host must always have a tenant to stand in).
    #[error("cannot delete the last workspace")]
    LastWorkspace,
    /// An underlying `sqlx` failure (IO, decode, …).
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    async fn seed_ws(pool: &SqlitePool, ws: &str) {
        sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
            .bind(ws)
            .bind(ws)
            .bind(ws)
            .bind(1_000_i64)
            .execute(pool)
            .await
            .unwrap();
    }

    fn ws(id: &str) -> WorkspaceId {
        WorkspaceId::from_str(id.to_string()).unwrap()
    }

    /// A freshly-seeded workspace reads back the all-`None` default config.
    #[tokio::test]
    async fn get_config_defaults_to_none_for_every_field() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        seed_ws(store.pool(), "ws-a").await;

        let cfg = WorkspaceRepo::get_config(store.pool(), &ws("ws-a")).await.unwrap().unwrap();
        assert_eq!(cfg, WorkspaceConfig::default());
        assert_eq!(cfg.context_prompt, None);
        assert_eq!(cfg.repo_whitelist, None);
        assert_eq!(cfg.issue_prefix, None);
    }

    /// An unknown workspace reads as `None`, never an error.
    #[tokio::test]
    async fn get_config_unknown_workspace_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let cfg = WorkspaceRepo::get_config(store.pool(), &ws("nope")).await.unwrap();
        assert!(cfg.is_none());
    }

    /// `set_config` round-trips every field, including a JSON whitelist.
    #[tokio::test]
    async fn set_config_round_trips_all_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        let cfg = WorkspaceConfig {
            context_prompt: Some("Always run cargo fmt.".to_string()),
            repo_whitelist: Some(vec!["org/api".to_string(), "org/web".to_string()]),
            issue_prefix: Some("[OPS] ".to_string()),
        };
        WorkspaceRepo::set_config(pool, &ws("ws-a"), &cfg).await.unwrap();

        let read = WorkspaceRepo::get_config(pool, &ws("ws-a")).await.unwrap().unwrap();
        assert_eq!(read, cfg, "config round-trips through the store");
    }

    /// Setting config on an unknown workspace is rejected, never a silent no-op.
    #[tokio::test]
    async fn set_config_unknown_workspace_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let err = WorkspaceRepo::set_config(store.pool(), &ws("nope"), &WorkspaceConfig::default())
            .await
            .unwrap_err();
        assert!(matches!(err, WorkspaceRepoError::NotFound), "got {err:?}");
    }

    /// A blank whitelist entry is rejected at write (a config error, not the
    /// allow-everything sentinel — that is `None`).
    #[tokio::test]
    async fn set_config_rejects_a_blank_whitelist_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        seed_ws(pool, "ws-a").await;

        let cfg = WorkspaceConfig {
            repo_whitelist: Some(vec!["org/api".to_string(), "  ".to_string()]),
            ..WorkspaceConfig::default()
        };
        let err = WorkspaceRepo::set_config(pool, &ws("ws-a"), &cfg).await.unwrap_err();
        assert!(
            matches!(err, WorkspaceRepoError::BadWhitelist { .. }),
            "got {err:?}"
        );
    }

    /// The whitelist gate allows everything when unset, and gates to the list
    /// when set.
    #[test]
    fn repo_allowed_gates_on_the_whitelist() {
        let no_gate = WorkspaceConfig::default();
        assert!(
            no_gate.repo_allowed("org/anything"),
            "no whitelist = allow all"
        );

        let gated = WorkspaceConfig {
            repo_whitelist: Some(vec!["org/api".to_string()]),
            ..WorkspaceConfig::default()
        };
        assert!(gated.repo_allowed("org/api"), "listed repo allowed");
        assert!(!gated.repo_allowed("org/web"), "unlisted repo gated");

        let empty = WorkspaceConfig {
            repo_whitelist: Some(Vec::new()),
            ..WorkspaceConfig::default()
        };
        assert!(
            !empty.repo_allowed("org/api"),
            "empty whitelist allows nothing"
        );
    }

    /// `apply_issue_prefix` prepends a present prefix and is a no-op for `None`
    /// / empty.
    #[test]
    fn apply_issue_prefix_prepends_when_set() {
        assert_eq!(apply_issue_prefix(None, "fix the build"), "fix the build");
        assert_eq!(
            apply_issue_prefix(Some(""), "fix the build"),
            "fix the build"
        );
        assert_eq!(
            apply_issue_prefix(Some("[OPS] "), "fix the build"),
            "[OPS] fix the build"
        );
    }

    /// A workspace with no explicit prefix reads its issues under the `HGR`
    /// default at the display layer — without storing `HGR` in the column
    /// (so a fresh issue's TITLE is left verbatim, 63l.3).
    #[test]
    fn default_issue_prefix_is_hgr_at_the_display_layer() {
        assert_eq!(DEFAULT_ISSUE_PREFIX, "HGR");
        // A None / blank column (the fresh-workspace default) reads HGR-<n>.
        assert_eq!(issue_display_id(None, 1), "HGR-1");
        assert_eq!(issue_display_id(None, 42), "HGR-42");
        assert_eq!(
            issue_display_id(Some(""), 7),
            "HGR-7",
            "blank prefix -> HGR"
        );
        assert_eq!(
            issue_display_id(Some("   "), 7),
            "HGR-7",
            "whitespace -> HGR"
        );
        // The default lives in the display helper, NOT the title prepend: an
        // unset prefix leaves the title verbatim (no `HGRtitle` mangling).
        assert_eq!(apply_issue_prefix(None, "fix the build"), "fix the build");
    }

    /// `issue_display_id` joins an explicit prefix + ordinal with a dash, trimming
    /// a trailing separator so a TITLE prefix like `[OPS] ` does not leak a stray
    /// space into the display id.
    #[test]
    fn issue_display_id_joins_explicit_prefix_and_ordinal() {
        assert_eq!(issue_display_id(Some("OPS"), 12), "OPS-12");
        assert_eq!(
            issue_display_id(Some("[OPS] "), 3),
            "[OPS]-3",
            "trailing separator trimmed from the display id"
        );
    }

    // ---- create / delete (multica gap #4) ----

    use crate::bootstrap::ensure_default_workspace;

    async fn ws_slugs(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar("SELECT slug FROM workspace ORDER BY created_at")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// Insert a bare issue into `workspace_id`, returning its id.
    async fn seed_issue(pool: &SqlitePool, workspace_id: &str, title: &str) -> String {
        let id = SystemIdGen.new_ulid();
        let owner = default_owner_id(pool).await.unwrap().unwrap();
        sqlx::query(
            "INSERT INTO issue (id, workspace_id, title, creator_type, creator_id, created_at) \
             VALUES (?, ?, ?, 'member', ?, ?)",
        )
        .bind(&id)
        .bind(workspace_id)
        .bind(title)
        .bind(&owner)
        .bind(1_000_i64)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    /// Insert a runtime + agent into `workspace_id`, returning the agent id.
    async fn seed_agent(pool: &SqlitePool, workspace_id: &str) -> String {
        let owner = default_owner_id(pool).await.unwrap().unwrap();
        let runtime_id = SystemIdGen.new_ulid();
        sqlx::query(
            "INSERT INTO agent_runtime \
             (id, workspace_id, daemon_id, provider, runtime_mode, status) \
             VALUES (?, ?, 'd', 'claude', 'local', 'online')",
        )
        .bind(&runtime_id)
        .bind(workspace_id)
        .execute(pool)
        .await
        .unwrap();
        let agent_id = SystemIdGen.new_ulid();
        sqlx::query(
            "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
             VALUES (?, ?, 'a', ?, 'workspace', ?)",
        )
        .bind(&agent_id)
        .bind(workspace_id)
        .bind(&runtime_id)
        .bind(&owner)
        .execute(pool)
        .await
        .unwrap();
        agent_id
    }

    async fn count(pool: &SqlitePool, sql: &str, bind: &str) -> i64 {
        sqlx::query_scalar(sql).bind(bind).fetch_one(pool).await.unwrap()
    }

    /// Create round-trips: the new workspace appears in the list beside the
    /// default, and an owner `member` row is linked to it.
    #[tokio::test]
    async fn create_round_trips_and_links_owner() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        ensure_default_workspace(pool).await.unwrap();

        let row = WorkspaceRepo::create(pool, "acme", "Acme", None).await.unwrap();
        assert_eq!(row.slug, "acme");
        assert_eq!(row.name, "Acme");
        assert!(!row.id.is_empty());

        assert_eq!(ws_slugs(pool).await, vec!["default", "acme"]);
        // One owner member per workspace (default's + acme's) sharing the one user.
        let owners: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM member WHERE role = 'owner'")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(owners, 2);
        let users: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user").fetch_one(pool).await.unwrap();
        assert_eq!(users, 1, "the owner user is shared, not duplicated");
    }

    /// A stored explicit `issue_prefix` is upper-cased; `None` leaves the column
    /// NULL (no title mangling — the bootstrap invariant).
    #[tokio::test]
    async fn create_issue_prefix_is_upper_or_null() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        ensure_default_workspace(pool).await.unwrap();

        let a = WorkspaceRepo::create(pool, "acme", "Acme", Some("ops")).await.unwrap();
        let b = WorkspaceRepo::create(pool, "beta", "Beta", None).await.unwrap();
        let pa: Option<String> =
            sqlx::query_scalar("SELECT issue_prefix FROM workspace WHERE id = ?")
                .bind(&a.id)
                .fetch_one(pool)
                .await
                .unwrap();
        let pb: Option<String> =
            sqlx::query_scalar("SELECT issue_prefix FROM workspace WHERE id = ?")
                .bind(&b.id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!(pa.as_deref(), Some("OPS"));
        assert_eq!(pb, None);
    }

    /// A duplicate slug is rejected by the UNIQUE index as `SlugTaken`.
    #[tokio::test]
    async fn create_duplicate_slug_is_slug_taken() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        ensure_default_workspace(pool).await.unwrap();

        WorkspaceRepo::create(pool, "acme", "Acme", None).await.unwrap();
        let err = WorkspaceRepo::create(pool, "acme", "Acme II", None).await.unwrap_err();
        assert!(matches!(err, WorkspaceRepoError::SlugTaken), "got {err:?}");
        assert_eq!(
            ws_slugs(pool).await,
            vec!["default", "acme"],
            "no phantom row"
        );
    }

    /// `validate_slug` accepts lower-alnum + internal hyphens, rejects everything
    /// else (space, upper, empty, edge hyphens).
    #[test]
    fn validate_slug_matches_multica_pattern() {
        assert_eq!(validate_slug("acme").unwrap(), "acme");
        assert_eq!(validate_slug("  acme-corp ").unwrap(), "acme-corp");
        assert_eq!(validate_slug("ws1").unwrap(), "ws1");
        for bad in [
            "Bad Slug",
            "UPPER",
            "",
            "  ",
            "-lead",
            "trail-",
            "a--b",
            "under_score",
        ] {
            assert!(
                matches!(validate_slug(bad), Err(WorkspaceRepoError::BadSlug { .. })),
                "{bad:?} must be BadSlug"
            );
        }
    }

    /// `generate_issue_prefix` mirrors multica's derive rule.
    #[test]
    fn generate_issue_prefix_cases() {
        assert_eq!(generate_issue_prefix("My Team"), "MYT");
        assert_eq!(generate_issue_prefix("AB"), "AB");
        assert_eq!(generate_issue_prefix("123"), "WS");
        assert_eq!(generate_issue_prefix(""), "WS");
        assert_eq!(generate_issue_prefix("a1b2c3"), "ABC");
    }

    /// Delete removes the workspace AND its issues/agents/runtime/member, while the
    /// sibling default workspace's rows survive (isolation proof).
    #[tokio::test]
    async fn delete_tears_down_children_and_spares_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let default_id = ensure_default_workspace(pool).await.unwrap();
        let acme = WorkspaceRepo::create(pool, "acme", "Acme", None).await.unwrap();

        let default_issue = seed_issue(pool, &default_id, "DEFAULT sentinel").await;
        let acme_issue = seed_issue(pool, &acme.id, "ACME only").await;
        let acme_agent = seed_agent(pool, &acme.id).await;

        WorkspaceRepo::delete(pool, &ws(&acme.id)).await.unwrap();

        // acme + every child gone.
        assert_eq!(ws_slugs(pool).await, vec!["default"]);
        assert_eq!(
            count(pool, "SELECT COUNT(*) FROM issue WHERE id = ?", &acme_issue).await,
            0
        );
        assert_eq!(
            count(pool, "SELECT COUNT(*) FROM agent WHERE id = ?", &acme_agent).await,
            0
        );
        assert_eq!(
            count(
                pool,
                "SELECT COUNT(*) FROM agent_runtime WHERE workspace_id = ?",
                &acme.id
            )
            .await,
            0
        );
        assert_eq!(
            count(
                pool,
                "SELECT COUNT(*) FROM member WHERE workspace_id = ?",
                &acme.id
            )
            .await,
            0
        );
        // Sibling default survives, owner user survives.
        assert_eq!(
            count(
                pool,
                "SELECT COUNT(*) FROM issue WHERE id = ?",
                &default_issue
            )
            .await,
            1,
            "the sibling workspace's issue must survive"
        );
        let users: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM user").fetch_one(pool).await.unwrap();
        assert_eq!(users, 1, "the shared owner user is never deleted");
    }

    /// Deleting the last workspace is refused.
    #[tokio::test]
    async fn delete_last_workspace_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        let only = ensure_default_workspace(pool).await.unwrap();
        let err = WorkspaceRepo::delete(pool, &ws(&only)).await.unwrap_err();
        assert!(
            matches!(err, WorkspaceRepoError::LastWorkspace),
            "got {err:?}"
        );
        assert_eq!(ws_slugs(pool).await, vec!["default"], "nothing removed");
    }

    /// Deleting an unknown id is `NotFound` (never a silent no-op).
    #[tokio::test]
    async fn delete_unknown_workspace_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open_in(dir.path()).await.unwrap();
        let pool = store.pool();
        ensure_default_workspace(pool).await.unwrap();
        WorkspaceRepo::create(pool, "acme", "Acme", None).await.unwrap();
        let err = WorkspaceRepo::delete(pool, &ws("01NONEXISTENT")).await.unwrap_err();
        assert!(matches!(err, WorkspaceRepoError::NotFound), "got {err:?}");
    }
}
