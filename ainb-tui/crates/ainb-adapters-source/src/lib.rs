//! ainb-adapters-source — per-source-type adapters.
//!
//! Each adapter implements [`SourceAdapter`] and reports whether a
//! fetched checkout matches its layout via [`SourceAdapter::detect`].
//! The CLI runs detection in priority order (marketplace → manifest →
//! raw → single), first match wins.

use std::path::Path;

pub mod frontmatter;
pub mod manifest;
pub mod marketplace;
pub mod raw;
pub mod single;
pub mod types;
pub mod walk;

pub use manifest::ManifestAdapter;
pub use marketplace::MarketplaceAdapter;
pub use raw::RawAdapter;
pub use single::SingleAdapter;
pub use types::{ResolvedUnit, UnitDescriptor};

/// Detection priority for source adapters (first match wins).
/// Spec §7.3 — `marketplace` > `manifest` > `raw` > `single`.
pub fn all_adapters() -> Vec<Box<dyn SourceAdapter>> {
    vec![
        Box::new(MarketplaceAdapter::new()),
        Box::new(ManifestAdapter::new()),
        Box::new(RawAdapter::new()),
        Box::new(SingleAdapter::new()),
    ]
}

/// Pick the first adapter whose `detect` matches `fetched_root`.
pub fn pick_adapter(fetched_root: &Path) -> Option<Box<dyn SourceAdapter>> {
    all_adapters().into_iter().find(|a| a.detect(fetched_root))
}

/// Source-adapter contract (spec §7.3).
pub trait SourceAdapter: Send + Sync {
    /// Stable identifier — `marketplace` / `manifest` / `raw` / `single`.
    fn name(&self) -> &'static str;

    /// Cheap structural check: would `list_units` find anything here?
    fn detect(&self, fetched_root: &Path) -> bool;

    /// Enumerate every unit available in this fetched source.
    fn list_units(&self, fetched_root: &Path) -> anyhow::Result<Vec<UnitDescriptor>>;

    /// Fully resolve one unit by its relative `path`. P3+ uses this to
    /// drive the install diff pipeline; P2 only needs a minimal stub.
    fn resolve_unit(
        &self,
        fetched_root: &Path,
        path: &str,
    ) -> anyhow::Result<ResolvedUnit>;
}
