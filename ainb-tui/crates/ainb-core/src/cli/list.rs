// ABOUTME: CLI list command - list all sessions with status
//
// Lists sessions from ~/.agents-in-a-box/sessions.json
// Shows: session ID, workspace, status (Claude running, idle, stopped), tmux session name

use super::util::load_session_store;
use super::{ListArgs, OutputFormat};
use crate::config::SessionLabelStore;
use crate::interactive::session_manager::SessionMetadata;
use crate::tmux::ClaudeProcessDetector;
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

/// Session status as displayed in the list
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Claude is running (tmux exists and Claude status bar detected)
    ClaudeRunning,
    /// Tmux session exists but Claude is not running
    Idle,
    /// Tmux session does not exist
    Stopped,
}

impl SessionStatus {
    /// Get the display icon for this status
    #[must_use]
    pub const fn icon(&self) -> &'static str {
        match self {
            Self::ClaudeRunning => "\u{25cf} Claude", // filled circle
            Self::Idle => "\u{25cb} Idle",            // empty circle
            Self::Stopped => "\u{23f8} Stopped",      // pause icon
        }
    }
}

/// A session with enriched status information for display
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub tmux_session_name: String,
    pub workspace_name: String,
    pub display_name: Option<String>,
    pub worktree_path: String,
    pub created_at: DateTime<Utc>,
    pub is_running: bool,
    pub claude_active: bool,
}

impl SessionInfo {
    /// Create `SessionInfo` from metadata and status.
    ///
    /// `workspace_name` is RE-DERIVED from the worktree path
    /// ([`SessionMetadata::display_workspace_name`]), not copied from the
    /// persisted field, so `ainb list` and the TUI session list name the same
    /// session identically. See that method for why the persisted value drifts.
    #[must_use]
    pub fn from_metadata(
        metadata: &SessionMetadata,
        is_running: bool,
        claude_active: bool,
    ) -> Self {
        Self {
            session_id: metadata.session_id.to_string(),
            tmux_session_name: metadata.tmux_session_name.clone(),
            workspace_name: metadata.display_workspace_name(),
            display_name: None,
            worktree_path: metadata.worktree_path.display().to_string(),
            created_at: metadata.created_at,
            is_running,
            claude_active,
        }
    }

    /// Get the status of this session
    #[must_use]
    pub const fn status(&self) -> SessionStatus {
        if !self.is_running {
            SessionStatus::Stopped
        } else if self.claude_active {
            SessionStatus::ClaudeRunning
        } else {
            SessionStatus::Idle
        }
    }
}

/// Execute the list command
pub async fn execute(args: ListArgs, format: OutputFormat) -> Result<()> {
    let sessions = list_sessions(&args).await?;

    if args.frame {
        return output_json(&web_rows(&sessions));
    }

    match format {
        OutputFormat::Json => output_json(&sessions)?,
        OutputFormat::Text | OutputFormat::Csv | OutputFormat::Markdown => output_text(&sessions),
    }

    Ok(())
}

/// List sessions with filtering applied
#[allow(clippy::unused_async)] // Async for consistency with other CLI commands
pub async fn list_sessions(args: &ListArgs) -> Result<Vec<SessionInfo>> {
    let store = load_session_store();
    let labels = SessionLabelStore::load();
    let detector = ClaudeProcessDetector::new();

    let mut sessions = Vec::new();

    for metadata in store.sessions.values() {
        // Check tmux session existence and Claude status
        let (is_running, claude_active) = detector
            .get_session_health(&metadata.tmux_session_name)
            .unwrap_or((false, false));

        let mut info = SessionInfo::from_metadata(metadata, is_running, claude_active);
        info.display_name = labels.get(&metadata.tmux_session_name).cloned();

        // Apply filters
        if args.running && !is_running {
            continue;
        }

        if let Some(ref workspace_filter) = args.workspace {
            if !info.workspace_name.contains(workspace_filter) {
                continue;
            }
        }

        sessions.push(info);
    }

    // Sort by created_at descending (newest first)
    sessions.sort_by(|a, b| b.created_at.cmp(&a.created_at));

    Ok(sessions)
}

