//! macOS Seatbelt backend: confine the provider via `/usr/bin/sandbox-exec`.
//!
//! We generate a Sandbox Profile Language (SBPL / `.sb`) document that
//! `(import "bsd.sb")`s the system base profile (so a dynamically-linked binary
//! and the dyld shared cache can load — without this every exec aborts with
//! `Abort trap: 6` before `main`), then `(deny default)`s every operation and
//! re-allows the narrow set a real agent needs:
//!
//! - `process-exec*` / `process-fork` so the program (and tools it shells out
//!   to) can run,
//! - `file-read*` on the policy read roots — so the agent can read its own
//!   inputs but NOT the operator's `$HOME`/`~/.ssh`/`~/.aws` (verified: under
//!   `bsd.sb` + `(deny default)`, reading a file in the real home is blocked
//!   with `Operation not permitted`),
//! - `file-write*` on the policy write roots (the task workdir/output/logs +
//!   temp) only — every other write is denied,
//! - `network*` when [`SandboxPolicy::allow_network`] (the agent must reach the
//!   model API).
//!
//! `bsd.sb` grants broad *system* reads (libs, frameworks) — those are
//! world-readable system content, never user secrets — so it widens nothing
//! that matters while making real binaries actually runnable.
//!
//! The profile is passed **inline** via `sandbox-exec -p '<profile>'` (not a
//! `-f <file>`): an inline profile has no temp-file lifetime to manage, so there
//! is no race where the profile is deleted before `sandbox-exec` reads it (a
//! `-f` file deleted right after `tokio::spawn()` returns makes `sandbox-exec`
//! exit 65 "No such file or directory"). No `unsafe` is used — `sandbox-exec`
//! is an external launcher.

use std::fmt::Write as _;
use std::path::Path;

use crate::{Enforcement, SandboxError, SandboxPolicy, SandboxedCommand, canonical_or_self};

/// The system launcher that applies a Seatbelt profile to a child process.
const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Build a `sandbox-exec`-wrapped command for `program` under `policy`.
pub fn build(program: &Path, policy: &SandboxPolicy) -> Result<SandboxedCommand, SandboxError> {
    if !Path::new(SANDBOX_EXEC).exists() {
        return Err(SandboxError::Unavailable(format!(
            "{SANDBOX_EXEC} not found"
        )));
    }

    let profile = render_profile(program, policy);

    // `sandbox-exec -p '<profile>' -- <program>` applies the inline Seatbelt
    // profile and then execs the program. Inline (vs `-f <file>`) means nothing
    // to keep alive across the spawn.
    let mut cmd = std::process::Command::new(SANDBOX_EXEC);
    cmd.arg("-p").arg(profile).arg("--").arg(program);

    Ok(SandboxedCommand {
        inner: cmd,
        enforcement: Enforcement::Enforced,
    })
}

/// Render the SBPL profile text for `program` confined to `policy`.
fn render_profile(program: &Path, policy: &SandboxPolicy) -> String {
    let mut p = String::new();
    p.push_str("(version 1)\n");
    // Import the system base profile FIRST: it grants the dyld shared cache +
    // system-library reads a dynamically-linked binary needs to load. Without
    // it, exec aborts (`Abort trap: 6`) before reaching `main`.
    p.push_str("(import \"bsd.sb\")\n");
    // Then deny everything not explicitly re-allowed below. `(with no-log)`
    // keeps the enforced denials from spamming the system log.
    p.push_str("(deny default (with no-log))\n");

    // The program (and any tool it shells out to) must be able to run.
    p.push_str("(allow process-fork)\n");
    p.push_str("(allow process-exec*)\n");

    // File reads: every policy read root as a subpath. `deny default` means
    // anything NOT listed here (the operator's $HOME, ~/.ssh, ~/.aws) is blocked
    // — the secret-leak boundary. Paths are canonicalised so the profile matches
    // the kernel's symlink-resolved view (`/var` -> `/private/var` on macOS).
    for root in &policy.read_roots {
        let c = canonical_or_self(root);
        let _ = writeln!(p, "(allow file-read* (subpath {}))", sb_quote(&c));
    }
    // The program binary itself must be readable even if its dir is not a
    // configured read root (e.g. a test stand-in under a tempdir).
    let prog = canonical_or_self(program);
    let _ = writeln!(p, "(allow file-read* (literal {}))", sb_quote(&prog));
    if let Some(dir) = prog.parent() {
        let _ = writeln!(p, "(allow file-read* (subpath {}))", sb_quote(dir));
    }

    // File writes: only the task roots (+ temp). Everything else denied.
    for root in &policy.write_roots {
        let c = canonical_or_self(root);
        let _ = writeln!(p, "(allow file-write* (subpath {}))", sb_quote(&c));
    }

    if policy.allow_network {
        p.push_str("(allow network*)\n");
    }

    // No `mach-lookup` is ever emitted — see the "no Keychain / securityd grant"
    // section on `SandboxPolicy`. `deny default` covers mach services, so the
    // absence of a rule here IS the denial.

    p
}

