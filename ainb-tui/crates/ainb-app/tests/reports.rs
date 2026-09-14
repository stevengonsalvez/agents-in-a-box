// ABOUTME: Report commands: a host tells the reducer what it measured or how
// something it ran ended, and the reducer, not the host, changes state.

use ainb_app::app::NoRenderer;
use ainb_app::app::reports;
use ainb_app::{AppState, Keymap, SectionId, dispatch};

fn bumped(before: &[u64], after: &[u64]) -> Vec<SectionId> {
    SectionId::ALL
        .into_iter()
        .filter(|id| before[id.index()] != after[id.index()])
        .collect()
}

/// One scratch `HOME` for the binary, so the migration's save lands nowhere
/// real.
fn isolated_home() {
    static HOME: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("scratch home");
        std::env::set_var("HOME", home.path());
        home
    });
}

#[test]
fn the_width_report_turns_saved_column_counts_into_fractions_of_the_host() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.config.app_config.ui_preferences.home_sidebar_width = Some(40);
    state.config.app_config.ui_preferences.skill_manager_sources_width = Some(32);
    let before = state.versions();

    let effects = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        reports::migrate_layout_widths(160),
    );

    assert!(effects.is_empty());
    let prefs = &state.config.app_config.ui_preferences;
    assert_eq!(prefs.home_sidebar_fraction, Some(0.25));
    assert_eq!(prefs.skill_manager_sources_fraction, Some(0.2));
    assert_eq!(
        (prefs.home_sidebar_width, prefs.skill_manager_sources_width),
        (None, None)
    );
    assert_eq!(bumped(&before, &state.versions()), vec![SectionId::Config]);
}

#[test]
fn the_width_report_with_nothing_to_migrate_changes_nothing() {
    isolated_home();
    let keymap = Keymap::defaults();
    let mut state = AppState::new();
    state.config.app_config.ui_preferences.home_sidebar_width = None;
    state.config.app_config.ui_preferences.skill_manager_sources_width = None;
    let before = state.versions();

    let _ = dispatch(
        &mut state,
        &keymap,
        &mut NoRenderer,
        reports::migrate_layout_widths(160),
    );

    assert!(bumped(&before, &state.versions()).is_empty());
}
