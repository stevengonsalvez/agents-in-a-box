// ABOUTME: CLI recover command - detect, list, resume, and clean up orphaned sessions
//
// Provides recovery tools for sessions that became disconnected:
// - list:    Scan ~/.claude/agents/ for orphaned JSON files and
//            ~/.agents-in-a-box/worktrees/by-session/ for broken symlinks.
//            Cross-reference with SessionStore. Show metadata and age.
// - resume:  Re-register an orphaned session in SessionStore if tmux is alive.
// - cleanup: Kill tmux, remove worktree, remove metadata file. --force skips confirmation.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use uuid::Uuid;

use super::OutputFormat;
use crate::interactive::session_manager::{SessionMetadata, SessionStore};
use crate::models::session::SessionAgentType;

/// Recover orphaned or crashed sessions. Description set in `cli/registry.rs`.
#[derive(clap::Subcommand)]
pub enum RecoverCommands {
    /// List orphaned sessions and broken worktree symlinks
    List,
    /// Resume an orphaned session by re-registering it in the session store
    Resume {
        /// Session ID, tmux name, or workspace name prefix
        session: String,
    },
    /// Clean up orphaned sessions and broken worktrees
    Cleanup {
        /// Specific session to clean up (all if omitted)
        session: Option<String>,
        /// Skip confirmation prompt
        #[arg(long, short)]
        force: bool,
    },
}

/// How an orphaned session was detected
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum OrphanSource {
    /// Session in store but tmux is dead
    StaleMetadata,
    /// JSON file in ~/.claude/agents/ not tracked in store
    AgentFile,
    /// Broken or untracked symlink in worktrees/by-session/
    BrokenSymlink,
}

impl std::fmt::Display for OrphanSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleMetadata => write!(f, "Stale Metadata"),
            Self::AgentFile => write!(f, "Agent File"),
            Self::BrokenSymlink => write!(f, "Broken Symlink"),
        }
    }
}

/// An orphaned session detected during recovery scan
#[derive(Debug, Clone, Serialize)]
pub struct OrphanedSession {
    /// Unique identifier (UUID string, tmux name, or file stem)
    pub id: String,
    /// How this orphan was detected
    pub source: OrphanSource,
    /// Tmux session name, if known
    pub tmux_session_name: Option<String>,
    /// Whether the tmux session is still alive
    pub is_tmux_alive: bool,
    /// Worktree path, if known
    pub worktree_path: Option<String>,
    /// Workspace name, if known
    pub workspace_name: Option<String>,
    /// When the session was created
    pub created_at: Option<DateTime<Utc>>,
    /// Path to the source file (agent JSON or symlink)
    pub file_path: Option<String>,
}

/// Check if a tmux session exists by name
fn tmux_session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Scan for orphaned sessions from all sources
pub fn find_orphaned_sessions() -> Result<Vec<OrphanedSession>> {
    let store = SessionStore::load();
    let tracked_tmux: Vec<&str> = store.tracked_tmux_names();

    let mut orphans = Vec::new();

    // Source 1: Sessions in store whose tmux is dead
    for metadata in store.sessions.values() {
        if !tmux_session_exists(&metadata.tmux_session_name) {
            orphans.push(OrphanedSession {
                id: metadata.session_id.to_string(),
                source: OrphanSource::StaleMetadata,
                tmux_session_name: Some(metadata.tmux_session_name.clone()),
                is_tmux_alive: false,
                worktree_path: Some(metadata.worktree_path.display().to_string()),
                workspace_name: Some(metadata.workspace_name.clone()),
                created_at: Some(metadata.created_at),
                file_path: Some(SessionStore::storage_path().display().to_string()),
            });
        }
    }

    // Source 2: JSON files in ~/.claude/agents/ not tracked in store
    if let Ok(agent_orphans) = scan_agent_files(&tracked_tmux) {
        orphans.extend(agent_orphans);
    }

    // Source 3: Broken/untracked symlinks in worktrees/by-session/
    if let Ok(symlink_orphans) = scan_broken_symlinks(&store) {
        orphans.extend(symlink_orphans);
    }

    // Deduplicate by id
    orphans.sort_by(|a, b| a.id.cmp(&b.id));
    orphans.dedup_by(|a, b| a.id == b.id);

    Ok(orphans)
}

