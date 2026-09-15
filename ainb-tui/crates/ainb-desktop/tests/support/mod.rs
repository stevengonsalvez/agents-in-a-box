//! Shared by the desktop's integration tests.

/// One scratch HOME for the whole test binary, set before any host is built,
/// so every test in the binary sees the same isolated home and none of them
/// drops it from under another.
pub fn isolated_home() -> &'static std::path::Path {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        std::env::set_var("AINB_HOME", home.path());
        std::env::set_var("AINB_HANGAR_HOME", home.path().join(".agents-in-a-box"));
        home
    })
    .path()
}
