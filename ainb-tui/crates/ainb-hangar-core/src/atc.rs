//! The ATC auto-`continue` cap, shared by every layer that enforces it.
//!
//! Two enforcers have to agree on this number: the ATC heartbeat in `ainb`,
//! which renders an exhausted session as ESCALATE-ONLY, and the daemon's
//! LLM-free retry sweep, which seeds its reserved instance's `err_retry_cap`
//! from it so both take the same `err_action` branch for the same count. `ainb`
//! sits ABOVE the daemon in the dependency graph, so a constant declared there
//! is unreadable here; it lives in this crate, which both already depend on,
//! and `ainb` re-exports it from its original path so no call site moved.

/// Maximum auto-`continue` attempts per session before the enforcer must
/// escalate to a human instead of retrying again. Bounds behaviour that was
/// once unbounded, so a permanently-broken session reaches a person instead of
/// looping forever.
pub const DEFAULT_ERR_RETRY_CAP: u32 = 3;
