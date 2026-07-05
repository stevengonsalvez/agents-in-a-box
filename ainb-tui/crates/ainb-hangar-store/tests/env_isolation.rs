//! Env-isolation tests for the `with_isolated_home` test helper.
//!
//! These prove that `Store::open_default` honours the `AINB_HANGAR_HOME`
//! override and that the helper restores the prior env value, so parallel
//! `cargo test --test-threads=N` runs never leak state into each other or into
//! the real `$HOME`. The shared `ENV_LOCK` mutex serialises the env mutation.

use ainb_hangar_store::Store;
use ainb_hangar_store::test_support::{lock_env, with_isolated_home, with_isolated_home_locked};

#[test]
fn home_helper_redirects_db_to_tempdir() {
    // The override directory points at a fresh tempdir, so the db cannot exist
    // before the open — capture that the redirect, not a stale file, is what we
    // observe. Also snapshot whether the real `$HOME/.agents-in-a-box/hangar.db` exists so
    // the negative control can prove the open did not freshly create it.
    let real_home_db =
        dirs::home_dir().expect("home dir").join(".agents-in-a-box").join("hangar.db");
    let real_home_db_existed_before = real_home_db.exists();

    with_isolated_home(|home| {
        let expected = home.join("hangar.db");
        assert!(
            !expected.exists(),
            "db must not exist before open (fresh tempdir): {expected:?}"
        );

        // Build our own runtime inside the helper so the env override is held
        // for the whole async open.
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let store = rt.block_on(Store::open_default()).expect("open default store");

        // Contract (P0.md:222 — the P0.7 daemon tripwire): when
        // `AINB_HANGAR_HOME` is set, it is the directory that DIRECTLY holds
        // `hangar.db` — no `.agents-in-a-box` segment is appended. The `.agents-in-a-box` sub-dir is
        // only used on the real-`$HOME` fallback path.
        assert!(
            expected.exists(),
            "db should live directly under isolated home at {expected:?}"
        );

        // Negative control: the db must NOT have been created under the
        // spec-divergent `.agents-in-a-box` sub-path of the override.
        let divergent = home.join(".agents-in-a-box").join("hangar.db");
        assert!(
            !divergent.exists(),
            "override branch must not append .agents-in-a-box (found db at {divergent:?})"
        );

        drop(store);
    });

    // Negative control: opening with the override active must not have created
    // a db in the real `$HOME/.agents-in-a-box`. If it did not exist before, it must not
    // exist now (a pre-existing real db on the dev box is left untouched and is
    // not a failure).
    if !real_home_db_existed_before {
        assert!(
            !real_home_db.exists(),
            "override open must not write to the real {real_home_db:?}"
        );
    }
}

#[test]
fn helper_restores_prior_env_value_after_run() {
    // This test mutates `AINB_HANGAR_HOME` at process scope (the set_var /
    // remove_var below run OUTSIDE `with_isolated_home`, which only holds the
    // lock for its closure). Hold `ENV_LOCK` across the whole test so a parallel
    // env-mutating test in this binary cannot clobber the sentinel mid-flight.
    // Per P0.md:78 ("no env mutation outside with_isolated_home") this is the
    // one sanctioned exception, and it is serialised by the same lock. We must
    // use `with_isolated_home_locked` (not `with_isolated_home`) below because
    // `ENV_LOCK` is a non-reentrant `std::sync::Mutex` — re-locking it while
    // holding `guard` would deadlock.
    let guard = lock_env();

    let key = Store::home_env();
    let prior = std::env::var_os(key);

    // Set a sentinel before, ensure it is restored after.
    let sentinel = "/tmp/ainb-hangar-sentinel-value";
    std::env::set_var(key, sentinel);

    with_isolated_home_locked(&guard, |home| {
        // Inside, the var points at the tempdir, not the sentinel.
        let current = std::env::var(key).expect("var set inside");
        assert_eq!(current, home.to_string_lossy());
    });

    // After, the sentinel is back.
    assert_eq!(
        std::env::var(key).as_deref(),
        Ok(sentinel),
        "prior AINB_HANGAR_HOME value must be restored"
    );

    // Restore whatever the env was before this test ran (not unconditionally
    // remove) so we leave the process env exactly as we found it.
    match prior {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}
