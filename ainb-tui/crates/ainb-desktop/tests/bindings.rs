// ABOUTME: Desktop.ts is the TypeScript contract for what the webview sends and
// receives. It is generated from the Rust types and committed; this test
// regenerates it and fails on any difference, and CI runs it with the feature
// on (#1158).

#![cfg(feature = "typescript-bindings")]

use ainb_desktop::bindings::{DESKTOP_TS, typescript};

#[test]
fn desktop_ts_matches_the_rust_types() {
    let rendered = typescript().unwrap_or_else(|error| panic!("{error}"));
    if std::env::var_os("UPDATE_DESKTOP_TS").is_some() {
        std::fs::write(DESKTOP_TS, &rendered).expect("write Desktop.ts");
        return;
    }
    let committed = std::fs::read_to_string(DESKTOP_TS).unwrap_or_default();
    assert!(
        committed == rendered,
        "ainb-desktop/bindings/Desktop.ts is stale. Regenerate with\n  \
         UPDATE_DESKTOP_TS=1 cargo test -p ainb-desktop --features typescript-bindings --test bindings\n\
         and commit it."
    );
}
