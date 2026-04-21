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
#[allow(clippy::unused_async)]
pub async fn execute(command: ConfigCommands, format: OutputFormat) -> Result<()> {
    match command {
        ConfigCommands::Show => cmd_show(format),
        ConfigCommands::Get { key } => cmd_get(&key, format),
        ConfigCommands::Set { key, value } => cmd_set(&key, &value),
        ConfigCommands::Reset { force } => cmd_reset(force),
        ConfigCommands::Path => cmd_path(format),
        ConfigCommands::Edit => cmd_edit(),
    }
}

/// Display the full merged configuration
fn cmd_show(format: OutputFormat) -> Result<()> {
    let config = AppConfig::load().context("Failed to load configuration")?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&config)
                .context("Failed to serialize config as JSON")?;
            println!("{json}");
        }
        OutputFormat::Text => {
            let toml_str =
                toml::to_string_pretty(&config).context("Failed to serialize config as TOML")?;
            println!("{toml_str}");
        }
    }

    Ok(())
}

/// Get a specific config value by dot-notation key
fn cmd_get(key: &str, format: OutputFormat) -> Result<()> {
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
        OutputFormat::Text => {
            print_toml_value(value);
        }
    }

    Ok(())
}

/// Set a config value in the user-level config file
fn cmd_set(key: &str, value: &str) -> Result<()> {
    let config_dir = AppConfig::get_user_config_dir()?;
    fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");

    // Load existing user config or start with empty table
    let mut root = if config_path.exists() {
        let content = fs::read_to_string(&config_path).context("Failed to read user config")?;
        content.parse::<toml::Value>().context("Failed to parse user config")?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };

    set_toml_value(&mut root, key, value)?;

    let content = toml::to_string_pretty(&root).context("Failed to serialize config")?;
    fs::write(&config_path, &content).context("Failed to write user config")?;

    println!("Set {key} = {value}");
    println!("Saved to {}", config_path.display());

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
        OutputFormat::Text => {
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
        fs::write(&config_path, &content).context("Failed to create default config file")?;
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

// --- TOML navigation helpers ---

/// Navigate a TOML value tree using dot-notation keys
fn navigate_toml<'a>(value: &'a toml::Value, key: &str) -> Result<&'a toml::Value> {
    let parts = parse_dot_key(key);
    if parts.is_empty() {
        return Err(anyhow!("Empty key"));
    }

    let mut current = value;
    for (i, part) in parts.iter().enumerate() {
        if let toml::Value::Table(table) = current {
            current = table.get(part.as_str()).ok_or_else(|| {
                let path = parts[..=i].join(".");
                anyhow!("Key '{path}' not found in configuration")
            })?;
        } else {
            let path = parts[..i].join(".");
            return Err(anyhow!("Cannot index into non-table value at '{path}'"));
        }
    }

    Ok(current)
}

/// Set a value in a TOML tree using dot-notation keys
fn set_toml_value(root: &mut toml::Value, key: &str, raw_value: &str) -> Result<()> {
    let parts = parse_dot_key(key);
    if parts.is_empty() {
        return Err(anyhow!("Empty key"));
    }

    // Navigate to parent, creating intermediate tables as needed
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        current = current
            .as_table_mut()
            .ok_or_else(|| anyhow!("Cannot set key in non-table value"))?
            .entry(part)
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    }

    let table = current
        .as_table_mut()
        .ok_or_else(|| anyhow!("Cannot set key in non-table value"))?;

    let leaf_key = &parts[parts.len() - 1];
    let parsed = parse_toml_scalar(raw_value);
    table.insert(leaf_key.clone(), parsed);

    Ok(())
}

/// Parse a dot-notation key into parts
fn parse_dot_key(key: &str) -> Vec<String> {
    key.split('.').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// Parse a raw string value into the most appropriate TOML scalar
fn parse_toml_scalar(raw: &str) -> toml::Value {
    // Try boolean
    if raw.eq_ignore_ascii_case("true") {
        return toml::Value::Boolean(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return toml::Value::Boolean(false);
    }

    // Try integer
    if let Ok(i) = raw.parse::<i64>() {
        return toml::Value::Integer(i);
    }

    // Try float (only if it contains a dot to avoid int-like strings)
    if raw.contains('.') {
        if let Ok(f) = raw.parse::<f64>() {
            return toml::Value::Float(f);
        }
    }

    // Default to string
    toml::Value::String(raw.to_string())
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
    fn test_config_paths_returns_three_locations() {
        let paths = AppConfig::get_config_paths();
        assert_eq!(
            paths.len(),
            3,
            "Expected project, user, and system config paths"
        );
    }
}
