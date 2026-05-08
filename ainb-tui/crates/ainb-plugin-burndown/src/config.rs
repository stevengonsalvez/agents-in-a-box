//! Plugin-side mirror of the burndown-relevant slice of ainb-core's config.
//!
//! ainb-core lives on the host side and the plugin can't link against it,
//! so we define a narrow set of POD types matching the on-disk
//! `~/.agents-in-a-box/config.toml` schema for the fields the plugin
//! cares about (plan projection + currency display).
//!
//! Wire shape is identical to `ainb-core::config::{UsagePlan, UsagePlanId,
//! UsagePlanProvider, CurrencyConfig}` — the host serialises its config
//! into msgpack on `_init`, the plugin deserialises into these mirrors.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UsagePlan {
    pub id: UsagePlanId,
    pub monthly_usd: f64,
    pub provider: UsagePlanProvider,
    pub reset_day: u8,
    pub set_at: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UsagePlanId {
    ClaudePro,
    ClaudeMax,
    ClaudeMax5x,
    CursorPro,
    Custom,
    None,
}

impl UsagePlanId {
    pub fn monthly_usd(self) -> Option<f64> {
        match self {
            Self::ClaudePro => Some(20.0),
            Self::ClaudeMax => Some(200.0),
            Self::ClaudeMax5x => Some(100.0),
            Self::CursorPro => Some(20.0),
            Self::Custom | Self::None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UsagePlanProvider {
    All,
    Claude,
    Codex,
    Cursor,
}

impl Default for UsagePlanProvider {
    fn default() -> Self {
        Self::All
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CurrencyConfig {
    #[serde(default = "default_currency_code")]
    pub code: String,
    #[serde(default = "default_currency_symbol")]
    pub symbol: String,
    #[serde(default = "default_exchange_rate")]
    pub usd_rate: f64,
}

impl Default for CurrencyConfig {
    fn default() -> Self {
        Self {
            code: default_currency_code(),
            symbol: default_currency_symbol(),
            usd_rate: default_exchange_rate(),
        }
    }
}

fn default_currency_code() -> String {
    "USD".to_string()
}

fn default_currency_symbol() -> String {
    "$".to_string()
}

fn default_exchange_rate() -> f64 {
    1.0
}

/// Subset of ainb-core's `UsageConfig` the plugin reads/writes.
///
/// The on-disk schema in `~/.agents-in-a-box/config.toml` lives under the
/// `[usage]` table. The plugin only owns this slice — other top-level
/// tables (docker, mcp, ui_preferences, etc.) are read-modify-write
/// preserved through `toml::Value` round-tripping in [`AppConfig::save`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct UsageConfig {
    #[serde(default)]
    pub plan: Option<UsagePlan>,
    #[serde(default)]
    pub currency: CurrencyConfig,
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
}

/// Plugin-side AppConfig: only the `usage` table is hydrated; all other
/// host-managed tables are kept as opaque `toml::Value`s in
/// `_other_tables` so `save()` doesn't clobber them.
#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    pub usage: UsageConfig,
    /// Original parsed TOML — used by `save()` to preserve sections the
    /// plugin doesn't own (docker / ui_preferences / mcp_servers / …).
    /// `None` when the file didn't exist on `load()`.
    _original: Option<toml::Value>,
}

impl AppConfig {
    /// Read `~/.agents-in-a-box/config.toml` (if present), parsing only
    /// the `[usage]` table. Other tables are retained as raw
    /// `toml::Value` so `save()` can write them back unchanged.
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let original: toml::Value = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;

        let usage: UsageConfig = original
            .get("usage")
            .cloned()
            .map(|v| v.try_into())
            .transpose()
            .context("failed to deserialize [usage] table")?
            .unwrap_or_default();

        Ok(Self { usage, _original: Some(original) })
    }

    /// Atomic write of `~/.agents-in-a-box/config.toml`. Replaces only
    /// the `[usage]` table; leaves every other table the plugin doesn't
    /// own intact.
    pub fn save(&self) -> Result<()> {
        let path = config_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Start from the parsed original (or an empty inline table) so
        // we preserve [docker], [ui_preferences], etc.
        let mut root = self
            ._original
            .clone()
            .unwrap_or_else(|| toml::Value::Table(toml::map::Map::new()));
        if let toml::Value::Table(ref mut table) = root {
            let usage_value = toml::Value::try_from(&self.usage)
                .context("failed to serialise [usage] table")?;
            table.insert("usage".to_string(), usage_value);
        }
        let content = toml::to_string_pretty(&root)
            .context("failed to serialise config to TOML")?;

        // Best-effort atomic write: tmp + rename.
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

fn config_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home.join(".agents-in-a-box").join("config.toml"))
}
