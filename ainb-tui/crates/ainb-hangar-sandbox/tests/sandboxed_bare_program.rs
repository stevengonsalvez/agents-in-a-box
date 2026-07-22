//! Regression tripwire for the class of bug where a bare provider name
//! (`claude`/`codex`/`copilot`) installed under a NON-system root cannot be
//! exec'd under the OS sandbox because the profile referenced the unresolved
//! relative name instead of the real PATH-resolved absolute binary.
//!
//! macOS-only: it drives a real `sandbox-exec` Seatbelt confinement end to end.
//! A bare-name program placed in a tempdir (under `/private/var/folders/…`,
//! outside every `SYSTEM_READ_ROOTS` entry) must still exec when that tempdir is
//! on `$PATH`. Before the fix the emitted rule was `(literal "prov")`, relative,
//! matching nothing the kernel resolves, and the sandboxed exec was denied.

#![cfg(target_os = "macos")]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use ainb_hangar_sandbox::{Enforcement, SandboxPolicy, sandboxed_command};

#[test]
fn sandbox_can_exec_bare_program_installed_under_non_system_root() {
    // A provider binary living outside every system read root (a tempdir under
    // /private/var/folders on macOS), reachable only by name via `$PATH`.
    let bin_dir = tempfile::tempdir().expect("temp bin dir");
    let prog = bin_dir.path().join("prov");
    std::fs::write(&prog, "#!/bin/sh\nexit 0\n").expect("write stub provider");
    std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755))
        .expect("chmod +x stub provider");

    // Put the tempdir on `$PATH` so a BARE name resolves to it; this is the
    // exact shape of the daemon's default `claude` on a `~/.local/bin` install.
    // Edition 2021: `set_var` is safe. This test is the sole test in its own
    // integration binary, so the process-global `$PATH` mutation races nothing.
    let orig_path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin_dir.path().to_path_buf()];
    paths.extend(std::env::split_paths(&orig_path));
    let joined = std::env::join_paths(paths).expect("join PATH");
    std::env::set_var("PATH", &joined);

    // A task root the child may read/write; confinement ON.
    let task_root = tempfile::tempdir().expect("task root");
    let policy = SandboxPolicy::confined_to(task_root.path());

    let built = sandboxed_command(Path::new("prov"), &policy).expect("build sandboxed command");

    // Only meaningful when the OS primitive actually enforces.
    if built.enforcement() != Enforcement::Enforced {
        std::env::set_var("PATH", &orig_path);
        eprintln!("SKIP: OS sandbox not enforcing on this host");
        return;
    }

    let status = built.into_inner().status().expect("spawn sandboxed provider");

    std::env::set_var("PATH", &orig_path);

    assert!(
        status.success(),
        "sandboxed exec of a bare-name provider under a non-system root must \
         succeed (exit {:?}); a failure is the unresolved-path regression",
        status.code()
    );
}
