//! OS-level filesystem confinement for the spawned agent provider subprocess
//! (e38.23).
//!
//! The Hangar daemon already denies the provider child the daemon's secrets via
//! a 12-var env allowlist + deny family + process-group kill, but **nothing
//! confines the child's ambient filesystem**: a spawned agent runs with the
//! daemon user's full read/write access to `$HOME`, `~/.ssh`, `~/.aws`, etc.
//! Before non-`claude` providers (codex/copilot/gemini) land, the spawned
//! process must be confined so it can only touch the task's isolated roots.
//!
//! This crate is a thin, per-OS **spawn wrapper**:
//!
//! ```text
//! ┌──────────────┐  program +   ┌──────────────────┐  confined  ┌───────────┐
//! │ Runner       │  SandboxPolicy│ sandboxed_command │  Command  │ provider  │
//! │ (daemon)     │─────────────▶│ (per-OS cfg)      │──────────▶│ subprocess│
//! └──────────────┘              └──────────────────┘            └───────────┘
//! ```
//!
//! - **macOS** ([`imp_macos`]): generates a Seatbelt (`.sb`) profile that
//!   `deny default`s file reads/writes, re-allows reads of the system roots a
//!   real agent needs (`/usr`, `/System`, `/Library`, the dynamic linker, the
//!   provider binary, the read roots) and writes only to the task roots (+
//!   temp), allows network egress to the model API, then execs the program
//!   under `/usr/bin/sandbox-exec -p <profile> -- <program> …`. No `unsafe`.
//! - **Linux** ([`imp_linux`]): builds a Landlock ruleset that grants the child
//!   read/exec on the read roots and read/write on the write roots, then applies
//!   it via a `pre_exec` hook so the confinement lands in the forked child
//!   *before* exec — never on the daemon. Network is left to a follow-up
//!   (seccomp); FS confinement is the secret-leak boundary this bead delivers.
//! - **Any other OS / primitive unavailable**: the wrapper returns the program
//!   *unconfined* with [`Enforcement::None`] so the daemon still runs the
//!   `claude` provider, and the confinement test SKIPs rather than vacuously
//!   passing. A no-op is never reported as enforcement.
//!
//! # Default-on, overridable
//!
//! [`sandboxed_command`] confines by default. A caller that must opt out (the
//! existing `claude` provider keeps working either way — it only needs network
//! and the workdir, both allowed) can branch on [`SandboxPolicy::disabled`] and
//! [`Enforcement`] without touching this crate's internals.

use std::path::{Path, PathBuf};

mod policy;
pub use policy::SandboxPolicy;

#[cfg(target_os = "linux")]
mod imp_linux;
#[cfg(target_os = "macos")]
mod imp_macos;

/// Whether the OS sandbox is actually enforcing for a built command.
///
/// The daemon and the confinement test branch on this: an `Enforced` command is
/// genuinely confined; a `None` command is a passthrough on a platform without
/// a supported primitive (the test SKIPs instead of asserting against a no-op).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// The OS sandbox is applied — FS access is confined to the policy roots.
    Enforced,
    /// No OS sandbox is applied (unsupported platform / primitive unavailable).
    /// The command runs unconfined; the caller decides whether that is
    /// acceptable for the provider in question.
    None,
}

/// Errors building a sandboxed command.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    /// The sandbox launcher / primitive was expected but is missing
    /// (e.g. `/usr/bin/sandbox-exec` absent on a stripped macOS image).
    #[error("sandbox primitive unavailable: {0}")]
    Unavailable(String),
    /// An I/O fault while preparing the sandbox (e.g. writing the `.sb` profile).
    #[error("sandbox setup io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A command pre-configured to spawn its program under the OS sandbox.
///
/// Holds an owned [`std::process::Command`] the caller finishes configuring
/// (args, `current_dir`, env, stdio) and then spawns. The program and any
/// sandbox-launcher wrapping are already baked in; callers MUST NOT replace the
/// program (doing so would bypass the sandbox), only append args/env.
///
/// On macOS the held command is `/usr/bin/sandbox-exec` with the profile +
/// `--` + the real program already pushed as the leading args, so any caller
/// `.arg(...)` lands as an argument to the *real* program. On Linux it is the
/// real program with a `pre_exec` Landlock hook installed. With
/// [`Enforcement::None`] it is the bare program.
pub struct SandboxedCommand {
    inner: std::process::Command,
    enforcement: Enforcement,
}

impl SandboxedCommand {
    /// Borrow the underlying command for final configuration (args, cwd, env,
    /// stdio) before spawning. Appended args go to the confined program.
    pub const fn command(&mut self) -> &mut std::process::Command {
        &mut self.inner
    }

    /// A bare, unconfined command around `program` (the explicit opt-out: the
    /// caller has decided this provider does not need OS confinement). Carries
    /// [`Enforcement::None`].
    #[must_use]
    pub fn passthrough(program: &Path) -> Self {
        passthrough(program)
    }

    /// Whether this command is actually confined by the OS.
    #[must_use]
    pub const fn enforcement(&self) -> Enforcement {
        self.enforcement
    }

    /// Consume into the underlying [`std::process::Command`].
    ///
    /// The macOS Seatbelt profile is passed inline (`sandbox-exec -p`) and the
    /// Linux Landlock rules live in a moved-in `pre_exec` closure on the
    /// command itself — so the confinement travels entirely *with* the returned
    /// command. There is no external file or guard to keep alive; a caller can
    /// freely convert the result into a `tokio::process::Command` and spawn it.
    ///
    /// Callers that finish configuring via [`Self::command`] and spawn directly
    /// never need this — it exists for the runner, which moves the std command
    /// into a `tokio::process::Command`.
    #[must_use]
    pub fn into_inner(self) -> std::process::Command {
        self.inner
    }
}

/// Build a [`SandboxedCommand`] that runs `program` confined to `policy`'s
/// roots.
///
/// Returns a passthrough command with [`Enforcement::None`] (never an error) on
/// an unsupported platform, so the daemon keeps working; returns
/// [`SandboxError::Unavailable`] only when a primitive that *should* be present
/// is missing on a supported OS, and [`SandboxError::Io`] for a setup fault.
///
/// # Errors
///
/// See [`SandboxError`].
pub fn sandboxed_command(
    program: &Path,
    policy: &SandboxPolicy,
) -> Result<SandboxedCommand, SandboxError> {
    if policy.disabled {
        return Ok(passthrough(program));
    }

    #[cfg(target_os = "macos")]
    {
        return imp_macos::build(program, policy);
    }

    #[cfg(target_os = "linux")]
    {
        return imp_linux::build(program, policy);
    }

    #[allow(unreachable_code)]
    {
        // Unsupported OS: run unconfined. The caller sees `Enforcement::None`.
        let _ = policy;
        Ok(passthrough(program))
    }
}

/// A bare, unconfined command around `program`.
fn passthrough(program: &Path) -> SandboxedCommand {
    SandboxedCommand {
        inner: std::process::Command::new(program),
        enforcement: Enforcement::None,
    }
}

/// Canonicalise `p`, falling back to the path as-given if it does not yet exist
/// (a write root may be created by the child). Used by the per-OS impls so the
/// generated profile / ruleset references real, symlink-resolved paths.
pub(crate) fn canonical_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}
