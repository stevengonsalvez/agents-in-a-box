//! Session-reader plugin entry point — runs the SDK Server over stdio.
//!
//! Phase 7c: subprocess + ABI v2. The plugin walks per-provider session
//! logs (Claude/Codex/Gemini/Copilot) directly via [`std::fs`] (the host
//! enforces the `read_claude_logs` / `read_codex_logs` capabilities at
//! manifest-grant time, not at every fs read), aggregates the resulting
//! calls into a [`UsageData`] snapshot, and publishes the snapshot under
//! the `sessions.usage_data` topic for downstream consumers (burndown).
//!
//! [`UsageData`]: ainb_plugin_types_sessions::UsageData

#![forbid(unsafe_code)]

use ainb_plugin_sdk::{SdkError, Server};

#[cfg(not(target_arch = "wasm32"))]
mod cache;
mod config;
mod fnv;
mod parsers;
mod plugin;
mod scanner;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), SdkError> {
    Server::new(plugin::SessionReader::new()).run_stdio().await
}
