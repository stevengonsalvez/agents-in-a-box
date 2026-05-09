//! ainb session-reader plugin.
//!
//! Phase 6b data-plane plugin. Walks Claude / Codex / Gemini / Copilot
//! per-provider session logs (via host fns gated by the
//! `read_claude_logs` / `read_codex_logs` capabilities), aggregates
//! the resulting calls into a [`UsageData`] snapshot, and publishes
//! that snapshot on the `sessions.usage_data` topic.
//!
//! The plugin has no UI surface and no CLI namespace — it produces
//! pure data for downstream plugins (burndown, future kanban, etc.).
//!
//! ## Module map
//!
//! * [`abi`]      — `_init` / `_render` / `_handle_event` / `_shutdown`
//!                  / `_alloc` exports the host calls into.
//! * [`host`]     — safe Rust wrappers around the unsafe `extern "C"`
//!                  imports declared in `ainb-plugin-api::host`.
//! * [`parsers`]  — per-provider JSONL parsers (Claude, Codex, Gemini,
//!                  Copilot). Each returns
//!                  `Result<Vec<ProviderCall>, ParserError>` so a
//!                  single broken provider degrades cleanly without
//!                  taking the snapshot down with it.
//! * [`scanner`]  — orchestrates per-provider walks, aggregates into
//!                  `UsageData`, applies the sort/dedupe rules
//!                  consumers (burndown) rely on for byte-stable
//!                  output.
//!
//! [`UsageData`]: ainb_plugin_types_sessions::UsageData

#![allow(clippy::missing_safety_doc)]
#![allow(dead_code)] // Phase 6b in flight — wired up incrementally.
// `_init` / `_alloc` use `static mut` + raw-pointer ABI shims (the wasmi
// host treats every cross-boundary pointer as i32). `unsafe` is the
// only way to write that — wrapping inside the smallest possible
// blocks. The plugin runs single-threaded inside its own wasmi store,
// so the static-mut access is sound.

mod abi;
mod fnv;
mod host;
mod parsers;
mod scanner;

/// Host ABI version this plugin targets. Phase 6 bumped to 1.2.0
/// (introduces fs_read_dir/file, cache_get/put, request_data).
#[no_mangle]
pub static AINB_PLUGIN_HOST_VERSION: [u32; 3] = [1, 2, 0];
