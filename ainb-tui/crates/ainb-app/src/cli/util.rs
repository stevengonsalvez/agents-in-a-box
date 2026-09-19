// ABOUTME: Shared CLI utilities for session lookup and common operations
//
// Provides consistent session finding logic across all CLI commands.
// Uses prefix matching for both UUID and workspace name for user convenience.

use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

use crate::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use crate::models::SessionAgentType;
use ainb_hangar_client::DaemonClient;
use ainb_hangar_proto::protocol::CAP_WORKSPACE_SESSIONS;
use ainb_hangar_proto::sessions::{
    WorkspaceSessionDeleteParams, WorkspaceSessionEntry, WorkspaceSessionListParams,
    WorkspaceSessionUpsertParams,
};

/// Convert a proto [`WorkspaceSessionEntry`] into local [`SessionMetadata`].
///
/// # Errors
///
/// Returns why the entry cannot be used when its `session_id` is not a UUID.
/// The caller skips such a row; it never invents an id for it, because an
/// id minted on read would differ on every read and match no worktree.
pub fn entry_to_metadata(entry: &WorkspaceSessionEntry) -> Result<SessionMetadata, String> {
    let session_id = Uuid::parse_str(&entry.session_id).map_err(|e| {
        format!(
            "session {:?} has a non-UUID id: {e}",
            entry.tmux_session_name
        )
    })?;
    let created_at = DateTime::from_timestamp_millis(entry.created_at).unwrap_or_else(Utc::now);
    let agent_type = serde_json::from_value(serde_json::Value::String(entry.agent_type.clone()))
        .unwrap_or(SessionAgentType::Claude);
    let model_source =
        serde_json::from_value(serde_json::Value::String(entry.model_source.clone()))
            .unwrap_or(ModelSource::LegacyTyped);
    let codex_model = entry
        .codex_model
        .as_ref()
        .and_then(|cm| serde_json::from_value(serde_json::Value::String(cm.clone())).ok());

    Ok(SessionMetadata {
        session_id,
        tmux_session_name: entry.tmux_session_name.clone(),
        worktree_path: PathBuf::from(&entry.worktree_path),
        workspace_name: entry.workspace_name.clone(),
        created_at,
        agent_type,
        headroom_enabled: entry.headroom_enabled,
        rtk_enabled: entry.rtk_enabled,
        skip_permissions: entry.skip_permissions,
        model: entry.model.clone(),
        model_source,
        codex_model,
        codex_thread_id: entry.codex_thread_id.clone(),
    })
}

/// Convert local [`SessionMetadata`] into proto [`WorkspaceSessionEntry`].
#[must_use]
pub fn metadata_to_entry(meta: &SessionMetadata) -> WorkspaceSessionEntry {
    let agent_type = serde_json::to_value(meta.agent_type)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "Claude".to_string());
    let model_source = serde_json::to_value(meta.model_source)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| "LegacyTyped".to_string());
    let codex_model = meta
        .codex_model
        .as_ref()
        .and_then(|cm| serde_json::to_value(cm).ok().and_then(|v| v.as_str().map(String::from)));

    WorkspaceSessionEntry {
        session_id: meta.session_id.to_string(),
        tmux_session_name: meta.tmux_session_name.clone(),
        worktree_path: meta.worktree_path.to_string_lossy().to_string(),
        workspace_name: meta.workspace_name.clone(),
        created_at: meta.created_at.timestamp_millis(),
        agent_type,
        headroom_enabled: meta.headroom_enabled,
        rtk_enabled: meta.rtk_enabled,
        skip_permissions: meta.skip_permissions,
        model: meta.model.clone(),
        model_source,
        codex_model,
        codex_thread_id: meta.codex_thread_id.clone(),
    }
}

/// Where this process reads and writes sessions.
///
/// ```text
/// resolve ──▶ hello advertises hangar.workspace.sessions?
///               └─▶ session_list says import_complete? ──▶ Daemon
///             anything else ─────────────────────────────▶ File
/// ```
///
/// Decided once per process by [`session_source`], so one command cannot
/// read from the table and write to the file. On [`Daemon`](Self::Daemon)
/// the table is authoritative and `sessions.json` is never written; a daemon
/// that fails later in the command is an error, not a fallback. On
/// [`File`](Self::File) the flocked `SessionStore` path is used unchanged.
#[derive(Debug, Clone)]
pub enum SessionSource {
    /// The daemon's `sessions` table, behind RPC.
    Daemon(DaemonClient),
    /// `~/.agents-in-a-box/sessions.json`.
    File,
}

