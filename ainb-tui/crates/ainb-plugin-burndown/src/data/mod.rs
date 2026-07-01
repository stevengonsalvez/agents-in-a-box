//! Burndown data layer — usage parsing + project resolution.
//!
//! Lifted verbatim from `ainb-core::models::{usage, repo_lookup}` with
//! import paths rewired to the plugin-local module tree. The data
//! semantics (UsageData, ProviderCall, UsagePeriod, etc.) are
//! unchanged from the pre-Phase-3 core implementation.

pub mod repo_lookup;
pub mod savings;
pub mod usage;

// Re-export the surface that `crate::models::*` provided in core, so
// downstream Phase 3 moves (UI, CLI) can keep their `crate::data::X`
// or `crate::data::usage::X` import paths working.
pub use usage::*;
