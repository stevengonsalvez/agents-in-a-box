//! e38.23 confinement acceptance test — the OS-level agent sandbox actually
//! blocks filesystem access outside the allowed task roots.
//!
//! This mirrors the env-allowlist tripwire shape
//! (`crates/ainb-hangar-daemon/tests/tripwire_env_allowlist_blocks_ld_preload.rs`):
//! a paired POSITIVE (allowed path works) + NEGATIVE (denied path is blocked)
//! that proves the boundary is real, not a no-op wrapper.
//!
//! ## Platform behaviour
//!
//! - **macOS** (`sandbox-exec` present): the test ENFORCES. A read of a secret
//!   file placed outside the allowed read roots is denied (the spawned process
//!   exits non-zero / cannot open the file), while a read inside the workdir
//!   succeeds. This is the locally-runnable, CI-on-macOS proof leg.
//! - **Linux** (Landlock available, kernel >= 5.13 with the ruleset enabled):
//!   ENFORCES the same shape via the `landlock` crate's `pre_exec` ruleset.
//! - **Unavailable primitive** (no `sandbox-exec`, Landlock unsupported, or any
//!   other OS): the test SKIPs rather than fails, so CI on a host without the
//!   primitive is not vacuously red. A skip prints a clear reason.
//!
//! The sandbox under test confines FS *reads* to the explicit read roots and FS
//! *writes* to the explicit write roots, while still allowing the system libs a
//! real provider needs. We assert the read boundary here because it is the
//! sharpest secret-leak proof (an agent that cannot read `~/.aws/credentials`
//! cannot exfiltrate it).

use std::fs;
use std::path::Path;
use std::process::Stdio;

use ainb_hangar_sandbox::{Enforcement, SandboxPolicy, sandboxed_command};

/// Resolve `/bin/sh` (every supported OS has it) as the confined test program.
fn shell() -> &'static Path {
    Path::new("/bin/sh")
}

/// Build a policy that allows reads of the system roots a shell needs plus the
/// task workdir, and writes only to the workdir. The secret lives OUTSIDE every
/// allowed read root.
fn policy_for(workdir: &Path) -> SandboxPolicy {
    SandboxPolicy::confined_to(workdir)
}

#[test]
fn sandbox_blocks_reading_secret_outside_allowed_roots() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();

    // Allowed task workdir, and a secret placed in a SIBLING dir that is NOT an
    // allowed read root. Both are under the same tempdir so the only thing
    // separating them is the sandbox policy, not filesystem location.
    let workdir = root.join("task-workdir");
    let secrets_dir = root.join("user-secrets");
    fs::create_dir_all(&workdir).expect("mkdir workdir");
    fs::create_dir_all(&secrets_dir).expect("mkdir secrets");
    let secret = secrets_dir.join("credentials");
    fs::write(&secret, "AKIA_TOP_SECRET\n").expect("write secret");

    let policy = policy_for(&workdir);

    let mut cmd = match sandboxed_command(shell(), &policy) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: sandbox primitive unavailable: {e}");
            return;
        }
    };

    if cmd.enforcement() == Enforcement::None {
        eprintln!(
            "SKIP: no OS sandbox enforcement on this platform ({})",
            std::env::consts::OS
        );
        return;
    }

    // The shell tries to read the secret. Under enforcement this open is denied,
    // so `cat` fails and the script exits non-zero.
    let status = cmd
        .command()
        .arg("-c")
        .arg(format!("cat {}", secret.display()))
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn confined shell");

    assert!(
        !status.success(),
        "NEGATIVE leg failed: the sandbox MUST block reading a secret outside \
         the allowed read roots, but `cat {}` succeeded (status {status:?})",
        secret.display()
    );
}

