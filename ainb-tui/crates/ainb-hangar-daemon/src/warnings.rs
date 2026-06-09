//! P5.6 — danger-full-access warning emission at provider invocation.
//!
//! The daemon warns the user about `danger-full-access` (full filesystem +
//! network, no container sandbox at v1) the **first time each provider is
//! invoked per session**. The pure show/skip decision + ack-key shape live in
//! [`ainb_hangar_core::warnings`]; this module owns the daemon-side `state.toml`
//! IO and the [`maybe_warn_provider`] seam called from the claim loop just
//! before a provider subprocess is spawned.
//!
//! ## Why a daemon-side decision (not only the TUI modal)
//!
//! The first-run *modal* is a TUI concern (rendered + acked host-side). The
//! per-provider-per-session warning is a *dispatch* concern: a task can be
//! enqueued from the CLI with no TUI attached, so the authoritative
//! "have we warned for this (provider, session)?" check has to sit where the
//! provider is actually launched — the daemon. [`maybe_warn_provider`] records
//! the ack into the same `warnings_ack` array the TUI modal writes, so a TUI
//! re-launch sees the daemon's acks and vice-versa (one file, disjoint keys).
//!
//! At v1 the daemon has no live event-push channel to the plugin, so the
//! warning surfaces as a structured `tracing::warn!` log carrying the ack key.
//! When the daemon→plugin event stream lands (a later bead) the same seam emits
//! a `hangar/event` notification — the decision + ack persistence are already
//! correct here; only the delivery channel is deferred.

use std::path::{Path, PathBuf};

use ainb_hangar_core::warnings::{provider_session_key, should_warn_provider};

/// The `state.toml` array key holding dismissed-warning keys (shared with the
/// host-side workspace store + warning store).
const WARNINGS_KEY: &str = "warnings_ack";

/// Resolve the daemon's `state.toml` path: `{hangar_home}/hangar/state.toml`.
///
/// Mirrors [`crate::dispatch::default_allow_path`]'s home resolution
/// (`$AINB_HANGAR_HOME`, else `~/.ainb`) so the daemon, the TUI host, and the
/// CLI all read/write one file.
///
/// # Errors
///
/// Returns an error if the home directory cannot be resolved.
pub fn default_state_path() -> anyhow::Result<PathBuf> {
    let dir = match std::env::var_os(ainb_hangar_store::Store::home_env()).filter(|p| !p.is_empty())
    {
        Some(p) => PathBuf::from(p),
        None => dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("could not resolve home directory"))?
            .join(".ainb"),
    };
    Ok(dir.join("hangar").join("state.toml"))
}

/// Read the `warnings_ack` array from `path` (missing file / key → empty).
///
/// A corrupt entry must never down a dispatch, so a parse failure surfaces as an
/// error the caller can log-and-default.
///
/// # Errors
///
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read_acks_at(path: &Path) -> anyhow::Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = std::fs::read_to_string(path)?;
    let doc: toml::Value = toml::from_str(&raw)?;
    Ok(doc
        .get(WARNINGS_KEY)
        .and_then(toml::Value::as_array)
        .map(|arr| arr.iter().filter_map(toml::Value::as_str).map(str::to_string).collect())
        .unwrap_or_default())
}

/// Append `key` to `path`'s `warnings_ack` array if absent, preserving every
/// foreign key/section, writing atomically (temp + rename). Returns whether a
/// write happened.
///
/// # Errors
///
/// Returns an error if the parent dir, the temp write, or the rename fails.
pub fn ack_at(path: &Path, key: &str) -> anyhow::Result<bool> {
    let mut acks = read_acks_at(path)?;
    if acks.iter().any(|a| a == key) {
        return Ok(false);
    }
    acks.push(key.to_string());

    let mut doc: toml::Value = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw)?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = doc
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("state.toml root is not a table"))?;
    table.insert(
        WARNINGS_KEY.to_string(),
        toml::Value::Array(acks.iter().map(|a| toml::Value::String(a.clone())).collect()),
    );

    let serialised = toml::to_string_pretty(&doc)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp.warnd");
    std::fs::write(&tmp, serialised.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Remove every ack matching `pred` from `path`'s `warnings_ack` array.
///
/// Preserves foreign keys, returns how many were removed. A `--provider` reset
/// passes [`is_provider_ack`](ainb_hangar_core::warnings::is_provider_ack); a
/// full reset matches everything.
///
/// # Errors
///
/// Returns an error if reading or writing the state file fails.
pub fn reset_at(path: &Path, pred: impl Fn(&str) -> bool) -> anyhow::Result<usize> {
    let acks = read_acks_at(path)?;
    let before = acks.len();
    let kept: Vec<String> = acks.into_iter().filter(|a| !pred(a)).collect();
    let removed = before - kept.len();
    if removed == 0 {
        return Ok(0);
    }

    let mut doc: toml::Value = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw)?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = doc
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("state.toml root is not a table"))?;
    table.insert(
        WARNINGS_KEY.to_string(),
        toml::Value::Array(kept.iter().map(|a| toml::Value::String(a.clone())).collect()),
    );
    let serialised = toml::to_string_pretty(&doc)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp.warnd");
    std::fs::write(&tmp, serialised.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(removed)
}

