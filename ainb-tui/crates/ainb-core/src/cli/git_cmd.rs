// ABOUTME: CLI git command - worktree inspection and cleanup
//
// Subcommands:
//   worktrees: List all managed worktrees with session association status
//   cleanup:   Remove orphaned worktrees not referenced by any session
//   status:    Show git status for a specific session's worktree

use anyhow::{Context, Result};
use clap::Subcommand;
use git2::{Repository, StatusOptions};
use serde::Serialize;
use std::io::{self, Write};
use std::path::Path;
use uuid::Uuid;

use super::OutputFormat;
use crate::cli::util::find_session;
use crate::git::{WorktreeInfo, WorktreeManager};
use crate::interactive::session_manager::SessionStore;

/// Manage git worktrees + inspect session changes. Description set in `cli/registry.rs`.
#[derive(Subcommand)]
pub enum GitCommands {
    /// List all managed worktrees and their session association
    Worktrees,
    /// Remove orphaned worktrees (not referenced by any session)
    Cleanup {
        /// Skip confirmation prompt
        #[arg(long, short)]
        force: bool,
        /// Preview what would be removed without making changes
        #[arg(long)]
        dry_run: bool,
    },
    /// Show git status for a specific session's worktree
    Status {
        /// Session ID (full/partial UUID) or workspace name prefix
        session: String,
    },
}

/// Worktree association status relative to the session store
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WorktreeStatus {
    /// Worktree is referenced by an active session
    Active,
    /// Worktree has no corresponding session entry
    Orphaned,
}

impl std::fmt::Display for WorktreeStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Active => write!(f, "active"),
            Self::Orphaned => write!(f, "orphaned"),
        }
    }
}

/// Serializable entry for the `worktrees` subcommand output
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeEntry {
    pub session_id: String,
    pub path: String,
    pub branch: String,
    pub commit: Option<String>,
    pub source_repository: String,
    pub status: WorktreeStatus,
    pub workspace_name: Option<String>,
}

/// Serializable per-file git status record
#[derive(Debug, Clone, Serialize)]
pub struct FileStatus {
    pub path: String,
    pub status: String,
}

/// Serializable git status report for the `status` subcommand
#[derive(Debug, Clone, Serialize)]
pub struct GitStatusReport {
    pub session_id: String,
    pub workspace_name: String,
    pub worktree_path: String,
    pub branch: String,
    pub commit: Option<String>,
    pub files_changed: usize,
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub files: Vec<FileStatus>,
}

/// Entry point for the `git` command family
#[allow(clippy::unused_async)]
pub async fn execute(command: GitCommands, format: OutputFormat) -> Result<()> {
    match command {
        GitCommands::Worktrees => cmd_worktrees(format),
        GitCommands::Cleanup { force, dry_run } => cmd_cleanup(force, dry_run, format),
        GitCommands::Status { session } => cmd_status(&session, format),
    }
}

/// List all managed worktrees with their session association status
fn cmd_worktrees(format: OutputFormat) -> Result<()> {
    let manager = WorktreeManager::new().context("Failed to initialize WorktreeManager")?;
    let worktrees =
        manager.list_all_worktrees().context("Failed to enumerate managed worktrees")?;
    let store = SessionStore::load();

    let entries: Vec<WorktreeEntry> =
        worktrees.iter().map(|(id, info)| build_entry(*id, info, &store)).collect();

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&entries)
                .context("Failed to serialize worktree entries")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            if entries.is_empty() {
                println!("No managed worktrees found.");
                return Ok(());
            }

            println!(
                "{:<9} {:<20} {:<30} {:<10} PATH",
                "STATUS", "WORKSPACE", "BRANCH", "SESSION"
            );
            println!("{}", "-".repeat(100));

            for entry in &entries {
                let short_id = short_session_id(&entry.session_id);
                let workspace = entry.workspace_name.as_deref().unwrap_or("-");
                println!(
                    "{:<9} {:<20} {:<30} {:<10} {}",
                    entry.status.to_string(),
                    truncate(workspace, 20),
                    truncate(&entry.branch, 30),
                    short_id,
                    entry.path,
                );
            }

            println!();
            let orphaned_count =
                entries.iter().filter(|e| e.status == WorktreeStatus::Orphaned).count();
            let active_count = entries.len() - orphaned_count;
            println!(
                "{} worktree(s) - {} active, {} orphaned.",
                entries.len(),
                active_count,
                orphaned_count
            );
            if orphaned_count > 0 {
                println!("  Run: ainb git cleanup [--dry-run] [--force] to remove orphans.");
            }
        }
    }

    Ok(())
}