#[test]
fn sandbox_allows_reading_inside_workdir() {
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path();
    let workdir = root.join("task-workdir");
    fs::create_dir_all(&workdir).expect("mkdir workdir");
    let allowed = workdir.join("input.txt");
    fs::write(&allowed, "legitimate task input\n").expect("write allowed");

    let policy = policy_for(&workdir);

    let mut cmd = match sandboxed_command(shell(), &policy) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: sandbox primitive unavailable: {e}");
            return;
        }
    };

    if cmd.enforcement() == Enforcement::None {
        eprintln!(
            "SKIP: no OS sandbox enforcement on this platform ({})",
            std::env::consts::OS
        );
        return;
    }

    // The same confined shell CAN read a file inside the allowed workdir.
    let status = cmd
        .command()
        .arg("-c")
        .arg(format!("cat {}", allowed.display()))
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn confined shell");

    assert!(
        status.success(),
        "POSITIVE leg failed: the sandbox MUST allow reading a file inside the \
         workdir, but `cat {}` was blocked (status {status:?})",
        allowed.display()
    );
}

#[test]
fn sandbox_blocks_writing_outside_workdir() {
    let tmp = tempfile::tempdir().expect("tmp");
    let workdir = tmp.path().join("task-workdir");
    fs::create_dir_all(&workdir).expect("mkdir workdir");

    // The write-block target must live OUTSIDE every write root. The default
    // policy makes the process temp dir writable (agents scribble there), so a
    // sibling tempdir is NOT a valid "outside" — use the operator's real home,
    // which is never a write root and is exactly the kind of path an escaped
    // agent would tamper with. We never actually create it; the sandbox must
    // refuse the write so it stays absent.
    let home = dirs_home();
    let target = home.join(".agents-in-a-box-hangar-sandbox-write-probe");
    // Defensive: ensure a stale probe from a previous crashed run is gone.
    let _ = fs::remove_file(&target);

    let policy = policy_for(&workdir);

    let mut cmd = match sandboxed_command(shell(), &policy) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: sandbox primitive unavailable: {e}");
            return;
        }
    };

    if cmd.enforcement() == Enforcement::None {
        eprintln!(
            "SKIP: no OS sandbox enforcement on this platform ({})",
            std::env::consts::OS
        );
        return;
    }

    let status = cmd
        .command()
        .arg("-c")
        .arg(format!("echo pwned > {}", target.display()))
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn confined shell");

    let wrote = target.exists();
    // Clean up before asserting so a failure does not leave the probe behind.
    let _ = fs::remove_file(&target);

    assert!(
        !status.success(),
        "NEGATIVE leg failed: the sandbox MUST block writing outside the \
         workdir, but `echo > {}` succeeded (status {status:?})",
        target.display()
    );
    assert!(
        !wrote,
        "the escape file must not exist after a blocked write"
    );
}

/// The operator's real home directory (the write-block target's parent). Falls
/// back to `/` if `$HOME` is unset (no realistic CI host, but keep it total).
fn dirs_home() -> std::path::PathBuf {
    std::env::var_os("HOME").map_or_else(|| std::path::PathBuf::from("/"), std::path::PathBuf::from)
}

#[test]
fn sandbox_allows_writing_inside_workdir() {
    let tmp = tempfile::tempdir().expect("tmp");
    let workdir = tmp.path().join("task-workdir");
    fs::create_dir_all(&workdir).expect("mkdir workdir");

    let policy = policy_for(&workdir);

    let mut cmd = match sandboxed_command(shell(), &policy) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIP: sandbox primitive unavailable: {e}");
            return;
        }
    };

    if cmd.enforcement() == Enforcement::None {
        eprintln!(
            "SKIP: no OS sandbox enforcement on this platform ({})",
            std::env::consts::OS
        );
        return;
    }

    let target = workdir.join("artifact.txt");
    let status = cmd
        .command()
        .arg("-c")
        .arg(format!("echo ok > {}", target.display()))
        .current_dir(&workdir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("spawn confined shell");

    assert!(
        status.success(),
        "POSITIVE leg failed: the sandbox MUST allow writing inside the workdir, \
         but `echo > {}` was blocked (status {status:?})",
        target.display()
    );
    assert!(
        target.exists(),
        "the artifact must exist after an allowed write"
    );
}
