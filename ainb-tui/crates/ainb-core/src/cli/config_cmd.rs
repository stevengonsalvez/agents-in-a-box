// ABOUTME: CLI config command for viewing and modifying application configuration
//
// Subcommands:
//   show:  Display full merged config (TOML or JSON)
//   get:   Get a specific value using dot-notation
//   set:   Set a value in user-level config
//   reset: Reset user config to defaults
//   path:  Show config file locations with existence markers
//   edit:  Open user config in $EDITOR

use anyhow::{Context, Result, anyhow};
use clap::Subcommand;
use serde::Serialize;
use std::fs;
use std::io::{self, Write as _};
use std::process::Command;

use super::OutputFormat;
use crate::config::AppConfig;
use crate::config::registry::{navigate_toml, set_validated};

/// JSON output for the `path` subcommand
#[derive(Debug, Serialize)]
struct ConfigPathEntry {
    scope: String,
    path: String,
    exists: bool,
}

#[derive(Subcommand)]
pub enum ConfigCommands {
    /// Display current configuration (merged from all sources)
    Show,
    /// Get a specific config value using dot-notation (e.g., `authentication.default_model`)
    Get {
        /// Config key in dot-notation
        key: String,
    },
    /// Set a config value in user-level config
    Set {
        /// Config key in dot-notation
        key: String,
        /// Value to set
        value: String,
    },
    /// Reset user configuration to defaults
    Reset {
        /// Skip confirmation prompt
        #[arg(long, short)]
        force: bool,
    },
    /// Show config file locations
    Path,
    /// Open user config in $EDITOR
    Edit,
}

/// Execute a config subcommand
pub async fn execute(command: ConfigCommands, format: OutputFormat) -> Result<()> {
    match command {
        ConfigCommands::Show => cmd_show(format).await,
        ConfigCommands::Get { key } => cmd_get(&key, format).await,
        ConfigCommands::Set { key, value } => cmd_set(&key, &value).await,
        ConfigCommands::Reset { force } => cmd_reset(force),
        ConfigCommands::Path => cmd_path(format),
        ConfigCommands::Edit => cmd_edit(),
    }
}

/// Display the full merged configuration.
///
/// Includes the `hangar_daemon.*` knobs, which are NOT in config.toml: they live
/// in the Hangar daemon's SQLite table. `show` used to be silent about them, so
/// a key `ainb config set` accepts did not appear in the dump of "the merged
/// config", which reads as the key not existing.
async fn cmd_show(format: OutputFormat) -> Result<()> {
    let config = AppConfig::load().context("Failed to load configuration")?;
    let daemon = hangar_daemon_values().await;

    match format {
        OutputFormat::Json => {
            let mut json =
                serde_json::to_value(&config).context("Failed to serialize config as JSON")?;
            if let Some(object) = json.as_object_mut() {
                object.insert(
                    "hangar_daemon".to_string(),
                    serde_json::Value::Object(
                        daemon
                            .iter()
                            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                            .collect(),
                    ),
                );
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&json)
                    .context("Failed to serialize config as JSON")?
            );
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            let toml_str =
                toml::to_string_pretty(&config).context("Failed to serialize config as TOML")?;
            println!("{toml_str}");
            // A comment block, not a `[hangar_daemon]` table: the text form is
            // valid TOML a user may paste back into config.toml, where these
            // keys do nothing at all.
            println!("# Hangar daemon knobs (stored in hangar.db, NOT in this file).");
            println!("# Read/write with `ainb config get|set hangar_daemon.<key>`.");
            for (key, value) in &daemon {
                println!("#   hangar_daemon.{key} = {value}");
            }
        }
    }

    Ok(())
}

/// Every Hangar daemon knob and its effective value (stored, else the coded
/// default). Falls back to the coded defaults when the database is missing or
/// unreadable, because `show` must never fail on an optional backend.
async fn hangar_daemon_values() -> Vec<(String, String)> {
    use ainb_hangar_core::daemon_config::DAEMON_CONFIG_REGISTRY;
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    let store = ainb_hangar_store::Store::open_default().await.ok();
    let mut out = Vec::with_capacity(DAEMON_CONFIG_REGISTRY.len());
    for descriptor in DAEMON_CONFIG_REGISTRY {
        let stored = match &store {
            Some(store) => DaemonConfigRepo::get(store.pool(), descriptor.key).await.ok().flatten(),
            None => None,
        };
        out.push((
            descriptor.key.to_string(),
            stored.unwrap_or_else(|| descriptor.default.to_string()),
        ));
    }
    out
}