/// Remove orphaned worktrees not referenced by any session
fn cmd_cleanup(force: bool, dry_run: bool, format: OutputFormat) -> Result<()> {
    let manager = WorktreeManager::new().context("Failed to initialize WorktreeManager")?;
    let worktrees =
        manager.list_all_worktrees().context("Failed to enumerate managed worktrees")?;
    let store = SessionStore::load();

    let orphans: Vec<(Uuid, WorktreeInfo)> =
        worktrees.into_iter().filter(|(id, _)| is_orphaned(id, &store)).collect();

    let entries: Vec<WorktreeEntry> =
        orphans.iter().map(|(id, info)| build_entry(*id, info, &store)).collect();

    if orphans.is_empty() {
        match format {
            OutputFormat::Json => println!("[]"),
            OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
                println!("No orphaned worktrees to clean up.")
            }
        }
        return Ok(());
    }

    if dry_run {
        match format {
            OutputFormat::Json => {
                let json = serde_json::to_string_pretty(&entries)
                    .context("Failed to serialize orphan entries")?;
                println!("{json}");
            }
            OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
                println!(
                    "Dry run - {} orphaned worktree(s) would be removed:",
                    orphans.len()
                );
                for entry in &entries {
                    let short_id = short_session_id(&entry.session_id);
                    println!("  - {short_id}  {}  ({})", entry.branch, entry.path);
                }
                println!();
                println!("Re-run without --dry-run to perform cleanup.");
            }
        }
        return Ok(());
    }

    if !force {
        println!("Will remove {} orphaned worktree(s):", orphans.len());
        for entry in &entries {
            let short_id = short_session_id(&entry.session_id);
            println!("  - {short_id}  {}", entry.path);
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

    let mut removed = 0u32;
    let mut errors = 0u32;
    for (id, info) in &orphans {
        match manager.remove_worktree(*id) {
            Ok(()) => {
                removed += 1;
                println!("  Removed: {}", info.path.display());
            }
            Err(e) => {
                errors += 1;
                eprintln!("  Error removing {}: {e}", info.path.display());
            }
        }
    }

    println!();
    println!("Cleanup complete: {removed} removed, {errors} error(s).");
    Ok(())
}

/// Show git status for a specific session's worktree
fn cmd_status(session: &str, format: OutputFormat) -> Result<()> {
    let metadata = find_session(session)?;
    let report = build_status_report(
        &metadata.session_id.to_string(),
        &metadata.workspace_name,
        &metadata.worktree_path,
    )?;

    match format {
        OutputFormat::Json => {
            let json = serde_json::to_string_pretty(&report)
                .context("Failed to serialize git status report")?;
            println!("{json}");
        }
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => {
            let short_id = short_session_id(&report.session_id);
            println!("Session: {} ({})", report.workspace_name, short_id);
            println!("Branch:  {}", report.branch);
            if let Some(ref c) = report.commit {
                let short = &c[..12.min(c.len())];
                println!("Commit:  {short}");
            }
            println!("Path:    {}", report.worktree_path);
            println!();
            if report.files_changed == 0 {
                println!("Working tree clean.");
            } else {
                println!(
                    "{} file(s) changed - {} staged, {} unstaged, {} untracked",
                    report.files_changed, report.staged, report.unstaged, report.untracked
                );
                for file in &report.files {
                    println!("  [{}] {}", file.status, file.path);
                }
            }
        }
    }

    Ok(())
}

// --- pure helpers (unit-tested) ---

/// Check whether a worktree's `session_id` is not referenced by any session in the store
pub fn is_orphaned(session_id: &Uuid, store: &SessionStore) -> bool {
    !store.sessions.values().any(|m| m.session_id == *session_id)
}

/// Build a `WorktreeEntry` from a worktree record + session store context
fn build_entry(session_id: Uuid, info: &WorktreeInfo, store: &SessionStore) -> WorktreeEntry {
    let status = if is_orphaned(&session_id, store) {
        WorktreeStatus::Orphaned
    } else {
        WorktreeStatus::Active
    };

    let workspace_name = store
        .sessions
        .values()
        .find(|m| m.session_id == session_id)
        .map(|m| m.workspace_name.clone());

    WorktreeEntry {
        session_id: session_id.to_string(),
        path: info.path.display().to_string(),
        branch: info.branch_name.clone(),
        commit: info.commit_hash.clone(),
        source_repository: info.source_repository.display().to_string(),
        status,
        workspace_name,
    }
}

/// Open a worktree with git2 and produce a structured status report
fn build_status_report(
    session_id: &str,
    workspace_name: &str,
    worktree_path: &Path,
) -> Result<GitStatusReport> {
    let repo = Repository::open(worktree_path).with_context(|| {
        format!(
            "Failed to open git repository at {}",
            worktree_path.display()
        )
    })?;

    let head = repo.head().ok();
    let branch = head
        .as_ref()
        .and_then(|h| h.shorthand().map(String::from))
        .unwrap_or_else(|| "unknown".to_string());
    let commit = head.as_ref().and_then(|h| h.target()).map(|oid| oid.to_string());

    let mut opts = StatusOptions::new();
    opts.include_untracked(true).recurse_untracked_dirs(true);

    let statuses = repo.statuses(Some(&mut opts)).context("Failed to read git status")?;

    let mut files = Vec::new();
    let mut staged = 0usize;
    let mut unstaged = 0usize;
    let mut untracked = 0usize;

    for entry in statuses.iter() {
        let flags = entry.status();
        let path = entry.path().unwrap_or("").to_string();
        let status_str = status_flags_to_string(flags);

        if flags.is_wt_new() {
            untracked += 1;
        }
        if flags.is_index_new()
            || flags.is_index_modified()
            || flags.is_index_deleted()
            || flags.is_index_renamed()
            || flags.is_index_typechange()
        {
            staged += 1;
        }
        if flags.is_wt_modified()
            || flags.is_wt_deleted()
            || flags.is_wt_renamed()
            || flags.is_wt_typechange()
        {
            unstaged += 1;
        }

        files.push(FileStatus {
            path,
            status: status_str,
        });
    }

    Ok(GitStatusReport {
        session_id: session_id.to_string(),
        workspace_name: workspace_name.to_string(),
        worktree_path: worktree_path.display().to_string(),
        branch,
        commit,
        files_changed: files.len(),
        staged,
        unstaged,
        untracked,
        files,
    })
}

/// Convert git2 status flags to a short porcelain-ish status string
fn status_flags_to_string(flags: git2::Status) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if flags.is_index_new() {
        parts.push("A");
    }
    if flags.is_index_modified() {
        parts.push("M");
    }
    if flags.is_index_deleted() {
        parts.push("D");
    }
    if flags.is_index_renamed() {
        parts.push("R");
    }
    if flags.is_index_typechange() {
        parts.push("T");
    }
    if flags.is_wt_new() {
        parts.push("??");
    }
    if flags.is_wt_modified() {
        parts.push("m");
    }
    if flags.is_wt_deleted() {
        parts.push("d");
    }
    if flags.is_wt_renamed() {
        parts.push("r");
    }
    if flags.is_wt_typechange() {
        parts.push("t");
    }
    if flags.is_conflicted() {
        parts.push("U");
    }
    if parts.is_empty() {
        parts.push("?");
    }
    parts.join("")
}

