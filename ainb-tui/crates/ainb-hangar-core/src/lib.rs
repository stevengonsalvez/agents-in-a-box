//! Hangar domain core: pure, IO-free types shared across the managed-agents
//! control plane.
//!
//! This crate carries the vocabulary of the Hangar domain — polymorphic
//! actors, strongly-typed IDs, the task-status lifecycle enum, and the
//! clock / id-generation injection seams — with **no IO dependencies** (no
//! `tokio`, `sqlx`, or `ratatui`). The persistence layer
//! ([`ainb_hangar_store`](../ainb_hangar_store/index.html)) and the daemon both
//! depend on it, never the other way around.

/// Structured acceptance criteria: per-criterion stable id + checked state
/// (multica parity #11-rest), plus the tolerant legacy/structured JSON codec
/// shared by the store column and the wire.
pub mod acceptance;
/// Polymorphic actor references (`member:<id>` / `agent:<id>`).
pub mod actor;
/// The card provider-agent kind (`claude`/`codex`/`copilot`) + the task-create
/// default cascade (spec F4).
pub mod agent_kind;
/// Polymorphic assignee crosswalk between Hangar actors and `bd` strings (P2.3).
pub mod assignee_crosswalk;
/// Cron-scheduled autopilots (P7): the IO-free cron parser + next-tick math.
pub mod autopilot;
/// Notification channels + the per-kind channel SET a routed attention lands on
/// (tcp T5 — notification routing rules).
pub mod channel;
/// Wall-clock injection (`HangarClock` + `SystemClock` / `FixedClock`).
pub mod clock;
/// The single source of truth for the daemon's user-configurable knobs — the
/// typed descriptor registry both the TUI Settings pane and the
/// `ainb hangar daemon config` CLI iterate (keys, kinds, defaults, validation).
pub mod daemon_config;
/// Environment allowlist policy (P5.3): allowlist passthrough with a hardcoded
/// deny family that always overrides. Pure + IO-free; the TOML loader and
/// daemon wiring live in `ainb-hangar-daemon`.
pub mod env_policy;
/// Id generation injection (`IdGen` + `SystemIdGen` / `FixedIdGen`).
pub mod idgen;
/// Strongly-typed entity id newtypes with a non-empty invariant.
pub mod ids;
/// Structured-log line model + `daemon.<date>` reader shared by the
/// `ainb hangar logs tail` CLI verb and the TUI `LogsScreen` (P8.6).
pub mod logs;
/// The single source of truth for the Hangar home directory
/// ([`paths::hangar_home`], re-exported at the crate root). Every Hangar
/// resolver delegates here so the `$AINB_HANGAR_HOME`-else-`~/.agents-in-a-box`
/// contract lives in one place.
pub mod paths;
/// Parse the canonical `gh pr create` PR-URL line from agent stdout (P9.1).
pub mod pr_url;
/// Agent-profile domain (P5): the canonical Claude-subagent `.md` master format,
/// the logical [`profile::ModelTier`], and the lossless-Claude / lossy-Codex
/// down-compilers. Pure + IO-free — the daemon adds disk/DB/watch, the plugin
/// borrows it for live preview.
pub mod profile;
/// The structured `agent_task_queue.result` JSON shape ([`result::TaskResult`]).
pub mod result;
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
/// Webhook signing primitives for webhook-triggered autopilots (e38.18).
///
/// HMAC-SHA256-of-body signature compute + constant-time verify, secret minting
/// (digest stored, plaintext returned once), and the optional event filter — all
/// IO-free. The HTTP ingress + secret-file storage live in the daemon / store.
pub mod webhook;

/// The Hangar home resolver, re-exported at the crate root so every consumer
/// can call `ainb_hangar_core::hangar_home()` without naming the `paths`
/// module. This is the single source of truth for the home contract.
pub use paths::hangar_home;
