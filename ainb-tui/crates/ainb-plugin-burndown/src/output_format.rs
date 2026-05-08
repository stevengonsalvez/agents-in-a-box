//! Plugin-local mirror of `ainb-core::cli::OutputFormat`.
//!
//! The host's `ainb usage` clap dispatch passes the chosen output format
//! to the plugin via the command registry; the plugin uses this enum to
//! drive text/json/csv formatting in `cli::execute`.

use clap::ValueEnum;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
    Csv,
}