/// Get a specific config value by dot-notation key
async fn cmd_get(key: &str, format: OutputFormat) -> Result<()> {
    // Same routing as `cmd_set`. Without it, the very sequence
    // `example.config.toml` prescribes ("config set hangar_daemon.x, then read
    // it back") wrote successfully and then errored with "Key not found",
    // which reads as the write having failed.
    if let Some(daemon_key) = crate::config::registry::hangar_daemon_key(key) {
        return get_hangar_daemon(key, daemon_key, format).await;
    }

    let config = AppConfig::load().context("Failed to load configuration")?;
    let toml_value =
        toml::Value::try_from(&config).context("Failed to convert config to TOML value")?;

    let value = navigate_toml(&toml_value, key)?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&toml_to_json(value))
                .context("Failed to serialize value as JSON")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            print_toml_value(value);
        }
    }

    Ok(())
}

/// Set a config value in the user-level config file
async fn cmd_set(key: &str, value: &str) -> Result<()> {
    let config_dir = AppConfig::get_user_config_dir()?;
    fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");

    // A `hangar_daemon.*` key is not in this file: its backend is the Hangar
    // daemon's `daemon_config` SQLite table. Writing it here would produce a
    // `[hangar_daemon]` section nothing ever reads, which is exactly the silent
    // no-op the registry exists to stop.
    if let Some(daemon_key) = crate::config::registry::hangar_daemon_key(key) {
        return set_hangar_daemon(key, daemon_key, value).await;
    }

    // Validate against CONFIG_REGISTRY first: a mistyped key or an out-of-range
    // value fails here rather than landing in the file and being dropped by the
    // next load, which is how a `set` could look like it worked and do nothing.
    // `read_existing` maps only "not there" to empty: a present-but-unreadable
    // file must abort rather than be replaced by a fresh one.
    let existing = crate::config::read_existing(&config_path)?;
    let mut probe = if existing.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        existing.parse::<toml::Value>().context("Failed to parse user config")?
    };
    // `[usage]` is owned by the burndown plugin. Its rows are registered and
    // validated, so without this the value passes every check and then dies
    // three calls later on "is in a section ainb-core must not write" — a
    // correct refusal with nothing actionable in it.
    if crate::config::is_burndown_owned(key) {
        anyhow::bail!(
            "'{key}' is owned by the burndown plugin — set it with `ainb burndown plan set`"
        );
    }
    set_validated(&mut probe, key, value)?;

    // An emptied optional key is a REMOVAL, not a value: `set_validated` drops
    // it from `probe` rather than storing `""`, because `Some("")` is not the
    // same as unset (an empty `docker.host` means "connect to nothing", not
    // "autodetect"). Looking it up unconditionally then failed with "Key not
    // found" and wrote nothing, so there was no way to clear one at all.
    let validated = match crate::config::registry::navigate_toml(&probe, key) {
        Ok(value) => Some(value.clone()),
        Err(_) => None,
    };

    // Write through the shared key-level writer, which edits the document in
    // place. Serializing `probe` instead would be a whole-file rewrite that
    // deletes every comment — and this file is meant to be started from
    // `config/example.config.toml`, which is ~320 lines of comments explaining
    // the keys. `ainb config set docker.timeout 90` must change one line, not
    // strip the manual.
    let cleared = validated.is_none();
    match validated {
        Some(value) => {
            crate::config::write_keys_into(&config_path, &[(key.to_string(), value)])
                .context("Failed to write user config")?;
        }
        None => {
            crate::config::remove_key_from(&config_path, key)
                .context("Failed to write user config")?;
        }
    }

    // Keyed off what actually happened: only an OPTIONAL_KEYS row is removed
    // when emptied. `skills.api_key = ""` stores an empty string, and calling
    // that "Cleared" would be a lie.
    // The promoted tunables read a process-wide snapshot; refresh it so a
    // long-lived embedding of this crate does not keep serving the old value.
    // Free in the one-shot CLI, where the process exits next.
    crate::config::tunables::refresh_snapshot();

    if cleared {
        println!("Cleared {key}");
    } else {
        println!("Set {key} = {value}");
    }
    println!("Saved to {}", config_path.display());

    Ok(())
}