fn daemon_io_error(what: &str, error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(format!("daemon session {what} failed: {error}"))
}

impl SessionSource {
    /// Decide against the daemon named by the environment.
    pub async fn resolve() -> Self {
        match DaemonClient::from_env() {
            Ok(client) => Self::resolve_with(client).await,
            Err(_) => Self::File,
        }
    }

    /// Decide against the daemon at `socket` with `token` (the test seam).
    pub async fn resolve_at(socket: PathBuf, token: String) -> Self {
        Self::resolve_with(DaemonClient::with_parts(socket, token)).await
    }

    async fn resolve_with(client: DaemonClient) -> Self {
        let Ok(hello) = client.hello().await else {
            return Self::File;
        };
        if !hello.advertises(CAP_WORKSPACE_SESSIONS) {
            return Self::File;
        }
        let probe = WorkspaceSessionListParams {
            workspace_name: None,
            limit: Some(1),
        };
        match client.workspace_session_list(probe).await {
            Ok(res) if res.import_complete => Self::Daemon(client),
            Ok(_) => {
                eprintln!(
                    "Warning: the daemon has not finished importing sessions.json; \
                     reading the file this run."
                );
                Self::File
            }
            Err(e) => {
                eprintln!(
                    "Warning: daemon session list failed ({e}); reading sessions.json this run."
                );
                Self::File
            }
        }
    }

    /// Read the whole store from this source.
    ///
    /// # Errors
    ///
    /// On [`Daemon`](Self::Daemon), when the RPC fails. The file is not read
    /// instead: the process already decided where its sessions live.
    pub async fn load(&self) -> std::io::Result<SessionStore> {
        let client = match self {
            Self::File => return Ok(SessionStore::load()),
            Self::Daemon(client) => client,
        };
        let res = client
            .workspace_session_list(WorkspaceSessionListParams::default())
            .await
            .map_err(|e| daemon_io_error("list", e))?;
        if res.truncated {
            eprintln!(
                "Warning: the daemon returned only the newest {} sessions.",
                res.sessions.len()
            );
        }
        let mut store = SessionStore::default();
        for entry in &res.sessions {
            match entry_to_metadata(entry) {
                Ok(meta) => {
                    store.sessions.insert(meta.tmux_session_name.clone(), meta);
                }
                Err(why) => eprintln!("Warning: skipping {why}"),
            }
        }
        Ok(store)
    }

    /// Apply `f` to the store and persist the difference.
    ///
    /// On [`Daemon`](Self::Daemon) the store is read fresh, `f` runs, and only
    /// what changed is written: one delete per session id that disappeared,
    /// one upsert per session that is new or different. The `sessions.json`
    /// lock is held across the whole read-modify-write so two CLI processes
    /// cannot interleave, as on the file path.
    ///
    /// # Errors
    ///
    /// The first failed RPC, so a caller such as `ainb run` can roll back.
    pub async fn mutate<F>(&self, f: F) -> std::io::Result<()>
    where
        F: FnOnce(&mut SessionStore),
    {
        let client = match self {
            Self::File => return SessionStore::mutate(f),
            Self::Daemon(client) => client,
        };
        let _guard = SessionStore::lock()?;
        let mut store = self.load().await?;
        let before = entries_by_id(&store);
        f(&mut store);
        let after = entries_by_id(&store);

        for (id, _) in before.iter().filter(|(id, _)| !after.contains_key(*id)) {
            client
                .workspace_session_delete(WorkspaceSessionDeleteParams {
                    session_id: Some(id.to_string()),
                    tmux_session_name: None,
                })
                .await
                .map_err(|e| daemon_io_error("delete", e))?;
        }
        for (id, entry) in &after {
            if before.get(id) == Some(entry) {
                continue;
            }
            client
                .workspace_session_upsert(WorkspaceSessionUpsertParams {
                    session: entry.clone(),
                })
                .await
                .map_err(|e| daemon_io_error("write", e))?;
        }
        Ok(())
    }
}

