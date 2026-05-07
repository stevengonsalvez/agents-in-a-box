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
