//! Safety-guard test for `scripts/skill-manager-sandbox.sh`.
//!
//! The sandbox launcher is a pure-bash `rm -rf` machine — by far the
//! riskiest untested surface in the SkillManager toolchain. A bad
//! refactor of its safety belts could `rm -rf /` or wipe a real
//! `$HOME`. This test shells out to the real script and pins the four
//! load-bearing guarantees:
//!
//!   1. `down --root /` is refused (non-zero, prints `refusing`) —
//!      never clobber the filesystem root.
//!   2. `up --root $HOME` is refused (non-zero, prints `refusing`) —
//!      never seed/overwrite the real home dir.
//!   3. `down` on a directory that lacks the `.ainb-sandbox-marker`
//!      sentinel is refused (non-zero) — never `rm -rf` a dir the
//!      script did not build.
//!   4. A clean `up` → `down` round-trip at a `/tmp` root succeeds
//!      (exit 0, sentinel + env.sh written, dir removed) and `down`
//!      is idempotent (a second `down` on the now-missing dir exits 0).
//!
//! Runs in plain `cargo test` scope. Needs only `bash` + `git` on PATH
//! (both present on every CI runner). No tmux. To run just this file:
//!
//! ```bash
//! cd ainb-tui
//! cargo test -p ainb --test sandbox_script_safety_guards
//! ```
//!
//! Wired into CI via the `ainb-hooks` job (see `.github/workflows/ci.yml`).
//!
//! Bead `ai-e7t` follow-up — hardening the sandbox teardown path.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Absolute path to the script under test. `CARGO_MANIFEST_DIR` is the
/// `ainb-core` crate dir; the repo root is three levels up.
fn script_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .join("scripts")
        .join("skill-manager-sandbox.sh")
}

fn bash_available() -> bool {
    Command::new("bash")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run the script with `args`, returning the full `Output`. `home`
/// overrides the child's `HOME` so the `$HOME` guard test can point at
/// a known, non-destructive value instead of the developer's real home.
fn run_script(args: &[&str], home: Option<&str>) -> Output {
    let script = script_path();
    assert!(
        script.exists(),
        "script under test not found at {}",
        script.display()
    );
    let mut cmd = Command::new("bash");
    cmd.arg(&script).args(args);
    if let Some(h) = home {
        cmd.env("HOME", h);
    }
    cmd.output()
        .unwrap_or_else(|e| panic!("failed to spawn {} {:?}: {e}", script.display(), args))
}

fn stderr_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// A unique `/tmp` root for the round-trip test so parallel test
/// processes never collide.
fn unique_tmp_root() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "ainb-sandbox-guardtest-{}-{}",
        std::process::id(),
        // Nanosecond clock keeps two calls in the same process distinct.
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    p
}

#[test]
fn down_root_slash_is_refused() {
    if !bash_available() {
        eprintln!("SKIP: bash not on PATH");
        return;
    }
    let out = run_script(&["down", "--root", "/"], None);
    assert!(
        !out.status.success(),
        "`down --root /` MUST exit non-zero — it would clobber the \
         filesystem root.\nstdout:\n{}\nstderr:\n{}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("refusing"),
        "`down --root /` must print a `refusing` message on stderr.\nstderr:\n{err}"
    );
}

#[test]
fn up_root_home_is_refused() {
    if !bash_available() {
        eprintln!("SKIP: bash not on PATH");
        return;
    }
    // Point the child's HOME at a throwaway tmp path so even a broken
    // guard cannot touch the developer's real home; the script compares
    // `--root` against `$HOME`, so set them equal.
    let fake_home = unique_tmp_root();
    let fake_home_str = fake_home.to_str().expect("utf-8 tmp path");
    let out = run_script(&["up", "--root", fake_home_str], Some(fake_home_str));
    assert!(
        !out.status.success(),
        "`up --root $HOME` MUST exit non-zero — it would seed over the \
         real home dir.\nstdout:\n{}\nstderr:\n{}",
        stdout_of(&out),
        stderr_of(&out)
    );
    let err = stderr_of(&out);
    assert!(
        err.contains("refusing"),
        "`up --root $HOME` must print a `refusing` message on stderr.\nstderr:\n{err}"
    );
    // Belt-and-braces: the guard must fire BEFORE any seeding, so the
    // fake-home dir must not have been created/populated by the script.
    assert!(
        !fake_home.exists(),
        "guard fired but the script still created {} — seeding ran before \
         the $HOME refusal",
        fake_home.display()
    );
}

