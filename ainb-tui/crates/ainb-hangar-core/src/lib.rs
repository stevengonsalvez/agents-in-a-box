//! Hangar domain core: pure, IO-free types shared across the managed-agents
//! control plane.
//!
//! This crate carries the vocabulary of the Hangar domain — polymorphic
//! actors, strongly-typed IDs, the task-status lifecycle enum, and the
//! clock / id-generation injection seams — with **no IO dependencies** (no
//! `tokio`, `sqlx`, or `ratatui`). The persistence layer
//! ([`ainb_hangar_store`](../ainb_hangar_store/index.html)) and the daemon both
//! depend on it, never the other way around.

/// Polymorphic actor references (`member:<id>` / `agent:<id>`).
pub mod actor;
/// Wall-clock injection (`HangarClock` + `SystemClock` / `FixedClock`).
pub mod clock;
/// Id generation injection (`IdGen` + `SystemIdGen` / `FixedIdGen`).
pub mod idgen;
/// Strongly-typed entity id newtypes with a non-empty invariant.
pub mod ids;
/// The task domain: the `agent_task_queue` lifecycle FSM.
pub mod task;
/// The `agent_task_queue` lifecycle status enum (P0 placeholder).
pub mod task_status;
