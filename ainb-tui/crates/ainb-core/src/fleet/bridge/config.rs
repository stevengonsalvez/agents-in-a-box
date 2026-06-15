// ABOUTME: Load + validate the native phone-bridge config from ainb's config.toml.
//
// Config lives in ainb's layered config at
//   ~/.agents-in-a-box/config/config.toml
// under `[fleet.bridge.telegram]` and/or `[fleet.bridge.slack]`. Tokens and the
// authorized user id are resolved through the secret resolver so the token is
// NEVER written on argv or into the launchd/systemd unit — it stays in config
// (or a keychain/env ref) and is read in-process at startup.
//
//   [fleet.bridge]
//   response_timeout = 300            # optional, seconds (shared default)
//
//   [fleet.bridge.telegram]
//   token = "$TELEGRAM_BOT_TOKEN"     # or "keychain:svc" or a literal
//   user_id = 123456789              # authorized Telegram user id
//   default_target = "conductor"     # optional: name to prefer with no prefix
//   require_mention_in_groups = true # optional, default true
//   response_timeout = 300           # optional, overrides the shared default
//
//   [fleet.bridge.slack]
//   bot_token = "$SLACK_BOT_TOKEN"    # xoxb-… (Web API calls)
//   app_token = "$SLACK_APP_TOKEN"    # xapp-… (socket-mode connection)
//   user_id = "U0123ABC"             # authorized Slack user id (string)
//   default_target = "conductor"     # optional
//   listen_mode = "mentions"         # "mentions" (default) | "all"
//   response_timeout = 300           # optional
//
// At least ONE channel table must be present and valid.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use super::secrets::resolve_secret;

/// Default time (seconds) the bridge waits for a session to finish its turn
/// before giving up on capturing a reply.
pub const RESPONSE_TIMEOUT: u64 = 300;

/// How a Slack channel decides a message is addressed to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlackListenMode {
    /// Only act on messages that @mention the bot (or are DMs). Default.
    Mentions,
    /// Act on every message in channels the bot is a member of (and DMs).
    All,
}

/// Resolved, validated Telegram channel config.
#[derive(Debug, Clone)]
pub struct TelegramConfig {
    pub token: String,
    pub authorized_user_id: i64,
    pub default_target: Option<String>,
    pub require_mention_in_groups: bool,
    pub response_timeout: u64,
}

/// Resolved, validated Slack channel config.
#[derive(Debug, Clone)]
pub struct SlackConfig {
    pub bot_token: String,
    pub app_token: String,
    pub authorized_user_id: String,
    pub default_target: Option<String>,
    pub listen_mode: SlackListenMode,
    pub response_timeout: u64,
}

/// Resolved bridge config — at least one channel is present.
#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub telegram: Option<TelegramConfig>,
    pub slack: Option<SlackConfig>,
}

impl BridgeConfig {
    /// Whether any channel is configured to run.
    #[must_use]
    pub fn any_channel(&self) -> bool {
        self.telegram.is_some() || self.slack.is_some()
    }
}

// ── Raw TOML shapes ─────────────────────────────────────────────────────────

#[derive(Debug, Default, Deserialize)]
struct RawRoot {
    fleet: Option<RawFleet>,
}

#[derive(Debug, Default, Deserialize)]
struct RawFleet {
    bridge: Option<RawBridge>,
}

#[derive(Debug, Default, Deserialize)]
struct RawBridge {
    response_timeout: Option<toml::Value>,
    telegram: Option<RawTelegram>,
    slack: Option<RawSlack>,
}

#[derive(Debug, Default, Deserialize)]
struct RawTelegram {
    token: Option<String>,
    user_id: Option<toml::Value>,
    default_target: Option<String>,
    require_mention_in_groups: Option<bool>,
    response_timeout: Option<toml::Value>,
}

#[derive(Debug, Default, Deserialize)]
struct RawSlack {
    bot_token: Option<String>,
    app_token: Option<String>,
    user_id: Option<String>,
    default_target: Option<String>,
    listen_mode: Option<String>,
    response_timeout: Option<toml::Value>,
}

/// Resolve ainb's config.toml path, honouring `AINB_CONFIG_PATH`.
#[must_use]
pub fn default_config_path() -> PathBuf {
    if let Ok(override_path) = std::env::var("AINB_CONFIG_PATH") {
        return PathBuf::from(override_path);
    }
    let mut p = dirs::home_dir().unwrap_or_default();
    p.push(".agents-in-a-box");
    p.push("config");
    p.push("config.toml");
    p
}

