// ABOUTME: The config sections promoted out of env vars and hardcoded literals,
// plus the `env > config > default` resolver every promoted read site uses.

//! Promoted tunables.
//!
//! Before this module a knob was either an env var with a literal fallback
//! (`AINB_HEADROOM_PORT` or 8787) or a bare `const` no user could reach. Both
//! shapes hide the value from `ainb config` and from the settings screen, and
//! the env-var shape repeatedly grew *two* fallbacks for one name. See
//! [`FleetConfig::state_stale_ms`](crate::config::FleetConfig::state_stale_ms).
//!
//! Every knob here now has exactly one coded default, expressed once as a
//! serde default on the field, and one resolution ladder:
//!
//! ```text
//! env var  >  config.toml  >  the field's serde default
//! ```
//!
//! matching the ladder the plugin `[[config]]` mechanism already uses. The env
//! var keeps winning: scripts, CI and `just` recipes set them today and must
//! not start losing to a file.
//!
//! ## Two ways a value reaches a reader
//!
//! A read site **inside `ainb`** resolves in code, via [`resolved`] /
//! [`resolved_bool`] over [`snapshot`].
//!
//! A read site in another crate cannot: `ainb-fleet-core`, `ainb-adapters-tool`
//! and the plugin binaries do not depend on `ainb`, and giving each of them its
//! own config parser would make a fifth parser of one file. Those sites keep
//! reading their env var, and [`export_env_bridge`] publishes the config value
//! into the process environment once at startup, only where the variable is
//! unset, so an operator's `AINB_FLEET_IDLE_MIN=2` still wins. Child processes
//! (plugin subprocesses, the hangar daemon) inherit it for free, which is the
//! same path their config already travels.

use std::str::FromStr;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::AppConfig;

// ── The resolver ────────────────────────────────────────────────────────────

/// The process-wide merged config, loaded once.
///
/// Read sites like `headroom::proxy_port()` are free functions with no
/// `&AppConfig` in hand and are called from render paths, so re-reading four
/// files per call is not an option. A failed load falls back to defaults rather
/// than panicking: a broken config.toml must not take the port resolver with
/// it.
#[must_use]
pub fn snapshot() -> &'static AppConfig {
    static SNAPSHOT: OnceLock<AppConfig> = OnceLock::new();
    SNAPSHOT.get_or_init(|| AppConfig::load().unwrap_or_default())
}

/// `env_var`, else `from_config`.
///
/// `from_config` is already the bottom two rungs of the ladder: it is the value
/// serde produced, which is either what the file said or the field's coded
/// default. An env value that does not parse is ignored rather than fatal:
/// a typo in a shell profile must not brick the reader that consumes it.
#[must_use]
pub fn resolved<T: FromStr>(env_var: &str, from_config: T) -> T {
    match std::env::var(env_var) {
        Ok(raw) => raw.trim().parse().unwrap_or(from_config),
        Err(_) => from_config,
    }
}

/// [`resolved`] for booleans, which have no useful `FromStr`.
///
/// Accepts the tolerant token family the rest of ainb uses
/// (`1`/`true`/`yes`/`on` and their negatives, case-insensitively), so the two
/// promoted flags stop disagreeing about what "off" looks like:
/// `AINB_FLEET_ENRICH` used to treat everything but `0` as on, while
/// `AGENTS_BOX_SYNTAX_HIGHLIGHT` treated everything but `true` as off.
#[must_use]
pub fn resolved_bool(env_var: &str, from_config: bool) -> bool {
    match std::env::var(env_var) {
        Ok(raw) => parse_bool_token(&raw).unwrap_or(from_config),
        Err(_) => from_config,
    }
}