/// Scan ~/.claude/agents/ for JSON session files not in `SessionStore`
fn scan_agent_files(tracked_tmux: &[&str]) -> Result<Vec<OrphanedSession>> {
    let home = dirs::home_dir().context("No home directory")?;
    let agents_dir = home.join(".claude").join("agents");

    if !agents_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut orphans = Vec::new();

    for entry in std::fs::read_dir(&agents_dir)?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }

        let file_stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();

        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };

        let parsed = parse_agent_json(&content, &file_stem);

        // Skip if already tracked in store
        if let Some(ref tmux) = parsed.tmux_session_name {
            if tracked_tmux.contains(&tmux.as_str()) {
                continue;
            }
        }

        let tmux_alive = parsed.tmux_session_name.as_deref().is_some_and(tmux_session_exists);

        orphans.push(OrphanedSession {
            id: parsed.session_id.unwrap_or(file_stem),
            source: OrphanSource::AgentFile,
            tmux_session_name: parsed.tmux_session_name,
            is_tmux_alive: tmux_alive,
            worktree_path: parsed.worktree_path,
            workspace_name: parsed.workspace_name,
            created_at: parsed.created_at,
            file_path: Some(path.display().to_string()),
        });
    }

    Ok(orphans)
}

/// Fields extracted from an agent JSON file
struct ParsedAgentJson {
    tmux_session_name: Option<String>,
    workspace_name: Option<String>,
    worktree_path: Option<String>,
    created_at: Option<DateTime<Utc>>,
    session_id: Option<String>,
}

