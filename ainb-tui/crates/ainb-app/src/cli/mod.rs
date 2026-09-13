// ABOUTME: The renderer-agnostic half of the CLI: statusline caches, dependency
// probes, update checks and session lookup that the service layer calls into.
// Command routing and every other subcommand stay in `ainb-core`, which
// re-exports this module from its own `cli`.

pub mod codex_statusline;
pub mod deps;
pub mod hangar;
pub mod statusline;
pub mod update;
pub mod util;

use clap::ValueEnum;

/// Output format for commands
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Csv,
    #[value(alias = "md")]
    Markdown,
}
