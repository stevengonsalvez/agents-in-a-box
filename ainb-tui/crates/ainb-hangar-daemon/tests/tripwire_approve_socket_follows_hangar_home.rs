//! The daemon must dial the SAME approve broker socket the waiting Claude hook
//! registers on.
//!
//! The hook resolves it through `ainb_plugin_notifyd::paths::Paths::from_home()`
//! (`$AINB_HANGAR_HOME`, else `$AINB_HOME`, else `~/.agents-in-a-box`). The
//! daemon used to resolve it independently from `$AINB_HOME` alone, so any stack
//! running under `$AINB_HANGAR_HOME` posted every Fleet interview answer to the
//! DEFAULT home's broker — a broker that had never seen the session. The answer
//! came back unmatched, Fleet recorded `Claude request no longer waiting`, and
//! the hook stayed blocked until its 600s timeout. Nothing caught it because
//! every other test overrides the socket path instead of resolving it.
//!
//! One test in its own binary: it mutates process env, so it must not share a
//! test process with anything that reads a home (no `ENV_LOCK` can protect
//! against a sibling that does not take it).

use std::path::PathBuf;

#[test]
fn approve_socket_follows_hangar_home_like_the_hook() {
    let home = tempfile::tempdir().expect("temp hangar home");
    // Prove the resolver, not the override.
    ainb_hangar_daemon::rpc::set_approve_socket_for_test(None);

    std::env::set_var("AINB_HANGAR_HOME", home.path());

    let daemon = ainb_hangar_daemon::rpc::approve_socket_path_for_test()
        .expect("daemon must resolve an approve socket");
    let hook = ainb_plugin_notifyd::paths::Paths::from_home()
        .expect("hook must resolve an approve socket")
        .approve_socket;

    std::env::remove_var("AINB_HANGAR_HOME");

    assert_eq!(
        daemon,
        PathBuf::from(home.path()).join("approve.sock"),
        "a set $AINB_HANGAR_HOME must own the daemon's approve socket"
    );
    assert_eq!(
        daemon, hook,
        "daemon and hook must resolve the same broker socket, or answers are \
         delivered to a broker that never saw the session"
    );
}
