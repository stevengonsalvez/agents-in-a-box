//! The OS-agnostic confinement policy: which paths the confined child may read
//! and write, and whether network egress is allowed.

use std::path::{Path, PathBuf};

/// Filesystem-confinement policy for a spawned provider subprocess.
///
/// The policy is OS-agnostic; each per-OS backend translates it into a Seatbelt
/// profile (macOS) or a Landlock ruleset (Linux). The defaults are the
/// secret-leak boundary: the child can read the system roots a real agent needs
/// and read/write only the task's isolated roots — it CANNOT read the operator's
/// `$HOME`, `~/.ssh`, `~/.aws`, etc., which is the exfiltration path this bead
/// closes.
///
/// # There is deliberately no Keychain / securityd grant
///
/// A confined provider whose credential lives in the macOS login Keychain cannot
/// reach it (`deny default` blocks the securityd mach-lookup), so a headless run
/// fails "Not logged in". Do NOT fix that by re-allowing
/// `mach-lookup (global-name "com.apple.SecurityServer")`. That grant was built,
/// measured, and removed:
///
/// * It is not per-item. Under this exact profile,
///   `security find-generic-password -s 'Chrome Safe Storage' -w` goes from
///   `DENIED rc=44` to returning the secret, silently and with no prompt — and
///   that item is the master key for every Chrome-saved password, not the
///   agent's own token. "securityd still arbitrates which items the caller may
///   have" FAILS OPEN for any item that is ACL-permissive (`-A`) or was ever
///   marked "Always Allow".
/// * It cannot be scoped to one caller: the profile allows `process-exec*`, so a
///   confined agent simply spawns `/usr/bin/security`.
/// * With `allow_network` also on, and briefs built from board issue text, the
///   chain prompt-injection -> `security` -> egress is complete.
///
/// The supported path is to inject the credential as an env var from the
/// UNSANDBOXED parent daemon (`claude setup-token` ->
/// `CLAUDE_CODE_OAUTH_TOKEN`), which needs no grant into `$HOME` at all. Until
/// that lands, a headless run needs `HANGAR_DAEMON_DISABLE_SANDBOX=1`.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Paths the child may read (and execute). Includes the system roots a real
    /// agent legitimately needs (libs, the dynamic linker, the provider binary)
    /// plus any task-specific read roots.
    pub read_roots: Vec<PathBuf>,
    /// Paths the child may write to (the task workdir/output/logs + temp).
    pub write_roots: Vec<PathBuf>,
    /// Whether to allow outbound network (the agent must reach the model API).
    /// Enforced on macOS (Seatbelt `network*`); on Linux network filtering is a
    /// seccomp follow-up, so this is advisory there.
    pub allow_network: bool,
    /// When `true`, [`crate::sandboxed_command`] skips confinement entirely
    /// (returns a passthrough with [`crate::Enforcement::None`]). The override
    /// seam: default is `false` (confine).
    pub disabled: bool,
}

/// The default system roots a confined agent may READ on a Unix host: the
/// shared libraries, the dynamic linker, and the standard binary dirs the
/// provider (or a shell it spawns) needs to even start. Reading these leaks no
/// user secrets — they are world-readable system content.
#[cfg(unix)]
const SYSTEM_READ_ROOTS: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/etc",         // resolv.conf, ssl certs, nsswitch — needed for DNS/TLS to the API
    "/System",      // macOS frameworks + dyld
    "/Library",     // macOS shared frameworks
    "/private/etc", // macOS canonical /etc
    "/dev/null",
    "/dev/zero",
    "/dev/urandom",
    "/dev/random",
];

impl SandboxPolicy {
    /// Build the default confinement policy for a task whose isolated root is
    /// `task_root` (the dir containing `workdir/`, `output/`, `logs/`).
    ///
    /// Reads: the system roots + the task root. Writes: the task root + the
    /// process temp dir. Network: allowed (the agent needs the model API).
    #[must_use]
    pub fn confined_to(task_root: &Path) -> Self {
        let mut read_roots: Vec<PathBuf> = system_read_roots();
        read_roots.push(task_root.to_path_buf());

        let mut write_roots = vec![task_root.to_path_buf()];
        // The process temp dir: agents and the tools they shell out to scribble
        // here constantly; confining writes to it (rather than denying temp
        // entirely) keeps a real agent functional without widening the blast
        // radius beyond ephemeral scratch.
        write_roots.push(std::env::temp_dir());

        Self {
            read_roots,
            write_roots,
            allow_network: true,
            disabled: false,
        }
    }

    /// Add a path the child may read (e.g. the provider binary's own directory,
    /// or a materialised skills home outside the task root).
    #[must_use]
    pub fn allow_read(mut self, p: &Path) -> Self {
        self.read_roots.push(p.to_path_buf());
        self
    }

    /// Add a path the child may write (e.g. a `*_HOME` the provider mutates).
    #[must_use]
    pub fn allow_write(mut self, p: &Path) -> Self {
        self.write_roots.push(p.to_path_buf());
        self
    }

    /// Disable confinement (the override seam). The resulting policy makes
    /// [`crate::sandboxed_command`] return a passthrough.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            read_roots: Vec::new(),
            write_roots: Vec::new(),
            allow_network: true,
            disabled: true,
        }
    }
}

/// The system read roots that actually exist on this host (filtering keeps the
/// generated profile / ruleset from referencing absent paths, which Landlock
/// rejects and which bloats the Seatbelt profile).
fn system_read_roots() -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        SYSTEM_READ_ROOTS.iter().map(PathBuf::from).filter(|p| p.exists()).collect()
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}
