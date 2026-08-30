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

use std::collections::BTreeSet;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

use super::AppConfig;

// ── The resolver ────────────────────────────────────────────────────────────

/// The process-wide merged config the promoted read sites consult.
///
/// `RwLock`, not `OnceLock`. A value frozen at startup would make every
/// promoted key settable and inert: the settings screen would report "saved"
/// for `general.syntax_highlight` and code blocks would stay coloured until the
/// next launch, which is the opposite of what promoting them was for. Writers
/// call [`refresh_snapshot`] so a save takes effect on the next read.
///
/// `Arc` so a reader holds a cheap owned handle rather than the lock: a render
/// path that kept the guard alive across a repaint would block the save it is
/// racing.
static SNAPSHOT: RwLock<Option<Arc<AppConfig>>> = RwLock::new(None);

/// The current config.
///
/// Read sites like `headroom::proxy_port()` are free functions with no
/// `&AppConfig` in hand and are called from render paths, so re-reading four
/// files per call is not an option. The first call loads; a failed load falls
/// back to defaults rather than panicking, because a broken config.toml must
/// not take the port resolver with it.
#[must_use]
pub fn snapshot() -> Arc<AppConfig> {
    if let Some(config) = SNAPSHOT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return Arc::clone(config);
    }
    let mut slot = SNAPSHOT.write().unwrap_or_else(|e| e.into_inner());
    // Another thread may have loaded while this one waited for the write lock.
    Arc::clone(slot.get_or_insert_with(|| Arc::new(AppConfig::load().unwrap_or_default())))
}

/// Re-read config.toml into the snapshot.
///
/// Call after any successful write to the user config, so an edit made from the
/// settings screen or `ainb config set` is live on the next read instead of on
/// the next launch. Deliberately re-loads rather than taking the caller's
/// in-memory struct: `load()` also merges the project and system layers, and
/// the caller only ever holds the user one.
pub fn refresh_snapshot() {
    let loaded = Arc::new(AppConfig::load().unwrap_or_default());
    *SNAPSHOT.write().unwrap_or_else(|e| e.into_inner()) = Some(loaded);
}

/// Install `config` as the snapshot. Test seam, and the escape hatch for a
/// caller that has already loaded and does not want a second read.
/// The one lock every test that mutates the environment or the snapshot must
/// hold.
///
/// Both are process-global. A module that declares its own private mutex is
/// guarding nothing — two different locks around the same `setenv` is still a
/// `setenv`/`getenv` data race, and a snapshot installed by one test is read by
/// every other test in the binary. There is exactly one of these on purpose.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn install_snapshot(config: AppConfig) {
    *SNAPSHOT.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(config));
}

/// Variables [`export_env_bridge`] planted into this process's own environment.
///
/// The bridge exists for readers in OTHER crates and in child processes, but it
/// writes into the environment this process also reads. Without this set, the
/// bridge would defeat its own ladder: publishing `AINB_HEADROOM_PORT=8787` at
/// startup makes [`resolved`] find an env value on every later call, so
/// `usage_client.headroom_port` could never change again and
/// [`refresh_snapshot`] would have nothing to do. A name in here is treated as
/// unset BY US and honoured by everyone else.
///
/// An operator's own export is never in this set, because the bridge only
/// plants a variable that was unset.
static BRIDGED: RwLock<Option<BTreeSet<&'static str>>> = RwLock::new(None);

/// Forget which variables the bridge planted.
///
/// Tests only: `export_env_bridge` is once-per-process in production, but a test
/// that plants `AINB_HEADROOM_PORT` would otherwise leave every later test in
/// the binary ignoring that variable.
#[cfg(test)]
pub(crate) fn clear_bridged_for_test() {
    *BRIDGED.write().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Whether this process planted `env_var` itself, and so must not read it back.
fn is_self_planted(env_var: &str) -> bool {
    BRIDGED
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|planted| planted.contains(env_var))
}

/// The raw env value for `env_var`, or `None` when it is unset or was planted
/// by this process's own [`export_env_bridge`].
pub fn env_override(env_var: &str) -> Option<String> {
    if is_self_planted(env_var) {
        return None;
    }
    std::env::var(env_var).ok()
}