/// Parse one tolerant boolean token, or `None` when it is not one.
#[must_use]
pub fn parse_bool_token(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Publish config-sourced values into the process environment, for the read
/// sites that live outside this crate.
///
/// Call ONCE from a binary's startup, before any thread is spawned, for the
/// same reason and in the same place as
/// [`AppConfig::migrate_legacy_paths`](crate::config::AppConfig::migrate_legacy_paths).
///
/// Only sets a variable that is currently unset, which is what keeps `env >
/// config`: an operator who exported `AINB_FLEET_TRANSPORT=broker` for this
/// shell still beats the file. `general.home` and `usage_client.cache_db` are
/// `Option` and are skipped when unset, because exporting a derived default
/// would freeze a path the readers deliberately compute themselves.
pub fn export_env_bridge(config: &AppConfig) {
    let publish = |name: &str, value: String| {
        if std::env::var_os(name).is_none() {
            std::env::set_var(name, value);
        }
    };

    if let Some(home) = &config.general.home {
        publish("AINB_HOME", home.clone());
    }
    publish(
        "AINB_USE_REAL_HOMES",
        config.general.skill_install_real_homes.to_string(),
    );
    publish("AINB_FLEET_IDLE_MIN", config.fleet.idle_min.to_string());
    publish("AINB_FLEET_TRANSPORT", config.fleet.transport.clone());
    publish("AINB_FLEET_ENRICH", config.fleet.enrich.to_string());
    publish(
        "AINB_FLEET_TMUX_IDLE_AFTER_SECS",
        config.fleet.tmux_idle_after_secs.to_string(),
    );
    publish(
        "AINB_FLEET_STATE_STALE_MS",
        config.fleet.state_stale_ms.to_string(),
    );
    publish(
        "AINB_FLEET_HEALTHY_STATE_STALE_MS",
        config.fleet.healthy_state_stale_ms.to_string(),
    );
    publish(
        "AINB_HEADROOM_PORT",
        config.usage_client.headroom_port.to_string(),
    );
    publish(
        "AINB_USAGE_TIMEOUT_SECS",
        config.usage_client.fetch_timeout_secs.to_string(),
    );
    publish(
        "AINB_CODEX_USAGE_TTL_SECS",
        config.usage_client.codex_ttl_secs.to_string(),
    );
    if let Some(db) = &config.usage_client.cache_db {
        publish("AINB_USAGE_CACHE_DB", db.clone());
    }
}

// ── [general] ───────────────────────────────────────────────────────────────

/// Cross-cutting knobs that belong to no other section.
///
/// Example `config.toml`:
/// ```toml
/// [general]
/// syntax_highlight = true
/// home = "/Volumes/work/ainb"
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneralConfig {
    /// Colourise fenced code blocks in agent output. Off is a real preference
    /// on a slow terminal or a low-contrast theme; `NO_COLOR` still wins over
    /// both this and its env var.
    #[serde(default = "crate::config::default_true")]
    pub syntax_highlight: bool,

    /// Install skills into each tool's REAL config dir (`~/.claude`,
    /// `~/.codex`, …) rather than the managed sandbox under `home`.
    ///
    /// True is the shipped behaviour: an installed skill has to be where the
    /// tool actually looks or it may as well not exist. False routes every
    /// install into the sandbox, which is what a machine shared with a
    /// hand-managed `~/.claude` wants.
    #[serde(default = "crate::config::default_true")]
    pub skill_install_real_homes: bool,

    /// Base directory ainb keeps its state under: `<home>/.agents-in-a-box/…`.
    ///
    /// `None` means the OS home directory. This does NOT move config.toml,
    /// which is always read from the OS home. A config file that said where
    /// to find itself could not be found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            syntax_highlight: true,
            skill_install_real_homes: true,
            home: None,
        }
    }
}

// ── [ui] ────────────────────────────────────────────────────────────────────

