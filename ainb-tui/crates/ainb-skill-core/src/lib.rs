//! ainb-core — business logic for the unit manager.
//!
//! Owns the unit URI grammar, manifest + lockfile types, and path
//! resolution for the on-disk state under `$AINB_HOME`. Higher layers
//! (`ainb-cli`, `ainb-fetch`, adapters) compose these primitives.

pub mod catalog;
pub mod catalog_index;
pub mod drift;
pub mod error;
pub mod kind;
pub mod library;
pub mod lockfile;
pub mod manifest;
pub mod mapping;
pub mod paths;
pub mod sync;
pub mod uri;

pub use catalog::{
    CatalogBackend, CatalogEntryKind, CatalogError, CatalogHit, SKILLS_SH_DEFAULT_BASE,
    SkillsShUrlBuilder, is_blank_query, rank_by_stars,
};
pub use drift::{DriftBackend, DriftStatus, GitLsRemoteBackend, detect_all, detect_drift};
pub use error::CoreError;
pub use kind::UnitKind;
pub use library::{
    LIBRARY_SCHEMA_VERSION, Library, OwnedUnit, default_library_path, library_path_in,
};
pub use lockfile::{
    DeployedRef, LOCKFILE_SCHEMA_VERSION, LockedSource, LockedUnit, Lockfile, UsageRecord,
};
pub use manifest::{
    Defaults, Manifest, Options, SourceEntry, SourceKind, TargetMapping, UnitEntry,
};
pub use mapping::{
    BOOTSTRAP_DEFAULT_MAPPINGS, bootstrap_default_mappings, resolve_pair, strip_tool_dotdir,
};
pub use paths::{
    default_ainb_home, default_cache_dir, default_lockfile_path, default_manifest_path,
};
pub use sync::{
    ApplyToRepoOpts, ContentFetcher, FetchError as SyncFetchError, SYNC_SKIP_PUSH_ENV,
    SideSnapshot, SyncAction, SyncDirection, SyncEngineError, UnitSnapshot, apply_to_home,
    apply_to_repo, plan_sync,
};
pub use uri::{MarketplaceUri, SourceType, Uri};

// Test-fixture builder. Gated so the production binary never links
// the fixture seeding code or the bytes of seeded SKILL.md content.
// Enabled by integration tests via `--features test-fixtures`.
#[cfg(feature = "test-fixtures")]
pub mod fixtures;
#[cfg(feature = "test-fixtures")]
pub use fixtures::{
    OWN_SKILL_NAME, SANDBOX_MARKER_FILE, SandboxLayout, SandboxTier, build_skill_manager_sandbox,
};
