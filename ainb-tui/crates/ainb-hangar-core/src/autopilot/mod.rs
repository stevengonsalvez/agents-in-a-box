//! Autopilots: cron-scheduled, repeating agent runs (P7).
//!
//! At v1 an autopilot is a stored `(cron_expr, agent, instructions)` triple the
//! daemon's scheduler thread (P7.3) fires at each tick. This module carries the
//! IO-free pieces of that machinery.
//!
//! # Submodules
//!
//! - [`cron`] — the cron-expression parser + next-tick calculator (P7.1).
//!
//! P7.2 will add the autopilot service/types alongside `cron`.

/// Cron-expression parsing and next-tick calculation (P7.1).
pub mod cron;