/// TUI cadences and query bounds.
///
/// Separate from `[ui_preferences]` on purpose: that section holds what the
/// interface LOOKS like (theme, which columns show, sidebar widths) and is
/// written by the TUI itself. These are performance tunables (how often the
/// loop wakes, how many rows a query returns) that a user changes for a slow
/// terminal or a very large fleet and that nothing writes back.
///
/// Example `config.toml`:
/// ```toml
/// [ui]
/// tick_rate_ms = 16      # 60fps event polling
/// inbox_list_limit = 500
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiConfig {
    /// Event-poll cadence in ms: how often the loop wakes to check for a
    /// keystroke. Drives perceived input latency. ~30fps by default; lower
    /// costs CPU, higher makes typing feel laggy.
    #[serde(default = "default_tick_rate_ms")]
    pub tick_rate_ms: u64,

    /// App-tick cadence in ms for the heavy periodic work (mascot animation,
    /// tmux preview capture, OAuth refresh checks). Deliberately much coarser
    /// than `tick_rate_ms`; running it per poll starves the event loop.
    #[serde(default = "default_app_tick_ms")]
    pub app_tick_ms: u64,

    /// How many recent hook events the attention-marker query reads per
    /// refresh. Bounds query cost on a large notifications DB.
    #[serde(default = "default_session_query_limit")]
    pub session_query_limit: u32,

    /// Rolling window (hours) an event can still mark a session from. Older
    /// events never raise a `[?]`/`[!]` marker.
    #[serde(default = "default_session_lookback_hours")]
    pub session_lookback_hours: u32,

    /// Rows the Inbox screen lists. Kept small because the query runs every
    /// render.
    #[serde(default = "default_inbox_list_limit")]
    pub inbox_list_limit: u32,

    /// Window (ms) in which two clicks count as a double-click on the sidebar
    /// edge. Raise it if your hands or your terminal are slow.
    #[serde(default = "default_double_click_ms")]
    pub double_click_ms: u64,
}

fn default_tick_rate_ms() -> u64 {
    33
}
fn default_app_tick_ms() -> u64 {
    250
}
fn default_session_query_limit() -> u32 {
    500
}
fn default_session_lookback_hours() -> u32 {
    6
}
fn default_inbox_list_limit() -> u32 {
    200
}
fn default_double_click_ms() -> u64 {
    300
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            tick_rate_ms: default_tick_rate_ms(),
            app_tick_ms: default_app_tick_ms(),
            session_query_limit: default_session_query_limit(),
            session_lookback_hours: default_session_lookback_hours(),
            inbox_list_limit: default_inbox_list_limit(),
            double_click_ms: default_double_click_ms(),
        }
    }
}

// ── [daemons] ───────────────────────────────────────────────────────────────

/// How long a daemon may go quiet before the Daemons view calls it stale.
///
/// Example `config.toml`:
/// ```toml
/// [daemons]
/// stale_after_ms = 120000
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DaemonsConfig {
    /// A heartbeat older than this, from a pid that is STILL alive (a wedged
    /// daemon that stopped ticking), reads as stale. A dead pid is caught
    /// immediately regardless of this window.
    #[serde(default = "default_stale_after_ms")]
    pub stale_after_ms: i64,

    /// The bridge's outbound-push window: how long it may go without a
    /// SUCCESSFUL poll of the attention source before its proactive-push half
    /// counts as broken. A distinct clock from `stale_after_ms`: that one asks
    /// "is the process alive", this one "can it still do the job".
    #[serde(default = "default_attention_stale_after_ms")]
    pub attention_stale_after_ms: i64,
}

fn default_stale_after_ms() -> i64 {
    90_000
}
fn default_attention_stale_after_ms() -> i64 {
    45_000
}

impl Default for DaemonsConfig {
    fn default() -> Self {
        Self {
            stale_after_ms: default_stale_after_ms(),
            attention_stale_after_ms: default_attention_stale_after_ms(),
        }
    }
}

// ── [usage_client] ──────────────────────────────────────────────────────────