/// Quote a path as an SBPL string literal: wrap in double quotes and escape any
/// embedded `"` / `\`. SBPL paths are absolute, so this is the only escaping
/// the profile needs.
fn sb_quote(p: &Path) -> String {
    let s = p.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn profile_denies_default_and_allows_roots() {
        let policy = SandboxPolicy {
            read_roots: vec![PathBuf::from("/usr")],
            write_roots: vec![PathBuf::from("/tmp/task")],
            allow_network: true,
            disabled: false,
        };
        let prof = render_profile(Path::new("/bin/sh"), &policy);
        assert!(
            prof.contains("(import \"bsd.sb\")"),
            "must import system base"
        );
        assert!(prof.contains("(deny default"), "must deny by default");
        assert!(prof.contains("(allow file-read* (subpath \"/usr\"))"));
        assert!(prof.contains("(allow file-write* (subpath \"/tmp/task\"))"));
        assert!(prof.contains("(allow network*)"));
        // The operator home must NOT be a blanket read root.
        assert!(!prof.contains("(subpath \"/Users\")"));
    }

    /// No profile this crate can generate reaches a mach service — in
    /// particular securityd, which is the macOS Keychain.
    ///
    /// A `mach-lookup` on `com.apple.SecurityServer` was briefly shipped as an
    /// opt-in "credential grant". It is not per-item: under this exact profile it
    /// also hands over `Chrome Safe Storage` (the master key for every
    /// Chrome-saved password) silently and with no prompt, and `process-exec*`
    /// means a confined agent reaches it by spawning `/usr/bin/security`. Pin the
    /// absence so it cannot come back by accident — see the rationale on
    /// `SandboxPolicy`.
    #[test]
    fn profile_never_grants_mach_lookup() {
        let policy = SandboxPolicy {
            read_roots: vec![PathBuf::from("/usr")],
            write_roots: vec![PathBuf::from("/tmp/task")],
            allow_network: true,
            disabled: false,
        };
        let prof = render_profile(Path::new("/bin/sh"), &policy);
        assert!(
            !prof.contains("mach-lookup"),
            "no mach service may be re-allowed — securityd least of all: {prof}"
        );
        assert!(
            !prof.contains("SecurityServer"),
            "the Keychain grant must stay gone: {prof}"
        );
        // The full confined policy a real task gets must be just as silent.
        let prof = render_profile(
            Path::new("/bin/sh"),
            &SandboxPolicy::confined_to(Path::new("/tmp/task")),
        );
        assert!(
            !prof.contains("mach-lookup"),
            "the default task policy must grant no mach service: {prof}"
        );
    }

    #[test]
    fn sb_quote_escapes_quotes_and_backslashes() {
        assert_eq!(sb_quote(Path::new("/a/b")), "\"/a/b\"");
        assert_eq!(sb_quote(Path::new("/a\"b")), "\"/a\\\"b\"");
    }
}
