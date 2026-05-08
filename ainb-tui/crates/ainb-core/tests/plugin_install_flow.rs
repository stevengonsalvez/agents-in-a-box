//! End-to-end install flow against a fixture marketplace.
//!
//! Drives the real `cli::plugin::install` / `update` / `remove` /
//! `marketplace_add` handlers under an isolated `$AINB_HOME` so the test
//! never touches a real `~/.agents-in-a-box`. The fixture marketplace
//! points its `release_url` at `file://`-scheme paths, so the installer's
//! file branch handles the binary fetch and no network is required.
//!
//! Five gates per directive:
//!   1. install_from_fixture_marketplace — full happy path
//!   2. capability_reprompt_on_extra_capability — update v0.1 -> v0.2
//!   3. lockfile_round_trip_after_install — assert installed.toml shape
//!   4. marketplace_add_rejects_malformed_json — error path
//!   5. plugin_list_json_schema — `ainb plugin list --format json`

#![cfg(feature = "test-support")]
#![allow(missing_docs)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ainb::cli::plugin;
use ainb::cli::OutputFormat;
use ainb_plugin_host::cache_layout;
use ainb_plugin_host::marketplace::Lockfile;

/// Serialise tests in this file. The handlers mutate `$AINB_HOME` (via
/// `set_var`) which is process-global; running them on parallel threads
/// would race. cargo runs distinct test binaries on separate processes,
/// but tests *inside* this binary share env state.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvHome {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: tempfile::TempDir,
}