/// How ainb FETCHES and caches usage data.
///
/// Deliberately not `[usage]`. That section is the burndown plugin's: it does
/// its own load-modify-save against this same file, so core lists it in
/// `NEVER_WRITE_PATHS` and the settings screen renders it read-only. These four
/// knobs are core's: the proxy port core spawns on, the deadline core waits on
/// the plugin for, the throttle on core's Codex statusline, and the path core's
/// own usage cache opens. Putting them under `usage` would either make them
/// unwritable from the only surface that offers them, or turn a clean
/// section-level ownership boundary into a per-key allowlist that drifts.
///
/// Example `config.toml`:
/// ```toml
/// [usage_client]
/// headroom_port = 9787
/// fetch_timeout_secs = 300
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageClientConfig {
    /// Port the ainb-managed Headroom compression proxy listens on.
    #[serde(default = "default_headroom_port")]
    pub headroom_port: u16,

    /// Hard deadline (seconds) for `ainb usage` to surface output. The first
    /// scan of a large `~/.claude/projects` is slow and must finish before the
    /// burndown dispatch returns; raise this against a very large archive.
    #[serde(default = "default_fetch_timeout_secs")]
    pub fetch_timeout_secs: u64,

    /// Throttle (seconds) between Codex statusline usage refreshes.
    #[serde(default = "default_codex_ttl_secs")]
    pub codex_ttl_secs: u64,

    /// Override for the usage cache DB. `None` derives
    /// `<home>/.agents-in-a-box/cache/usage.db`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_db: Option<String>,
}

fn default_headroom_port() -> u16 {
    8787
}
fn default_fetch_timeout_secs() -> u64 {
    120
}
fn default_codex_ttl_secs() -> u64 {
    60
}

impl Default for UsageClientConfig {
    fn default() -> Self {
        Self {
            headroom_port: default_headroom_port(),
            fetch_timeout_secs: default_fetch_timeout_secs(),
            codex_ttl_secs: default_codex_ttl_secs(),
            cache_db: None,
        }
    }
}

// ── [notifyd] ───────────────────────────────────────────────────────────────

/// Notification-daemon knobs.
///
/// The daemon is a subprocess plugin and parses this table itself off the same
/// config.toml (`ainb_plugin_notifyd::config`), exactly as `[session_reader]`
/// does. It is mirrored here so `AppConfig` round-trips the section on save
/// instead of deleting it, and so the settings screen has a schema to render.
///
/// Example `config.toml`:
/// ```toml
/// [notifyd]
/// os_debounce_secs = 30
/// approval_timeout_secs = 900
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotifydConfig {
    /// Per-`(session, event)` debounce window for OS notifications. Keeps a
    /// noisy session from spamming Notification Center.
    #[serde(default = "default_os_debounce_secs")]
    pub os_debounce_secs: u64,

    /// How long an unanswered permission request waits before the broker
    /// auto-DENIES it.
    ///
    /// The bottom of a timeout ladder: broker AWAIT < the client's re-dial
    /// deadline < Claude's `PermissionRequest` hook timeout. Raising it past
    /// the client deadline (640s) means the hook is hard-killed before the
    /// broker answers, so the ceiling here is deliberately below it.
    #[serde(default = "default_approval_timeout_secs")]
    pub approval_timeout_secs: u64,
}

fn default_os_debounce_secs() -> u64 {
    60
}
fn default_approval_timeout_secs() -> u64 {
    600
}

impl Default for NotifydConfig {
    fn default() -> Self {
        Self {
            os_debounce_secs: default_os_debounce_secs(),
            approval_timeout_secs: default_approval_timeout_secs(),
        }
    }
}

// ── [web] ───────────────────────────────────────────────────────────────────

/// Defaults for `ainb web`, which had none: [`ainb_web::WebConfig`] was built
/// from CLI flags on every run and persisted nothing, so anyone serving on a
/// fixed address retyped it every time.
///
/// The CLI flags still win, because a flag is a per-invocation decision and
/// must beat
/// a file. The bearer token is deliberately absent: it goes to the OS keychain
/// via `--token`, never into a world-readable toml.
///
/// Example `config.toml`:
/// ```toml
/// [web]
/// listen = "0.0.0.0:8420"
/// read_only = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebServerConfig {
    /// Address the dashboard binds. A non-loopback value still has to clear
    /// `WebConfig::check_bind_security` at startup, so setting it here cannot
    /// expose an unauthenticated surface that a flag could not.
    #[serde(default = "default_web_listen")]
    pub listen: String,

    /// Serve viewer-only: every write surface (the WS terminal, `POST
    /// /api/answer`) is refused.
    #[serde(default)]
    pub read_only: bool,

    /// Allow a non-loopback bind with no token. Honoured only alongside
    /// `read_only`; the security check refuses it otherwise.
    #[serde(default)]
    pub insecure_bind: bool,
}