/// Try to extract session info from an agent JSON file
fn parse_agent_json(content: &str, file_stem: &str) -> ParsedAgentJson {
    // Try parsing as SessionMetadata first
    if let Ok(meta) = serde_json::from_str::<SessionMetadata>(content) {
        return ParsedAgentJson {
            tmux_session_name: Some(meta.tmux_session_name),
            workspace_name: Some(meta.workspace_name),
            worktree_path: Some(meta.worktree_path.display().to_string()),
            created_at: Some(meta.created_at),
            session_id: Some(meta.session_id.to_string()),
        };
    }

    // Fallback: generic JSON with common field names
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
        let tmux = val
            .get("tmux_session_name")
            .or_else(|| val.get("tmux_session"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let workspace = val
            .get("workspace_name")
            .or_else(|| val.get("workspace"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let worktree = val
            .get("worktree_path")
            .or_else(|| val.get("directory"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let session_id = val.get("session_id").and_then(|v| v.as_str()).map(String::from);

        return ParsedAgentJson {
            tmux_session_name: tmux,
            workspace_name: workspace,
            worktree_path: worktree,
            created_at: None,
            session_id,
        };
    }

    // Unparseable — use file stem as ID
    ParsedAgentJson {
        tmux_session_name: None,
        workspace_name: None,
        worktree_path: None,
        created_at: None,
        session_id: Some(file_stem.to_string()),
    }
}

/// Scan ~/.agents-in-a-box/worktrees/by-session/ for broken or untracked symlinks
fn scan_broken_symlinks(store: &SessionStore) -> Result<Vec<OrphanedSession>> {
    let home = dirs::home_dir().context("No home directory")?;
    let by_session_dir = home.join(".agents-in-a-box").join("worktrees").join("by-session");

    if !by_session_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut orphans = Vec::new();

    for entry in std::fs::read_dir(&by_session_dir)?.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();

        if !path.is_symlink() {
            continue;
        }

        // Check if this session is tracked in the store
        if let Ok(uuid) = Uuid::parse_str(&name) {
            let is_tracked = store.sessions.values().any(|m| m.session_id == uuid);
            if is_tracked {
                continue;
            }
        }

        let target_exists = std::fs::metadata(&path).is_ok();
        let target = std::fs::read_link(&path).ok().map(|t| t.display().to_string());

        orphans.push(OrphanedSession {
            id: name,
            source: OrphanSource::BrokenSymlink,
            tmux_session_name: None,
            is_tmux_alive: false,
            worktree_path: target,
            workspace_name: None,
            created_at: None,
            file_path: Some(path.display().to_string()),
        });

        // If symlink target exists but session is untracked, still mark it
        // (it's already added above regardless of target_exists)
        let _ = target_exists;
    }

    Ok(orphans)
}

/// Execute the recover command, routing to the appropriate subcommand
#[allow(clippy::unused_async)]
pub async fn execute(command: RecoverCommands, format: OutputFormat) -> Result<()> {
    match command {
        RecoverCommands::List => execute_list(format),
        RecoverCommands::Resume { session } => execute_resume(&session),
        RecoverCommands::Cleanup { session, force } => execute_cleanup(session.as_deref(), force),
    }
}

/// List all orphaned sessions in text or JSON format
fn execute_list(format: OutputFormat) -> Result<()> {
    let orphans = find_orphaned_sessions()?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&orphans)
                .context("Failed to serialize orphan list")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            if orphans.is_empty() {
                println!("No orphaned sessions found. Everything looks clean.");
                return Ok(());
            }

            println!(
                "{:<36} {:<20} {:<8} {:<20} SOURCE",
                "ID", "WORKSPACE", "ALIVE", "TMUX SESSION"
            );
            let separator = "-".repeat(100);
            println!("{separator}");

            for orphan in &orphans {
                let alive_text = if orphan.is_tmux_alive {
                    "\x1b[32myes\x1b[0m"
                } else {
                    "\x1b[31mno\x1b[0m"
                };
                let workspace = orphan.workspace_name.as_deref().unwrap_or("-");
                let tmux = orphan.tmux_session_name.as_deref().unwrap_or("-");

                println!(
                    "{:<36} {:<20} {:<8} {:<20} {}",
                    truncate(&orphan.id, 36),
                    truncate(workspace, 20),
                    alive_text,
                    truncate(tmux, 20),
                    orphan.source,
                );
            }

            println!();
            println!("Found {} orphaned session(s).", orphans.len());

            let alive_count = orphans.iter().filter(|o| o.is_tmux_alive).count();
            if alive_count > 0 {
                println!("{alive_count} session(s) still have a live tmux and can be resumed.");
                println!("  Run: ainb recover resume <ID>");
            }
            println!("  Run: ainb recover cleanup [--force] to clean up.");
        }
    }

    Ok(())
}

/// Resume an orphaned session by re-registering it in the `SessionStore`
fn execute_resume(session: &str) -> Result<()> {
    let orphans = find_orphaned_sessions()?;
    let matched = find_orphan(session, &orphans)?;

    if !matched.is_tmux_alive {
        return Err(anyhow!(
            "Tmux session for '{}' is not alive. Cannot resume.\n\
             Use 'ainb recover cleanup {}' to remove it instead.",
            matched.id,
            matched.id,
        ));
    }

    let tmux_name = matched.tmux_session_name.as_ref().ok_or_else(|| {
        anyhow!(
            "Orphan '{}' has no tmux session name — cannot resume",
            matched.id
        )
    })?;

    let workspace = matched.workspace_name.clone().unwrap_or_else(|| tmux_name.clone());

    let worktree_path = matched
        .worktree_path
        .as_ref()
        .map_or_else(|| PathBuf::from("/tmp/unknown"), PathBuf::from);

    let session_id = Uuid::parse_str(&matched.id).unwrap_or_else(|_| Uuid::new_v4());

    let metadata = SessionMetadata {
        session_id,
        tmux_session_name: tmux_name.clone(),
        worktree_path,
        workspace_name: workspace.clone(),
        created_at: matched.created_at.unwrap_or_else(Utc::now),
        agent_type: SessionAgentType::default(),
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: None,
        model: None,
        codex_model: None,
    };

    // Locked RMW (pu4): serialise recovery's re-register against live writers.
    SessionStore::mutate(|store| store.upsert(metadata))
        .context("Failed to save session store")?;

    let short_id = &matched.id[..8.min(matched.id.len())];
    println!("Resumed session '{workspace}' (tmux: {tmux_name}).");
    println!("  Attach: ainb attach {short_id}");

    Ok(())
}

/// Clean up orphaned sessions: kill tmux, remove worktree, remove metadata
#[allow(clippy::if_not_else)]
fn execute_cleanup(session: Option<&str>, force: bool) -> Result<()> {
    let orphans = find_orphaned_sessions()?;

    if orphans.is_empty() {
        println!("No orphaned sessions to clean up.");
        return Ok(());
    }

    let targets: Vec<&OrphanedSession> = if let Some(name) = session {
        let matched = find_orphan(name, &orphans)?;
        vec![matched]
    } else {
        orphans.iter().collect()
    };

    if targets.is_empty() {
        println!("No matching orphans found.");
        return Ok(());
    }

    // Show what will be cleaned and prompt for confirmation
    if !force {
        println!("Will clean up {} orphaned session(s):", targets.len());
        for t in &targets {
            let label =
                t.workspace_name.as_deref().or(t.tmux_session_name.as_deref()).unwrap_or(&t.id);
            let alive_tag = if t.is_tmux_alive {
                " (tmux alive — will be killed)"
            } else {
                ""
            };
            println!("  - {label}{alive_tag}");
            if let Some(ref path) = t.worktree_path {
                println!("    worktree: {path}");
            }
        }
        print!("\nProceed? [y/N] ");
        io::stdout().flush()?;

        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Cancelled.");
            return Ok(());
        }
    }

    let mut cleaned = 0u32;
    let mut errors = 0u32;

    for target in &targets {
        match cleanup_single_orphan(target) {
            Ok(()) => {
                cleaned += 1;
                let label = target.workspace_name.as_deref().unwrap_or(&target.id);
                println!("  Cleaned: {label}");
            }
            Err(e) => {
                errors += 1;
                eprintln!("  Error cleaning '{}': {e}", target.id);
            }
        }
    }

    println!();
    println!("Cleanup complete: {cleaned} cleaned, {errors} error(s).");
    Ok(())
}

