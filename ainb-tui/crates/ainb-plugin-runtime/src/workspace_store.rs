//! Host-side logic for the `host/workspace_*` caps (P5.5).
//!
//! Workspace *switching* is host-only state: the active and default workspace
//! ids (plus the danger-warning acks) live in `~/.ainb/hangar/state.toml`, NOT
//! in the daemon's `SQLite` store. The daemon owns the *catalogue* of workspaces
//! (their ULID `id` + `slug` + `name`); the host owns *which one is active*.
//! The plugin reads the catalogue + the active/default flags via these caps and
//! re-fetches its workspace-scoped snapshots whenever the host broadcasts a
//! [`HangarEvent::WorkspaceChanged`].
//!
//! This module mirrors the `secret_store` DI shape: a [`WorkspaceStore`] trait
//! is injected into the runtime (production reads/writes `state.toml`; tests use
//! an in-memory double), and the cap-form-independent helpers
//! ([`set_active_logic`], [`set_default_logic`], …) are unit-testable without a
//! plugin subprocess.
//!
//! ## State identity (critical)
//!
//! `state.toml` is keyed by the workspace's stable **ULID `id`**, never its
//! `slug`. A `set_active`/`set_default` request carries an `id`; the store
//! validates it against the known catalogue (rejecting an unknown id with
//! `-32602`) before writing. The slug is display-only.
//!
//! ## `state.toml` shape
//!
//! ```toml
//! active_workspace = "01J9ZX8QK7"
//! default_workspace = "01J9ZX8QK7"
//! warnings_ack = []
//! ```
//!
//! Foreign sections are preserved on save (read keys, stash the original
//! [`toml::Value`], merge on write, atomic temp+rename) — the same pattern P5.3
//! used for `env.allow.toml`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ainb_hangar_proto::events::HangarEvent;
use ainb_plugin_protocol::errors::RpcError;
use ainb_plugin_protocol::manifest::CapabilityGrant;
use ainb_plugin_protocol::params::{
    WorkspaceEntry, WorkspaceGetActiveResult, WorkspaceListResult, WorkspaceSetActiveResult,
    WorkspaceSetDefaultResult,
};
use parking_lot::RwLock;
use serde_json::Value;
use tokio::sync::broadcast;

/// A workspace in the host's catalogue: stable id + display fields.
///
/// The daemon is the source of truth for this catalogue; the host caches it so
/// `host/workspace_list` can resolve active/default flags without a socket
/// round-trip. `id` is the ULID `state.toml` keys on; `slug`/`name` are display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceInfo {
    /// Stable ULID workspace id.
    pub id: String,
    /// Short display handle (e.g. `default`).
    pub slug: String,
    /// Human-readable display name.
    pub name: String,
}

/// The switching state persisted in `state.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SwitchState {
    /// The explicitly-selected active workspace id, if any.
    pub active: Option<String>,
    /// The configured default workspace id, if any.
    pub default: Option<String>,
}

impl SwitchState {
    /// Resolve the *effective* active workspace id against `catalogue`.
    ///
    /// Order: explicit `active` (if it still names a known workspace), else
    /// `default` (likewise), else the first catalogue entry, else `None`.
    #[must_use]
    pub fn effective_active(&self, catalogue: &[WorkspaceInfo]) -> Option<String> {
        let known = |id: &str| catalogue.iter().any(|w| w.id == id);
        self.active
            .as_deref()
            .filter(|id| known(id))
            .or_else(|| self.default.as_deref().filter(|id| known(id)))
            .map(str::to_string)
            .or_else(|| catalogue.first().map(|w| w.id.clone()))
    }
}

/// The injected host workspace store.
///
/// Production reads/writes `~/.ainb/hangar/state.toml` and pushes
/// `WorkspaceChanged` on a broadcast channel; tests use an in-memory double.
/// All methods are infallible at the trait boundary except the IO-touching
/// setters, which surface an [`RpcError`] the handler returns verbatim.
pub trait WorkspaceStore: Send + Sync {
    /// The workspace catalogue (daemon-sourced, host-cached).
    fn catalogue(&self) -> Vec<WorkspaceInfo>;

    /// The current switching state (active + default ids).
    fn switch_state(&self) -> SwitchState;

