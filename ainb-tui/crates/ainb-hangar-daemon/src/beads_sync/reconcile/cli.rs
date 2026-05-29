//! Clap surface for `ainb hangar beads reconcile` (P2.5).
//!
//! This is the parse layer only — it defines the `beads` subgroup, its
//! `reconcile` verb, and the flags (`--dry-run`, repeated `--label`,
//! `--format`). The verb's options round-trip into [`ReconcileOpts`] via
//! [`ReconcileArgs::to_opts`]; the daemon's dispatcher composes a
//! [`ReconcileService`](super::ReconcileService), runs it, and renders the
//! [`ReconcileReport`](super::ReconcileReport) in the chosen [`OutputFormat`].
//!
//! Keeping the clap types here (rather than in `main.rs`) means the surface is
//! unit-testable without spawning the binary and stays composable into the host
//! `ainb hangar` command tree.

use clap::{Args, Parser, Subcommand, ValueEnum};
// `Args` is used by the `ReconcileArgs` derive below.

use super::ReconcileOpts;

/// The full `ainb hangar beads …` command path.
///
/// Models the shape the host `ainb hangar` tree plugs in: a `beads` subcommand
/// token wrapping the [`BeadsCli`] subgroup. The daemon/host parses with this;
/// the [`BeadsCli`] type is also a standalone `Parser` so the subgroup can be
/// tested (and dispatched) in isolation once the parent has consumed `beads`.
#[derive(Parser, Debug)]
#[command(name = "ainb-hangar")]
pub struct HangarBeadsCli {
    /// The `beads` subgroup.
    #[command(subcommand)]
    pub beads: BeadsSubgroup,
}

/// The single `beads` subcommand of [`HangarBeadsCli`].
#[derive(Subcommand, Debug)]
pub enum BeadsSubgroup {
    /// Sync Hangar issues with the beads (`bd`) tracker.
    Beads(BeadsCli),
}

impl HangarBeadsCli {
    /// Borrow the `beads` subgroup payload (there is exactly one variant).
    #[must_use]
    pub const fn beads(&self) -> &BeadsCli {
        let BeadsSubgroup::Beads(cli) = &self.beads;
        cli
    }
}

/// The `beads` subgroup of the `ainb hangar` command tree.
///
/// Derives [`Parser`] (standalone parse + `try_parse_from`) which also provides
/// the [`Args`] impl letting it nest as the [`BeadsSubgroup::Beads`] payload.
#[derive(Parser, Debug)]
#[command(name = "beads", about = "Sync Hangar issues with the beads (bd) tracker")]
pub struct BeadsCli {
    /// Which beads subcommand to run.
    #[command(subcommand)]
    pub command: BeadsCommand,
}

/// Subcommands under `ainb hangar beads`.
#[derive(Subcommand, Debug)]
pub enum BeadsCommand {
    /// Walk the mapping table and repair Hangar ↔ bd drift (last-writer-wins).
    Reconcile(ReconcileArgs),
}

/// Arguments for `ainb hangar beads reconcile`.
#[derive(Args, Debug, Clone)]
pub struct ReconcileArgs {
    /// Diff only — report drift without writing either side.
    #[arg(long)]
    pub dry_run: bool,

    /// Restrict to bd issues carrying this label (repeatable).
    #[arg(long = "label")]
    pub label: Vec<String>,

    /// Output format for the reconcile report.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,
}

impl ReconcileArgs {
    /// Project the parsed CLI args onto a [`ReconcileOpts`] for the service,
    /// binding adopted-issue creation to `workspace_id`.
    #[must_use]
    pub fn to_opts(&self, workspace_id: impl Into<String>) -> ReconcileOpts {
        ReconcileOpts {
            dry_run: self.dry_run,
            label_filter: self.label.clone(),
            workspace_id: workspace_id.into(),
        }
    }
}

/// How the reconcile report is rendered to stdout.
#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// A human-readable summary line plus per-conflict detail.
    #[default]
    Text,
    /// The full reconcile report as JSON (stable for scripting).
    Json,
}

impl OutputFormat {
    /// Render a finished [`ReconcileReport`](super::ReconcileReport) to a string
    /// in this format.
    ///
    /// `Json` is the serialized report (stable for scripting); `Text` is a
    /// one-line summary followed by an indented line per resolved conflict.
    ///
    /// # Errors
    ///
    /// Returns a [`serde_json::Error`] only in `Json` mode if serialization fails
    /// (it cannot for the report's plain types, but the signature stays honest).
    pub fn render(self, report: &super::ReconcileReport) -> Result<String, serde_json::Error> {
        match self {
            Self::Json => serde_json::to_string_pretty(report),
            Self::Text => {
                use std::fmt::Write as _;
                let s = &report.stats;
                let mut out = format!(
                    "reconcile: scanned={} updated={} adopted={} deleted={} in_sync={} skipped={} errors={}",
                    s.scanned, s.updated, s.adopted, s.deleted, s.in_sync, s.skipped, s.errors
                );
                for c in &report.conflicts {
                    // `write!` into a String is infallible; the unused Result is
                    // discarded deliberately.
                    let _ = write!(
                        out,
                        "\n  conflict {} <-> {}: hangar={} bd={} winner={:?}",
                        c.hangar_id, c.bd_id, c.hangar_state, c.bd_status, c.winner
                    );
                }
                Ok(out)
            }
        }
    }
}
