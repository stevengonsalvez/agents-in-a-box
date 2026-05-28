// ABOUTME: New-session component family — the redesigned 2-screen flow
// (`pick_repo` + `configure`) plus the thin top-level `dispatcher` that
// renders the right screen for the current `NewSessionStep`.
//
// Finding #10: `dispatcher.rs` was previously named `legacy.rs` (a Phase 6
// import-path holdover); the rename makes the filename match the actual
// responsibility.

pub mod configure;
pub mod dispatcher;
pub mod pick_repo;

// Re-export the public surface so existing callers (`layout.rs`,
// `components::mod`) keep compiling unchanged.
pub use dispatcher::NewSessionComponent;