    /// Persist `active` as the active workspace id.
    ///
    /// # Errors
    /// Returns an [`RpcError`] when the underlying state file cannot be written.
    fn set_active(&self, active: &str) -> Result<(), RpcError>;

    /// Persist `default` as the default workspace id (does NOT change active).
    ///
    /// # Errors
    /// Returns an [`RpcError`] when the underlying state file cannot be written.
    fn set_default(&self, default: &str) -> Result<(), RpcError>;

    /// Broadcast a [`HangarEvent::WorkspaceChanged`] to subscribers.
    fn broadcast(&self, event: HangarEvent);
}

/// A shared, thread-safe handle to the host's workspace store.
pub type SharedWorkspaceStore = Arc<dyn WorkspaceStore>;

/// Build a `WorkspaceChanged` event from `from`/`to` ids.
#[must_use]
pub const fn workspace_changed(from: Option<String>, to: String) -> HangarEvent {
    HangarEvent::WorkspaceChanged { from, to }
}

/// `host/workspace_list` logic: resolve every catalogue row's active/default
/// flags against the switch state.
#[must_use]
pub fn list_logic(store: &dyn WorkspaceStore) -> Value {
    let catalogue = store.catalogue();
    let state = store.switch_state();
    let effective = state.effective_active(&catalogue);
    let workspaces = catalogue
        .iter()
        .map(|w| WorkspaceEntry {
            id: w.id.clone(),
            slug: w.slug.clone(),
            name: w.name.clone(),
            active: effective.as_deref() == Some(w.id.as_str()),
            default: state.default.as_deref() == Some(w.id.as_str()),
        })
        .collect();
    serde_json::to_value(WorkspaceListResult { workspaces })
        .expect("WorkspaceListResult serializable")
}

/// `host/workspace_get_active` logic: the effective active id (active → default
/// → first), or `None`.
#[must_use]
pub fn get_active_logic(store: &dyn WorkspaceStore) -> Value {
    let catalogue = store.catalogue();
    let workspace_id = store.switch_state().effective_active(&catalogue);
    serde_json::to_value(WorkspaceGetActiveResult { workspace_id })
        .expect("WorkspaceGetActiveResult serializable")
}

/// Validate that `id` names a known workspace, else `-32602`.
fn ensure_known(store: &dyn WorkspaceStore, id: &str) -> Result<(), RpcError> {
    if store.catalogue().iter().any(|w| w.id == id) {
        Ok(())
    } else {
        Err(RpcError::invalid_params(format!(
            "unknown workspace id: {id:?}"
        )))
    }
}

/// The `workspace:write` cap gate: any granted form (bool-true or a non-empty
/// list) permits the write; an ungranted plugin gets `-32001` before the store
/// is touched.
///
/// # Errors
/// Returns `-32001 CAPABILITY_DENIED` when the grant is absent.
pub fn ensure_write_granted(grant: &CapabilityGrant) -> Result<(), RpcError> {
    if grant.is_granted() {
        Ok(())
    } else {
        Err(RpcError::capability_denied("workspace:write"))
    }
}

/// `host/workspace_set_active` logic: gate on `workspace:write`, validate the
/// id, persist it, and broadcast `WorkspaceChanged { from, to }`.
///
/// `from` is the *previously effective* active id, so a no-op switch (same id)
/// still validates but the event records `from == to` honestly.
///
/// # Errors
/// Returns `-32001` when the cap is ungranted (before any store hit), `-32602`
/// for an unknown id, or the store's write error verbatim.
pub fn set_active_logic(
    grant: &CapabilityGrant,
    store: &dyn WorkspaceStore,
    id: &str,
) -> Result<Value, RpcError> {
    ensure_write_granted(grant)?;
    ensure_known(store, id)?;
    let catalogue = store.catalogue();
    let from = store.switch_state().effective_active(&catalogue);
    store.set_active(id)?;
    store.broadcast(workspace_changed(from, id.to_string()));
    Ok(serde_json::to_value(WorkspaceSetActiveResult {})
        .expect("WorkspaceSetActiveResult serializable"))
}