/// `env_var`, else `from_config`.
///
/// `from_config` is already the bottom two rungs of the ladder: it is the value
/// serde produced, which is either what the file said or the field's coded
/// default. An env value that does not parse is ignored rather than fatal:
/// a typo in a shell profile must not brick the reader that consumes it.
#[must_use]
pub fn resolved<T: FromStr>(env_var: &str, from_config: T) -> T {
    match env_override(env_var) {
        Some(raw) => raw.trim().parse().unwrap_or(from_config),
        None => from_config,
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
    match env_override(env_var) {
        Some(raw) => parse_bool_token(&raw).unwrap_or(from_config),
        None => from_config,
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
/// shell still beats the file. `usage_client.cache_db` is `Option` and is
/// skipped when unset, because exporting a derived default would freeze a path
/// the reader deliberately computes itself.
///
/// `AINB_HOME` is deliberately NOT bridged. Its readers disagree about what it
/// means: `ainb_skill_core::default_ainb_home` and
/// `fleet::plumbing::paths::ainb_home` treat it as the state directory ITSELF,
/// while others join `.agents-in-a-box` onto it. A config key would have to
/// pick one and silently relocate the other's files. The variable keeps working
/// exactly as it does today; resolving the ambiguity is its own change.
pub fn export_env_bridge(config: &AppConfig) {
    let mut planted = BTreeSet::new();
    let mut publish = |name: &'static str, value: String| {
        if std::env::var_os(name).is_none() {
            std::env::set_var(name, value);
            // Remembered so this process does not read its own plant back and
            // pin the value for the rest of the run. See `BRIDGED`.
            planted.insert(name);
        }
    };

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

    *BRIDGED.write().unwrap_or_else(|e| e.into_inner()) = Some(planted);
}

// ── [general] ───────────────────────────────────────────────────────────────

/// Cross-cutting knobs that belong to no other section.
///
/// Example `config.toml`:
/// ```toml
/// [general]
/// syntax_highlight = true
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GeneralConfig {
    /// Colourise fenced code blocks in agent output. Off is a real preference
    /// on a slow terminal or a low-contrast theme; `NO_COLOR` still wins over
    /// both this and its env var.
    #[serde(default = "crate::config::default_true")]
    pub syntax_highlight: bool,

    /// Install skills into each tool's REAL config dir (`~/.claude`,
    /// `~/.codex`, …) rather than ainb's managed sandbox.
    ///
    /// True is the shipped behaviour: an installed skill has to be where the
    /// tool actually looks or it may as well not exist. False routes every
    /// install into the sandbox, which is what a machine shared with a
    /// hand-managed `~/.claude` wants.
    #[serde(default = "crate::config::default_true")]
    pub skill_install_real_homes: bool,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            syntax_highlight: true,
            skill_install_real_homes: true,
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
    use super::TEST_ENV_LOCK as ENV_LOCK;

    /// Set `name` for the duration of `body`, restoring it afterwards.
    ///
    /// Also clears `BRIDGED` on the way out. That set is process-global, so a
    /// test that runs `export_env_bridge` would otherwise leave every later
    /// test in the binary silently ignoring the variables it planted.
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
        clear_bridged_for_test();
        out
    }

    fn from_toml(text: &str) -> AppConfig {
        toml::from_str(text).expect("config parses")
    }

    /// The full ladder for a numeric knob, one rung at a time.
    #[test]
    fn headroom_port_ladder_is_env_then_config_then_default() {
        // `AINB_HEADROOM_PORT` is also mutated by `headroom::tests` and
        // `interactive::session_manager::tests`, which serialize on
        // HEADROOM_ENV_LOCK. A second, private mutex guards nothing: both locks
        // have to be the same one for the variable to be safe.
        let _headroom =
            crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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

    /// The bridge must not pin the value it publishes.
    ///
    /// It writes into the environment this process also reads, so without the
    /// self-planted guard `resolved` finds the startup value on every later
    /// call: `usage_client.headroom_port` could never change again and
    /// `refresh_snapshot` would have nothing to do. Fails before the guard with
    /// `assertion `left == right` failed: left: 8787, right: 9001`.
    #[test]
    fn the_bridge_does_not_pin_the_value_it_published() {
        // Deliberately NOT the headroom port: that variable is shared with two
        // other test modules. `AINB_USAGE_TIMEOUT_SECS` has one reader and no
        // other test.
        let startup = AppConfig::default();
        with_env("AINB_USAGE_TIMEOUT_SECS", None, || {
            export_env_bridge(&startup);
            assert_eq!(
                std::env::var("AINB_USAGE_TIMEOUT_SECS").as_deref(),
                Ok("120"),
                "the bridge must publish for readers in other crates"
            );
            // A later config says 300. Nothing re-runs the bridge, so the
            // planted 120 is still in the environment.
            let edited = from_toml("[usage_client]\nfetch_timeout_secs = 300\n");
            assert_eq!(
                resolved(
                    "AINB_USAGE_TIMEOUT_SECS",
                    edited.usage_client.fetch_timeout_secs
                ),
                300,
                "a value this process planted itself must not outrank the config"
            );
            std::env::remove_var("AINB_USAGE_TIMEOUT_SECS");
        });
    }

    /// An operator's OWN export still wins, because the bridge never plants a
    /// variable that was already set and so never records it.
    #[test]
    fn an_operator_export_still_outranks_the_config() {
        with_env("AINB_USAGE_TIMEOUT_SECS", Some("4242"), || {
            export_env_bridge(&AppConfig::default());
            let edited = from_toml("[usage_client]\nfetch_timeout_secs = 300\n");
            assert_eq!(
                resolved(
                    "AINB_USAGE_TIMEOUT_SECS",
                    edited.usage_client.fetch_timeout_secs
                ),
                4242
            );
        });
    }

    /// `snapshot()` must reflect a config that changed after startup.
    ///
    /// With the old `OnceLock` this fails with
    /// `assertion failed: !snapshot().general.syntax_highlight`: the first read
    /// froze the value, so every promoted key was settable and inert.
    #[test]
    fn the_snapshot_picks_up_a_later_config() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        install_snapshot(AppConfig::default());
        assert!(snapshot().general.syntax_highlight, "the shipped default");

        install_snapshot(from_toml("[general]\nsyntax_highlight = false\n"));
        assert!(
            !snapshot().general.syntax_highlight,
            "a config installed after the first read must be visible"
        );

        // And `refresh_snapshot` re-reads from disk rather than keeping it.
        refresh_snapshot();
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
        assert!(config.usage_client.cache_db.is_none());
        with_env("AINB_USAGE_CACHE_DB", None, || {
            export_env_bridge(&config);
            assert!(
                std::env::var_os("AINB_USAGE_CACHE_DB").is_none(),
                "an absent usage_client.cache_db must not freeze the derived path"
            );
        });
    }

    /// `AINB_HOME` relocates the whole state directory and its readers do not
    /// agree on what it names, so nothing here may set it. See
    /// [`export_env_bridge`].
    #[test]
    fn the_bridge_never_touches_ainb_home() {
        with_env("AINB_HOME", None, || {
            export_env_bridge(&AppConfig::default());
            assert!(std::env::var_os("AINB_HOME").is_none());
        });
    }
}