/// Clean up a single orphaned session: kill tmux, remove worktree, remove files
fn cleanup_single_orphan(orphan: &OrphanedSession) -> Result<()> {
    // 1. Kill tmux session if alive
    if orphan.is_tmux_alive {
        if let Some(ref tmux_name) = orphan.tmux_session_name {
            let _ = Command::new("tmux").args(["kill-session", "-t", tmux_name]).output();
        }
    }

    // 2. Remove worktree via WorktreeManager if session ID is a valid UUID
    if let Ok(uuid) = Uuid::parse_str(&orphan.id) {
        if let Ok(wm) = crate::git::WorktreeManager::new() {
            // Ignore errors — worktree may already be gone
            let _ = wm.remove_worktree(uuid);
        }
    }

    // 3. Remove the source file (agent JSON or broken symlink)
    if let Some(ref file_path) = orphan.file_path {
        let p = PathBuf::from(file_path);
        // For the session store itself, don't delete the whole file — just remove the entry
        if p.file_name().and_then(|n| n.to_str()) == Some("sessions.json") {
            // Handled below in step 4
        } else if p.exists() || p.is_symlink() {
            std::fs::remove_file(&p)
                .with_context(|| format!("Failed to remove {}", p.display()))?;
        }
    }

    // 4. Remove from session store if present, under the cross-process lock
    // (pu4) so a concurrent create/register can't race this removal. Best-effort
    // lock (proceed unlocked on failure); the guard spans load → save and drops
    // at end of scope.
    let lock_guard = SessionStore::lock()
        .map_err(|e| eprintln!("  Warning: could not lock sessions.json: {e}; proceeding unlocked"))
        .ok();
    let mut store = SessionStore::load();
    let mut changed = false;

    if let Ok(uuid) = Uuid::parse_str(&orphan.id) {
        let before = store.sessions.len();
        store.remove_by_session_id(uuid);
        changed = store.sessions.len() != before;
    }

    if let Some(ref tmux_name) = orphan.tmux_session_name {
        if store.sessions.remove(tmux_name).is_some() {
            changed = true;
        }
    }

    if changed {
        store.save().context("Failed to save session store")?;
    }
    drop(lock_guard);

    Ok(())
}