/// The web dashboard's session rows for `sessions` (`ainb list --frame`).
///
/// The sessions are put into an app state's Sessions section, labels
/// included, and the rows are projected from that section's redacted frame
/// (`ainb_app::wire::web`). The label is withheld by the frame, so the browser
/// gets what a mirror renderer gets, never the operator's own list as typed
/// (#1056). The tmux session name passes through as stored; the worktree is
/// sent as its scrubbed directory name, never its absolute path (#1097).
fn web_rows(sessions: &[SessionInfo]) -> Vec<ainb_app::wire::web::WebSessionRow> {
    use ainb_app::models::{Session, SessionStatus as ModelStatus, Workspace};
    let mut workspaces: Vec<Workspace> = Vec::new();
    for info in sessions {
        // A row the browser cannot attach to is worse than no row: a bad id is
        // skipped and logged, never replaced with a fresh one.
        let Ok(id) = uuid::Uuid::parse_str(&info.session_id) else {
            tracing::warn!(
                session_id = %info.session_id,
                "list --frame: skipping a session whose id is not a UUID"
            );
            continue;
        };
        let mut session = Session::new(info.workspace_name.clone(), info.worktree_path.clone());
        session.id = id;
        session.tmux_session_name = Some(info.tmux_session_name.clone());
        session.display_name.clone_from(&info.display_name);
        session.created_at = info.created_at;
        session.status = match info.status() {
            SessionStatus::ClaudeRunning => ModelStatus::Running,
            SessionStatus::Idle => ModelStatus::Idle,
            SessionStatus::Stopped => ModelStatus::Stopped,
        };
        if let Some(workspace) =
            workspaces.iter_mut().find(|workspace| workspace.name == info.workspace_name)
        {
            workspace.add_session(session);
        } else {
            let mut workspace = Workspace::new(
                info.workspace_name.clone(),
                std::path::PathBuf::from(&info.worktree_path),
            );
            workspace.add_session(session);
            workspaces.push(workspace);
        }
    }
    let mut state = ainb_app::AppState::new();
    state.sessions.get_mut().workspaces = workspaces;
    // The frame groups sessions by workspace; the web list draws newest first,
    // as `ainb list` does. Sorted on the parsed instant: RFC 3339 text is not in
    // time order when two stamps carry fractions of different widths.
    let mut rows = ainb_app::wire::web::session_rows(&state);
    sort_newest_first(&mut rows);
    rows
}

/// Newest `created_at` first. A stamp that does not parse sorts last, as the
/// oldest possible instant.
fn sort_newest_first(rows: &mut [ainb_app::wire::web::WebSessionRow]) {
    rows.sort_by_cached_key(|row| {
        std::cmp::Reverse(
            DateTime::parse_from_rfc3339(&row.created_at)
                .map_or(DateTime::<Utc>::MIN_UTC, |instant| {
                    instant.with_timezone(&Utc)
                }),
        )
    });
}

/// Output sessions as JSON
fn output_json<T: Serialize + ?Sized>(sessions: &T) -> Result<()> {
    let json = serde_json::to_string_pretty(sessions)?;
    println!("{json}");
    Ok(())
}

/// Output sessions as a text table
fn output_text(sessions: &[SessionInfo]) {
    if sessions.is_empty() {
        println!("No sessions found.");
        return;
    }

    // Print header
    println!(
        "{:<36} {:<22} {:<22} {:<10} TMUX SESSION",
        "ID", "WORKSPACE", "SESSION LABEL", "STATUS"
    );
    let separator = "-".repeat(100);
    println!("{separator}");

    // Print each session
    for session in sessions {
        let status = session.status();
        let workspace = truncate(&session.workspace_name, 25);
        println!(
            "{:<36} {:<22} {:<22} {:<10} {}",
            session.session_id,
            workspace,
            truncate(session.display_name.as_deref().unwrap_or(""), 22),
            status.icon(),
            session.tmux_session_name
        );
    }
}