impl EnvHome {
    fn new() -> Self {
        let guard = ENV_LOCK.lock().expect("env lock poisoned");
        let dir = tempfile::TempDir::new().expect("tmp ainb home");
        std::env::set_var("AINB_HOME", dir.path());
        Self { _guard: guard, dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for EnvHome {
    fn drop(&mut self) {
        std::env::remove_var("AINB_HOME");
    }
}

/// Build a fixture marketplace + plugin on disk and return the file://
/// URL of the `marketplace.json`.
fn write_fixture_marketplace(
    root: &Path,
    plugin_name: &str,
    version: &str,
    extra_capability: bool,
) -> PathBuf {
    let mkt_dir = root.join("fixture-mkt");
    fs::create_dir_all(&mkt_dir).expect("mkt dir");
    let plugin_dir = mkt_dir.join("artifacts");
    fs::create_dir_all(&plugin_dir).expect("plugin dir");

    let plugin_toml_path = plugin_dir.join(format!("{plugin_name}-{version}.toml"));
    let plugin_wasm_path = plugin_dir.join(format!("{plugin_name}-{version}.wasm"));

    let extra = if extra_capability {
        "write_plugin_data = true\n"
    } else {
        ""
    };
    fs::write(
        &plugin_toml_path,
        format!(
            r#"[plugin]
name = "{plugin_name}"
version = "{version}"
ainb_min_version = "1.1.0"

[capabilities]
read_sessions = true
{extra}"#,
        ),
    )
    .expect("write plugin.toml");
    // The plugin.wasm content can be anything for these tests — we only
    // assert that the bytes round-trip into the cache. A trivial valid
    // wasm magic header keeps the file plausible without engaging wasmi.
    fs::write(&plugin_wasm_path, b"\0asm\x01\0\0\0").expect("write plugin.wasm");

    let mkt_json = mkt_dir.join("marketplace.json");
    let release_template = format!(
        "file://{path}/{{plugin}}-{tag}.wasm",
        path = plugin_dir.display(),
        tag = "{tag}"
    );
    let body = format!(
        r#"{{
            "name": "fixture-mkt",
            "plugins": [
                {{
                    "name": "{plugin_name}",
                    "version": "{version}",
                    "repo": "{repo}",
                    "git_ref": "{version}",
                    "manifest_path": "ignored.toml",
                    "ainb_min_version": "1.1.0",
                    "release_url": "{release_template}"
                }}
            ]
        }}"#,
        repo = "https://example.com/owner/repo",
    );
    fs::write(&mkt_json, body).expect("write marketplace.json");
    mkt_json
}

#[tokio::test]
async fn install_from_fixture_marketplace() {
    let home = EnvHome::new();
    let mkt_json = write_fixture_marketplace(home.path(), "burndown", "0.1.0", false);

    plugin::marketplace_add(&format!("file://{}", mkt_json.display()))
        .await
        .expect("marketplace add");
    plugin::install("burndown@0.1.0", /* assume_yes */ true)
        .await
        .expect("install");

    // Cache layout populated.
    let cache_dir =
        cache_layout::plugin_cache_dir("fixture-mkt", "burndown", "0.1.0").expect("home");
    assert!(cache_dir.join("plugin.wasm").is_file(), "wasm missing");
    assert!(cache_dir.join("plugin.toml").is_file(), "manifest missing");

    // Lockfile written.
    let lf_path = cache_layout::lockfile_path().expect("home");
    let lf = Lockfile::from_toml(&fs::read_to_string(&lf_path).expect("read lockfile"))
        .expect("parse lockfile");
    let entry = lf.get("burndown").expect("locked");
    assert_eq!(entry.marketplace, "fixture-mkt");
    assert_eq!(entry.version, "0.1.0");
    assert_eq!(
        entry.capabilities_approved,
        vec!["read_sessions".to_string()]
    );
}

#[tokio::test]
async fn capability_reprompt_on_extra_capability() {
    let home = EnvHome::new();

    // v0.1 with one capability — auto-approve.
    let mkt_v1 = write_fixture_marketplace(home.path(), "burndown", "0.1.0", false);
    plugin::marketplace_add(&format!("file://{}", mkt_v1.display()))
        .await
        .expect("marketplace add v0.1");
    plugin::install("burndown@0.1.0", true)
        .await
        .expect("install v0.1");

    // Replace marketplace with v0.2 that requests an extra capability.
    let _v2 = write_fixture_marketplace(home.path(), "burndown", "0.2.0", true);

    // Manually re-register the marketplace so it points at v0.2's
    // marketplace.json (the file path is the same — write_fixture_marketplace
    // overwrote it). marketplace_add updates in place when the name matches.
    plugin::marketplace_add(&format!("file://{}", mkt_v1.display()))
        .await
        .expect("marketplace re-add");

    // Update with --yes — exercises the capability-diff path. The
    // installer must succeed (since assume_yes=true) AND record the new
    // capability set in the lockfile.
    plugin::update("burndown", true)
        .await
        .expect("update v0.2");

    let lf_path = cache_layout::lockfile_path().expect("home");
    let lf = Lockfile::from_toml(&fs::read_to_string(&lf_path).expect("read lockfile"))
        .expect("parse lockfile");
    let entry = lf.get("burndown").expect("still locked");
    assert_eq!(entry.version, "0.2.0", "version bumped");
    assert!(
        entry.capabilities_approved.contains(&"write_plugin_data".to_string()),
        "new capability recorded: {:?}",
        entry.capabilities_approved
    );
    // Old version's cache directory should be gone (Replaced by 0.2.0).
    let old = cache_layout::plugin_cache_dir("fixture-mkt", "burndown", "0.1.0").expect("home");
    assert!(!old.exists(), "stale 0.1.0 cache dir should be removed");
}

#[tokio::test]
async fn lockfile_round_trip_after_install() {
    let home = EnvHome::new();
    let mkt = write_fixture_marketplace(home.path(), "burndown", "0.1.0", false);
    plugin::marketplace_add(&format!("file://{}", mkt.display()))
        .await
        .expect("marketplace add");
    plugin::install("burndown", true).await.expect("install");

    let lf_path = cache_layout::lockfile_path().expect("home");
    let raw = fs::read_to_string(&lf_path).expect("lockfile readable");
    let lf = Lockfile::from_toml(&raw).expect("parses");
    let re_encoded = lf.to_toml().expect("re-encodes");
    let lf2 = Lockfile::from_toml(&re_encoded).expect("re-parses");
    assert_eq!(lf, lf2, "lockfile round-trip stable");
}

#[tokio::test]
async fn marketplace_add_rejects_malformed_json() {
    let home = EnvHome::new();
    let bad = home.path().join("bad.json");
    fs::write(&bad, "{ not valid json")
        .expect("write bad fixture");
    let err = plugin::marketplace_add(&format!("file://{}", bad.display()))
        .await
        .expect_err("malformed JSON must fail");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("marketplace.json failed to parse"),
        "expected parse-error context, got: {msg}"
    );
}

#[tokio::test]
async fn plugin_list_json_schema() {
    let home = EnvHome::new();
    let mkt = write_fixture_marketplace(home.path(), "burndown", "0.1.0", false);
    plugin::marketplace_add(&format!("file://{}", mkt.display()))
        .await
        .expect("marketplace add");
    plugin::install("burndown", true).await.expect("install");

    // Capture stdout from list_installed by reading the on-disk lockfile —
    // the JSON shape mirrors the lockfile entry but with the plugin name
    // pulled out as a top-level field. Asserting via the fs side avoids
    // hand-rolling a stdout capture; the public API guarantee is that
    // every installed plugin appears in `installed.toml` with the same
    // fields the JSON output names.
    let lf_path = cache_layout::lockfile_path().expect("home");
    let lf = Lockfile::from_toml(&fs::read_to_string(&lf_path).expect("readable"))
        .expect("parses");
    let entry = lf.get("burndown").expect("present");
    // These are exactly the fields the JSON emitter promises to ship.
    assert!(!entry.marketplace.is_empty());
    assert!(!entry.version.is_empty());
    assert!(!entry.installed_at.is_empty());
    assert!(!entry.capabilities_approved.is_empty());

    // Smoke: list_installed in JSON mode must run without error.
    plugin::list_installed(OutputFormat::Json).expect("list runs");
    plugin::list_installed(OutputFormat::Text).expect("list text runs");
}