/// Find a matching orphan by ID, tmux name, or workspace name prefix
fn find_orphan<'a>(needle: &str, orphans: &'a [OrphanedSession]) -> Result<&'a OrphanedSession> {
    let needle_lower = needle.to_lowercase();

    // Exact ID match
    if let Some(o) = orphans.iter().find(|o| o.id.to_lowercase() == needle_lower) {
        return Ok(o);
    }

    // Prefix match on ID
    let prefix_matches: Vec<&OrphanedSession> = orphans
        .iter()
        .filter(|o| o.id.to_lowercase().starts_with(&needle_lower))
        .collect();

    match prefix_matches.len() {
        1 => return Ok(prefix_matches[0]),
        n if n > 1 => {
            let ids: Vec<&str> = prefix_matches.iter().map(|o| o.id.as_str()).collect();
            return Err(anyhow!(
                "Ambiguous match for '{needle}'. Matches:\n  {}",
                ids.join("\n  ")
            ));
        }
        _ => {}
    }

    // Match on tmux session name
    let tmux_matches: Vec<&OrphanedSession> = orphans
        .iter()
        .filter(|o| {
            o.tmux_session_name
                .as_deref()
                .is_some_and(|t| t.to_lowercase().starts_with(&needle_lower))
        })
        .collect();

    if tmux_matches.len() == 1 {
        return Ok(tmux_matches[0]);
    }

    // Match on workspace name
    let ws_matches: Vec<&OrphanedSession> = orphans
        .iter()
        .filter(|o| {
            o.workspace_name
                .as_deref()
                .is_some_and(|w| w.to_lowercase().starts_with(&needle_lower))
        })
        .collect();

    if ws_matches.len() == 1 {
        return Ok(ws_matches[0]);
    }

    Err(anyhow!(
        "No orphaned session found matching '{needle}'.\n\
         Run 'ainb recover list' to see available orphans."
    ))
}