/// Shorten a session id to its first 8 chars (UUID prefix) for display
fn short_session_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// Truncate a string for fixed-width table columns. Delegates to the
/// canonical `truncate_with_ellipsis` (uses `…`, single Unicode char).
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

    fn make_metadata(id: Uuid, tmux: &str, workspace: &str) -> SessionMetadata {
        SessionMetadata {
            session_id: id,
            tmux_session_name: tmux.to_string(),
            worktree_path: PathBuf::from(format!("/tmp/wt-{tmux}")),
            workspace_name: workspace.to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::default(),
            headroom_enabled: false,
            rtk_enabled: false,
        }
    }

    fn make_worktree_info(id: Uuid, branch: &str) -> WorktreeInfo {
        WorktreeInfo {
            id,
            path: PathBuf::from(format!(
                "/tmp/worktrees/by-name/repo--{branch}--{}",
                &id.to_string()[..8]
            )),
            session_path: PathBuf::from(format!("/tmp/worktrees/by-session/{id}")),
            branch_name: branch.to_string(),
            source_repository: PathBuf::from("/tmp/source-repo"),
            commit_hash: Some("abcdef1234567890".to_string()),
        }
    }

    // --- is_orphaned ---

    #[test]
    fn test_is_orphaned_empty_store() {
        let store = SessionStore::default();
        let id = Uuid::new_v4();
        assert!(is_orphaned(&id, &store));
    }

    #[test]
    fn test_is_orphaned_when_id_not_present() {
        let mut store = SessionStore::default();
        let meta = make_metadata(Uuid::new_v4(), "tmux_a", "ws-a");
        store.sessions.insert(meta.tmux_session_name.clone(), meta);
        let other = Uuid::new_v4();
        assert!(is_orphaned(&other, &store));
    }

    #[test]
    fn test_is_not_orphaned_when_id_present() {
        let id = Uuid::new_v4();
        let mut store = SessionStore::default();
        let meta = make_metadata(id, "tmux_a", "ws-a");
        store.sessions.insert(meta.tmux_session_name.clone(), meta);
        assert!(!is_orphaned(&id, &store));
    }

    #[test]
    fn test_is_orphaned_distinguishes_ids() {
        let id_a = Uuid::new_v4();
        let id_b = Uuid::new_v4();
        let mut store = SessionStore::default();
        let meta = make_metadata(id_a, "tmux_a", "ws-a");
        store.sessions.insert(meta.tmux_session_name.clone(), meta);
        assert!(!is_orphaned(&id_a, &store));
        assert!(is_orphaned(&id_b, &store));
    }

    // --- build_entry ---

    #[test]
    fn test_build_entry_active_copies_workspace_name() {
        let id = Uuid::new_v4();
        let info = make_worktree_info(id, "feat/foo");
        let mut store = SessionStore::default();
        let meta = make_metadata(id, "tmux_x", "my-workspace");
        store.sessions.insert(meta.tmux_session_name.clone(), meta);

        let entry = build_entry(id, &info, &store);
        assert_eq!(entry.status, WorktreeStatus::Active);
        assert_eq!(entry.workspace_name.as_deref(), Some("my-workspace"));
        assert_eq!(entry.branch, "feat/foo");
        assert_eq!(entry.session_id, id.to_string());
        assert_eq!(entry.commit.as_deref(), Some("abcdef1234567890"));
    }

    #[test]
    fn test_build_entry_orphaned_when_no_session() {
        let id = Uuid::new_v4();
        let info = make_worktree_info(id, "main");
        let store = SessionStore::default();

        let entry = build_entry(id, &info, &store);
        assert_eq!(entry.status, WorktreeStatus::Orphaned);
        assert!(entry.workspace_name.is_none());
    }

    // --- WorktreeStatus Display/Serialize ---

    #[test]
    fn test_worktree_status_display() {
        assert_eq!(WorktreeStatus::Active.to_string(), "active");
        assert_eq!(WorktreeStatus::Orphaned.to_string(), "orphaned");
    }

    #[test]
    fn test_worktree_status_json_active() {
        let json = serde_json::to_value(WorktreeStatus::Active).unwrap();
        assert_eq!(json, serde_json::Value::String("active".to_string()));
    }

    #[test]
    fn test_worktree_status_json_orphaned() {
        let json = serde_json::to_value(WorktreeStatus::Orphaned).unwrap();
        assert_eq!(json, serde_json::Value::String("orphaned".to_string()));
    }

    // --- WorktreeEntry serialization ---

    #[test]
    fn test_worktree_entry_json_fields() {
        let id = Uuid::new_v4();
        let info = make_worktree_info(id, "main");
        let store = SessionStore::default();
        let entry = build_entry(id, &info, &store);

        let json = serde_json::to_value(&entry).unwrap();
        assert!(json["session_id"].is_string());
        assert!(json["path"].is_string());
        assert_eq!(
            json["branch"],
            serde_json::Value::String("main".to_string())
        );
        assert_eq!(
            json["status"],
            serde_json::Value::String("orphaned".to_string())
        );
        assert!(json["commit"].is_string());
    }

    // --- truncate ---

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
    fn test_truncate_tiny_max_len() {
        // Canonical truncate_with_ellipsis returns N-1 chars + `…` for any
        // truncation, so max_len=2 yields one source char plus the ellipsis.
        assert_eq!(truncate("hello", 2), "h…");
    }

    // --- short_session_id ---

    #[test]
    fn test_short_session_id_full_uuid() {
        let id = "12345678-1234-1234-1234-123456789abc";
        assert_eq!(short_session_id(id), "12345678");
    }

    #[test]
    fn test_short_session_id_short_input() {
        assert_eq!(short_session_id("abc"), "abc");
    }

    // --- status_flags_to_string ---

    #[test]
    fn test_status_flags_empty() {
        let flags = git2::Status::empty();
        // Empty = no flags set; helper returns "?" placeholder
        assert_eq!(status_flags_to_string(flags), "?");
    }

    #[test]
    fn test_status_flags_wt_new_is_untracked() {
        let flags = git2::Status::WT_NEW;
        assert_eq!(status_flags_to_string(flags), "??");
    }

    #[test]
    fn test_status_flags_index_modified() {
        let flags = git2::Status::INDEX_MODIFIED;
        assert_eq!(status_flags_to_string(flags), "M");
    }

    #[test]
    fn test_status_flags_index_new() {
        let flags = git2::Status::INDEX_NEW;
        assert_eq!(status_flags_to_string(flags), "A");
    }

    #[test]
    fn test_status_flags_combined_staged_and_unstaged() {
        let flags = git2::Status::INDEX_MODIFIED | git2::Status::WT_MODIFIED;
        let s = status_flags_to_string(flags);
        assert!(s.contains('M'), "expected 'M' in {s}");
        assert!(s.contains('m'), "expected 'm' in {s}");
    }

    // --- GitStatusReport serialization ---

    #[test]
    fn test_git_status_report_serialize() {
        let report = GitStatusReport {
            session_id: "12345678-1234-1234-1234-123456789abc".to_string(),
            workspace_name: "test-ws".to_string(),
            worktree_path: "/tmp/wt".to_string(),
            branch: "main".to_string(),
            commit: Some("deadbeef".to_string()),
            files_changed: 2,
            staged: 1,
            unstaged: 1,
            untracked: 0,
            files: vec![
                FileStatus {
                    path: "a.rs".to_string(),
                    status: "M".to_string(),
                },
                FileStatus {
                    path: "b.rs".to_string(),
                    status: "m".to_string(),
                },
            ],
        };

        let json = serde_json::to_value(&report).unwrap();
        assert_eq!(json["files_changed"], serde_json::json!(2));
        assert_eq!(json["staged"], serde_json::json!(1));
        assert_eq!(
            json["branch"],
            serde_json::Value::String("main".to_string())
        );
        assert_eq!(json["files"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn test_file_status_serialize() {
        let fs = FileStatus {
            path: "src/main.rs".to_string(),
            status: "M".to_string(),
        };
        let json = serde_json::to_value(&fs).unwrap();
        assert_eq!(
            json["path"],
            serde_json::Value::String("src/main.rs".to_string())
        );
        assert_eq!(json["status"], serde_json::Value::String("M".to_string()));
    }

    // --- build_status_report uses git2 against a real repo ---

    #[test]
    fn test_build_status_report_clean_repo() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();

        // Create an initial commit so HEAD resolves
        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[]).unwrap();
        drop(tree);

        let report = build_status_report("session-id", "test-ws", tmp.path()).unwrap();

        assert_eq!(report.files_changed, 0);
        assert_eq!(report.staged, 0);
        assert_eq!(report.unstaged, 0);
        assert_eq!(report.untracked, 0);
        assert_eq!(report.workspace_name, "test-ws");
        assert!(report.commit.is_some());
    }

    #[test]
    fn test_build_status_report_detects_untracked() {
        let tmp = tempfile::TempDir::new().unwrap();
        let repo = git2::Repository::init(tmp.path()).unwrap();

        let sig = git2::Signature::now("Test", "test@example.com").unwrap();
        let tree_id = {
            let mut index = repo.index().unwrap();
            index.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "initial", &tree, &[]).unwrap();
        drop(tree);

        std::fs::write(tmp.path().join("new_file.txt"), "hello").unwrap();

        let report = build_status_report("session-id", "test-ws", tmp.path()).unwrap();

        assert_eq!(report.files_changed, 1);
        assert_eq!(report.untracked, 1);
        assert_eq!(report.files[0].path, "new_file.txt");
        assert_eq!(report.files[0].status, "??");
    }

    #[test]
    fn test_build_status_report_missing_repo_errors() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Not a git repository
        let result = build_status_report("session-id", "ws", tmp.path());
        assert!(result.is_err());
    }
}