/// Truncate a string to fit in the given width. Delegates to the canonical
/// `truncate_with_ellipsis` (uses `…`, single Unicode char).
fn truncate(s: &str, max_len: usize) -> String {
    crate::widgets::truncate_with_ellipsis(s, max_len).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::session::SessionAgentType;
    use std::path::PathBuf;
    use uuid::Uuid;

    #[test]
    fn the_frame_rows_withhold_a_label_and_keep_each_sessions_health() {
        let canary = "ghp_ProofCanary0123456789abcdefghijklmnopq";
        // One instant for all three, so the newest-first sort keeps this order.
        let created_at = chrono::Utc::now();
        let info = |id: &str, running: bool, active: bool| SessionInfo {
            session_id: id.to_string(),
            tmux_session_name: format!("tmux_repo-{id}"),
            workspace_name: "repo".to_string(),
            display_name: Some(format!("deploy {canary}")),
            worktree_path: format!("/w/repo-{id}"),
            created_at,
            is_running: running,
            claude_active: active,
        };
        let sessions = [
            info("5b1f2a8e-0000-4000-8000-000000000001", true, true),
            info("5b1f2a8e-0000-4000-8000-000000000002", true, false),
            info("5b1f2a8e-0000-4000-8000-000000000003", false, false),
        ];

        let rows = web_rows(&sessions);
        let json = serde_json::to_string(&rows).expect("rows serialise");

        assert!(!json.contains(canary), "{json}");
        let health: Vec<_> = rows
            .iter()
            .map(|row| (row.session_id.as_str(), row.is_running, row.claude_active))
            .collect();
        assert_eq!(
            health,
            [
                ("5b1f2a8e-0000-4000-8000-000000000001", true, true),
                ("5b1f2a8e-0000-4000-8000-000000000002", true, false),
                ("5b1f2a8e-0000-4000-8000-000000000003", false, false),
            ]
        );
        assert_eq!(
            rows[2].worktree_name,
            "repo-5b1f2a8e-0000-4000-8000-000000000003"
        );
    }

    #[test]
    fn an_unparseable_created_at_sorts_last() {
        let mut rows = vec![
            ainb_app::wire::web::WebSessionRow {
                session_id: "bad".to_string(),
                tmux_session_name: None,
                workspace_name: "repo".to_string(),
                worktree_name: "w".to_string(),
                created_at: "not a stamp".to_string(),
                is_running: true,
                claude_active: false,
            },
            ainb_app::wire::web::WebSessionRow {
                session_id: "good".to_string(),
                tmux_session_name: None,
                workspace_name: "repo".to_string(),
                worktree_name: "w".to_string(),
                created_at: "2026-09-15T00:00:00Z".to_string(),
                is_running: true,
                claude_active: false,
            },
        ];
        sort_newest_first(&mut rows);
        let ids: Vec<_> = rows.iter().map(|row| row.session_id.as_str()).collect();
        assert_eq!(ids, ["good", "bad"]);
    }

    #[test]
    fn frame_rows_sort_on_the_instant_not_the_stamp_text() {
        let row = |id: &str, created_at: &str| SessionInfo {
            session_id: id.to_string(),
            tmux_session_name: format!("tmux_repo-{id}"),
            workspace_name: "repo".to_string(),
            display_name: None,
            worktree_path: "/w/repo".to_string(),
            created_at: DateTime::parse_from_rfc3339(created_at).unwrap().with_timezone(&Utc),
            is_running: true,
            claude_active: false,
        };
        // Same millisecond: ".100000001Z" is one nanosecond later than
        // ".100Z", but sorts before it as text ('0' < 'Z').
        let rows = web_rows(&[
            row(
                "5b1f2a8e-0000-4000-8000-000000000001",
                "2026-09-15T00:00:00.100Z",
            ),
            row(
                "5b1f2a8e-0000-4000-8000-000000000002",
                "2026-09-15T00:00:00.100000001Z",
            ),
        ]);
        let ids: Vec<_> = rows.iter().map(|row| &row.session_id[33..]).collect();
        assert_eq!(ids, ["002", "001"]);
    }

    #[test]
    fn frame_rows_are_newest_first_across_workspaces() {
        let row = |id: &str, workspace: &str, minutes_ago: i64| SessionInfo {
            session_id: id.to_string(),
            tmux_session_name: format!("tmux_{workspace}-{id}"),
            workspace_name: workspace.to_string(),
            display_name: None,
            worktree_path: format!("/w/{workspace}"),
            created_at: chrono::Utc::now() - chrono::Duration::minutes(minutes_ago),
            is_running: true,
            claude_active: false,
        };
        let rows = web_rows(&[
            row("5b1f2a8e-0000-4000-8000-000000000001", "alpha", 30),
            row("5b1f2a8e-0000-4000-8000-000000000002", "beta", 10),
            row("5b1f2a8e-0000-4000-8000-000000000003", "alpha", 20),
        ]);
        let ids: Vec<_> = rows.iter().map(|row| &row.session_id[33..]).collect();
        assert_eq!(ids, ["002", "003", "001"]);
    }

    #[test]
    fn a_session_with_an_unparseable_id_is_skipped_not_given_a_fresh_one() {
        let row = |id: &str| SessionInfo {
            session_id: id.to_string(),
            tmux_session_name: format!("tmux_repo-{id}"),
            workspace_name: "repo".to_string(),
            display_name: None,
            worktree_path: "/w/repo".to_string(),
            created_at: chrono::Utc::now(),
            is_running: true,
            claude_active: true,
        };
        let rows = web_rows(&[
            row("not-a-uuid"),
            row("5b1f2a8e-0000-4000-8000-000000000001"),
        ]);
        let ids: Vec<_> = rows.iter().map(|row| row.session_id.as_str()).collect();
        assert_eq!(ids, ["5b1f2a8e-0000-4000-8000-000000000001"]);
    }

    #[test]
    fn test_session_status_icons() {
        assert_eq!(SessionStatus::ClaudeRunning.icon(), "\u{25cf} Claude");
        assert_eq!(SessionStatus::Idle.icon(), "\u{25cb} Idle");
        assert_eq!(SessionStatus::Stopped.icon(), "\u{23f8} Stopped");
    }

    #[test]
    fn test_session_info_status_claude_running() {
        let metadata = SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: "test_session".to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: "test-workspace".to_string(),
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

        let info = SessionInfo::from_metadata(&metadata, true, true);
        assert_eq!(info.status(), SessionStatus::ClaudeRunning);
    }

    #[test]
    fn test_session_info_status_idle() {
        let metadata = SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: "test_session".to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: "test-workspace".to_string(),
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

        let info = SessionInfo::from_metadata(&metadata, true, false);
        assert_eq!(info.status(), SessionStatus::Idle);
    }

    #[test]
    fn test_session_info_status_stopped() {
        let metadata = SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: "test_session".to_string(),
            worktree_path: PathBuf::from("/tmp/test"),
            workspace_name: "test-workspace".to_string(),
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

        let info = SessionInfo::from_metadata(&metadata, false, false);
        assert_eq!(info.status(), SessionStatus::Stopped);
    }

    #[test]
    fn test_truncate_short_string() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long_string() {
        assert_eq!(truncate("hello world", 8), "hello w…");
    }

    #[test]
    fn test_truncate_exact_length() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_session_info_serialization() {
        if !crate::test_support::git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        // A REAL checkout, because `workspace_name` is now derived from the
        // path: a made-up `/tmp/test` would only prove the "(broken)" branch.
        let fx = crate::test_support::real_git_fixture();
        let metadata = SessionMetadata {
            session_id: Uuid::parse_str("f79e07da-774d-415c-aedf-a2acd0bee0d3").unwrap(),
            tmux_session_name: "tmux_my-session".to_string(),
            worktree_path: fx.repo.clone(),
            workspace_name: "stale-persisted-name".to_string(),
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

        let info = SessionInfo::from_metadata(&metadata, true, true);
        let json = serde_json::to_value(&info).unwrap();

        assert_eq!(json["session_id"], "f79e07da-774d-415c-aedf-a2acd0bee0d3");
        assert_eq!(json["tmux_session_name"], "tmux_my-session");
        assert_eq!(
            json["workspace_name"], "myrepo",
            "`ainb list --format json` must publish the DERIVED name, not the persisted one"
        );
        assert_eq!(json["is_running"], true);
        assert_eq!(json["claude_active"], true);
    }

    /// F14: `ainb list` and the TUI session tree must name the same session
    /// identically, for every real on-disk shape.
    ///
    /// The CLI used to print the PERSISTED `workspace_name` (the `--repo`
    /// basename recorded by `ainb run`) while the TUI re-derived the owning
    /// repository from the path, so the two surfaces disagreed for any session
    /// rooted at a subdirectory of a checkout. The persisted value here is
    /// deliberately the wrong-but-realistic one `ainb run --repo <clone>/deep`
    /// would have written.
    #[test]
    fn cli_and_tui_agree_on_workspace_name_for_every_shape() {
        use crate::interactive::InteractiveSessionManager;

        if !crate::test_support::git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let fx = crate::test_support::real_git_fixture();

        // (label, session root, what `ainb run` would have persisted)
        let cases = [
            ("plain checkout root", fx.repo.clone(), "myrepo"),
            ("subdirectory of a checkout", fx.subdir.clone(), "deep"),
            (
                "linked worktree",
                fx.worktree.clone(),
                "myrepo--feature--abc123",
            ),
        ];

        for (label, worktree_path, persisted) in cases {
            let metadata = SessionMetadata {
                session_id: Uuid::new_v4(),
                tmux_session_name: format!("tmux_{persisted}"),
                worktree_path: worktree_path.clone(),
                workspace_name: persisted.to_string(),
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

            // What the TUI paints: `AppState::load_real_workspaces` longhand.
            let tui = {
                let source = InteractiveSessionManager::get_source_repository(&worktree_path)
                    .unwrap_or_else(|| worktree_path.clone());
                InteractiveSessionManager::derive_workspace_name(&worktree_path, &source)
            };
            // What `ainb list` / `ainb status` print.
            let cli = SessionInfo::from_metadata(&metadata, false, false).workspace_name;

            assert_eq!(
                cli, tui,
                "{label}: `ainb list` says {cli:?}, the TUI says {tui:?}"
            );
            assert_eq!(
                cli, "myrepo",
                "{label}: both surfaces must name the owning repository"
            );
        }
    }
}