#[test]
fn down_without_marker_is_refused() {
    if !bash_available() {
        eprintln!("SKIP: bash not on PATH");
        return;
    }
    // A real dir with no `.ainb-sandbox-marker` sentinel — the script
    // must refuse to `rm -rf` it.
    let dir = unique_tmp_root();
    std::fs::create_dir_all(&dir).expect("create unmarked tmp dir");
    // Drop a decoy file so the dir is non-empty — proves the refusal is
    // about the missing sentinel, not about an empty dir.
    std::fs::write(dir.join("important-user-data.txt"), b"do not delete me")
        .expect("write decoy file");
    let dir_str = dir.to_str().expect("utf-8 tmp path");

    let out = run_script(&["down", "--root", dir_str], None);

    // Read the verdict before any cleanup so a failing assert still
    // leaves a useful message; clean up the dir ourselves afterwards.
    let success = out.status.success();
    let err = stderr_of(&out);
    let still_there = dir.join("important-user-data.txt").exists();
    let _ = std::fs::remove_dir_all(&dir);

    assert!(
        !success,
        "`down` on a dir lacking the sentinel MUST exit non-zero.\nstderr:\n{err}"
    );
    assert!(
        err.contains(".ainb-sandbox-marker"),
        "refusal must name the missing `.ainb-sandbox-marker` sentinel.\nstderr:\n{err}"
    );
    assert!(
        still_there,
        "the script deleted an unmarked dir's contents — the sentinel \
         guard failed to protect user data"
    );
}

#[test]
fn up_down_round_trip_succeeds_and_is_idempotent() {
    if !bash_available() {
        eprintln!("SKIP: bash not on PATH");
        return;
    }
    if !git_available() {
        eprintln!("SKIP: git not on PATH (sandbox `up` seeds a bare git remote)");
        return;
    }

    let root = unique_tmp_root();
    let root_str = root.to_str().expect("utf-8 tmp path");

    // ── up ───────────────────────────────────────────────────────────
    let up = run_script(&["up", "--root", root_str], None);
    let up_ok = up.status.success();
    let up_err = stderr_of(&up);
    let up_out = stdout_of(&up);
    if !up_ok {
        let _ = std::fs::remove_dir_all(&root);
        panic!("`up --root <tmp>` failed.\nstdout:\n{up_out}\nstderr:\n{up_err}");
    }
    // Sentinel + env.sh must exist after a successful `up`.
    let marker_present = root.join(".ainb-sandbox-marker").exists();
    let env_present = root.join("env.sh").exists();
    let claude_present = root.join(".claude").join("skills").exists();
    if !(marker_present && env_present && claude_present) {
        let _ = std::fs::remove_dir_all(&root);
        panic!(
            "`up` succeeded but did not lay down the expected sandbox shape: \
             marker={marker_present} env.sh={env_present} .claude/skills={claude_present}"
        );
    }

    // ── down ─────────────────────────────────────────────────────────
    let down = run_script(&["down", "--root", root_str], None);
    let down_ok = down.status.success();
    let down_err = stderr_of(&down);
    if !down_ok {
        let _ = std::fs::remove_dir_all(&root);
        panic!("`down --root <tmp>` failed after a clean `up`.\nstderr:\n{down_err}");
    }
    let removed = !root.exists();
    if !removed {
        let _ = std::fs::remove_dir_all(&root);
        panic!(
            "`down` exited 0 but the sandbox dir still exists: {}",
            root.display()
        );
    }

    // ── idempotent down ──────────────────────────────────────────────
    // `down` on an already-removed dir must be a clean no-op (exit 0).
    let down_again = run_script(&["down", "--root", root_str], None);
    assert!(
        down_again.status.success(),
        "a second `down` on a missing dir must be idempotent (exit 0).\nstderr:\n{}",
        stderr_of(&down_again)
    );
    assert!(
        !root.exists(),
        "idempotent `down` must leave the dir absent"
    );
}
