#![allow(missing_docs)]

// ABOUTME: P6e-6, the flip, criterion 4. The capability is advertised by the
// build itself, so a process resolves the daemon's sessions table with no
// test-only switch anywhere, and `AINB_SESSION_SOURCE=file` still puts that
// process back on `sessions.json` without a re-release.
//
// Own binary: the process's session source is decided once, and `AINB_HOME` /
// `AINB_HANGAR_HOME` are process-wide. Nothing here calls
// `advertise_workspace_sessions_for_tests`: that is the point of the test, so
// it does not take `test-support` either.

use std::path::PathBuf;

use ainb::cli::util::{SESSION_SOURCE_ENV, SessionSource};
use ainb_hangar_proto::protocol::{CAP_WORKSPACE_SESSIONS, advertises};

#[path = "support/fake_session_daemon.rs"]
mod fake_session_daemon;
#[path = "support/fleet_hangar.rs"]
mod fleet_hangar;
use fake_session_daemon::{fake_daemon, ready_list};
use fleet_hangar::EnvGuard;

/// `AINB_SESSION_SOURCE` is process-wide and one of these tests sets it, so
/// the two that read it take turns.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A home with a hangar directory, as a daemon's would be.
fn home(root: &std::path::Path) -> (PathBuf, PathBuf) {
    let home = root.join("home");
    let hangar_home = root.join("hangar");
    std::fs::create_dir_all(home.join(".agents-in-a-box")).unwrap();
    std::fs::create_dir_all(hangar_home.join("hangar")).unwrap();
    (home, hangar_home)
}

/// The flip itself, read from the build: no daemon needed to say that this
/// binary speaks the capability.
#[test]
fn this_build_advertises_the_sessions_capability() {
    assert!(
        advertises(CAP_WORKSPACE_SESSIONS),
        "the flip did not reach the catalogue this binary was built from"
    );
}

/// Criterion 4: against a daemon that advertises the capability and has
/// reconciled, `resolve` answers `Daemon` with no test switch set.
#[test]
fn resolve_answers_daemon_with_no_test_switch() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let (home, hangar_home) = home(root.path());
    let _env = [
        EnvGuard::set("HOME", &home),
        EnvGuard::set("AINB_HOME", &home),
        EnvGuard::set("AINB_HANGAR_HOME", &hangar_home),
    ];
    // The kill switch is left exactly as the environment has it, which in a
    // test run is unset: the build is what decides, and that is what the flip
    // means.
    assert!(
        std::env::var(SESSION_SOURCE_ENV).is_err(),
        "this test says nothing while {SESSION_SOURCE_ENV} is set"
    );

    fake_daemon(&rt, &hangar_home, |_, n| (n == 0).then(ready_list));
    let source = rt.block_on(SessionSource::resolve());
    assert!(
        matches!(source, SessionSource::Daemon(_)),
        "the flipped build did not resolve the table: {source:?}"
    );
    rt.shutdown_background();
}

/// The rollback that needs no re-release: `AINB_SESSION_SOURCE=file` is read
/// before anything is dialled, so it wins over the advertised capability.
#[test]
fn the_kill_switch_still_forces_the_file_after_the_flip() {
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let root = tempfile::tempdir().unwrap();
    let (home, hangar_home) = home(root.path());
    let _env = [
        EnvGuard::set("HOME", &home),
        EnvGuard::set("AINB_HOME", &home),
        EnvGuard::set("AINB_HANGAR_HOME", &hangar_home),
        EnvGuard::set(SESSION_SOURCE_ENV, std::path::Path::new("file")),
    ];

    let source = rt.block_on(SessionSource::resolve());
    assert!(
        matches!(source, SessionSource::File),
        "the kill switch did not force the file: {source:?}"
    );
}
