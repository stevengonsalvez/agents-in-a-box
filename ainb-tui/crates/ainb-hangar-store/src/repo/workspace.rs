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

use ainb_hangar_core::ids::WorkspaceId;
use sqlx::SqlitePool;

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

/// Stateless typed wrapper over the `workspace` table's config columns.
pub struct WorkspaceRepo;

impl WorkspaceRepo {
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
}
