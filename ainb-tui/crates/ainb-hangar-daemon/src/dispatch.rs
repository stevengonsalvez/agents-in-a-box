//! P5.3 — env allowlist config loader + task-env builder.
//!
//! This module owns the *IO* half of the env-allowlist feature (the pure policy
//! lives in [`ainb_hangar_core::env_policy`]):
//!
//! - [`load_allow_at`] / [`save_allow_at`] read and write
//!   `~/.ainb/hangar/env.allow.toml`, reading only the `[env].allow` array while
//!   **preserving every foreign section** of the document (per
//!   `reference_toml_value_roundtrip_plugin_config`): the original
//!   [`toml::Value`] is round-tripped untouched and only the `env.allow` array
//!   is rewritten on save. Saves are atomic (temp file + rename).
//! - [`build_task_env`] is the single env-build seam the daemon calls before
//!   spawning a provider subprocess: it applies the env policy to the daemon's
//!   ambient environment **first**, then layers the keychain-resident API keys
//!   on top. Keys are therefore never governed by the ambient allowlist — they
//!   are injected after the policy pass — while a code-injection variable like
//!   `LD_PRELOAD` can never reach the child even if a user allowlisted it.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use ainb_hangar_core::env_policy::{apply_policy, EnvPolicy};

/// The parsed `env.allow.toml` config: the user allow set plus the original
/// document, kept so foreign sections survive a save.
#[derive(Debug, Clone)]
pub struct EnvAllowConfig {
    /// The allowlisted env-var names (the `[env].allow` array). Exact keys plus
    /// `*`-suffix globs (e.g. `LC_*`); see [`EnvPolicy`].
    pub allow: BTreeSet<String>,
    /// The full parsed document, so [`save_allow_at`] can rewrite only the
    /// `env.allow` array and leave every other (foreign) section intact.
    document: toml::Value,
}

/// The TOML table key holding this feature's section.
const ENV_SECTION: &str = "env";
/// The array key inside `[env]` holding the allowlist.
const ALLOW_KEY: &str = "allow";

impl EnvAllowConfig {
    /// Build an [`EnvPolicy`] from this config: the user allow set with the
    /// always-on hardcoded deny family.
    #[must_use]
    pub fn into_policy(self) -> EnvPolicy {
        EnvPolicy {
            allow: self.allow,
            deny_hardcoded: ainb_hangar_core::env_policy::DENY,
        }
    }

    /// Borrow a policy view without consuming the config.
    #[must_use]
    pub fn policy(&self) -> EnvPolicy {
        EnvPolicy {
            allow: self.allow.clone(),
            deny_hardcoded: ainb_hangar_core::env_policy::DENY,
        }
    }
}

/// Resolve the default config path: `{hangar_home}/hangar/env.allow.toml`.
///
/// Mirrors the daemon's home resolution (`$AINB_HANGAR_HOME`, else `~/.ainb`),
/// so the config lives beside `hangar.db`.
///
/// # Errors
///
/// Returns an error if the home directory cannot be resolved.
pub fn default_allow_path() -> anyhow::Result<PathBuf> {
    let dir = match std::env::var_os(ainb_hangar_store::Store::home_env())
        .filter(|p| !p.is_empty())
    {
        Some(p) => PathBuf::from(p),
        None => dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
            .join(".ainb"),
    };
    Ok(dir.join("hangar").join("env.allow.toml"))
}

/// Load the allow config from `path`.
///
/// A missing file yields the operator-default policy (the 12 built-in allow
/// entries) — a first run has no config yet, which is not an error. An existing
/// file is parsed in full; only `[env].allow` is read, but the whole document is
/// retained for foreign-section preservation on the next save.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or is not valid TOML.
pub fn load_allow_at(path: &Path) -> anyhow::Result<EnvAllowConfig> {
    if !path.exists() {
        return Ok(EnvAllowConfig {
            allow: EnvPolicy::default_allow(),
            document: toml::Value::Table(toml::map::Map::new()),
        });
    }

    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
    let document: toml::Value = toml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))?;

    let allow = document
        .get(ENV_SECTION)
        .and_then(|env| env.get(ALLOW_KEY))
        .and_then(toml::Value::as_array)
        .map_or_else(EnvPolicy::default_allow, |arr| {
            arr.iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_string)
                .collect::<BTreeSet<String>>()
        });

    Ok(EnvAllowConfig { allow, document })
}

/// Save the allow config to `path`, preserving every foreign section.
///
/// Only the `[env].allow` array is rewritten from [`EnvAllowConfig::allow`];
/// every other section of the retained document is serialised back verbatim.
/// The write is atomic: serialise to a sibling temp file, then rename over the
/// target so a crash mid-write can never leave a truncated config.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, the temp file
/// cannot be written, or the rename fails.
pub fn save_allow_at(path: &Path, cfg: &EnvAllowConfig) -> anyhow::Result<()> {
    // Clone the retained document and overwrite only `env.allow`.
    let mut doc = cfg.document.clone();
    let table = doc
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not a TOML table"))?;

    let allow_array = toml::Value::Array(
        cfg.allow
            .iter()
            .map(|k| toml::Value::String(k.clone()))
            .collect(),
    );

    // Upsert `[env].allow` without disturbing other keys in `[env]`.
    if let Some(env_tbl) = table.get_mut(ENV_SECTION).and_then(toml::Value::as_table_mut) {
        env_tbl.insert(ALLOW_KEY.to_string(), allow_array);
    } else {
        let mut env_tbl = toml::map::Map::new();
        env_tbl.insert(ALLOW_KEY.to_string(), allow_array);
        table.insert(ENV_SECTION.to_string(), toml::Value::Table(env_tbl));
    }

    let serialised =
        toml::to_string_pretty(&doc).map_err(|e| anyhow::anyhow!("serialise config: {e}"))?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow::anyhow!("create {}: {e}", parent.display()))?;
    }

    // Atomic write: temp sibling + rename. The temp lives in the same directory
    // so the rename is a same-filesystem (atomic) operation.
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, serialised.as_bytes())
        .map_err(|e| anyhow::anyhow!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| anyhow::anyhow!("rename {} -> {}: {e}", tmp.display(), path.display()))?;
    Ok(())
}

/// Build the environment for a spawned provider subprocess.
///
/// Two layers, in order:
/// 1. apply the env [`policy`](EnvPolicy) to the daemon's ambient `parent_env`
///    (allowlist passthrough + hardcoded deny) — see
///    [`apply_policy`](ainb_hangar_core::env_policy::apply_policy);
/// 2. inject the keychain-resident keys *on top*, so an API key reaches the
///    child even though it is never on the ambient allowlist (keys come from the
///    keychain, not the parent environment).
///
/// A keychain key that collides with an allowlisted ambient var wins (the
/// keychain is the source of truth for secrets). A code-injection variable in
/// the deny family is dropped in step 1 and is never a keychain key, so it can
/// never reach the child.
#[must_use]
pub fn build_task_env<I, S>(
    parent_env: &HashMap<String, String, S>,
    keychain_keys: I,
    policy: &EnvPolicy,
) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
    S: std::hash::BuildHasher,
{
    let mut env = apply_policy(policy, parent_env);
    for (k, v) in keychain_keys {
        env.insert(k, v);
    }
    env
}
