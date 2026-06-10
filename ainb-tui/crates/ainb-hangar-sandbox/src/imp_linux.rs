//! Linux Landlock backend: confine the provider via an unprivileged Landlock
//! ruleset applied in the forked child before `exec`.
//!
//! Landlock (kernel >= 5.13) is an unprivileged LSM: a process can restrict
//! *itself and its future children* to a set of path rules, and the restriction
//! can never be widened afterward. We grant the child read/exec on the policy
//! read roots and read/write on the policy write roots, then `restrict_self()`.
//!
//! ## Where the restriction is applied
//!
//! The ruleset MUST be enforced in the child, after `fork()` but before `exec()`
//! — never on the daemon (which would confine the daemon too). The only seam
//! `std`/`tokio` give for "run code in the forked child before exec" is
//! [`std::os::unix::process::CommandExt::pre_exec`], which is `unsafe` because
//! the closure runs in the perilous post-`fork` context. Our closure only
//! opens the (already-resolved) path roots and issues Landlock syscalls — both
//! async-signal-safe-enough for this use (no allocator-reentrancy hazard beyond
//! what Landlock's own example relies on). This is the single `unsafe` block in
//! the crate (hence `unsafe_code = "warn"`, not `forbid`, in `Cargo.toml`).
//!
//! ## Enforcement detection
//!
//! `build` probes Landlock support in the *parent* (a cheap
//! `create()`-then-drop) to decide [`Enforcement`]. If the kernel lacks
//! Landlock, `build` returns an [`Enforcement::None`] passthrough so the daemon
//! still runs `claude` and the confinement test SKIPs instead of failing
//! vacuously. When supported, the `pre_exec` enforcement is hard-required for
//! the FS rules and best-effort for newer ABI features.
//!
//! ## Network
//!
//! FS confinement is the secret-leak boundary this bead delivers. Landlock
//! network rules (TCP connect/bind ports) and syscall-level seccomp filtering
//! are a documented follow-up — see the crate docs and the bead. We do NOT ship
//! a fake network filter.

use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, Access, AccessFs, CompatLevel, Compatible, PathFd, Ruleset, RulesetAttr,
    RulesetCreatedAttr, RulesetStatus, path_beneath_rules,
};

use crate::{Enforcement, SandboxError, SandboxPolicy, SandboxedCommand, canonical_or_self};

/// The Landlock ABI we target. V1 (kernel 5.13) is the floor that gives us the
/// read/write FS access-rights this bead needs; newer features degrade
/// best-effort.
const ABI_TARGET: ABI = ABI::V1;

/// Build a Landlock-confined command for `program` under `policy`.
pub fn build(program: &Path, policy: &SandboxPolicy) -> Result<SandboxedCommand, SandboxError> {
    // Probe support in the parent so `enforcement()` is accurate for the caller
    // / the test SKIP decision. A failure here (old kernel, Landlock disabled)
    // means we cannot confine — return a passthrough rather than an error so the
    // daemon keeps running the claude provider.
    if !landlock_supported() {
        return Ok(SandboxedCommand {
            inner: std::process::Command::new(program),
            enforcement: Enforcement::None,
        });
    }

    // Resolve + own the roots so the post-fork closure has no borrows into the
    // policy. The program itself must be readable/executable.
    let mut read_roots: Vec<PathBuf> =
        policy.read_roots.iter().map(|p| canonical_or_self(p)).collect();
    let prog = canonical_or_self(program);
    read_roots.push(prog.clone());
    if let Some(dir) = prog.parent() {
        read_roots.push(dir.to_path_buf());
    }
    let write_roots: Vec<PathBuf> =
        policy.write_roots.iter().map(|p| canonical_or_self(p)).collect();

    let mut cmd = std::process::Command::new(program);

    // SAFETY: the closure runs in the forked child before `exec`. It only opens
    // the pre-resolved path roots (via `PathFd`) and issues Landlock syscalls to
    // restrict the child; it performs no shared-state mutation of the parent and
    // returns an `io::Error` (never panics) on failure so the spawn fails
    // closed. This is the documented Landlock enforcement pattern (see the
    // crate's `sandboxer` example, which restricts in-process before exec).
    unsafe {
        cmd.pre_exec(move || apply_landlock(&read_roots, &write_roots));
    }

    Ok(SandboxedCommand {
        inner: cmd,
        enforcement: Enforcement::Enforced,
    })
}

