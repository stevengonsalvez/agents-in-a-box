//! ainb learnings plugin — browse, search & graph the reflect knowledge
//! base.
//!
//! P3 (scaffold): subprocess plugin built against `ainb-plugin-sdk-rust`.
//! `src/main.rs` runs `Server::new(LearningsPlugin::default()).run_stdio()`;
//! the [`plugin`] module wires the empty-state shell onto the SDK's
//! `Plugin` trait and [`config`] owns the typed config resolved from the
//! injected `[plugins.learnings]` table. The data layer (P4) and the
//! Browse / Detail / Search / Graph tabs (P5–P8) hang off this crate in
//! later phases.

pub mod config;
pub mod plugin;

pub use config::LearningsConfig;
pub use plugin::{LearningsPlugin, TITLE_TOKEN};
