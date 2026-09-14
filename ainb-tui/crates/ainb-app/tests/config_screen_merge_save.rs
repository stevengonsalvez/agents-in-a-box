// ABOUTME: Regression test for issue #987: two TUIs that loaded the same
// config.toml each save a DIFFERENT setting from the Config screen, and both
// changes must survive on disk.
//
// The second writer's in-memory `AppConfig` is the snapshot it loaded at
// startup. Saving it whole wrote every key it did not touch back with its
// stale value, so the first writer's change was reverted. The test runs in a
// child process with `HOME` pointed at a tempdir, because the config path is
// resolved from `HOME` and mutating the environment of the test harness
// itself would race other tests.

use std::env;
use std::fs;
use std::process::Command;

use ainb_app::AppState;
use ainb_app::app::EventHandler;
use ainb_app::app::events::AppEvent;
use ainb_app::app::state::ConfigScreenState;
use ainb_app::config::AppConfig;
use ainb_app::config::settings_model::ConfigValue;

const WORKER_ENV: &str = "AINB_CONFIG_MERGE_SAVE_WORKER";
const TEST_NAME: &str = "two_config_screens_saving_different_settings_keep_both";

const INITIAL: &str = "\
# user comment that a key-level save keeps
[workspace_defaults]
branch_prefix = \"agents/\"
scan_max_depth = 3
";

#[test]
fn two_config_screens_saving_different_settings_keep_both() {
    if env::var_os(WORKER_ENV).is_some() {
        run_worker();
        return;
    }

    let home = tempfile::tempdir().expect("temporary home");
    let config_path = home.path().join(".agents-in-a-box/config/config.toml");
    fs::create_dir_all(config_path.parent().unwrap()).expect("config dir");
    fs::write(&config_path, INITIAL).expect("seed config.toml");

    let output = Command::new(env::current_exe().expect("test binary path"))
        .args(["--exact", TEST_NAME, "--nocapture", "--test-threads=1"])
        .env("HOME", home.path())
        .env(WORKER_ENV, "1")
        .output()
        .expect("run worker");
    assert!(
        output.status.success(),
        "worker failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let saved = fs::read_to_string(&config_path).expect("read saved config");
    let config: toml::Value = saved.parse().expect("saved config parses");
    assert_eq!(
        config["workspace_defaults"]["branch_prefix"].as_str(),
        Some("g6a/"),
        "writer A's branch_prefix must survive writer B's save:\n{saved}"
    );
    assert_eq!(
        config["workspace_defaults"]["scan_max_depth"].as_integer(),
        Some(4),
        "writer B's scan_max_depth must be written:\n{saved}"
    );
    assert!(
        saved.contains("# user comment that a key-level save keeps"),
        "the save must edit keys in place, not rewrite the file:\n{saved}"
    );
}

/// A TUI as it stands after startup: config loaded, Config screen seeded.
fn tui_from_disk() -> AppState {
    let snapshot = AppConfig::load().expect("load config");
    let mut state = AppState::new();
    state.config.config_screen_state = ConfigScreenState::from_app_config(&snapshot);
    state.config.app_config = snapshot;
    state
}

fn run_worker() {
    // Both TUIs start before either saves, so both hold the same snapshot.
    let mut a = tui_from_disk();
    let mut b = tui_from_disk();

    a.config.config_screen_state.set_row_value(
        "workspace_defaults.branch_prefix",
        ConfigValue::Text("g6a/".to_string()),
    );
    EventHandler::process_event(AppEvent::ConfigSaveAll, &mut a);

    b.config
        .config_screen_state
        .set_row_value("workspace_defaults.scan_max_depth", ConfigValue::Number(4));
    EventHandler::process_event(AppEvent::ConfigSaveAll, &mut b);

    // A restart reads both values back.
    let restarted = AppConfig::load().expect("reload config");
    assert_eq!(restarted.workspace_defaults.branch_prefix, "g6a/");
    assert_eq!(restarted.workspace_defaults.scan_max_depth, 4);
}

const CLEAR_WORKER_ENV: &str = "AINB_CONFIG_CLEAR_SAVE_WORKER";
const CLEAR_TEST_NAME: &str = "clearing_an_optional_setting_removes_only_that_key";

/// Clearing an optional row (`docker.host`) removes that key from disk, and
/// a second TUI confirming a row without changing it writes nothing, so a
/// value another writer saved in between survives.
#[test]
fn clearing_an_optional_setting_removes_only_that_key() {
    if env::var_os(CLEAR_WORKER_ENV).is_some() {
        run_clear_worker();
        return;
    }

    let home = tempfile::tempdir().expect("temporary home");
    let config_path = home.path().join(".agents-in-a-box/config/config.toml");
    fs::create_dir_all(config_path.parent().unwrap()).expect("config dir");
    fs::write(
        &config_path,
        format!("{INITIAL}\n[docker]\nhost = \"unix:///var/run/other.sock\"\n"),
    )
    .expect("seed config.toml");

    let output = Command::new(env::current_exe().expect("test binary path"))
        .args([
            "--exact",
            CLEAR_TEST_NAME,
            "--nocapture",
            "--test-threads=1",
        ])
        .env("HOME", home.path())
        .env(CLEAR_WORKER_ENV, "1")
        .output()
        .expect("run worker");
    assert!(
        output.status.success(),
        "worker failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let saved = fs::read_to_string(&config_path).expect("read saved config");
    let config: toml::Value = saved.parse().expect("saved config parses");
    assert!(
        config.get("docker").and_then(|docker| docker.get("host")).is_none(),
        "the cleared optional key must be removed:\n{saved}"
    );
    assert_eq!(
        config["workspace_defaults"]["branch_prefix"].as_str(),
        Some("from-another-writer/"),
        "confirming an unchanged row must not write the startup value back:\n{saved}"
    );
}

fn run_clear_worker() {
    let mut a = tui_from_disk();
    let mut b = tui_from_disk();

    a.config
        .config_screen_state
        .set_row_value("docker.host", ConfigValue::Text(String::new()));
    EventHandler::process_event(AppEvent::ConfigSaveAll, &mut a);

    // Another writer changes branch_prefix after B loaded its snapshot.
    ainb_app::config::write_keys_into(
        &AppConfig::get_user_config_dir().unwrap().join("config.toml"),
        &[(
            "workspace_defaults.branch_prefix".to_string(),
            toml::Value::String("from-another-writer/".to_string()),
        )],
    )
    .expect("another writer");

    // B opens the Branch Prefix editor and presses enter on the value it shows.
    b.config.config_screen_state.set_row_value(
        "workspace_defaults.branch_prefix",
        ConfigValue::Text("agents/".to_string()),
    );
    EventHandler::process_event(AppEvent::ConfigSaveAll, &mut b);
}
