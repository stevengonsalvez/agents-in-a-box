// ABOUTME: One rule, shared by every place that would re-execute THIS binary as
// a long-lived daemon: never do it from a cargo test harness (issue #715).

//! Refuse to self-exec a cargo test binary as a daemon.
//!
//! Several daemons are started by re-executing `current_exe()` with a
//! subcommand — `mcp daemon`, `hangar daemon run`, `notifyd run`. That is
//! correct for a real `ainb`, and catastrophic under `cargo test`, where
//! `current_exe()` is the TEST binary:
//!
//! * libtest reads the trailing argv as **name filters**, not a subcommand, so
//!   `<test-bin> mcp daemon` re-runs every test matching `mcp` or `daemon`
//!   instead of erroring out.
//! * Any of those re-run tests can reach the same spawn again, so the recursion
//!   is unbounded.
//! * The spawns use `process_group(0)`, so each copy is detached from the test
//!   runner and outlives it.
//!
//! Observed 2026-08-23: 135 orphaned test binaries, which wedged the worktree's
//! `target/debug/deps` badly enough that `readdir` on it never returned.

/// Is this process a cargo-built **test** binary rather than a real `ainb`?
///
/// Thin IO wrapper over [`is_cargo_test_exe`]; see it for why both halves of
/// the check are required.
pub fn running_under_cargo_test() -> bool {
    let cargo_env =
        std::env::var_os("CARGO").is_some() || std::env::var_os("CARGO_MANIFEST_DIR").is_some();
    let exe = std::env::current_exe().ok();
    is_cargo_test_exe(cargo_env, exe.as_deref())
}

/// Pure predicate behind [`running_under_cargo_test`], split out so the rule is
/// testable without faking the process environment.
///
/// BOTH halves are load-bearing, and each alone gives a wrong answer:
///
/// * `cargo` runs test binaries with `CARGO` / `CARGO_MANIFEST_DIR` set — but it
///   sets those for `cargo run` too, and a `cargo run` of the real binary
///   (`target/<profile>/ainb`) is a perfectly good daemon host. The env alone
///   would block a legitimate spawn.
/// * Test binaries live in `target/<profile>/deps/` — but so does the real bin
///   ARTIFACT (`deps/ainb-<hash>`, hard-linked up to `target/<profile>/ainb`).
///   The path alone would block someone running that artifact directly.
///
/// Only the conjunction — cargo is driving *and* the exe is the `deps/` copy —
/// identifies the test-harness case.
pub fn is_cargo_test_exe(cargo_env: bool, exe: Option<&std::path::Path>) -> bool {
    cargo_env && exe.and_then(std::path::Path::parent).is_some_and(|dir| dir.ends_with("deps"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn cargo_driven_deps_binary_is_a_test_binary() {
        assert!(is_cargo_test_exe(
            true,
            Some(Path::new("/w/target/debug/deps/ainb-39a2b8"))
        ));
        assert!(is_cargo_test_exe(
            true,
            Some(Path::new("/w/target/release/deps/ainb-6b08f1"))
        ));
    }

    /// `cargo run` sets the same env but executes the profile-root binary.
    #[test]
    fn cargo_run_of_the_real_binary_is_not_a_test_binary() {
        assert!(!is_cargo_test_exe(
            true,
            Some(Path::new("/w/target/debug/ainb"))
        ));
    }

    /// Running the `deps/` artifact by hand is a legitimate daemon host.
    #[test]
    fn deps_artifact_without_cargo_is_not_a_test_binary() {
        assert!(!is_cargo_test_exe(
            false,
            Some(Path::new("/w/target/debug/deps/ainb-39a2b8"))
        ));
    }

    #[test]
    fn installed_binary_is_not_a_test_binary() {
        assert!(!is_cargo_test_exe(
            false,
            Some(Path::new("/opt/homebrew/bin/ainb"))
        ));
        assert!(!is_cargo_test_exe(
            true,
            Some(Path::new("/opt/homebrew/bin/ainb"))
        ));
    }

    #[test]
    fn unresolvable_exe_is_not_a_test_binary() {
        assert!(!is_cargo_test_exe(true, None));
    }

    /// This very process is a cargo test binary, so the live probe must say so.
    /// Skipped when the binary is invoked outside cargo (env absent).
    #[test]
    fn live_probe_agrees_when_cargo_is_driving() {
        if std::env::var_os("CARGO").is_none() && std::env::var_os("CARGO_MANIFEST_DIR").is_none() {
            return;
        }
        assert!(running_under_cargo_test());
    }
}
