//! ainb learnings plugin — browse, search & graph the reflect knowledge
//! base.
//!
//! Subprocess plugin built against `ainb-plugin-sdk-rust`. `src/main.rs`
//! runs `Server::new(LearningsPlugin::default()).run_stdio()`; the
//! [`plugin`] module wires the render shell onto the SDK's `Plugin`
//! trait, [`config`] owns the typed config resolved from the injected
//! `[plugins.learnings]` table, and [`data`] (P4) is the hybrid data
//! layer — fs reader for the `.md`+`.entities.yaml` notes, a GraphML +
//! community-json reader, and a `qmd` shell behind a trait for semantic
//! search. The Browse / Detail / Search / Graph tabs (P5–P8) consume
//! [`data`] in later phases.

pub mod config;
pub mod data;
pub mod plugin;

pub use config::LearningsConfig;
pub use plugin::{LearningsPlugin, TITLE_TOKEN};
