//! Cross-process regression coverage for config.toml read-modify-write saves.

use std::env;
use std::fs;
use std::process::Command;

use ainb::config::AppConfig;

const WORKER_ENV: &str = "AINB_CONFIG_CONCURRENT_SAVE_WORKER";

#[test]
fn concurrent_config_writers_preserve_both_keys_and_share_lock_file() {
    if let Ok(worker) = env::var(WORKER_ENV) {
        run_worker(&worker);
        return;
    }

    let home = tempfile::tempdir().expect("temporary home");
    let test_binary = env::current_exe().expect("test binary path");
    let test_name = "concurrent_config_writers_preserve_both_keys_and_share_lock_file";

    let mut core = Command::new(&test_binary)
        .args(["--exact", test_name, "--nocapture"])
        .env("HOME", home.path())
        .env(WORKER_ENV, "core")
        .spawn()
        .expect("start core config writer");
    let mut external = Command::new(&test_binary)
        .args(["--exact", test_name, "--nocapture"])
        .env("HOME", home.path())
        .env(WORKER_ENV, "external")
        .spawn()
        .expect("start external config writer");

    assert!(core.wait().expect("wait core writer").success());
    assert!(external.wait().expect("wait external writer").success());

    let config_path = home.path().join(".agents-in-a-box/config/config.toml");
    let config: toml::Value = fs::read_to_string(&config_path)
        .expect("read config saved by workers")
        .parse()
        .expect("parse config saved by workers");
    assert_eq!(
        config["general"]["syntax_highlight"].as_bool(),
        Some(false),
        "AppConfig::save value survives the external writer"
    );
    assert_eq!(
        config["skills"]["catalog_release"].as_str(),
        Some("concurrent-test"),
        "external key survives AppConfig::save"
    );
    assert!(
        config_path.with_file_name("config.toml.lock").exists(),
        "all config.toml writers use one shared lock filename"
    );
}

fn run_worker(worker: &str) {
    for _ in 0..50 {
        match worker {
            "core" => {
                let mut config = AppConfig::default();
                config.general.syntax_highlight = false;
                config.save().expect("save core config value");
            }
            "external" => AppConfig::save_external_keys(&[(
                "skills.catalog_release".to_string(),
                "concurrent-test".to_string(),
            )])
            .expect("save external config value"),
            other => panic!("unknown concurrent-save worker: {other}"),
        }
    }
}