/// Coerce a TOML scalar (`response_timeout`) into a positive `u64` seconds.
fn coerce_timeout(value: Option<&toml::Value>, default: u64) -> Result<u64> {
    let Some(value) = value else {
        return Ok(default);
    };
    let secs = match value {
        toml::Value::Integer(i) => *i,
        toml::Value::String(s) => {
            s.trim().parse::<i64>().context("response_timeout is not an integer")?
        }
        _ => bail!("response_timeout must be an integer"),
    };
    if secs <= 0 {
        bail!("response_timeout must be positive");
    }
    Ok(secs as u64)
}

/// Coerce a TOML scalar into an `i64` user id, resolving string refs via the
/// secret resolver (so `user_id = "$MY_ID"` works).
fn coerce_user_id_i64(value: Option<&toml::Value>) -> Result<i64> {
    match value {
        Some(toml::Value::Integer(i)) => Ok(*i),
        Some(toml::Value::String(s)) => {
            let resolved = resolve_secret(s);
            let resolved = resolved.trim();
            if resolved.is_empty() {
                bail!("user_id is empty after secret resolution");
            }
            resolved
                .parse::<i64>()
                .with_context(|| format!("user_id is not an integer: {resolved:?}"))
        }
        Some(_) => bail!("user_id must be a number or string"),
        None => bail!("missing `user_id`"),
    }
}

fn parse_telegram(raw: RawTelegram, shared_timeout: u64) -> Result<TelegramConfig> {
    let token_ref =
        raw.token.ok_or_else(|| anyhow!("[fleet.bridge.telegram] is missing `token`"))?;
    let token = resolve_secret(&token_ref);
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("telegram token resolved to empty — check the env var / keychain ref");
    }
    let authorized_user_id =
        coerce_user_id_i64(raw.user_id.as_ref()).context("[fleet.bridge.telegram] `user_id`")?;
    let default_target = raw.default_target.and_then(|s| {
        let t = s.trim().to_string();
        (!t.is_empty()).then_some(t)
    });
    let require_mention_in_groups = raw.require_mention_in_groups.unwrap_or(true);
    let response_timeout = coerce_timeout(raw.response_timeout.as_ref(), shared_timeout)
        .context("[fleet.bridge.telegram] `response_timeout`")?;
    Ok(TelegramConfig {
        token,
        authorized_user_id,
        default_target,
        require_mention_in_groups,
        response_timeout,
    })
}

fn parse_slack(raw: RawSlack, shared_timeout: u64) -> Result<SlackConfig> {
    let bot_ref = raw
        .bot_token
        .ok_or_else(|| anyhow!("[fleet.bridge.slack] is missing `bot_token`"))?;
    let app_ref = raw
        .app_token
        .ok_or_else(|| anyhow!("[fleet.bridge.slack] is missing `app_token`"))?;
    let bot_token = resolve_secret(&bot_ref).trim().to_string();
    if bot_token.is_empty() {
        bail!("slack bot_token resolved to empty — check the env var / keychain ref");
    }
    let app_token = resolve_secret(&app_ref).trim().to_string();
    if app_token.is_empty() {
        bail!("slack app_token resolved to empty — check the env var / keychain ref");
    }
    let user_ref = raw
        .user_id
        .ok_or_else(|| anyhow!("[fleet.bridge.slack] is missing `user_id`"))?;
    let authorized_user_id = resolve_secret(&user_ref).trim().to_string();
    if authorized_user_id.is_empty() {
        bail!("slack user_id resolved to empty");
    }
    let default_target = raw.default_target.and_then(|s| {
        let t = s.trim().to_string();
        (!t.is_empty()).then_some(t)
    });
    let listen_mode = match raw.listen_mode.as_deref().map(str::trim) {
        Some("all") => SlackListenMode::All,
        Some("mentions") | None => SlackListenMode::Mentions,
        Some(other) => {
            bail!("[fleet.bridge.slack] listen_mode must be \"mentions\" or \"all\", got {other:?}")
        }
    };
    let response_timeout = coerce_timeout(raw.response_timeout.as_ref(), shared_timeout)
        .context("[fleet.bridge.slack] `response_timeout`")?;
    Ok(SlackConfig {
        bot_token,
        app_token,
        authorized_user_id,
        default_target,
        listen_mode,
        response_timeout,
    })
}

/// Build a [`BridgeConfig`] from a parsed TOML string. Split out so it can be
/// unit-tested without touching the filesystem.
pub fn parse_config(toml_text: &str) -> Result<BridgeConfig> {
    let root: RawRoot = toml::from_str(toml_text).context("parsing config.toml")?;
    let bridge = root
        .fleet
        .and_then(|f| f.bridge)
        .ok_or_else(|| anyhow!("config has no [fleet.bridge] table"))?;

    let shared_timeout = coerce_timeout(bridge.response_timeout.as_ref(), RESPONSE_TIMEOUT)
        .context("[fleet.bridge] `response_timeout`")?;

    let telegram = bridge.telegram.map(|t| parse_telegram(t, shared_timeout)).transpose()?;
    let slack = bridge.slack.map(|s| parse_slack(s, shared_timeout)).transpose()?;

    if telegram.is_none() && slack.is_none() {
        bail!(
            "[fleet.bridge] has neither a [fleet.bridge.telegram] nor a \
             [fleet.bridge.slack] channel — configure at least one"
        );
    }
    Ok(BridgeConfig { telegram, slack })
}