fn entries_by_id(store: &SessionStore) -> HashMap<Uuid, WorkspaceSessionEntry> {
    store.sessions.values().map(|m| (m.session_id, metadata_to_entry(m))).collect()
}

static SESSION_SOURCE: tokio::sync::OnceCell<SessionSource> = tokio::sync::OnceCell::const_new();

/// This process's [`SessionSource`], resolved on first use and then fixed.
pub async fn session_source() -> &'static SessionSource {
    SESSION_SOURCE.get_or_init(SessionSource::resolve).await
}

fn run_async<F: std::future::Future<Output = T> + Send, T: Send>(fut: F) -> T {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        match handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(|| handle.block_on(fut))
            }
            _ => std::thread::scope(|s| {
                s.spawn(|| handle.block_on(fut)).join().expect("thread join")
            }),
        }
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("create tokio runtime")
            .block_on(fut)
    }
}

/// Load the session store from this process's [`session_source`].
///
/// # Errors
///
/// When the daemon was chosen and its RPC fails.
pub async fn load_session_store_async() -> std::io::Result<SessionStore> {
    session_source().await.load().await
}

/// [`load_session_store_async`] from sync code.
///
/// # Errors
///
/// As [`load_session_store_async`].
pub fn load_session_store() -> std::io::Result<SessionStore> {
    run_async(load_session_store_async())
}

/// Mutate the session store through this process's [`session_source`].
///
/// # Errors
///
/// The lock, file or RPC failure; never swallowed.
pub async fn mutate_session_store_async<F>(f: F) -> std::io::Result<()>
where
    F: FnOnce(&mut SessionStore),
{
    session_source().await.mutate(f).await
}

/// [`mutate_session_store_async`] from sync code.
///
/// # Errors
///
/// As [`mutate_session_store_async`].
pub fn mutate_session_store<F>(f: F) -> std::io::Result<()>
where
    F: FnOnce(&mut SessionStore) + Send,
{
    run_async(mutate_session_store_async(f))
}

/// Find a session by ID (full or partial UUID) or workspace name prefix
///
/// Matching priority:
/// 1. Exact UUID match
/// 2. UUID prefix match (e.g., "abc" matches "abc12345-...")
/// 3. Workspace name prefix match (case-insensitive)
///
/// Returns an error if no match is found or if multiple sessions match.
pub fn find_session(id_or_name: &str) -> Result<SessionMetadata> {
    let store = load_session_store()?;
    find_session_in_store(id_or_name, &store)
}