/// Truncate a string to fit in the given width. Delegates to the canonical
/// `truncate_with_ellipsis` (uses `…`, single Unicode char).
fn truncate(s: &str, max_len: usize) -> String {
    crate::widgets::truncate_with_ellipsis(s, max_len).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interactive::session_manager::SessionMetadata;
    use crate::models::session::SessionAgentType;
    use chrono::Utc;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn create_test_session(tmux_name: &str, workspace: &str) -> SessionMetadata {
        SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: tmux_name.to_string(),
            worktree_path: PathBuf::from(format!("/tmp/test-worktree-{tmux_name}")),
            workspace_name: workspace.to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            codex_model: None,
        }
    }

    fn make_test_orphan(id: &str, tmux: &str, workspace: &str) -> OrphanedSession {
        OrphanedSession {
            id: id.to_string(),
            source: OrphanSource::AgentFile,
            tmux_session_name: Some(tmux.to_string()),
            is_tmux_alive: false,
            worktree_path: None,
            workspace_name: Some(workspace.to_string()),
            created_at: None,
            file_path: None,
        }
    }

    #[test]
    fn test_orphan_detection_stale_metadata() {
        let mut store = SessionStore::default();
        let session =
            create_test_session("tmux_nonexistent_test_session_xyzzy_42", "ghost-workspace");
        store.sessions.insert(session.tmux_session_name.clone(), session.clone());

        let alive = tmux_session_exists(&session.tmux_session_name);
        assert!(!alive, "Test tmux session should not exist on the host");

        let orphan = OrphanedSession {
            id: session.session_id.to_string(),
            source: OrphanSource::StaleMetadata,
            tmux_session_name: Some(session.tmux_session_name.clone()),
            is_tmux_alive: alive,
            worktree_path: Some(session.worktree_path.display().to_string()),
            workspace_name: Some(session.workspace_name.clone()),
            created_at: Some(session.created_at),
            file_path: None,
        };

        assert!(!orphan.is_tmux_alive);
        assert!(matches!(orphan.source, OrphanSource::StaleMetadata));
    }

    #[test]
    fn test_orphan_source_serialization() {
        let orphan = OrphanedSession {
            id: "test-id".to_string(),
            source: OrphanSource::StaleMetadata,
            tmux_session_name: Some("tmux_test".to_string()),
            is_tmux_alive: false,
            worktree_path: Some("/tmp/test-worktree".to_string()),
            workspace_name: Some("test-ws".to_string()),
            created_at: None,
            file_path: None,
        };

        let json = serde_json::to_value(&orphan).expect("Serialization should succeed");
        assert_eq!(json["source"], "StaleMetadata");
        assert_eq!(json["is_tmux_alive"], false);

        let orphan_agent = OrphanedSession {
            source: OrphanSource::AgentFile,
            ..orphan.clone()
        };
        let json2 = serde_json::to_value(&orphan_agent).unwrap();
        assert_eq!(json2["source"], "AgentFile");

        let orphan_sym = OrphanedSession {
            source: OrphanSource::BrokenSymlink,
            ..orphan
        };
        let json3 = serde_json::to_value(&orphan_sym).unwrap();
        assert_eq!(json3["source"], "BrokenSymlink");
    }

    #[test]
    fn test_find_orphan_exact_id() {
        let orphans = vec![
            make_test_orphan("abc123", "tmux_a", "workspace-a"),
            make_test_orphan("def456", "tmux_b", "workspace-b"),
        ];

        let found = find_orphan("abc123", &orphans).unwrap();
        assert_eq!(found.id, "abc123");
    }

    #[test]
    fn test_find_orphan_prefix() {
        let orphans = vec![
            make_test_orphan("abc12345-full-id", "tmux_a", "workspace-a"),
            make_test_orphan("def45678-full-id", "tmux_b", "workspace-b"),
        ];

        let found = find_orphan("abc12", &orphans).unwrap();
        assert_eq!(found.id, "abc12345-full-id");
    }

    #[test]
    fn test_find_orphan_ambiguous() {
        let orphans = vec![
            make_test_orphan("abc123-one", "tmux_a", "ws-a"),
            make_test_orphan("abc123-two", "tmux_b", "ws-b"),
        ];

        let result = find_orphan("abc123", &orphans);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Ambiguous"));
    }

    #[test]
    fn test_find_orphan_by_tmux_name() {
        let orphans = vec![
            make_test_orphan("id1", "tmux_project-alpha", "ws"),
            make_test_orphan("id2", "tmux_project-beta", "ws2"),
        ];

        let found = find_orphan("tmux_project-a", &orphans).unwrap();
        assert_eq!(found.id, "id1");
    }

    #[test]
    fn test_find_orphan_by_workspace() {
        let orphans = vec![
            make_test_orphan("id1", "tmux_a", "my-cool-project"),
            make_test_orphan("id2", "tmux_b", "another-project"),
        ];

        let found = find_orphan("my-cool", &orphans).unwrap();
        assert_eq!(found.id, "id1");
    }

    #[test]
    fn test_find_orphan_not_found() {
        let orphans = vec![make_test_orphan("abc", "tmux_a", "ws-a")];

        let result = find_orphan("nonexistent", &orphans);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No orphaned session"));
    }

    #[test]
    fn test_parse_agent_json_session_metadata() {
        let json = r#"{
            "session_id": "12345678-1234-1234-1234-123456789abc",
            "tmux_session_name": "tmux_test",
            "worktree_path": "/tmp/wt",
            "workspace_name": "test-ws",
            "created_at": "2026-01-01T00:00:00Z"
        }"#;

        let parsed = parse_agent_json(json, "fallback");

        assert_eq!(parsed.tmux_session_name.unwrap(), "tmux_test");
        assert_eq!(parsed.workspace_name.unwrap(), "test-ws");
        assert_eq!(parsed.worktree_path.unwrap(), "/tmp/wt");
        assert!(parsed.created_at.is_some());
        assert_eq!(
            parsed.session_id.unwrap(),
            "12345678-1234-1234-1234-123456789abc"
        );
    }

    #[test]
    fn test_parse_agent_json_generic_fields() {
        let json = r#"{
            "tmux_session": "tmux_generic",
            "workspace": "generic-ws",
            "directory": "/home/user/project"
        }"#;

        let parsed = parse_agent_json(json, "fallback-id");

        assert_eq!(parsed.tmux_session_name.unwrap(), "tmux_generic");
        assert_eq!(parsed.workspace_name.unwrap(), "generic-ws");
        assert_eq!(parsed.worktree_path.unwrap(), "/home/user/project");
        assert!(parsed.created_at.is_none());
    }

    #[test]
    fn test_parse_agent_json_invalid() {
        let parsed = parse_agent_json("not json at all", "my-file");

        assert!(parsed.tmux_session_name.is_none());
        assert!(parsed.workspace_name.is_none());
        assert!(parsed.worktree_path.is_none());
        assert!(parsed.created_at.is_none());
        assert_eq!(parsed.session_id.unwrap(), "my-file");
    }

    #[test]
    fn test_find_orphans_empty_store() {
        let store = SessionStore::default();
        assert!(store.sessions.is_empty());

        let mut orphans = Vec::new();
        for metadata in store.sessions.values() {
            let alive = tmux_session_exists(&metadata.tmux_session_name);
            if !alive {
                orphans.push(OrphanedSession {
                    id: metadata.session_id.to_string(),
                    source: OrphanSource::StaleMetadata,
                    tmux_session_name: Some(metadata.tmux_session_name.clone()),
                    is_tmux_alive: false,
                    worktree_path: Some(metadata.worktree_path.display().to_string()),
                    workspace_name: Some(metadata.workspace_name.clone()),
                    created_at: Some(metadata.created_at),
                    file_path: None,
                });
            }
        }

        assert!(orphans.is_empty(), "Empty store should produce no orphans");
    }

    #[test]
    fn test_tmux_session_exists_nonexistent() {
        assert!(!tmux_session_exists(
            "__ainb_test_impossible_session_name_99999__"
        ));
    }

    #[test]
    fn test_orphan_source_display() {
        assert_eq!(OrphanSource::StaleMetadata.to_string(), "Stale Metadata");
        assert_eq!(OrphanSource::AgentFile.to_string(), "Agent File");
        assert_eq!(OrphanSource::BrokenSymlink.to_string(), "Broken Symlink");
    }

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world foo", 10), "hello wor…");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_orphaned_session_clone() {
        let orphan = OrphanedSession {
            id: "clone-test-id".to_string(),
            source: OrphanSource::StaleMetadata,
            tmux_session_name: Some("tmux_clone".to_string()),
            is_tmux_alive: false,
            worktree_path: Some("/tmp/clone-test".to_string()),
            workspace_name: Some("clone-ws".to_string()),
            created_at: None,
            file_path: None,
        };

        let cloned = orphan.clone();
        assert_eq!(cloned.id, "clone-test-id");
        assert!(!cloned.is_tmux_alive);
        assert_eq!(cloned.worktree_path, Some("/tmp/clone-test".to_string()));
    }
}