/// The danger-full-access warning copy surfaced to the operator (build-plan
/// "trust-the-user-with-warnings"). Kept as a constant so the TUI modal and the
/// daemon log share verbatim text.
pub const WARNING_TEXT: &str = "danger-full-access: agents run with FULL filesystem and network \
     access — no container sandbox. They can read, write, and delete any file your user can, and \
     reach any network your machine can. Only run agents you trust.";

/// The outcome of a [`maybe_warn_provider`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarnOutcome {
    /// The warning was shown (and its ack key recorded) for this
    /// `(provider, session)` — the first invocation in this session.
    Warned {
        /// The ack key recorded into `warnings_ack`.
        ack_key: String,
    },
    /// Already warned this session for this provider — suppressed.
    Suppressed,
}

impl WarnOutcome {
    /// `true` when this outcome warned (and recorded an ack).
    #[must_use]
    pub const fn warned(&self) -> bool {
        matches!(self, Self::Warned { .. })
    }
}

/// Decide whether to warn about `provider` in `session_id`, recording the ack
/// when it does.
///
/// Reads the recorded acks from `state_path`, asks
/// [`should_warn_provider`](ainb_hangar_core::warnings::should_warn_provider),
/// and on a first invocation logs the [`WARNING_TEXT`] + records the
/// `provider:<name>:session:<sid>` ack so a re-dispatch in the same session is
/// quiet. Returns the [`WarnOutcome`].
///
/// # Errors
///
/// Returns an error if reading or writing `state_path` fails. The caller treats
/// that as non-fatal (log + proceed) — a warning-IO fault must never block a
/// dispatch.
pub fn maybe_warn_provider(
    state_path: &Path,
    provider: &str,
    session_id: &str,
) -> anyhow::Result<WarnOutcome> {
    let acks = read_acks_at(state_path)?;
    if !should_warn_provider(&acks, provider, session_id) {
        return Ok(WarnOutcome::Suppressed);
    }
    let ack_key = provider_session_key(provider, session_id);
    tracing::warn!(
        provider,
        session_id,
        ack_key = %ack_key,
        "{WARNING_TEXT}"
    );
    ack_at(state_path, &ack_key)?;
    Ok(WarnOutcome::Warned { ack_key })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hangar").join("state.toml");
        (dir, path)
    }

    /// RED test 3: first provider invocation per session warns; a re-dispatch
    /// in the SAME session is suppressed.
    #[test]
    fn per_provider_first_invocation_shows_warning() {
        let (_dir, path) = state_path();

        // First dispatch of claude in session s1 → warns + records the ack.
        let first = maybe_warn_provider(&path, "claude", "s1").unwrap();
        assert!(first.warned(), "first invocation must warn: {first:?}");
        assert_eq!(
            read_acks_at(&path).unwrap(),
            vec!["provider:claude:session:s1".to_string()]
        );

        // Re-dispatch claude in the SAME session → suppressed, no second ack.
        let second = maybe_warn_provider(&path, "claude", "s1").unwrap();
        assert_eq!(second, WarnOutcome::Suppressed);
        assert_eq!(read_acks_at(&path).unwrap().len(), 1, "no duplicate ack");
    }

    /// RED test 4: a new session re-warns per provider.
    #[test]
    fn new_session_re_warns_per_provider() {
        let (_dir, path) = state_path();
        assert!(maybe_warn_provider(&path, "claude", "s1").unwrap().warned());

        // Bump the session id → re-warned.
        let new_sess = maybe_warn_provider(&path, "claude", "s2").unwrap();
        assert!(new_sess.warned(), "new session must re-warn: {new_sess:?}");

        let acks = read_acks_at(&path).unwrap();
        assert!(acks.contains(&"provider:claude:session:s1".to_string()));
        assert!(acks.contains(&"provider:claude:session:s2".to_string()));

        // A second provider in an already-warned session also warns (per provider).
        assert!(maybe_warn_provider(&path, "codex", "s1").unwrap().warned());
    }

    /// The daemon writer preserves foreign keys (workspace switch + plugins).
    #[test]
    fn ack_preserves_foreign_sections() {
        let (_dir, path) = state_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "active_workspace = \"01ID_ACME\"\n\n[other_plugin]\nfoo = \"bar\"\n",
        )
        .unwrap();
        maybe_warn_provider(&path, "claude", "s1").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("active_workspace"), "{raw}");
        assert!(raw.contains("[other_plugin]"), "{raw}");
        assert!(raw.contains("warnings_ack"), "{raw}");
    }
}