/// Load and validate the bridge config from disk.
pub fn load_config(path: Option<&Path>) -> Result<BridgeConfig> {
    let owned;
    let cfg_path = match path {
        Some(p) => p,
        None => {
            owned = default_config_path();
            &owned
        }
    };
    if !cfg_path.exists() {
        bail!("config file not found: {}", cfg_path.display());
    }
    let text = std::fs::read_to_string(cfg_path)
        .with_context(|| format!("failed to read {}", cfg_path.display()))?;
    parse_config(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_telegram_only() {
        let cfg = parse_config(
            r#"
            [fleet.bridge.telegram]
            token = "tg-literal"
            user_id = 42
            default_target = "conductor"
            "#,
        )
        .unwrap();
        let tg = cfg.telegram.unwrap();
        assert_eq!(tg.token, "tg-literal");
        assert_eq!(tg.authorized_user_id, 42);
        assert_eq!(tg.default_target.as_deref(), Some("conductor"));
        assert!(tg.require_mention_in_groups);
        assert_eq!(tg.response_timeout, RESPONSE_TIMEOUT);
        assert!(cfg.slack.is_none());
    }

    #[test]
    fn parses_slack_only_with_listen_mode() {
        let cfg = parse_config(
            r#"
            [fleet.bridge.slack]
            bot_token = "xoxb-1"
            app_token = "xapp-1"
            user_id = "U123"
            listen_mode = "all"
            "#,
        )
        .unwrap();
        let sl = cfg.slack.unwrap();
        assert_eq!(sl.bot_token, "xoxb-1");
        assert_eq!(sl.app_token, "xapp-1");
        assert_eq!(sl.authorized_user_id, "U123");
        assert_eq!(sl.listen_mode, SlackListenMode::All);
        assert!(cfg.telegram.is_none());
    }

    #[test]
    fn parses_both_channels_and_shared_timeout() {
        let cfg = parse_config(
            r#"
            [fleet.bridge]
            response_timeout = 120

            [fleet.bridge.telegram]
            token = "tg"
            user_id = 1

            [fleet.bridge.slack]
            bot_token = "xoxb"
            app_token = "xapp"
            user_id = "U1"
            "#,
        )
        .unwrap();
        assert_eq!(cfg.telegram.unwrap().response_timeout, 120);
        assert_eq!(cfg.slack.unwrap().response_timeout, 120);
    }

    #[test]
    fn per_channel_timeout_overrides_shared() {
        let cfg = parse_config(
            r#"
            [fleet.bridge]
            response_timeout = 120

            [fleet.bridge.telegram]
            token = "tg"
            user_id = 1
            response_timeout = 999
            "#,
        )
        .unwrap();
        assert_eq!(cfg.telegram.unwrap().response_timeout, 999);
    }

    #[test]
    fn missing_bridge_table_errors() {
        let err = parse_config("[fleet]\n").unwrap_err();
        assert!(err.to_string().contains("no [fleet.bridge] table"));
    }

    #[test]
    fn no_channel_errors() {
        let err = parse_config("[fleet.bridge]\nresponse_timeout = 60\n").unwrap_err();
        assert!(err.to_string().contains("at least one"));
    }

    #[test]
    fn empty_token_after_resolution_errors() {
        let err = parse_config(
            r#"
            [fleet.bridge.telegram]
            token = "$AINB_BRIDGE_UNSET_TOKEN_VAR"
            user_id = 1
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("resolved to empty"));
    }

    #[test]
    fn invalid_listen_mode_errors() {
        let err = parse_config(
            r#"
            [fleet.bridge.slack]
            bot_token = "xoxb"
            app_token = "xapp"
            user_id = "U1"
            listen_mode = "loud"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("listen_mode"));
    }

    #[test]
    fn user_id_as_unresolvable_secret_ref_errors() {
        // A string user_id that resolves to empty (unset env var) must error
        // rather than silently parsing as 0. (The successful secret-ref path is
        // covered in `secrets::tests` with an injected env to avoid mutating the
        // process environment, which the crate's `forbid(unsafe)` would block.)
        let err = parse_config(
            r#"
            [fleet.bridge.telegram]
            token = "tg"
            user_id = "$AINB_BRIDGE_USER_ID_DEFINITELY_UNSET"
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("user_id"));
    }
}
