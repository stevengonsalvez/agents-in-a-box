//! ainb burndown plugin — daily/weekly/project usage analytics.
//!
//! Phase 3 scaffold: empty cdylib with the four extern "C" entrypoints
//! the host expects (per ainb-plugin-api host_version 1.1.0). Source
//! moves (data/cache/ui/cli) and the Plugin trait impl land in
//! follow-up commits per the Phase 3 atomic-commit plan.

#![allow(clippy::missing_safety_doc)]
#![allow(dead_code)] // Phase 3 in flight — large surface area lifted from core

mod abi;
pub mod cache;
pub mod cli;
pub mod config;
pub mod data;
pub mod live_window;
pub mod output_format;
pub mod ui;
mod ui_helpers;

/// Host ABI version this plugin targets, exposed as a `[u32; 3]` so the
/// host can read it directly via `Memory::read` without going through
/// serde / utf-8 decoding.
///
/// Phase 6c bump: 1.1.0 → 1.2.0. Matches the manifest's
/// `ainb_min_version` and reflects burndown's dependency on the host's
/// auto-subscribe loader (host 1.2.0 reads `[subscribes].topics`) and
/// the opaque-bytes Custom event payload (so msgpack survives the bus).
#[no_mangle]
pub static AINB_PLUGIN_HOST_VERSION: [u32; 3] = [1, 2, 0];