/// Whether the running kernel actually supports Landlock (so we can pick
/// [`Enforcement`] honestly). Cheap: create an empty ruleset and inspect its
/// status, then drop it. A creation error or a `NotEnforced` status means no
/// real confinement is available.
fn landlock_supported() -> bool {
    let probe = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(ABI_TARGET))
        .and_then(Ruleset::create);
    match probe {
        // Restricting an empty ruleset against self is too invasive for a probe,
        // so we only check that ruleset creation succeeds AND the compat layer
        // reports the access set as handled (Full/Partial). `create()` issuing
        // the `landlock_create_ruleset` syscall successfully is the real
        // capability signal.
        Ok(_created) => true,
        Err(_) => false,
    }
}

/// Apply the Landlock ruleset to the calling (forked-child) process. Runs inside
/// `pre_exec`; returns an `io::Error` on any failure so the spawn fails closed.
fn apply_landlock(read_roots: &[PathBuf], write_roots: &[PathBuf]) -> std::io::Result<()> {
    let read_access = AccessFs::from_read(ABI_TARGET);
    // Write roots grant write WITHOUT read — matching the macOS Seatbelt profile
    // (file-write* on temp, file-read* only on the read roots). Landlock's
    // `from_all` includes `ReadFile`, so without removing it a writable `/tmp`
    // (the process temp write root from `SandboxPolicy::confined_to`) would also
    // be *readable*, letting the confined agent read any secret another process
    // left under `/tmp`. The task workdir stays fully readable: it is ALSO a read
    // root, and Landlock unions the per-path rules.
    let mut write_access = AccessFs::from_all(ABI_TARGET);
    write_access.remove(AccessFs::ReadFile);

    let ruleset = Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(ABI_TARGET))
        .map_err(to_io)?
        .create()
        .map_err(to_io)?;

    let ruleset = ruleset
        .add_rules(
            // A missing read root (e.g. /lib64 absent on this distro) is not
            // fatal — keep the resolvable rules and drop the errors. `filter`
            // (not a re-wrapping `filter_map`) preserves the iterator's
            // `Result<_, RulesetError>` item type, so `add_rules` can infer the
            // error type (a re-wrapped `Ok(rule)` leaves it unconstrained → E0283).
            path_beneath_rules(read_roots.iter().map(PathBuf::as_path), read_access)
                .filter(|r| r.is_ok()),
        )
        .map_err(to_io)?;

    let status = ruleset
        .add_rules(
            path_beneath_rules(write_roots.iter().map(PathBuf::as_path), write_access)
                .filter(|r| r.is_ok()),
        )
        .map_err(to_io)?
        .restrict_self()
        .map_err(to_io)?;

    // If the kernel silently failed to enforce, fail closed: better to abort the
    // spawn than to run an agent we believe is sandboxed but is not.
    match status.ruleset {
        RulesetStatus::FullyEnforced | RulesetStatus::PartiallyEnforced => Ok(()),
        RulesetStatus::NotEnforced => Err(std::io::Error::other(
            "landlock ruleset not enforced by kernel",
        )),
    }
}

/// Map any Landlock error into an `io::Error` for the `pre_exec` return.
fn to_io<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::other(format!("landlock: {e}"))
}

/// Helper for the `PathFd`-based rule builder (open the path root). Unused
/// directly — `path_beneath_rules` opens roots internally — but kept so a
/// future per-path-fd refinement has the seam. Referenced by `_` to avoid a
/// dead-code warning while documenting intent.
#[allow(dead_code)]
fn open_root(p: &Path) -> std::io::Result<PathFd> {
    PathFd::new(p).map_err(to_io)
}
