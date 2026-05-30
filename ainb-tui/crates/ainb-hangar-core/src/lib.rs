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
/// Polymorphic assignee crosswalk between Hangar actors and `bd` strings (P2.3).
pub mod assignee_crosswalk;
/// Wall-clock injection (`HangarClock` + `SystemClock` / `FixedClock`).
pub mod clock;
/// Environment allowlist policy (P5.3): allowlist passthrough with a hardcoded
/// deny family that always overrides. Pure + IO-free; the TOML loader and
/// daemon wiring live in `ainb-hangar-daemon`.
pub mod env_policy;
/// Id generation injection (`IdGen` + `SystemIdGen` / `FixedIdGen`).
pub mod idgen;
/// Strongly-typed entity id newtypes with a non-empty invariant.
pub mod ids;
/// Skill domain vocabulary: normalised [`skill::SkillName`], the
/// [`skill::SkillWithFiles`] aggregate, and ordered file inputs.
pub mod skill;
/// The IO-free skill service (P6.5).
///
/// Workspace-scoped orchestration over a [`skill_service::SkillBackend`] the
/// daemon wraps with sqlx and tests fake.
pub mod skill_service;
/// The task domain: the `agent_task_queue` lifecycle FSM.
pub mod task;
/// The `agent_task_queue` lifecycle status enum (P0 placeholder).
pub mod task_status;
/// Embedded curated `agent_template` registry (P6.3).
///
/// The 10 repo-only templates baked into the binary via `include_str!`, plus
/// their skill-reference invariant (enforced by this crate's `build.rs`).
pub mod template;
/// Token minting + verification primitives (PAT / daemon-token, `sha256` only).
pub mod token;
/// Danger-full-access warning ack keys + pure show/skip decision logic.
pub mod warnings;
