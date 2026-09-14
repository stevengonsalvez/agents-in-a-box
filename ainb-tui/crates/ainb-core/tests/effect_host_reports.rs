#![allow(missing_docs)]

// ABOUTME: The terminal effect executor reads state and never writes it. What
// its work changed reaches the reducer as report intents, so a second host
// dispatches the same reports instead of copying the executor's state writes.

use std::path::Path;

fn executor_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/effect_host.rs");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn the_executor_takes_state_read_only() {
    let source = executor_source();
    assert!(
        source.contains("state: &AppState,"),
        "execute reads state through a shared reference"
    );
    for forbidden in [
        "&mut AppState",
        "&mut App",
        "app.state",
        "use crate::app::{App",
    ] {
        assert!(
            !source.contains(forbidden),
            "the executor must not reach writable state: found `{forbidden}`"
        );
    }
}

#[test]
fn the_executor_hands_back_reports_not_notices() {
    let source = executor_source();
    for forbidden in [
        "add_error_notification",
        "add_success_notification",
        "add_warning_notification",
        "add_info_notification",
        "ui_needs_refresh",
    ] {
        assert!(
            !source.contains(forbidden),
            "a notice is the reducer's to post from a report: found `{forbidden}`"
        );
    }
    assert!(source.contains("-> impl std::future::Future<Output = Result<Vec<Intent>>>"));
}
