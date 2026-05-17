//! ainb burndown plugin — daily/weekly/project usage analytics.
//!
//! Phase 7c: subprocess plugin built against `ainb-plugin-sdk-rust`.
//! `src/main.rs` runs `Server::new(BurndownPlugin::default()).run_stdio()`;
//! the modules below own the analytics domain (cache + report
//! generation + CLI) and the [`plugin`] module wires them onto the
//! SDK's `Plugin` trait.

#![allow(dead_code)] // Lifted analytics surface — many entry points used only by CLI subcommands.
// The cli/ui/data modules are a 1:1 port of the Phase 6c-cli host-side
// burndown that ran inside ainb-tui. Style nits live with the original
// code; the migration's value is the new subprocess shell, not a
// refactor pass on 12k lines of legacy analytics. Re-tightening these
// lints crate-by-crate is out of scope for Phase 7c.
#![allow(
    clippy::large_enum_variant,
    clippy::derivable_impls,
    clippy::manual_map,
    clippy::map_unwrap_or,
    clippy::unnecessary_map_or,
    clippy::unnecessary_sort_by,
    clippy::ptr_arg,
    clippy::manual_pattern_char_comparison,
    clippy::unnecessary_unwrap,
    clippy::type_complexity
)]

pub mod cache;
pub mod cli;
pub mod config;
pub mod data;
pub mod live_window;
pub mod output_format;
pub mod plugin;
pub mod ui;
mod ui_helpers;
pub mod wire;

#[cfg(test)]
pub(crate) mod test_support;