/// `host/workspace_set_default` logic.
///
/// Gates on `workspace:write`, validates the id, and persists it as the
/// default. Independent of the active workspace — never changes `active` and
/// emits no `WorkspaceChanged` event.
///
/// # Errors
/// Returns `-32001` when the cap is ungranted (before any store hit), `-32602`
/// for an unknown id, or the store's write error verbatim.
pub fn set_default_logic(
    grant: &CapabilityGrant,
    store: &dyn WorkspaceStore,
    id: &str,
) -> Result<Value, RpcError> {
    ensure_write_granted(grant)?;
    ensure_known(store, id)?;
    store.set_default(id)?;
    Ok(serde_json::to_value(WorkspaceSetDefaultResult {})
        .expect("WorkspaceSetDefaultResult serializable"))
}

// =====================================================================
// state.toml-backed production store
// =====================================================================

/// The `state.toml` TOML keys.
const ACTIVE_KEY: &str = "active_workspace";
const DEFAULT_KEY: &str = "default_workspace";

/// Resolve the default state-file path: `{hangar_home}/hangar/state.toml`.
///
/// Mirrors the daemon's home resolution (`$AINB_HANGAR_HOME`, else `~/.ainb`),
/// so the file lives beside `hangar.db` / `env.allow.toml`.
///
/// # Errors
/// Returns an error if the home directory cannot be resolved.
pub fn default_state_path() -> std::io::Result<PathBuf> {
    let dir = match std::env::var_os("AINB_HANGAR_HOME").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => dirs::home_dir()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "could not resolve home directory",
                )
            })?
            .join(".ainb"),
    };
    Ok(dir.join("hangar").join("state.toml"))
}

/// Read the [`SwitchState`] from `path`, treating a missing file as empty.
///
/// Only the `active_workspace` / `default_workspace` keys are read; every other
/// (foreign) section is ignored here and preserved by [`write_switch_state_at`].
///
/// # Errors
/// Returns an error if the file exists but cannot be read or parsed.
pub fn read_switch_state_at(path: &Path) -> std::io::Result<SwitchState> {
    if !path.exists() {
        return Ok(SwitchState::default());
    }
    let raw = std::fs::read_to_string(path)?;
    let doc: toml::Value = toml::from_str(&raw)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let read = |key: &str| {
        doc.get(key)
            .and_then(toml::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Ok(SwitchState {
        active: read(ACTIVE_KEY),
        default: read(DEFAULT_KEY),
    })
}

/// Write `state` to `path`, preserving every foreign key/section.
///
/// Reads the existing document (if any), upserts only `active_workspace` /
/// `default_workspace`, and writes back atomically (temp sibling + rename) so a
/// crash mid-write can never truncate the file. A `None` field clears the key.
///
/// # Errors
/// Returns an error if the parent dir, the temp write, or the rename fails.
pub fn write_switch_state_at(path: &Path, state: &SwitchState) -> std::io::Result<()> {
    // Start from the existing document so foreign sections survive.
    let mut doc: toml::Value = if path.exists() {
        let raw = std::fs::read_to_string(path)?;
        toml::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let table = doc.as_table_mut().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "state.toml root is not a table")
    })?;

    let upsert = |table: &mut toml::map::Map<String, toml::Value>, key: &str, val: &Option<String>| {
        match val {
            Some(v) => {
                table.insert(key.to_string(), toml::Value::String(v.clone()));
            }
            None => {
                table.remove(key);
            }
        }
    };
    upsert(table, ACTIVE_KEY, &state.active);
    upsert(table, DEFAULT_KEY, &state.default);

    let serialised = toml::to_string_pretty(&doc)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, serialised.as_bytes())?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// The production `WorkspaceStore`: `state.toml`-backed switch state + a
/// daemon-sourced catalogue + a `tokio::sync::broadcast` event channel.
///
/// The catalogue is supplied at construction (the host populates it from the
/// daemon's `workspace/list` snapshot). The switch state is read from / written
/// to `state.toml` on every access so a CLI write (`ainb hangar ...`) and the
/// TUI stay coherent without an in-process cache to invalidate.
pub struct StateTomlWorkspaceStore {
    path: PathBuf,
    catalogue: RwLock<Vec<WorkspaceInfo>>,
    events: broadcast::Sender<HangarEvent>,
}

impl StateTomlWorkspaceStore {
    /// Construct a store backed by `path` with the given workspace `catalogue`.
    #[must_use]
    pub fn new(path: PathBuf, catalogue: Vec<WorkspaceInfo>) -> Self {
        let (events, _rx) = broadcast::channel(64);
        Self {
            path,
            catalogue: RwLock::new(catalogue),
            events,
        }
    }

