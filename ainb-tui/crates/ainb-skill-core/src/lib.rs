//! ainb-core — business logic for the unit manager.
//!
//! Owns the unit URI grammar, manifest + lockfile types, and path
//! resolution for the on-disk state under `$AINB_HOME`. Higher layers
//! (`ainb-cli`, `ainb-fetch`, adapters) compose these primitives.

pub mod drift;
pub mod error;
pub mod kind;
pub mod lockfile;
pub mod manifest;
pub mod mapping;
pub mod paths;
pub mod sync;
pub mod uri;

pub use drift::{detect_all, detect_drift, DriftBackend, DriftStatus, GitLsRemoteBackend};
pub use error::CoreError;
pub use kind::UnitKind;
pub use lockfile::{
    DeployedRef, LockedSource, LockedUnit, Lockfile, UsageRecord, LOCKFILE_SCHEMA_VERSION,
};
pub use manifest::{
    Defaults, Manifest, Options, SourceEntry, SourceKind, TargetMapping, UnitEntry,
};
pub use mapping::{
    bootstrap_default_mappings, resolve_pair, strip_tool_dotdir, BOOTSTRAP_DEFAULT_MAPPINGS,
};
pub use paths::{
    default_ainb_home, default_cache_dir, default_lockfile_path, default_manifest_path,
};
pub use sync::{
    apply_to_home, apply_to_repo, plan_sync, ApplyToRepoOpts, ContentFetcher,
    FetchError as SyncFetchError, SideSnapshot, SyncAction, SyncDirection, SyncEngineError,
    UnitSnapshot, SYNC_SKIP_PUSH_ENV,
};
pub use uri::{MarketplaceUri, SourceType, Uri};