fn default_web_listen() -> String {
    "127.0.0.1:8420".to_string()
}

impl Default for WebServerConfig {
    fn default() -> Self {
        Self {
            listen: default_web_listen(),
            read_only: false,
            insecure_bind: false,
        }
    }
}

// ── [acp] ───────────────────────────────────────────────────────────────────

/// The ACP adapter registry, previously a hardcoded two-entry map in
/// `ainb-hangar-daemon`'s `PoolConfig::default`, with no user surface at all:
/// a provider absent from it simply cannot be created.
///
/// An empty `adapters` map (the default) keeps the built-in `claude-agent-acp`
/// and `codex-acp` entries. Naming an adapter here overrides that entry;
/// naming a new one adds it.
///
/// Example `config.toml`:
/// ```toml
/// [acp.adapters.claude-agent-acp]
/// command = "/opt/homebrew/bin/claude-agent-acp"
/// permission_mode = "acceptEdits"
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpConfig {
    /// Adapter token → spawn recipe.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub adapters: std::collections::HashMap<String, AcpAdapterConfig>,
}

/// One ACP adapter's user-settable definition.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AcpAdapterConfig {
    /// Executable to spawn. Defaults to the adapter's own token, resolved on
    /// `PATH`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// The permission mode PINNED at `session/new` and re-asserted after every
    /// `session/load`. Never implicit: an adapter left to inherit ambient state
    /// was observed picking up `bypassPermissions`, which silently disables the
    /// whole permission surface.
    #[serde(default = "default_acp_permission_mode")]
    pub permission_mode: String,
}

fn default_acp_permission_mode() -> String {
    "default".to_string()
}

