//! ainb abtop plugin — surfaces "top for AI agents" inside ainb's TUI
//! by wrapping the external `abtop` CLI.
//!
//! Architecture is a thin wrapper, modelled on `ainb-plugin-witr`: the
//! host spawns this crate's binary (`ainb-plugin-abtop`), the binary
//! speaks JSON-RPC 2.0 over Content-Length-framed stdio via
//! `ainb-plugin-sdk-rust`, and on demand exec's the external
//! `abtop --once` binary.
//!
//! Unlike witr, abtop has **no machine-readable mode** (no `--json`, no
//! library crate) — its `--once` output is a human-readable text
//! snapshot. So this crate does *not* parse abtop output into native
//! data tabs. Its job is:
//!
//! 1. **Detect** abtop on PATH (present/absent only — no version gate).
//! 2. **Empty-state** — a platform-aware install hint when abtop is
//!    missing.
//! 3. **CLI seam** — `ainb abtop` execs `abtop --once` and prints the
//!    raw text.
//!
//! The live full-screen agent view is a host-side tmux attach wired in
//! later phases; this plugin mirrors witr's structure for discoverability
//! plus the empty-state plus the `cli_dispatch` seam.

pub mod cli;
pub mod detect;
pub mod exec;
pub mod plugin;
pub mod render;
pub mod state;
