//! Read-only walkers that scan tool homes for already-deployed units
//! so the skill-manager can offer one-touch adoption into the ainb
//! manifest on first open.
//!
//! Three classes, each with its own walker, all sharing the invariant
//! that no walker writes to disk and no walker panics on malformed
//! input (best-effort discovery, log + skip).
//!
//! - `class_a` — marketplace plugins installed via Claude Code's
//!   `/plugin install`, cached at `~/.claude/plugins/cache/<mp>/<plugin>/<ver>/`.
//! - `class_c` — orphan SKILL.md units under `~/.<tool>/skills/`,
//!   `~/.<tool>/agents/`, etc. across the 9 adapter tools.
//! - `class_b` (P4, opt-in) — name-match against a legacy
//!   `external-dependencies.yaml` to recognise bootstrap.js sources.

pub mod class_a;
pub mod class_c;