impl Default for AcpAdapterConfig {
    fn default() -> Self {
        Self {
            command: None,
            permission_mode: default_acp_permission_mode(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env-mutating tests in this module serialize against each other.
    /// [reference: ENV_LOCK for parallel tests]
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Set `name` for the duration of `body`, restoring it afterwards.
    fn with_env<T>(name: &str, value: Option<&str>, body: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os(name);
        match value {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
        let out = body();
        match prior {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
        out
    }

    fn from_toml(text: &str) -> AppConfig {
        toml::from_str(text).expect("config parses")
    }

    /// The full ladder for a numeric knob, one rung at a time.
    #[test]
    fn headroom_port_ladder_is_env_then_config_then_default() {
        // default: nothing set anywhere
        let bare = from_toml("");
        assert_eq!(bare.usage_client.headroom_port, 8787);
        with_env("AINB_HEADROOM_PORT", None, || {
            assert_eq!(
                resolved("AINB_HEADROOM_PORT", bare.usage_client.headroom_port),
                8787
            );
        });

        // config beats default
        let configured = from_toml("[usage_client]\nheadroom_port = 9001\n");
        assert_eq!(configured.usage_client.headroom_port, 9001);
        with_env("AINB_HEADROOM_PORT", None, || {
            assert_eq!(
                resolved("AINB_HEADROOM_PORT", configured.usage_client.headroom_port),
                9001
            );
        });

        // env beats config
        with_env("AINB_HEADROOM_PORT", Some("9999"), || {
            assert_eq!(
                resolved("AINB_HEADROOM_PORT", configured.usage_client.headroom_port),
                9999
            );
        });
    }

    /// The same ladder for a boolean, whose env form is a token family rather
    /// than a `FromStr`.
    #[test]
    fn syntax_highlight_ladder_is_env_then_config_then_default() {
        let bare = from_toml("");
        assert!(bare.general.syntax_highlight);

        let configured = from_toml("[general]\nsyntax_highlight = false\n");
        assert!(!configured.general.syntax_highlight);
        with_env("AGENTS_BOX_SYNTAX_HIGHLIGHT", None, || {
            assert!(!resolved_bool(
                "AGENTS_BOX_SYNTAX_HIGHLIGHT",
                configured.general.syntax_highlight
            ));
            assert!(resolved_bool(
                "AGENTS_BOX_SYNTAX_HIGHLIGHT",
                bare.general.syntax_highlight
            ));
        });
        with_env("AGENTS_BOX_SYNTAX_HIGHLIGHT", Some("1"), || {
            assert!(resolved_bool(
                "AGENTS_BOX_SYNTAX_HIGHLIGHT",
                configured.general.syntax_highlight
            ));
        });
    }

    /// A third knob, in the section that used to carry two different defaults
    /// for one env var.
    #[test]
    fn fleet_state_stale_ladder_is_env_then_config_then_default() {
        let bare = from_toml("");
        assert_eq!(bare.fleet.state_stale_ms, 0);
        assert_eq!(bare.fleet.healthy_state_stale_ms, 300_000);

        let configured = from_toml("[fleet]\nstate_stale_ms = 120000\n");
        assert_eq!(configured.fleet.state_stale_ms, 120_000);
        // The healthy floor is its OWN knob and keeps its own default.
        assert_eq!(configured.fleet.healthy_state_stale_ms, 300_000);

        with_env("AINB_FLEET_STATE_STALE_MS", None, || {
            assert_eq!(
                resolved("AINB_FLEET_STATE_STALE_MS", configured.fleet.state_stale_ms),
                120_000
            );
        });
        with_env("AINB_FLEET_STATE_STALE_MS", Some("7000"), || {
            assert_eq!(
                resolved("AINB_FLEET_STATE_STALE_MS", configured.fleet.state_stale_ms),
                7_000
            );
        });
    }

    #[test]
    fn an_unparseable_env_value_falls_back_instead_of_failing() {
        with_env("AINB_HEADROOM_PORT", Some("not-a-port"), || {
            assert_eq!(resolved("AINB_HEADROOM_PORT", 8787_u16), 8787);
        });
        with_env("AINB_FLEET_ENRICH", Some("maybe"), || {
            assert!(resolved_bool("AINB_FLEET_ENRICH", true));
        });
    }

    #[test]
    fn bool_tokens_cover_both_legacy_spellings() {
        // `AINB_FLEET_ENRICH=0` and `AGENTS_BOX_SYNTAX_HIGHLIGHT=true` are the
        // two forms that existed before promotion; both must still mean what
        // they used to.
        assert_eq!(parse_bool_token("0"), Some(false));
        assert_eq!(parse_bool_token("true"), Some(true));
        assert_eq!(parse_bool_token("OFF"), Some(false));
        assert_eq!(parse_bool_token(" yes "), Some(true));
        assert_eq!(parse_bool_token("perhaps"), None);
    }

    #[test]
    fn the_env_bridge_never_overwrites_an_existing_variable() {
        let mut config = AppConfig::default();
        config.fleet.transport = "broker".to_string();
        with_env("AINB_FLEET_TRANSPORT", Some("tmux-only"), || {
            export_env_bridge(&config);
            assert_eq!(
                std::env::var("AINB_FLEET_TRANSPORT").unwrap(),
                "tmux-only",
                "the bridge must not clobber an operator's env"
            );
        });
    }

    #[test]
    fn the_env_bridge_publishes_a_config_value_when_the_variable_is_unset() {
        let mut config = AppConfig::default();
        config.fleet.idle_min = 42;
        with_env("AINB_FLEET_IDLE_MIN", None, || {
            export_env_bridge(&config);
            assert_eq!(std::env::var("AINB_FLEET_IDLE_MIN").unwrap(), "42");
            // Leave the process as we found it for the other tests.
            std::env::remove_var("AINB_FLEET_IDLE_MIN");
        });
    }

    #[test]
    fn an_unset_optional_is_not_published() {
        let config = AppConfig::default();
        assert!(config.general.home.is_none());
        with_env("AINB_HOME", None, || {
            export_env_bridge(&config);
            assert!(
                std::env::var_os("AINB_HOME").is_none(),
                "an absent general.home must not freeze AINB_HOME to a derived default"
            );
        });
    }
}