/// Find a session within a given store (testable version)
///
/// This function accepts a store reference for easier unit testing.
pub fn find_session_in_store(id_or_name: &str, store: &SessionStore) -> Result<SessionMetadata> {
    if store.sessions.is_empty() {
        return Err(anyhow!(
            "No sessions found. Run 'ainb run' to create a session."
        ));
    }

    // First, try exact UUID match
    if let Ok(uuid) = Uuid::parse_str(id_or_name) {
        for session in store.sessions.values() {
            if session.session_id == uuid {
                return Ok(session.clone());
            }
        }
    }

    // Try UUID prefix match (case-insensitive)
    let id_lower = id_or_name.to_lowercase();
    let uuid_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| s.session_id.to_string().to_lowercase().starts_with(&id_lower))
        .collect();

    match uuid_matches.len() {
        1 => return Ok(uuid_matches[0].clone()),
        n if n > 1 => {
            let ids: Vec<String> = uuid_matches
                .iter()
                .map(|s| {
                    format!(
                        "  {} ({})",
                        &s.session_id.to_string()[..8],
                        s.display_workspace_name()
                    )
                })
                .collect();
            return Err(anyhow!(
                "Ambiguous session ID prefix '{id_or_name}'. Matches:\n{}",
                ids.join("\n")
            ));
        }
        _ => {}
    }

    // Try workspace name prefix match (case-insensitive).
    //
    // Matches the DISPLAYED name (re-derived from the worktree path, what
    // `ainb list` and the TUI print) OR the name persisted at creation time.
    // Displayed alone would strand legacy records whose path has moved;
    // persisted alone would mean a name the user just read off `ainb list`
    // does not resolve, which is precisely the drift this pair of surfaces is
    // supposed to have stopped having.
    let name_matches: Vec<&SessionMetadata> = store
        .sessions
        .values()
        .filter(|s| {
            s.display_workspace_name().to_lowercase().starts_with(&id_lower)
                || s.workspace_name.to_lowercase().starts_with(&id_lower)
        })
        .collect();

    match name_matches.len() {
        1 => return Ok(name_matches[0].clone()),
        n if n > 1 => {
            let names: Vec<String> = name_matches
                .iter()
                .map(|s| {
                    format!(
                        "  {} ({})",
                        s.display_workspace_name(),
                        &s.session_id.to_string()[..8]
                    )
                })
                .collect();
            return Err(anyhow!(
                "Ambiguous session name prefix '{id_or_name}'. Matches:\n{}",
                names.join("\n")
            ));
        }
        _ => {}
    }

    // No match found - provide helpful error message
    let available: Vec<String> = store
        .sessions
        .values()
        .map(|s| {
            format!(
                "  {} ({})",
                &s.session_id.to_string()[..8],
                s.display_workspace_name()
            )
        })
        .collect();

    if available.is_empty() {
        Err(anyhow!(
            "No sessions found. Run 'ainb run' to create a session."
        ))
    } else {
        Err(anyhow!(
            "No session found matching '{id_or_name}'. Available sessions:\n{}",
            available.join("\n")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::session::SessionAgentType;
    use chrono::Utc;
    use std::path::PathBuf;

    fn create_test_store() -> SessionStore {
        let mut store = SessionStore::default();

        let session1 = SessionMetadata {
            session_id: Uuid::parse_str("12345678-1234-1234-1234-123456789abc").unwrap(),
            tmux_session_name: "tmux_project-a".to_string(),
            worktree_path: PathBuf::from("/tmp/project-a"),
            workspace_name: "project-alpha".to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        let session2 = SessionMetadata {
            session_id: Uuid::parse_str("abcdef12-abcd-abcd-abcd-abcdef123456").unwrap(),
            tmux_session_name: "tmux_project-b".to_string(),
            worktree_path: PathBuf::from("/tmp/project-b"),
            workspace_name: "project-beta".to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        store.sessions.insert(session1.tmux_session_name.clone(), session1);
        store.sessions.insert(session2.tmux_session_name.clone(), session2);

        store
    }

    #[test]
    fn test_find_by_exact_uuid() {
        let store = create_test_store();
        let result = find_session_in_store("12345678-1234-1234-1234-123456789abc", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_uuid_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("12345678", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_workspace_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("project-a", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-alpha");
    }

    #[test]
    fn test_find_by_workspace_prefix_case_insensitive() {
        let store = create_test_store();
        let result = find_session_in_store("PROJECT-B", &store);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().workspace_name, "project-beta");
    }

    #[test]
    fn test_ambiguous_prefix() {
        let store = create_test_store();
        let result = find_session_in_store("project-", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Ambiguous"));
    }

    #[test]
    fn test_not_found() {
        let store = create_test_store();
        let result = find_session_in_store("nonexistent", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No session found"));
    }

    #[test]
    fn a_non_uuid_entry_is_refused_not_given_a_fresh_id() {
        let mut entry = metadata_to_entry(&create_test_store().sessions["tmux_project-a"]);
        entry.session_id = "01J8Z3K6Q2N4T5V7W9X0Y1Z2A3".to_string();
        assert!(entry_to_metadata(&entry).is_err());
    }

    #[test]
    fn entry_round_trip_keeps_the_id() {
        let meta = create_test_store().sessions["tmux_project-a"].clone();
        let back = entry_to_metadata(&metadata_to_entry(&meta)).unwrap();
        assert_eq!(back.session_id, meta.session_id);
    }

    #[test]
    fn test_empty_store() {
        let store = SessionStore::default();
        let result = find_session_in_store("anything", &store);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No sessions found"));
    }
}
