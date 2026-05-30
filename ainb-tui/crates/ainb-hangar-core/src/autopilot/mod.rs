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

/// Cron-expression parsing and next-tick calculation (P7.1).
pub mod cron;
/// The IO-free autopilot CRUD service (P7.2).
///
/// Workspace-scoped orchestration over an [`service::AutopilotBackend`] the
/// daemon wraps with sqlx (`AutopilotRepo`) and tests fake in memory. Owns the
/// cron-validation-before-insert and enable-recompute-from-now logic.
pub mod service;
