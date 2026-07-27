//! Autopilots: cron-scheduled, repeating agent runs (P7).
//!
//! At v1 an autopilot is a stored `(cron_expr, agent, instructions)` triple the
//! daemon's scheduler thread (P7.3) fires at each tick. This module carries the
//! IO-free pieces of that machinery.
//!
//! # Submodules
//!
//! - [`cron`] — the cron-expression parser + next-tick calculator (P7.1).
//! - [`service`] — the IO-free, workspace-scoped autopilot CRUD service (P7.2).
//! - [`rule_version`] — the accountability-ledger domain: what counts as a
//!   substantive publish (multica parity #14, migration 0061).

/// Cron-expression parsing and next-tick calculation (P7.1).
pub mod cron;
/// The autopilot rule-version domain (multica parity #14).
///
/// [`rule_version::RuleChangeKind`] is the ledger vocabulary and
/// [`rule_version::classify`] is the single definition of "substantive" — the
/// seam that decides whether a mutation mints a version row, and the reason a
/// rename never re-assigns blame for an unattended run.
pub mod rule_version;
/// The IO-free autopilot CRUD service (P7.2).
///
/// Workspace-scoped orchestration over an [`service::AutopilotBackend`] the
/// daemon wraps with sqlx (`AutopilotRepo`) and tests fake in memory. Owns the
/// cron-validation-before-insert and enable-recompute-from-now logic.
pub mod service;