    /// Replace the cached catalogue (e.g. when the daemon's workspace list
    /// changes). Switching/listing immediately reflects the new set.
    pub fn set_catalogue(&self, catalogue: Vec<WorkspaceInfo>) {
        *self.catalogue.write() = catalogue;
    }

    /// Subscribe to `WorkspaceChanged` broadcasts.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<HangarEvent> {
        self.events.subscribe()
    }
}

impl WorkspaceStore for StateTomlWorkspaceStore {
    fn catalogue(&self) -> Vec<WorkspaceInfo> {
        self.catalogue.read().clone()
    }

    fn switch_state(&self) -> SwitchState {
        read_switch_state_at(&self.path).unwrap_or_default()
    }

    fn set_active(&self, active: &str) -> Result<(), RpcError> {
        let mut state = self.switch_state();
        state.active = Some(active.to_string());
        write_switch_state_at(&self.path, &state)
            .map_err(|e| RpcError::internal(format!("write state.toml: {e}")))
    }

    fn set_default(&self, default: &str) -> Result<(), RpcError> {
        let mut state = self.switch_state();
        state.default = Some(default.to_string());
        write_switch_state_at(&self.path, &state)
            .map_err(|e| RpcError::internal(format!("write state.toml: {e}")))
    }

    fn broadcast(&self, event: HangarEvent) {
        // A send error only means there are no live subscribers — not a fault.
        let _ = self.events.send(event);
    }
}

/// Build the default workspace store for production.
///
/// Reads `state.toml` from the resolved hangar home with an **empty** catalogue
/// (the host repopulates it from the daemon's workspace list once connected). A
/// home-resolution failure falls back to a relative path so the runtime still
/// builds — the first write then surfaces the IO error to the plugin.
#[must_use]
pub fn default_store() -> SharedWorkspaceStore {
    let path = default_state_path()
        .unwrap_or_else(|_| PathBuf::from(".ainb").join("hangar").join("state.toml"));
    Arc::new(StateTomlWorkspaceStore::new(path, Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalogue() -> Vec<WorkspaceInfo> {
        vec![
            WorkspaceInfo {
                id: "01ID_DEFAULT".into(),
                slug: "default".into(),
                name: "Default".into(),
            },
            WorkspaceInfo {
                id: "01ID_ACME".into(),
                slug: "acme".into(),
                name: "Acme".into(),
            },
        ]
    }

    #[test]
    fn effective_active_prefers_explicit_then_default_then_first() {
        let cat = catalogue();
        // Explicit active wins.
        let s = SwitchState {
            active: Some("01ID_ACME".into()),
            default: Some("01ID_DEFAULT".into()),
        };
        assert_eq!(s.effective_active(&cat).as_deref(), Some("01ID_ACME"));
        // Falls back to default when active unset.
        let s = SwitchState {
            active: None,
            default: Some("01ID_ACME".into()),
        };
        assert_eq!(s.effective_active(&cat).as_deref(), Some("01ID_ACME"));
        // Falls back to first when both unset.
        let s = SwitchState::default();
        assert_eq!(s.effective_active(&cat).as_deref(), Some("01ID_DEFAULT"));
        // Empty catalogue → None.
        assert_eq!(SwitchState::default().effective_active(&[]), None);
    }

    #[test]
    fn effective_active_ignores_stale_id() {
        // An active id that no longer names a known workspace is dropped.
        let cat = catalogue();
        let s = SwitchState {
            active: Some("01ID_GONE".into()),
            default: Some("01ID_ACME".into()),
        };
        assert_eq!(s.effective_active(&cat).as_deref(), Some("01ID_ACME"));
    }

    #[test]
    fn ensure_known_rejects_unknown() {
        let store = StateTomlWorkspaceStore::new(
            std::env::temp_dir().join("nonexistent-state.toml"),
            catalogue(),
        );
        let err = ensure_known(&store, "01ID_GONE").unwrap_err();
        assert_eq!(err.code, ainb_plugin_protocol::errors::INVALID_PARAMS);
        assert!(ensure_known(&store, "01ID_ACME").is_ok());
    }
}