/// Read one knob back out of the Hangar daemon's `daemon_config` table.
///
/// A key with no stored row prints its coded default, which is what the daemon
/// actually runs: reporting "not found" would be a claim about the daemon's
/// behaviour that is not true.
async fn get_hangar_daemon(key: &str, daemon_key: &str, format: OutputFormat) -> Result<()> {
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    let descriptor = ainb_hangar_core::daemon_config::descriptor(daemon_key)
        .ok_or_else(|| anyhow!("'{key}' is not a Hangar daemon config key"))?;
    let store = ainb_hangar_store::Store::open_default()
        .await
        .context("open the Hangar database")?;
    let stored = DaemonConfigRepo::get(store.pool(), daemon_key)
        .await
        .with_context(|| format!("read daemon_config `{daemon_key}`"))?;
    let value = stored.as_deref().unwrap_or(descriptor.default);

    match format {
        OutputFormat::Json => {
            let json = serde_json::json!({
                "key": key,
                "value": value,
                "is_default": stored.is_none(),
                "default": descriptor.default,
                "source": "hangar daemon_config",
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&json).context("serialize value as JSON")?
            );
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => println!("{value}"),
    }
    Ok(())
}

/// Write one knob to the Hangar daemon's `daemon_config` table.
///
/// Validated by the daemon's OWN descriptor rather than by `CONFIG_REGISTRY`:
/// that table is the daemon's authority on what it accepts, and it is the same
/// gate the `hangar/daemon_config_set` RPC and `ainb hangar daemon config set`
/// pass. `CONFIG_REGISTRY` still owns the row's label, help and search entry.
///
/// Opening the store creates the database if it is missing, which is correct
/// here and only here: the user has explicitly asked for a value to be stored.
async fn set_hangar_daemon(key: &str, daemon_key: &str, value: &str) -> Result<()> {
    use ainb_hangar_store::repo::daemon_config::DaemonConfigRepo;

    let descriptor = ainb_hangar_core::daemon_config::descriptor(daemon_key)
        .ok_or_else(|| anyhow!("'{key}' is not a Hangar daemon config key"))?;
    let normalized = descriptor.validate(value).map_err(|why| anyhow!(why))?;
    let store = ainb_hangar_store::Store::open_default()
        .await
        .context("open the Hangar database")?;
    DaemonConfigRepo::set(store.pool(), daemon_key, &normalized)
        .await
        .with_context(|| format!("write daemon_config `{daemon_key}`"))?;
    println!("Set {key} = {normalized}");
    println!("Saved to the Hangar daemon database (takes effect on its next tick)");
    Ok(())
}

/// Reset user configuration to defaults
fn cmd_reset(force: bool) -> Result<()> {
    let config_dir = AppConfig::get_user_config_dir()?;
    let config_path = config_dir.join("config.toml");

    if !config_path.exists() {
        println!("No user config file found. Nothing to reset.");
        return Ok(());
    }

    if !force {
        print!("Reset user config at {}? [y/N] ", config_path.display());
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    fs::remove_file(&config_path).context("Failed to remove user config")?;

    println!("User config reset to defaults.");
    println!("Removed: {}", config_path.display());

    Ok(())
}

/// Show all config file locations with existence markers
fn cmd_path(format: OutputFormat) -> Result<()> {
    let paths = AppConfig::get_config_paths();

    let scopes = ["project", "user", "system"];

    match format {
        OutputFormat::Json => {
            let entries: Vec<ConfigPathEntry> = paths
                .iter()
                .zip(scopes.iter())
                .map(|(path, scope)| ConfigPathEntry {
                    scope: (*scope).to_string(),
                    path: path.display().to_string(),
                    exists: path.exists(),
                })
                .collect();
            let json = serde_json::to_string_pretty(&entries)
                .context("Failed to serialize config paths")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            let labels = ["Project config", "User config", "System config"];

            println!("Configuration file locations (highest precedence first):");
            println!("{}", "\u{2501}".repeat(60));

            for (i, path) in paths.iter().enumerate() {
                let exists = path.exists();
                let marker = if exists { "\u{2713}" } else { "\u{2717}" };
                let label = labels.get(i).unwrap_or(&"Config");
                println!("  {marker} {label}: {}", path.display());
            }

            println!();
            println!("Use 'ainb config edit' to open the user config in your editor.");
        }
    }

    Ok(())
}

/// Open user config in $EDITOR
fn cmd_edit() -> Result<()> {
    let config_dir = AppConfig::get_user_config_dir()?;
    fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");

    // Create with defaults if missing
    if !config_path.exists() {
        let default_config = AppConfig::default();
        let content = toml::to_string_pretty(&default_config)
            .context("Failed to serialize default config")?;
        crate::config::write_atomic(&config_path, &content)
            .context("Failed to create default config file")?;
        println!("Created default config at {}", config_path.display());
    }

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());

    println!("Opening {} with {editor}...", config_path.display());

    let status = Command::new(&editor)
        .arg(&config_path)
        .status()
        .with_context(|| format!("Failed to launch editor '{editor}'"))?;

    if !status.success() {
        return Err(anyhow!("Editor exited with non-zero status"));
    }

    Ok(())
}

/// Print a TOML value in human-readable text format
fn print_toml_value(value: &toml::Value) {
    match value {
        toml::Value::String(s) => println!("{s}"),
        toml::Value::Integer(i) => println!("{i}"),
        toml::Value::Float(f) => println!("{f}"),
        toml::Value::Boolean(b) => println!("{b}"),
        toml::Value::Array(arr) => {
            for item in arr {
                print_toml_value(item);
            }
        }
        toml::Value::Table(_) | toml::Value::Datetime(_) => {
            // For complex values, fall back to TOML representation
            let s = toml::to_string_pretty(value).unwrap_or_else(|_| format!("{value:?}"));
            print!("{s}");
        }
    }
}

/// Convert a `toml::Value` to `serde_json::Value` for JSON output
fn toml_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(i) => serde_json::json!(i),
        toml::Value::Float(f) => serde_json::json!(f),
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let map: serde_json::Map<String, serde_json::Value> =
                table.iter().map(|(k, v)| (k.clone(), toml_to_json(v))).collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // The dotted-path helpers now live next to the registry that validates
    // against them; these tests still pin their behaviour from the CLI side.
    use crate::config::registry::{parse_dot_key, parse_toml_scalar, set_toml_value};

    // --- parse_dot_key tests ---

    #[test]
    fn test_parse_dot_key_simple() {
        assert_eq!(parse_dot_key("authentication"), vec!["authentication"]);
    }

    #[test]
    fn test_parse_dot_key_nested() {
        assert_eq!(
            parse_dot_key("authentication.default_model"),
            vec!["authentication", "default_model"]
        );
    }

    #[test]
    fn test_parse_dot_key_deep() {
        assert_eq!(parse_dot_key("a.b.c.d"), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn test_parse_dot_key_empty() {
        let result: Vec<String> = parse_dot_key("");
        assert!(result.is_empty());
    }

    #[test]
    fn test_parse_dot_key_trims_whitespace() {
        assert_eq!(
            parse_dot_key(" authentication . default_model "),
            vec!["authentication", "default_model"]
        );
    }

    // --- parse_toml_scalar tests ---

    #[test]
    fn test_parse_scalar_boolean_true() {
        assert_eq!(parse_toml_scalar("true"), toml::Value::Boolean(true));
    }

    #[test]
    fn test_parse_scalar_boolean_false() {
        assert_eq!(parse_toml_scalar("false"), toml::Value::Boolean(false));
    }

    #[test]
    fn test_parse_scalar_boolean_case_insensitive() {
        assert_eq!(parse_toml_scalar("True"), toml::Value::Boolean(true));
        assert_eq!(parse_toml_scalar("FALSE"), toml::Value::Boolean(false));
    }

    #[test]
    fn test_parse_scalar_integer() {
        assert_eq!(parse_toml_scalar("42"), toml::Value::Integer(42));
        assert_eq!(parse_toml_scalar("-1"), toml::Value::Integer(-1));
        assert_eq!(parse_toml_scalar("0"), toml::Value::Integer(0));
    }

    #[test]
    fn test_parse_scalar_float() {
        assert_eq!(parse_toml_scalar("3.14"), toml::Value::Float(3.14));
    }

    #[test]
    fn test_parse_scalar_string() {
        assert_eq!(
            parse_toml_scalar("sonnet"),
            toml::Value::String("sonnet".to_string())
        );
    }

    #[test]
    fn test_parse_scalar_string_with_special_chars() {
        assert_eq!(
            parse_toml_scalar("agents/"),
            toml::Value::String("agents/".to_string())
        );
    }

    // --- `set` validation (the write path cmd_set takes) ---

    #[test]
    fn set_refuses_a_mistyped_key() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        let err = set_validated(&mut root, "docker.timout", "30").unwrap_err().to_string();
        assert!(err.contains("Unknown config key"), "{err}");
        assert!(
            root.as_table().expect("table").is_empty(),
            "nothing was written"
        );
    }

    #[test]
    fn set_refuses_an_out_of_range_value() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        let err = set_validated(&mut root, "docker.timeout", "0").unwrap_err().to_string();
        assert!(err.contains("between 1 and 3600"), "{err}");
    }

    #[test]
    fn set_writes_a_known_key() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        set_validated(&mut root, "docker.timeout", "120").unwrap();
        assert_eq!(
            navigate_toml(&root, "docker.timeout").unwrap(),
            &toml::Value::Integer(120)
        );
    }

    // --- navigate_toml tests ---

    #[test]
    fn test_navigate_toml_top_level() {
        let value = toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert(
                "version".to_string(),
                toml::Value::String("1.0".to_string()),
            );
            m
        });

        let result = navigate_toml(&value, "version").unwrap();
        assert_eq!(result.as_str(), Some("1.0"));
    }

    #[test]
    fn test_navigate_toml_nested() {
        let value = toml::Value::Table({
            let mut root = toml::map::Map::new();
            let mut auth = toml::map::Map::new();
            auth.insert(
                "default_model".to_string(),
                toml::Value::String("sonnet".to_string()),
            );
            root.insert("authentication".to_string(), toml::Value::Table(auth));
            root
        });

        let result = navigate_toml(&value, "authentication.default_model").unwrap();
        assert_eq!(result.as_str(), Some("sonnet"));
    }

    #[test]
    fn test_navigate_toml_missing_key() {
        let value = toml::Value::Table(toml::map::Map::new());
        let result = navigate_toml(&value, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn test_navigate_toml_index_into_non_table() {
        let value = toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("name".to_string(), toml::Value::String("test".to_string()));
            m
        });

        let result = navigate_toml(&value, "name.sub");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("non-table"));
    }

    #[test]
    fn test_navigate_toml_returns_subtable() {
        let value = toml::Value::Table({
            let mut root = toml::map::Map::new();
            let mut auth = toml::map::Map::new();
            auth.insert(
                "cli_provider".to_string(),
                toml::Value::String("claude".to_string()),
            );
            auth.insert(
                "default_model".to_string(),
                toml::Value::String("sonnet".to_string()),
            );
            root.insert("authentication".to_string(), toml::Value::Table(auth));
            root
        });

        let result = navigate_toml(&value, "authentication").unwrap();
        assert!(result.is_table());
    }

    // --- set_toml_value tests ---

    #[test]
    fn test_set_toml_value_top_level() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        set_toml_value(&mut root, "name", "test-project").unwrap();

        let table = root.as_table().unwrap();
        assert_eq!(table["name"].as_str(), Some("test-project"));
    }

    #[test]
    fn test_set_toml_value_nested_creates_intermediate() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        set_toml_value(&mut root, "authentication.default_model", "opus").unwrap();

        let auth = root.as_table().unwrap()["authentication"].as_table().unwrap();
        assert_eq!(auth["default_model"].as_str(), Some("opus"));
    }

    #[test]
    fn test_set_toml_value_preserves_existing() {
        let mut root = toml::Value::Table({
            let mut m = toml::map::Map::new();
            let mut auth = toml::map::Map::new();
            auth.insert(
                "cli_provider".to_string(),
                toml::Value::String("claude".to_string()),
            );
            auth.insert(
                "default_model".to_string(),
                toml::Value::String("sonnet".to_string()),
            );
            m.insert("authentication".to_string(), toml::Value::Table(auth));
            m
        });

        set_toml_value(&mut root, "authentication.default_model", "opus").unwrap();

        let auth = root.as_table().unwrap()["authentication"].as_table().unwrap();
        assert_eq!(auth["default_model"].as_str(), Some("opus"));
        assert_eq!(auth["cli_provider"].as_str(), Some("claude"));
    }

    #[test]
    fn test_set_toml_value_boolean() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        set_toml_value(&mut root, "ui_preferences.show_git_status", "false").unwrap();

        let ui = root.as_table().unwrap()["ui_preferences"].as_table().unwrap();
        assert_eq!(ui["show_git_status"].as_bool(), Some(false));
    }

    #[test]
    fn test_set_toml_value_integer() {
        let mut root = toml::Value::Table(toml::map::Map::new());
        set_toml_value(&mut root, "docker.timeout", "120").unwrap();

        let docker = root.as_table().unwrap()["docker"].as_table().unwrap();
        assert_eq!(docker["timeout"].as_integer(), Some(120));
    }

    // --- toml_to_json tests ---

    #[test]
    fn test_toml_to_json_string() {
        let toml_val = toml::Value::String("hello".to_string());
        let json_val = toml_to_json(&toml_val);
        assert_eq!(json_val, serde_json::Value::String("hello".to_string()));
    }

    #[test]
    fn test_toml_to_json_integer() {
        let toml_val = toml::Value::Integer(42);
        let json_val = toml_to_json(&toml_val);
        assert_eq!(json_val, serde_json::json!(42));
    }

    #[test]
    fn test_toml_to_json_boolean() {
        let toml_val = toml::Value::Boolean(true);
        let json_val = toml_to_json(&toml_val);
        assert_eq!(json_val, serde_json::Value::Bool(true));
    }

    #[test]
    fn test_toml_to_json_table() {
        let toml_val = toml::Value::Table({
            let mut m = toml::map::Map::new();
            m.insert("key".to_string(), toml::Value::String("value".to_string()));
            m
        });
        let json_val = toml_to_json(&toml_val);
        assert_eq!(
            json_val["key"],
            serde_json::Value::String("value".to_string())
        );
    }

    // --- cmd_show integration tests ---

    #[test]
    fn test_show_output_contains_version() {
        // Verify AppConfig can be serialized to TOML
        let config = AppConfig::default();
        let toml_str = toml::to_string_pretty(&config).unwrap();
        assert!(toml_str.contains("version"));
        assert!(toml_str.contains("authentication"));
    }

    #[test]
    fn test_show_output_json_contains_fields() {
        let config = AppConfig::default();
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("version"));
        assert!(json.contains("authentication"));
        assert!(json.contains("default_model"));
    }

    #[test]
    fn test_get_from_real_config() {
        // Load default config, convert to TOML value, navigate
        let config = AppConfig::default();
        let toml_value = toml::Value::try_from(&config).unwrap();

        let model = navigate_toml(&toml_value, "authentication.default_model").unwrap();
        assert_eq!(model.as_str(), Some("sonnet"));

        let version = navigate_toml(&toml_value, "version").unwrap();
        assert!(version.as_str().is_some());
    }

    // --- cmd_path test ---

    #[test]
    fn test_config_paths_returns_four_locations() {
        let paths = AppConfig::get_config_paths();
        assert_eq!(
            paths.len(),
            4,
            "Expected project (.ainb + legacy .agents-box), user, and system config paths"
        );
    }
}
