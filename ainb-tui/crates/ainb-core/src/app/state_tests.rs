// ABOUTME: Tests for AppState new session functionality, focusing on mode selection flow

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::EventHandler;
    use crate::app::events::AppEvent;
    use crate::app::state::{AppState, NewSessionState, NewSessionStep, SessionAgentOption};
    use crate::models::{OtherTmuxSession, SessionAgentType, SessionMode};
    use std::path::PathBuf;

    // ========================================================================
    // Stop / Resume coverage
    // ========================================================================

    use crate::app::state::{AsyncAction, ConfirmAction, ConfirmationDialog, DialogOption};

    fn state_with_other_tmux_sessions(names: &[&str]) -> AppState {
        let mut state = AppState::new();
        state.selected_workspace_index = None;
        state.selected_session_index = None;
        state.selected_other_tmux_index = Some(0);
        state.other_tmux_sessions = names
            .iter()
            .map(|name| OtherTmuxSession::new((*name).to_string(), false, 1))
            .collect();
        state
    }

    #[test]
    fn test_toggle_select_other_tmux_session_supports_multiple_names() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);

        state.toggle_select_session();
        state.selected_other_tmux_index = Some(1);
        state.toggle_select_session();

        assert_eq!(state.selected_other_tmux_sessions.len(), 2);
        assert!(state.selected_other_tmux_sessions.contains("alpha"));
        assert!(state.selected_other_tmux_sessions.contains("beta"));
        assert_eq!(
            state.selected_other_tmux_names_in_order(),
            vec!["alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn test_delete_selected_other_tmux_sessions_opens_bulk_kill_confirmation() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);
        state.selected_other_tmux_sessions.insert("alpha".to_string());
        state.selected_other_tmux_sessions.insert("beta".to_string());

        EventHandler::process_event(AppEvent::DeleteSelectedSessions, &mut state);

        let dialog = state.confirmation_dialog.as_ref().expect("bulk kill confirmation");
        assert_eq!(dialog.title, "Kill tmux Sessions");
        assert!(matches!(
            &dialog.confirm_action,
            ConfirmAction::KillOtherTmuxSessions(names)
                if names == &vec!["alpha".to_string(), "beta".to_string()]
        ));
    }

    #[test]
    fn test_delete_session_uses_checked_other_tmux_sessions_before_cursor_row() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);
        state.selected_other_tmux_index = Some(1);
        state.selected_other_tmux_sessions.insert("alpha".to_string());
        state.selected_other_tmux_sessions.insert("beta".to_string());

        EventHandler::process_event(AppEvent::DeleteSession, &mut state);

        let dialog = state.confirmation_dialog.as_ref().expect("bulk kill confirmation");
        assert!(matches!(
            &dialog.confirm_action,
            ConfirmAction::KillOtherTmuxSessions(names)
                if names == &vec!["alpha".to_string(), "beta".to_string()]
        ));
    }

    #[test]
    fn test_confirm_selected_other_tmux_sessions_queues_bulk_kill() {
        let mut state = state_with_other_tmux_sessions(&["alpha", "beta"]);
        state.selected_other_tmux_sessions.insert("alpha".to_string());
        state.selected_other_tmux_sessions.insert("beta".to_string());
        EventHandler::process_event(AppEvent::DeleteSelectedSessions, &mut state);

        state
            .confirmation_dialog
            .as_mut()
            .expect("bulk kill confirmation")
            .selected_option = true;
        EventHandler::process_event(AppEvent::ConfirmationConfirm, &mut state);

        assert!(state.selected_other_tmux_sessions.is_empty());
        assert!(matches!(
            state.pending_async_action,
            Some(AsyncAction::KillOtherTmuxSessions(ref names))
                if names == &vec!["alpha".to_string(), "beta".to_string()]
        ));
    }

    /// Verify the tri-option dialog defaults to Stop and cycles forward through
    /// all three options before wrapping back to Stop.
    #[test]
    fn test_show_delete_or_stop_confirmation_defaults_to_stop() {
        let mut state = AppState::new();
        let session_id = uuid::Uuid::new_v4();

        state.show_delete_or_stop_confirmation(session_id);

        let dialog = state.confirmation_dialog.as_ref().expect("Dialog should be present");
        let opts = dialog.options.as_ref().expect("Tri-option dialog");
        assert_eq!(opts.len(), 3, "Stop / Delete / Cancel");
        assert_eq!(opts[0].label, "Stop");
        assert_eq!(opts[1].label, "Delete");
        assert_eq!(opts[2].label, "Cancel");
        assert_eq!(dialog.selected_index, 0, "Default = Stop");
        assert!(
            matches!(opts[0].action, ConfirmAction::StopSession(id) if id == session_id),
            "First option must be StopSession for the right uuid"
        );
        assert!(
            matches!(opts[1].action, ConfirmAction::DeleteSession(id) if id == session_id),
            "Second option must be DeleteSession for the right uuid"
        );
        assert!(matches!(opts[2].action, ConfirmAction::Cancel));
    }

    /// Forward cycling and backwards cycling on a tri-option dialog.
    #[test]
    fn test_tri_option_dialog_cycle() {
        let mut dialog = ConfirmationDialog {
            title: "T".into(),
            message: "M".into(),
            confirm_action: ConfirmAction::Cancel,
            selected_option: false,
            warning: None,
            options: Some(vec![
                DialogOption {
                    label: "A".into(),
                    action: ConfirmAction::Cancel,
                },
                DialogOption {
                    label: "B".into(),
                    action: ConfirmAction::Cancel,
                },
                DialogOption {
                    label: "C".into(),
                    action: ConfirmAction::Cancel,
                },
            ]),
            selected_index: 0,
        };

        // Forward cycle 0 -> 1 -> 2 -> 0
        let len = dialog.options.as_ref().unwrap().len();
        dialog.selected_index = (dialog.selected_index + 1) % len;
        assert_eq!(dialog.selected_index, 1);
        dialog.selected_index = (dialog.selected_index + 1) % len;
        assert_eq!(dialog.selected_index, 2);
        dialog.selected_index = (dialog.selected_index + 1) % len;
        assert_eq!(dialog.selected_index, 0);

        // Backward cycle 0 -> 2 -> 1 -> 0
        dialog.selected_index = (dialog.selected_index + len - 1) % len;
        assert_eq!(dialog.selected_index, 2);
        dialog.selected_index = (dialog.selected_index + len - 1) % len;
        assert_eq!(dialog.selected_index, 1);
        dialog.selected_index = (dialog.selected_index + len - 1) % len;
        assert_eq!(dialog.selected_index, 0);
    }

    /// Binary dialog (existing callsites) keeps the legacy yes/no toggle.
    #[test]
    fn test_binary_dialog_unchanged() {
        let session_id = uuid::Uuid::new_v4();
        let mut state = AppState::new();
        state.show_delete_confirmation(session_id);
        let dialog = state.confirmation_dialog.as_ref().unwrap();
        assert!(dialog.options.is_none(), "Legacy binary dialog");
        assert!(!dialog.selected_option, "Default = No");
        assert!(matches!(
            dialog.confirm_action,
            ConfirmAction::DeleteSession(_)
        ));
    }

    /// Path encoding = Claude Code's rule: every non-alphanumeric UTF-16 code
    /// unit becomes `-`.
    #[test]
    fn test_encode_claude_project_dir() {
        use std::path::PathBuf;
        let p = PathBuf::from("/Users/stevie/code/foo-bar");
        assert_eq!(
            AppState::encode_claude_project_dir(&p),
            "-Users-stevie-code-foo-bar"
        );

        let p = PathBuf::from("/");
        assert_eq!(AppState::encode_claude_project_dir(&p), "-");

        // Regression: DOTTED path. Every ainb worktree lives under
        // `~/.agents-in-a-box/`, so the `.` MUST encode to `-`. The old
        // `/`-only rule produced `-.agents-in-a-box` and never matched the
        // real on-disk project dir, so Claude never resumed.
        let p =
            PathBuf::from("/Users/stevie/.agents-in-a-box/worktrees/by-name/repo--f-x--cc7dbd22");
        assert_eq!(
            AppState::encode_claude_project_dir(&p),
            "-Users-stevie--agents-in-a-box-worktrees-by-name-repo--f-x--cc7dbd22"
        );

        // Underscore, space, and non-ASCII all map to `-`; an astral char
        // (emoji, 2 UTF-16 units in Claude Code's JS encoder) maps to TWO.
        let p = PathBuf::from("/tmp/a_b c/é🚀");
        assert_eq!(AppState::encode_claude_project_dir(&p), "-tmp-a-b-c----");
    }

    /// End-to-end guard for the resume-history probe on a dotted worktree
    /// path: a transcript under `~/.claude/projects/{encoded}/` must be found
    /// so `--continue` is emitted. The expected directory name is a HARDCODED
    /// literal, not derived from the encoder under test, so this fails on the
    /// old `/`-only encoding rule.
    #[test]
    fn test_find_latest_transcript_dotted_worktree_path() {
        use std::fs;
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();

        let worktree = PathBuf::from("/Users/stevie/.agents-in-a-box/worktrees/by-name/repo--f-x");
        let project_dir = fake_home
            .join(".claude")
            .join("projects")
            .join("-Users-stevie--agents-in-a-box-worktrees-by-name-repo--f-x");
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("sess.jsonl"), "x").unwrap();

        let result = AppState::find_latest_transcript_in(&fake_home, &worktree);
        assert!(result.is_some(), "dotted worktree transcript must resolve");
    }

    /// The probe canonicalizes the worktree path before encoding: a symlinked
    /// path component (e.g. macOS `/tmp` → `/private/tmp`) must still resolve
    /// to the project dir of the PHYSICAL path, because that is what Claude
    /// Code keys its transcripts on.
    #[test]
    #[cfg(unix)]
    fn test_find_latest_transcript_resolves_symlinked_worktree() {
        use std::fs;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();

        // Real worktree dir + a symlink alias pointing at it.
        let physical = tmp.path().join("wt-real");
        fs::create_dir_all(&physical).unwrap();
        let alias = tmp.path().join("wt-alias");
        std::os::unix::fs::symlink(&physical, &alias).unwrap();

        // Transcript lives under the encoding of the CANONICAL path.
        let canonical = std::fs::canonicalize(&physical).unwrap();
        let project_dir = fake_home
            .join(".claude")
            .join("projects")
            .join(AppState::encode_claude_project_dir(&canonical));
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("sess.jsonl"), "x").unwrap();

        // Probing via the symlink alias must still find it.
        let result = AppState::find_latest_transcript_in(&fake_home, &alias);
        assert!(
            result.is_some(),
            "symlinked worktree transcript must resolve"
        );
    }

    /// `find_latest_transcript_in` returns the most recently modified `.jsonl`
    /// under `<home>/.claude/projects/{encoded}/`.
    #[test]
    fn test_find_latest_transcript_picks_newest_by_mtime() {
        use std::fs::{self, File};
        use std::time::{Duration, SystemTime};

        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();

        // A path inside our tempdir that is never created: canonicalize
        // always fails and falls back to the raw path, so the fixture dir
        // built from the raw encoding stays in agreement with the probe.
        let worktree = tmp.path().join("nonexistent-wt");
        let encoded = AppState::encode_claude_project_dir(&worktree);
        let project_dir = fake_home.join(".claude").join("projects").join(&encoded);
        fs::create_dir_all(&project_dir).unwrap();

        let older = project_dir.join("2024-01.jsonl");
        let newer = project_dir.join("2024-02.jsonl");
        fs::write(&older, "x").unwrap();
        fs::write(&newer, "y").unwrap();

        let now = SystemTime::now();
        File::open(&older)
            .unwrap()
            .set_modified(now - Duration::from_secs(120))
            .unwrap();
        File::open(&newer)
            .unwrap()
            .set_modified(now + Duration::from_secs(120))
            .unwrap();

        // Decoy non-jsonl file should be ignored.
        fs::write(project_dir.join("ignored.txt"), "z").unwrap();

        let result = AppState::find_latest_transcript_in(&fake_home, &worktree)
            .expect("Should find a transcript");
        assert_eq!(result.file_name().unwrap(), "2024-02.jsonl");
    }

    /// Missing project directory returns None (no transcript = fresh start).
    #[test]
    fn test_find_latest_transcript_missing_dir_returns_none() {
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();

        let nonexistent = PathBuf::from("/tmp/never-existed-xyz");
        let result = AppState::find_latest_transcript_in(&fake_home, &nonexistent);
        assert!(result.is_none());
    }

    /// Empty project directory (no `.jsonl` files) returns None.
    #[test]
    fn test_find_latest_transcript_empty_dir_returns_none() {
        use std::fs;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();
        // Never created: canonicalize falls back to the raw path (see
        // test_find_latest_transcript_picks_newest_by_mtime).
        let worktree = tmp.path().join("empty-wt");
        let encoded = AppState::encode_claude_project_dir(&worktree);
        let project_dir = fake_home.join(".claude").join("projects").join(&encoded);
        fs::create_dir_all(&project_dir).unwrap();
        fs::write(project_dir.join("not-a-jsonl.txt"), "z").unwrap();

        let result = AppState::find_latest_transcript_in(&fake_home, &worktree);
        assert!(result.is_none());
    }

    /// `stopped_session_from_metadata` produces a Session with status Stopped
    /// and the right tmux name + worktree path. Backbone of the discovery pass.
    #[test]
    fn test_stopped_session_from_metadata_marks_status_stopped() {
        use crate::interactive::SessionMetadata;
        use crate::models::{SessionAgentType, SessionStatus};
        use std::path::PathBuf;

        let metadata = SessionMetadata {
            session_id: uuid::Uuid::new_v4(),
            tmux_session_name: "tmux_some_branch".to_string(),
            worktree_path: PathBuf::from("/tmp/work-stopped"),
            workspace_name: "ws".to_string(),
            created_at: chrono::Utc::now(),
            agent_type: SessionAgentType::Claude,
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        let session = AppState::stopped_session_from_metadata(&metadata);
        assert_eq!(session.id, metadata.session_id);
        assert!(matches!(session.status, SessionStatus::Stopped));
        assert_eq!(
            session.tmux_session_name.as_deref(),
            Some("tmux_some_branch")
        );
        assert_eq!(session.workspace_path, "/tmp/work-stopped");
        assert_eq!(session.agent_type, SessionAgentType::Claude);
        // Legacy metadata (skip_permissions == None) must default to yolo.
        assert!(
            session.skip_permissions,
            "None skip_permissions must default to dangerously-skip-permissions"
        );
    }

    /// `stopped_session_from_metadata` recovers the CREATED-with launch settings
    /// (yolo off, model) rather than resetting them to defaults.
    #[test]
    fn test_stopped_session_from_metadata_preserves_launch_settings() {
        use crate::interactive::SessionMetadata;
        use crate::models::SessionAgentType;
        use std::path::PathBuf;

        let metadata = SessionMetadata {
            session_id: uuid::Uuid::new_v4(),
            tmux_session_name: "tmux_b".to_string(),
            worktree_path: PathBuf::from("/tmp/work2"),
            workspace_name: "ws".to_string(),
            created_at: chrono::Utc::now(),
            agent_type: SessionAgentType::Claude,
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: Some(false),
            model: Some("claude-opus-4-8".to_string()),
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        let session = AppState::stopped_session_from_metadata(&metadata);
        assert!(
            !session.skip_permissions,
            "Some(false) must be preserved, not defaulted to yolo"
        );
        assert_eq!(session.model.as_deref(), Some("claude-opus-4-8"));
    }

    /// Stopped sessions must recover the branch checked out in their worktree,
    /// rather than the `ainb/<workspace>` default used for brand-new sessions.
    #[test]
    fn test_stopped_session_from_metadata_preserves_worktree_branch() {
        use crate::interactive::SessionMetadata;
        use crate::models::SessionAgentType;

        if !crate::test_support::git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let fx = crate::test_support::real_git_fixture();
        let metadata = SessionMetadata {
            session_id: uuid::Uuid::new_v4(),
            tmux_session_name: "tmux_prefixed-feature".to_string(),
            worktree_path: fx.worktree,
            workspace_name: "fpl".to_string(),
            created_at: chrono::Utc::now(),
            agent_type: SessionAgentType::Claude,
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: Some(true),
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        let session = AppState::stopped_session_from_metadata(&metadata);

        assert_eq!(session.branch_name, "feature");
        assert_ne!(session.branch_name, "ainb/fpl");
    }

    #[tokio::test]
    async fn idle_restart_respawns_a_dead_codex_pane_with_exact_remote_thread() {
        use crate::interactive::InteractiveSessionManager;
        use std::process::Command;
        use std::time::Duration;

        if Command::new("tmux")
            .arg("-V")
            .output()
            .map_or(true, |output| !output.status.success())
        {
            eprintln!("skip: tmux unavailable");
            return;
        }

        struct ExactTmuxSession(String);
        impl Drop for ExactTmuxSession {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", &format!("={}", self.0)])
                    .output();
            }
        }

        let tmux_name = format!("ainb-idle-restart-{}", uuid::Uuid::new_v4());
        let _tmux = ExactTmuxSession(tmux_name.clone());
        let working_dir = std::env::current_dir().expect("current directory");
        assert!(
            Command::new("tmux")
                .args(["new-session", "-d", "-s", &tmux_name])
                .status()
                .expect("create tmux session")
                .success()
        );
        assert!(
            Command::new("tmux")
                .args(["set-option", "-w", "-t", &tmux_name, "remain-on-exit", "on"])
                .status()
                .expect("set remain-on-exit")
                .success()
        );
        assert!(
            Command::new("tmux")
                .args(["respawn-pane", "-k", "-t", &tmux_name, "sh", "-c", "exit 0"])
                .status()
                .expect("make pane dead")
                .success()
        );

        let mut pane_dead = false;
        for _ in 0..20 {
            let dead = Command::new("tmux")
                .args(["display-message", "-p", "-t", &tmux_name, "#{pane_dead}"])
                .output()
                .expect("read pane state");
            if String::from_utf8_lossy(&dead.stdout).trim() == "1" {
                pane_dead = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(pane_dead, "precondition: pane must be dead before restart");

        let remote = ainb_hangar_proto::fleet::CodexSessionEnsureResult {
            endpoint: "unix:///tmp/ainb-idle-restart.sock".to_string(),
            thread_id: Some("thread-idle-restart".to_string()),
        };
        InteractiveSessionManager::new()
            .expect("session manager")
            .start_cli_in_tmux(
                &tmux_name,
                &working_dir,
                true,
                Some("gpt-5.6-luna".to_string()),
                SessionAgentType::Codex,
                None,
                true,
                false,
                Some(&remote),
            )
            .await
            .expect("respawn remote Codex pane");

        let pane = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &tmux_name,
                "-F",
                "#{pane_start_command}",
            ])
            .output()
            .expect("read pane command");
        let pane_command = String::from_utf8_lossy(&pane.stdout);
        // Assert the ARGV, not tmux's rendering of it. `#{pane_start_command}`
        // reports the command as tmux chose to quote it, and that differs
        // between tmux versions: the socket comes back `'unix://...'` on some
        // and bare `unix://...` on others. Matching the quoted form passed
        // locally and failed on CI, which is a difference in the reporter
        // rather than in what was launched. Strip the quotes and assert the
        // pieces that carry meaning.
        let unquoted = pane_command.replace(['\'', '"'], "");
        for fragment in [
            "codex",
            "-c",
            "check_for_update_on_startup=false",
            "--remote",
            "unix:///tmp/ainb-idle-restart.sock",
            "resume",
            "thread-idle-restart",
        ] {
            assert!(
                unquoted.contains(fragment),
                "idle restart must replace dead pane with the exact remote Codex argv; \
                 missing {fragment:?} in: {pane_command}"
            );
        }
        assert!(!pane_command.contains("--last"));
    }

    // -- SessionFilter tests ------------------------------------------------

    use crate::app::state::SessionFilter;
    use crate::models::{Session, SessionStatus as Status, Workspace};

    fn make_filter_session(mode: SessionMode, status: Status) -> Session {
        let mut s = Session::new("test".to_string(), "/tmp/x".to_string());
        s.mode = mode;
        s.status = status;
        s
    }

    #[test]
    fn test_session_filter_cycle_order() {
        assert_eq!(SessionFilter::All.next(), SessionFilter::ActiveOnly);
        assert_eq!(SessionFilter::ActiveOnly.next(), SessionFilter::StoppedOnly);
        assert_eq!(SessionFilter::StoppedOnly.next(), SessionFilter::All);
    }

    #[test]
    fn test_session_filter_default_is_all() {
        assert_eq!(SessionFilter::default(), SessionFilter::All);
    }

    #[test]
    fn test_session_filter_title_label() {
        assert_eq!(SessionFilter::All.title_label(), None);
        assert_eq!(SessionFilter::ActiveOnly.title_label(), Some("active"));
        assert_eq!(SessionFilter::StoppedOnly.title_label(), Some("stopped"));
    }

    #[test]
    fn test_session_passes_filter_all_lets_everything_through() {
        let mut state = AppState::new();
        state.session_filter = SessionFilter::All;
        for mode in [SessionMode::Interactive, SessionMode::Boss] {
            for status in [Status::Running, Status::Stopped, Status::Idle] {
                assert!(
                    state.session_passes_filter(&make_filter_session(mode.clone(), status.clone())),
                    "All should pass mode={:?} status={:?}",
                    mode,
                    status
                );
            }
        }
    }

    #[test]
    fn test_session_passes_filter_active_only_hides_stopped_interactive() {
        let mut state = AppState::new();
        state.session_filter = SessionFilter::ActiveOnly;

        // Interactive Stopped → hidden
        assert!(!state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Stopped
        )));
        // Interactive Running → shown
        assert!(state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Running
        )));
        // Boss-mode Stopped → still passes (filter only touches Interactive)
        assert!(
            state.session_passes_filter(&make_filter_session(SessionMode::Boss, Status::Stopped))
        );
    }

    #[test]
    fn test_session_passes_filter_stopped_only() {
        let mut state = AppState::new();
        state.session_filter = SessionFilter::StoppedOnly;

        assert!(state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Stopped
        )));
        assert!(!state.session_passes_filter(&make_filter_session(
            SessionMode::Interactive,
            Status::Running
        )));
        // Boss-mode is exempt — always passes regardless of filter mode.
        assert!(
            state.session_passes_filter(&make_filter_session(SessionMode::Boss, Status::Running))
        );
    }

    #[test]
    fn test_cycle_session_filter_advances_state() {
        let mut state = AppState::new();
        // AppState restores the user's persisted UI preference; cycle behavior
        // itself must remain independent of that ambient configuration.
        state.session_filter = SessionFilter::All;
        assert_eq!(state.session_filter, SessionFilter::All);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::ActiveOnly);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::StoppedOnly);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::All);
    }

    #[test]
    fn filtered_navigation_skips_hidden_sessions_across_workspaces() {
        let mut state = AppState::new();
        state.workspaces.clear();
        state.session_filter = SessionFilter::ActiveOnly;

        let mut first = Workspace::new("first".to_string(), "/tmp/first".into());
        first.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Running,
        ));

        let mut second = Workspace::new("second".to_string(), "/tmp/second".into());
        second.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Stopped,
        ));
        second.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Running,
        ));
        second.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Stopped,
        ));
        second.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Running,
        ));

        state.workspaces = vec![first, second];
        state.selected_workspace_index = Some(0);
        state.selected_session_index = Some(0);

        state.next_session();
        assert_eq!(state.selected_workspace_index, Some(1));
        assert_eq!(state.selected_session_index, Some(1));

        state.previous_session();
        assert_eq!(state.selected_workspace_index, Some(0));
        assert_eq!(state.selected_session_index, Some(0));

        state.next_workspace();
        assert_eq!(state.selected_workspace_index, Some(1));
        assert_eq!(state.selected_session_index, Some(1));

        EventHandler::process_event(AppEvent::GoToBottom, &mut state);
        assert_eq!(state.selected_session_index, Some(3));
        EventHandler::process_event(AppEvent::GoToTop, &mut state);
        assert_eq!(state.selected_session_index, Some(1));

        state.previous_workspace();
        assert_eq!(state.selected_workspace_index, Some(0));
        assert_eq!(state.selected_session_index, Some(0));
    }

    #[test]
    fn initial_filtered_selection_skips_hidden_workspaces() {
        let mut state = AppState::new();
        state.workspaces.clear();
        state.session_filter = SessionFilter::ActiveOnly;

        let mut hidden = Workspace::new("hidden".to_string(), "/tmp/hidden".into());
        hidden.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Stopped,
        ));

        let mut visible = Workspace::new("visible".to_string(), "/tmp/visible".into());
        visible.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Stopped,
        ));
        visible.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Running,
        ));

        state.workspaces = vec![hidden, visible];
        assert!(state.select_first_visible_workspace_item_from(0));
        assert_eq!(state.selected_workspace_index, Some(1));
        assert_eq!(state.selected_session_index, Some(1));
    }

    #[test]
    fn previous_workspace_selects_its_first_visible_session() {
        let mut state = AppState::new();
        state.workspaces.clear();
        state.session_filter = SessionFilter::ActiveOnly;

        let mut first = Workspace::new("first".to_string(), "/tmp/first".into());
        first.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Stopped,
        ));
        first.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Running,
        ));
        first.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Running,
        ));

        let mut second = Workspace::new("second".to_string(), "/tmp/second".into());
        second.add_session(make_filter_session(
            SessionMode::Interactive,
            Status::Running,
        ));

        state.workspaces = vec![first, second];
        state.selected_workspace_index = Some(1);
        state.selected_session_index = Some(0);
        state.previous_workspace();

        assert_eq!(state.selected_workspace_index, Some(0));
        assert_eq!(state.selected_session_index, Some(1));
    }

    #[test]
    fn merge_oldest_call_day_keeps_earliest_across_loads() {
        use chrono::NaiveDate;
        let april = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let may = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();

        // First load establishes the anchor.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(None, Some(april)),
            Some(april)
        );

        // Reproduces the reported defect: a wide load establishes April,
        // a subsequent narrow (May-only) load must NOT raise the anchor.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(Some(april), Some(may)),
            Some(april),
            "narrow load must not raise the anchor above its earlier extent"
        );

        // Order-independent: narrow first, then wide, narrows the anchor.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(Some(may), Some(april)),
            Some(april)
        );

        // Empty load (no calls in the period) leaves anchor untouched.
        assert_eq!(
            crate::app::state::merge_oldest_call_day(Some(april), None),
            Some(april)
        );

        // Empty existing + empty candidate stays None.
        assert_eq!(crate::app::state::merge_oldest_call_day(None, None), None);
    }

    // -- statusline status TTL cache --------------------------------
    //
    // The cache backs both the global `W` shortcut (settings.json read on
    // every keystroke before this PR) and the top-of-Stats enable card
    // (settings.json read on every render frame). The tests below exercise
    // the pure inner helper with an injected clock + detector counter so
    // they're hermetic — no temp dirs or HOME mutation needed.

    use std::cell::Cell;
    use std::time::{Duration, Instant};

    #[test]
    fn statusline_cache_returns_value_on_first_call_and_records_time() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut cache = None;
        let calls = Cell::new(0u32);
        let now = Instant::now();
        let detect = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::NotConfigured)
        };

        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            now,
            detect,
        );

        assert_eq!(result, Some(StatuslineStatus::NotConfigured));
        assert_eq!(calls.get(), 1, "first call must hit the detector");
        assert!(cache.is_some(), "cache must be populated after first call");
    }

    #[test]
    fn statusline_cache_coalesces_within_ttl() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut cache = None;
        let calls = Cell::new(0u32);
        let t0 = Instant::now();
        let detect_first = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::Configured)
        };
        let _ = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t0,
            detect_first,
        );
        assert_eq!(calls.get(), 1);

        // Second call 5 seconds later — still inside the 15s TTL window.
        // Must serve from cache without invoking the detector.
        let t1 = t0 + Duration::from_secs(5);
        let detect_must_not_run = || -> anyhow::Result<StatuslineStatus> {
            panic!("detector must not run while cache is fresh");
        };
        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t1,
            detect_must_not_run,
        );
        assert_eq!(
            result,
            Some(StatuslineStatus::Configured),
            "fresh cache hit must return the original value"
        );
        assert_eq!(calls.get(), 1, "second call inside TTL must not re-detect");
    }

    #[test]
    fn statusline_cache_re_detects_after_ttl_expires() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut cache = None;
        let calls = Cell::new(0u32);
        let ttl = Duration::from_secs(15);
        let t0 = Instant::now();
        let detect = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::NotConfigured)
        };
        let _ = AppState::statusline_status_cached_inner(&mut cache, ttl, t0, detect);
        assert_eq!(calls.get(), 1);

        // 30 seconds later: well past the 15s TTL — detector must run.
        let t1 = t0 + Duration::from_secs(30);
        let detect_again = || {
            calls.set(calls.get() + 1);
            Ok(StatuslineStatus::Configured)
        };
        let result = AppState::statusline_status_cached_inner(&mut cache, ttl, t1, detect_again);
        assert_eq!(result, Some(StatuslineStatus::Configured));
        assert_eq!(calls.get(), 2, "expired cache must re-detect");
    }

    #[test]
    fn statusline_cache_stores_detector_failure_to_avoid_retry_storm() {
        // A failing detector returns None and the cache records None +
        // the timestamp. A subsequent call within TTL serves the None
        // from cache without re-hitting the detector — otherwise an IO
        // error on settings.json would translate into one filesystem
        // probe per render frame.
        let mut cache = None;
        let calls = Cell::new(0u32);
        let t0 = Instant::now();
        let detect_err = || -> anyhow::Result<crate::cli::statusline_install::StatuslineStatus> {
            calls.set(calls.get() + 1);
            anyhow::bail!("simulated read failure")
        };
        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t0,
            detect_err,
        );
        assert_eq!(result, None);
        assert_eq!(calls.get(), 1);

        // Within TTL, the cached None is returned without re-detecting.
        let t1 = t0 + Duration::from_secs(2);
        let detect_must_not_run =
            || -> anyhow::Result<crate::cli::statusline_install::StatuslineStatus> {
                panic!("detector must not run while cache is fresh, even when value is None");
            };
        let result = AppState::statusline_status_cached_inner(
            &mut cache,
            Duration::from_secs(15),
            t1,
            detect_must_not_run,
        );
        assert_eq!(result, None);
        assert_eq!(
            calls.get(),
            1,
            "cached None must not retrigger the detector"
        );
    }

    #[test]
    fn invalidate_statusline_status_cache_forces_refresh() {
        use crate::cli::statusline_install::StatuslineStatus;

        let mut state = AppState::new();
        // Seed the cache directly with a stale value.
        state.statusline_status_cache =
            Some((Some(StatuslineStatus::NotConfigured), Instant::now()));
        assert!(state.statusline_status_cache.is_some());

        state.invalidate_statusline_status_cache();
        assert!(
            state.statusline_status_cache.is_none(),
            "invalidation must drop the cached entry"
        );
    }

    // ========================================================================
    // Config "Default Workspace" round-trip
    //
    // Regression: editing Default Workspace appended to `workspace_scan_paths`
    // while the field is displayed from `first()`, so the edit never showed on
    // reopen ("back to the same folder"). It must replace the primary entry.
    // ========================================================================

    use crate::app::state::{ConfigCategory, ConfigScreenState, ConfigValue};
    use crate::config::AppConfig;

    // ========================================================================
    // Config screen category list
    // ========================================================================

    /// The screen renders only the categories that have rows. `ConfigCategory`
    /// now covers the whole TOML schema (registry.rs), so without this filter
    /// opening Settings would show eight empty sections.
    #[test]
    fn config_screen_lists_only_categories_that_have_rows() {
        let screen = ConfigScreenState::default();
        for category in &screen.categories {
            let rows = screen
                .settings
                .get(category)
                .unwrap_or_else(|| panic!("category {category:?} is listed but has no settings"));
            assert!(
                !rows.is_empty(),
                "category {category:?} is listed but has no rows"
            );
        }
    }

    /// The tmux tripwire (`tripwire_config_plugins`) steps down exactly five
    /// times to reach Plugins. Renumbering the category list silently breaks a
    /// test that lives in another file, so pin the order here where it is cheap
    /// to see.
    #[test]
    fn plugins_is_the_sixth_category() {
        let screen = ConfigScreenState::default();
        assert_eq!(
            screen.categories.get(5),
            Some(&ConfigCategory::Plugins),
            "categories: {:?}",
            screen.categories
        );
    }

    /// Edit a row by its dotted key, exactly as the popup confirm does.
    fn edit(screen: &mut ConfigScreenState, key: &str, value: ConfigValue) {
        assert!(
            screen.settings.values().flatten().any(|s| s.key == key),
            "no row for '{key}'; the registry and the screen have drifted"
        );
        screen.set_row_value(key, value);
    }

    fn row_display(screen: &ConfigScreenState, key: &str) -> String {
        screen
            .settings
            .values()
            .flatten()
            .find(|s| s.key == key)
            .unwrap_or_else(|| panic!("no row for '{key}'"))
            .value
            .display()
    }

    // ========================================================================
    // Registry-driven rows: an edit reaches AppConfig and survives a reopen
    // ========================================================================
    //
    // These replace the old hand-written `default_workspace` / `branch_prefix`
    // round-trip tests. Those covered four of the ~24 rows and were the only
    // rows with a persist branch worth testing; the point now is that ONE path
    // (serialize → set_validated → deserialize) covers every row, so the tests
    // pick one row per widget kind rather than one row per hand-written arm.

    #[test]
    fn a_text_edit_round_trips_through_the_registry() {
        let mut config = AppConfig::default();
        let mut screen = ConfigScreenState::from_app_config(&config);

        edit(
            &mut screen,
            "workspace_defaults.branch_prefix",
            ConfigValue::Text("squad/".to_string()),
        );
        screen.apply_to_app_config(&mut config).expect("edit applies");

        assert_eq!(config.workspace_defaults.branch_prefix, "squad/");
        let reopened = ConfigScreenState::from_app_config(&config);
        assert_eq!(
            row_display(&reopened, "workspace_defaults.branch_prefix"),
            "squad/"
        );
    }

    #[test]
    fn a_list_edit_round_trips_as_a_comma_separated_list() {
        let mut config = AppConfig::default();
        let mut screen = ConfigScreenState::from_app_config(&config);

        edit(
            &mut screen,
            "workspace_defaults.workspace_scan_paths",
            ConfigValue::Text("/a/projects, /a/work".to_string()),
        );
        screen.apply_to_app_config(&mut config).expect("edit applies");

        assert_eq!(
            config.workspace_defaults.workspace_scan_paths,
            vec![PathBuf::from("/a/projects"), PathBuf::from("/a/work")]
        );
        let reopened = ConfigScreenState::from_app_config(&config);
        assert_eq!(
            row_display(&reopened, "workspace_defaults.workspace_scan_paths"),
            "/a/projects, /a/work"
        );
    }

    #[test]
    fn a_number_edit_round_trips_and_is_range_checked() {
        let mut config = AppConfig::default();
        let mut screen = ConfigScreenState::from_app_config(&config);

        edit(&mut screen, "docker.timeout", ConfigValue::Number(120));
        screen.apply_to_app_config(&mut config).expect("edit applies");
        assert_eq!(config.docker.timeout, 120);

        // Out of the registry's declared range: the row is REPORTED and
        // skipped rather than written, and the pass still succeeds so one bad
        // value cannot block every other row (see
        // `a_rejected_edit_does_not_block_the_others`).
        let mut screen = ConfigScreenState::from_app_config(&config);
        edit(&mut screen, "docker.timeout", ConfigValue::Number(0));
        let applied = screen.apply_to_app_config(&mut config).expect("the pass succeeds");
        assert_eq!(applied.rejected.len(), 1, "{:?}", applied.rejected);
        assert!(
            applied.rejected[0].1.contains("between 1 and 3600"),
            "{:?}",
            applied.rejected
        );
        assert_eq!(
            config.docker.timeout, 120,
            "a rejected edit changes nothing"
        );
    }

    #[test]
    fn an_untouched_row_is_never_written() {
        // The screen seeds ~150 rows, most of them from serde defaults. Only
        // the rows the user actually edited may reach config.toml — otherwise
        // opening Settings and pressing S would materialise every default.
        let config = AppConfig::default();
        let screen = ConfigScreenState::from_app_config(&config);
        assert!(
            screen.pending_edits().is_empty(),
            "a freshly-opened screen has pending edits: {:?}",
            screen.pending_edits()
        );
    }

    #[test]
    fn a_read_only_usage_row_is_shown_but_never_written() {
        // `[usage]` belongs to the burndown plugin (READ_ONLY_SECTIONS). The
        // row exists so the value is visible, and is refused at both the
        // keypress and the save.
        let config = AppConfig::default();
        let mut screen = ConfigScreenState::from_app_config(&config);
        assert!(
            screen.settings.values().flatten().any(|s| s.key == "usage.currency.code"),
            "usage rows must still be visible"
        );
        // Even a forced edit (the keypress path refuses first) is filtered out.
        screen.set_row_value("usage.currency.code", ConfigValue::Text("EUR".to_string()));
        assert!(
            !screen.pending_edits().iter().any(|(key, _)| key.starts_with("usage.")),
            "a usage row must never reach a save: {:?}",
            screen.pending_edits()
        );
    }

    /// #11. `EXTERNAL_PREFIXES` holds DOTTED PATHS, so grafting them into the
    /// seed has to walk the path. The old code trimmed `"fleet.bridge."` to
    /// `"fleet.bridge"` and looked it up as a flat top-level key, which TOML can
    /// never contain — the section was silently never carried across.
    #[test]
    fn external_sections_graft_at_their_dotted_path() {
        let mut seed: toml::Value = toml::from_str("[docker]\ntimeout = 60\n").unwrap();
        let on_disk: toml::Value = toml::from_str(
            "[skills]\napi_key = \"sk-secret\"\n\n[fleet.bridge.telegram]\ntoken = \"keychain:t\"\n",
        )
        .unwrap();

        crate::app::state::merge_external_sections(&mut seed, &on_disk);

        assert_eq!(
            crate::config::registry::navigate_toml(&seed, "skills.api_key").ok(),
            Some(&toml::Value::String("sk-secret".to_string())),
            "single-segment external section was dropped: {seed}"
        );
        assert_eq!(
            crate::config::registry::navigate_toml(&seed, "fleet.bridge.telegram.token").ok(),
            Some(&toml::Value::String("keychain:t".to_string())),
            "a dotted external prefix was looked up as a flat key: {seed}"
        );
    }

    /// The seed must not overwrite a section it already carries — the loaded
    /// config wins over whatever is on disk.
    #[test]
    fn an_external_section_already_in_the_seed_is_not_overwritten() {
        let mut seed: toml::Value =
            toml::from_str("[skills]\napi_key = \"from-config\"\n").unwrap();
        let on_disk: toml::Value = toml::from_str("[skills]\napi_key = \"from-disk\"\n").unwrap();
        crate::app::state::merge_external_sections(&mut seed, &on_disk);
        assert_eq!(
            crate::config::registry::navigate_toml(&seed, "skills.api_key").ok(),
            Some(&toml::Value::String("from-config".to_string()))
        );
    }

    /// #2b. Expanding a node must not write anything until the screen is left,
    /// and a screen the user only browsed must leave the file alone entirely.
    #[test]
    fn expansion_is_flushed_once_on_exit_and_never_when_untouched() {
        let mut screen = ConfigScreenState::from_app_config(&AppConfig::default());
        assert!(
            screen.take_expansion_to_persist().is_none(),
            "a freshly opened screen wants to write the config"
        );

        let root = screen
            .tree
            .iter()
            .position(|node| node.category == ConfigCategory::ContainerTemplates && node.depth == 0)
            .expect("container templates root");
        screen.selected_node = screen
            .visible_nodes
            .iter()
            .position(|index| *index == root)
            .expect("root visible");

        assert!(screen.toggle_expanded());
        assert!(screen.toggle_expanded(), "collapse it again");

        let flushed = screen
            .take_expansion_to_persist()
            .expect("a toggled tree must be persisted on exit");
        assert!(flushed.is_empty(), "expanded then collapsed: {flushed:?}");
        assert!(
            screen.take_expansion_to_persist().is_none(),
            "the flush must happen once, not on every exit"
        );
    }

    /// #4. A Ctrl+K prompt that is escaped must disarm the row.
    ///
    /// Left armed, the NEXT ordinary edit of that same row is routed into the
    /// keychain: typing `$TELEGRAM_BOT_TOKEN` stores that literal string as a
    /// secret and rewrites the row to a `keychain:` ref, discarding the env
    /// reference the user asked for. Drives the real event so the fix cannot be
    /// "the state has a method nobody calls".
    #[test]
    fn cancelling_a_keychain_prompt_disarms_the_row() {
        let mut state = crate::app::state::AppState::new();
        state.config_screen_state.keychain_target = Some("fleet.bridge.telegram.token".to_string());

        crate::app::EventHandler::process_event(
            crate::app::events::AppEvent::ConfigPopupCancel,
            &mut state,
        );

        assert_eq!(
            state.config_screen_state.keychain_target, None,
            "an escaped Ctrl+K prompt left the row armed; the next edit of it \
             would be stored in the keychain"
        );
    }

    /// #8. One unwritable row must not wedge every later save. The registry
    /// refuses an out-of-range number; the other edit in the same pass has to
    /// land anyway, and the bad key must not stay pending.
    #[test]
    fn a_rejected_edit_does_not_block_the_others() {
        let mut config = AppConfig::default();
        let mut screen = ConfigScreenState::from_app_config(&config);

        edit(&mut screen, "docker.timeout", ConfigValue::Number(0)); // below the range
        edit(
            &mut screen,
            "workspace_defaults.branch_prefix",
            ConfigValue::Text("squad/".to_string()),
        );

        let applied = screen.apply_to_app_config(&mut config).expect("the pass itself succeeds");
        assert_eq!(applied.rejected.len(), 1, "{:?}", applied.rejected);
        assert!(
            applied.rejected[0].0 == "docker.timeout",
            "{:?}",
            applied.rejected
        );
        assert_eq!(
            config.workspace_defaults.branch_prefix, "squad/",
            "the good edit was blocked by the bad one"
        );

        // And once saved, the bad key is gone from `dirty` rather than
        // re-failing on every subsequent auto-persist.
        screen.mark_saved();
        assert!(
            screen.pending_edits().is_empty(),
            "a rejected edit stayed pending: {:?}",
            screen.pending_edits()
        );
    }

    #[test]
    fn the_search_filter_finds_a_row_by_its_dotted_key() {
        // The whole reason the tree got a `/` filter: a leaf is reachable
        // without knowing which section it lives in.
        let mut screen = ConfigScreenState::from_app_config(&AppConfig::default());
        screen.start_search();
        for c in "idle_grace".chars() {
            screen.push_search_char(c);
        }
        let hits: Vec<String> =
            screen.current_settings().iter().map(|row| row.key.clone()).collect();
        assert!(
            hits.contains(&"mcp_pool.idle_grace_secs".to_string()),
            "search missed the row: {hits:?}"
        );

        screen.clear_search();
        assert!(!screen.is_searching());
    }

    #[test]
    fn expanding_a_node_reveals_its_children() {
        let mut screen = ConfigScreenState::from_app_config(&AppConfig::default());
        // Select the Container Templates root, which nests per template.
        let root = screen
            .tree
            .iter()
            .position(|node| node.category == ConfigCategory::ContainerTemplates && node.depth == 0)
            .expect("container templates root");
        screen.selected_node = screen
            .visible_nodes
            .iter()
            .position(|index| *index == root)
            .expect("root visible");

        let before = screen.visible_nodes.len();
        assert!(screen.current_node().unwrap().has_children);
        assert!(screen.toggle_expanded(), "root expands");
        assert!(
            screen.visible_nodes.len() > before,
            "expanding revealed nothing: {} -> {}",
            before,
            screen.visible_nodes.len()
        );

        assert!(screen.toggle_expanded(), "root collapses");
        assert_eq!(screen.visible_nodes.len(), before);
    }

    // ========================================================================
    // P2 — Settings ▸ Plugins renders manifest [config] schema; edits persist
    // ========================================================================
    //
    // The Plugins-category settings builder turns each loaded plugin's
    // `[[config]]` schema into editable rows, and `apply_to_app_config` routes
    // those edits into `plugins.values[plugin][key]` (NOT a top-level field).
    // The composite row key `plugin:<name>:<field_key>` keeps rows unique across
    // plugins that share a field name and lets `apply` recover (plugin, key).

    use crate::config::PluginsConfig;
    use ainb_plugin_protocol::manifest::{
        Capabilities, ConfigField, ConfigKind, Lifecycle, Manifest, PluginMeta, Provides,
        SpawnMode, Subscribes,
    };

    /// Build a manifest with a `[config]` schema covering every `ConfigKind`
    /// so the kind→ConfigValue mapping is exercised in one fixture.
    fn manifest_with_config(name: &str, fields: Vec<ConfigField>) -> Manifest {
        Manifest {
            plugin: PluginMeta {
                name: name.into(),
                version: "0.1.0".into(),
                abi_version: 2,
                description: String::new(),
            },
            capabilities: Capabilities::default(),
            provides: Provides::default(),
            subscribes: Subscribes::default(),
            lifecycle: Lifecycle {
                spawn: SpawnMode::Lazy,
                idle_reap_secs: 600,
            },
            config: fields,
        }
    }

    fn field(key: &str, kind: ConfigKind, default: &str, choices: &[&str]) -> ConfigField {
        ConfigField {
            key: key.into(),
            kind,
            label: format!("{key} label"),
            default: default.into(),
            choices: choices.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    /// The composite row key the builder uses for a plugin config field.
    fn plugin_row_key(plugin: &str, key: &str) -> String {
        format!("plugin:{plugin}:{key}")
    }

    fn plugins_values(name: &str, pairs: &[(&str, &str)]) -> PluginsConfig {
        let mut table = toml::value::Table::new();
        for (k, v) in pairs {
            table.insert((*k).into(), toml::Value::String((*v).into()));
        }
        let mut values = std::collections::BTreeMap::new();
        values.insert(name.to_string(), toml::Value::Table(table));
        PluginsConfig {
            enabled: Vec::new(),
            disabled: Vec::new(),
            values,
        }
    }

    #[test]
    fn test_plugins_category_settings_from_manifest() {
        // Given a loaded plugin with a [config] schema, the Plugins-category
        // builder appends one ConfigSetting per ConfigField, mapping each
        // kind → the matching ConfigValue variant, and defaulting the value
        // from plugins.values first, then the schema default.
        let manifest = manifest_with_config(
            "learnings",
            vec![
                field("learnings_dir", ConfigKind::Path, "~/.learnings", &[]),
                field("qmd_collection", ConfigKind::String, "learnings", &[]),
                field("enabled", ConfigKind::Bool, "true", &[]),
                field("mode", ConfigKind::Enum, "local", &["local", "global"]),
                field("limit", ConfigKind::Int, "20", &[]),
            ],
        );
        // plugins.values overrides one path; the rest fall back to schema default.
        let cfg = plugins_values("learnings", &[("learnings_dir", "/tmp/kb")]);

        let mut screen = ConfigScreenState::default();
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);

        let rows = screen.settings.get(&ConfigCategory::Plugins).expect("Plugins category present");

        // The static "Installed Plugins: None installed" placeholder is gone,
        // replaced by a real per-plugin enable toggle.
        assert!(
            rows.iter().any(|s| s.key == "plugin-enabled:learnings"),
            "expected a real enable toggle for the loaded plugin, got: {:?}",
            rows.iter().map(|s| &s.key).collect::<Vec<_>>()
        );

        // learnings_dir: path → Text, value from plugins.values (override wins).
        let dir = rows
            .iter()
            .find(|s| s.key == plugin_row_key("learnings", "learnings_dir"))
            .expect("learnings_dir row present");
        assert!(matches!(dir.value, ConfigValue::Text(ref t) if t == "/tmp/kb"));
        assert_eq!(dir.label, "learnings_dir label");

        // qmd_collection: string → Text, value from schema default (no override).
        let coll = rows
            .iter()
            .find(|s| s.key == plugin_row_key("learnings", "qmd_collection"))
            .expect("qmd_collection row present");
        assert!(matches!(coll.value, ConfigValue::Text(ref t) if t == "learnings"));

        // bool → Bool, parsed from the schema default "true".
        let en = rows
            .iter()
            .find(|s| s.key == plugin_row_key("learnings", "enabled"))
            .expect("enabled row present");
        assert!(matches!(en.value, ConfigValue::Bool(true)));

        // enum → Choice(choices, selected_idx) with the default selected.
        let mode = rows
            .iter()
            .find(|s| s.key == plugin_row_key("learnings", "mode"))
            .expect("mode row present");
        match &mode.value {
            ConfigValue::Choice(opts, idx) => {
                assert_eq!(opts, &vec!["local".to_string(), "global".to_string()]);
                assert_eq!(*idx, 0, "default 'local' selected");
            }
            other => panic!("enum kind must map to Choice, got {other:?}"),
        }

        // int → Number, parsed from the schema default "20".
        let limit = rows
            .iter()
            .find(|s| s.key == plugin_row_key("learnings", "limit"))
            .expect("limit row present");
        assert!(matches!(limit.value, ConfigValue::Number(20)));
    }

    /// The same race for a per-plugin `[[config]]` row, which is what
    /// `tripwire_config_plugins` flaked on: discovery landing between the edit
    /// and its save rebuilt the row from the saved config and threw the typed
    /// value away.
    #[test]
    fn discovery_does_not_drop_an_edited_plugin_field() {
        let manifest = manifest_with_config(
            "learnings",
            vec![field(
                "learnings_dir",
                ConfigKind::Path,
                "~/.learnings",
                &[],
            )],
        );
        let cfg = plugins_values("learnings", &[("learnings_dir", "/tmp/seed")]);

        let mut screen = ConfigScreenState::default();
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);

        let row_key = plugin_row_key("learnings", "learnings_dir");
        screen.set_row_value(&row_key, ConfigValue::Text("/tmp/edited".to_string()));

        // Discovery re-runs (a second plugin finished loading).
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);

        let row = screen
            .settings
            .get(&ConfigCategory::Plugins)
            .and_then(|rows| rows.iter().find(|row| row.key == row_key))
            .expect("row rebuilt");
        assert!(
            matches!(row.value, ConfigValue::Text(ref t) if t == "/tmp/edited"),
            "discovery reverted an in-flight edit to {:?}",
            row.value
        );
    }

    /// Plugin discovery finishes after the screen is built, so it can land
    /// between an edit of `plugins.disabled` and that edit's save. Replacing
    /// the list rows with toggles at that moment used to take the pending value
    /// with them.
    #[test]
    fn discovery_does_not_drop_an_edited_plugin_list_row() {
        let mut screen = ConfigScreenState::default();
        screen.set_row_value(
            "plugins.disabled",
            ConfigValue::Text("burndown, witr".to_string()),
        );

        let manifest = manifest_with_config("learnings", vec![]);
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &PluginsConfig::default());

        assert!(
            screen
                .pending_edits()
                .iter()
                .any(|(key, raw)| key == "plugins.disabled" && raw == "burndown, witr"),
            "discovery discarded a pending edit: {:?}",
            screen.pending_edits()
        );
    }

    #[test]
    fn a_plugin_toggle_writes_the_denylist() {
        // The "real plugin list" that replaced the static "Installed Plugins:
        // None installed" placeholder. Turning a plugin off must land in
        // `plugins.disabled`; turning it back on must remove it, which a
        // diff-free rebuild gets right and an append would not.
        let manifest = manifest_with_config("learnings", vec![]);
        let cfg = PluginsConfig::default();

        let mut screen = ConfigScreenState::default();
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);
        screen.set_row_value("plugin-enabled:learnings", ConfigValue::Bool(false));

        let mut app = AppConfig::default();
        screen.apply_to_app_config(&mut app).expect("edits apply");
        assert_eq!(app.plugins.disabled, vec!["learnings".to_string()]);

        // Re-enable: the name comes back out of the denylist.
        screen.set_row_value("plugin-enabled:learnings", ConfigValue::Bool(true));
        screen.apply_to_app_config(&mut app).expect("edits apply");
        assert!(
            app.plugins.disabled.is_empty(),
            "re-enabling left the plugin disabled: {:?}",
            app.plugins.disabled
        );
    }

    #[test]
    fn a_plugin_disabled_elsewhere_is_not_dropped_by_a_toggle_save() {
        // A shared config can disable a plugin this machine never discovered.
        // Rebuilding `plugins.disabled` from the visible toggles alone would
        // silently re-enable it on the next save.
        let manifest = manifest_with_config("learnings", vec![]);
        let cfg = PluginsConfig::default();

        let mut screen = ConfigScreenState::default();
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);
        screen.set_row_value("plugin-enabled:learnings", ConfigValue::Bool(false));

        let mut app = AppConfig::default();
        app.plugins.disabled = vec!["not-installed-here".to_string()];
        screen.apply_to_app_config(&mut app).expect("edits apply");
        assert_eq!(
            app.plugins.disabled,
            vec!["learnings".to_string(), "not-installed-here".to_string()]
        );
    }

    #[test]
    fn a_save_with_no_edits_applies_nothing() {
        // Pressing `S` with nothing edited must not write. `save()` renders the
        // whole AppConfig from the snapshot taken at startup, so a no-op save
        // would revert anything `ainb config set` or another process wrote
        // since — while reporting "No changes to save".
        let manifest = manifest_with_config(
            "learnings",
            vec![field(
                "learnings_dir",
                ConfigKind::Path,
                "~/.learnings",
                &[],
            )],
        );
        let cfg = PluginsConfig::default();
        let mut screen = ConfigScreenState::default();
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);

        assert!(screen.pending_edits().is_empty(), "nothing was edited");

        let mut app = AppConfig::default();
        let applied = screen.apply_to_app_config(&mut app).expect("applies");
        assert!(
            applied.external.is_empty(),
            "a clean screen produced external writes: {:?}",
            applied.external
        );
    }

    #[test]
    fn untouched_plugin_rows_are_not_written_into_config() {
        // Saving an unrelated setting must not materialise every discovered
        // plugin's schema defaults into config.toml. Doing so pins today's
        // defaults, so a later change in the plugin's manifest can never take
        // effect for anyone who ever opened the settings screen.
        let manifest = manifest_with_config(
            "learnings",
            vec![field(
                "learnings_dir",
                ConfigKind::Path,
                "~/.learnings",
                &[],
            )],
        );
        let cfg = PluginsConfig::default();

        let mut screen = ConfigScreenState::default();
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);

        // Nothing edited: the row exists and shows its default, but is clean.
        let mut app = AppConfig::default();
        screen.apply_to_app_config(&mut app).expect("edits apply");

        assert!(
            app.plugins.values.get("learnings").is_none(),
            "an untouched plugin row was written into config.toml: {:?}",
            app.plugins.values
        );
    }

    #[test]
    fn test_apply_routes_plugin_edit_to_values() {
        // Editing a Plugins-category row then apply_to_app_config writes into
        // app_config.plugins.values[plugin][key] — NOT a top-level config field
        // — and a save()→reload (toml round-trip) preserves it.
        let manifest = manifest_with_config(
            "learnings",
            vec![field(
                "learnings_dir",
                ConfigKind::Path,
                "~/.learnings",
                &[],
            )],
        );
        let cfg = PluginsConfig::default();

        let mut screen = ConfigScreenState::default();
        screen.apply_plugin_manifests(std::slice::from_ref(&manifest), &cfg);

        // Simulate the popup-confirm edit: mutate the row value in place,
        // exactly as ConfigPopupConfirm does (matched by key).
        let row_key = plugin_row_key("learnings", "learnings_dir");
        // Through `set_row_value`, exactly as ConfigPopupConfirm does. Mutating
        // the row in place instead would leave it out of `dirty`, and only
        // dirty rows are written — an untouched plugin row must NOT be
        // materialised into config.toml just because some other setting was
        // saved.
        screen.set_row_value(&row_key, ConfigValue::Text("/tmp/edited-kb".to_string()));

        let mut app = AppConfig::default();
        screen.apply_to_app_config(&mut app).expect("edits apply");

        // The edit landed under plugins.values[learnings][learnings_dir],
        // not at any top-level config field.
        let learnings = app
            .plugins
            .values
            .get("learnings")
            .and_then(toml::Value::as_table)
            .expect("learnings value table created");
        assert_eq!(
            learnings.get("learnings_dir").and_then(toml::Value::as_str),
            Some("/tmp/edited-kb")
        );

        // save()→reload round trip (the toml pipeline AppConfig::save uses).
        let serialized = toml::to_string_pretty(&app).expect("serialize app config");
        assert!(
            serialized.contains("[plugins.learnings]"),
            "serialized config must carry the [plugins.learnings] table; got:\n{serialized}"
        );
        let reloaded: AppConfig = toml::from_str(&serialized).expect("reparse app config");
        assert_eq!(
            reloaded
                .plugins
                .values
                .get("learnings")
                .and_then(toml::Value::as_table)
                .and_then(|t| t.get("learnings_dir"))
                .and_then(toml::Value::as_str),
            Some("/tmp/edited-kb"),
            "plugin config edit must survive a save()→reload round trip"
        );
    }

    // ========================================================================
    // Per-session attention marker — derived from real ainb-hooks events
    // (NeedsPermission `[!]` / WaitingOnUser `[?]` / Finished `[✓]`), not
    // from "is the pane generating right now".
    // ========================================================================

    /// Build a notification record for the marker tests. `recent` slices
    /// passed to `attention_for_session` must be newest-first.
    fn rec(
        agent: &str,
        cwd: &str,
        raw_event: &str,
        ts: i64,
    ) -> ainb_plugin_notifyd::NotificationRecord {
        ainb_plugin_notifyd::NotificationRecord {
            id: format!("id-{ts}"),
            ts,
            agent: agent.into(),
            session_id: format!("s-{ts}"),
            cwd: cwd.into(),
            project: cwd.rsplit('/').next().unwrap_or("").into(),
            raw_event: raw_event.into(),
            payload_json: "{}".into(),
            read: false,
            dismissed: false,
        }
    }

    const NOW: i64 = 1_000_000_000;
    const CWD: &str = "/work/feat-x";

    /// The chip kind `attention_for_session` produced, dropping the timestamp
    /// the age-specific tests below assert on separately.
    fn kind_of(
        cwd: &str,
        agent: Option<&str>,
        generating: bool,
        baseline: i64,
        now: i64,
        recent: &[ainb_plugin_notifyd::NotificationRecord],
    ) -> Option<crate::fleet::attention::AttentionKind> {
        AppState::attention_for_session(cwd, agent, generating, baseline, now, recent)
            .map(|chip| chip.kind)
    }

    #[test]
    fn attention_permission_event_marks_needs_permission() {
        use crate::fleet::attention::AttentionKind;
        let recent = vec![rec("claude", CWD, "PermissionRequest", NOW - 1000)];
        assert_eq!(
            kind_of(CWD, Some("claude"), false, 0, NOW, &recent),
            Some(AttentionKind::Approve),
        );
    }

    #[test]
    fn a_local_chip_carries_the_hooks_own_message() {
        // Without this the `ask` pane opens on a locally-produced row saying
        // "the request carried no question text" while the producer plainly had
        // one — which is the daemon-down journey's whole opening line.
        let mut record = rec("claude", CWD, "Notification:idle_prompt", NOW - 1000);
        record.payload_json = r#"{"message":"Which sqlite path?"}"#.to_string();
        let chip = AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &[record])
            .expect("the notification marks the row");
        assert_eq!(chip.detail.as_deref(), Some("Which sqlite path?"));
    }

    #[test]
    fn a_payload_with_no_message_invents_nothing() {
        let record = rec("claude", CWD, "Notification:idle_prompt", NOW - 1000);
        let chip = AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &[record])
            .expect("the notification marks the row");
        assert_eq!(
            chip.detail, None,
            "a manufactured question reads as the agent's own"
        );
    }

    #[test]
    fn a_chip_ages_from_the_hooks_instant_not_from_now() {
        // The whole point of carrying `rec.ts`: restarting the TUI must not
        // reset a nine-minute-old question back to `0s`.
        let recent = vec![rec("claude", CWD, "PermissionRequest", NOW - 9 * 60 * 1000)];
        let chip = AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &recent)
            .expect("permission event marks the row");
        assert_eq!(chip.since_ms, NOW - 9 * 60 * 1000);
        assert_eq!(
            crate::fleet::attention::format_age(NOW, chip.since_ms),
            "9m"
        );
    }

    #[test]
    fn attention_notification_event_marks_waiting() {
        use crate::fleet::attention::AttentionKind;
        let recent = vec![rec("claude", CWD, "Notification:idle_prompt", NOW - 1000)];
        assert_eq!(
            kind_of(CWD, Some("claude"), false, 0, NOW, &recent),
            Some(AttentionKind::Ask),
        );
    }

    #[test]
    fn attention_fresh_stop_marks_finished_stale_stop_clears() {
        use crate::fleet::attention::AttentionKind;
        let fresh = vec![rec("claude", CWD, "Stop", NOW - 1000)];
        assert_eq!(
            kind_of(CWD, Some("claude"), false, 0, NOW, &fresh),
            Some(AttentionKind::Done),
        );
        // Older than the 5-minute DONE TTL → retired, no chip.
        let stale = vec![rec("claude", CWD, "Stop", NOW - 6 * 60 * 1000)];
        assert_eq!(kind_of(CWD, Some("claude"), false, 0, NOW, &stale), None);
    }

    #[test]
    fn attention_suppressed_while_generating() {
        let recent = vec![rec("claude", CWD, "PermissionRequest", NOW - 1000)];
        assert_eq!(
            kind_of(CWD, Some("claude"), true, 0, NOW, &recent),
            None,
            "a generating session shows the busy dot, not an attention chip",
        );
    }

    #[test]
    fn attention_blank_without_a_matching_event() {
        // No events at all → blank (this is the common idle case the old
        // "[?] on everything" behaviour got wrong).
        assert_eq!(kind_of(CWD, Some("claude"), false, 0, NOW, &[]), None);
        // Events exist, but for a different cwd or a different agent —
        // must not bleed across sessions.
        let other = vec![
            rec("codex", CWD, "Notification", NOW - 50),
            rec("claude", "/work/other", "Notification", NOW - 100),
        ];
        assert_eq!(kind_of(CWD, Some("claude"), false, 0, NOW, &other), None);
    }

    #[test]
    fn attention_ignores_events_at_or_before_baseline() {
        use crate::fleet::attention::AttentionKind;
        let recent = vec![rec("claude", CWD, "Notification", 500)];
        // Baseline at/after the event (e.g. user just attached) → cleared.
        assert_eq!(kind_of(CWD, Some("claude"), false, 500, NOW, &recent), None);
        // Baseline just before the event → still marks.
        assert_eq!(
            kind_of(CWD, Some("claude"), false, 499, NOW, &recent),
            Some(AttentionKind::Ask),
        );
    }

    #[test]
    fn attention_newest_event_wins() {
        use crate::fleet::attention::AttentionKind;
        // Question asked, then the turn finished — newest (Stop) supersedes
        // the older Notification.
        let recent = vec![
            rec("claude", CWD, "Stop", NOW - 1000),
            rec("claude", CWD, "Notification", NOW - 5000),
        ];
        assert_eq!(
            kind_of(CWD, Some("claude"), false, 0, NOW, &recent),
            Some(AttentionKind::Done),
        );
    }

    #[test]
    fn attention_none_agent_never_marks() {
        // Shell / SSH session (no hook agent) → never a marker, even with
        // a permission event sitting in the same cwd.
        let recent = vec![rec("claude", CWD, "PermissionRequest", NOW - 1000)];
        assert_eq!(kind_of(CWD, None, false, 0, NOW, &recent), None);
    }

    #[test]
    fn attention_matches_cwd_ignoring_trailing_slash() {
        use crate::fleet::attention::AttentionKind;
        let recent = vec![rec("claude", "/work/feat-x/", "Notification", NOW - 100)];
        assert_eq!(
            kind_of("/work/feat-x", Some("claude"), false, 0, NOW, &recent),
            Some(AttentionKind::Ask),
        );
    }

    // ========================================================================
    // Attention merge: the daemon's rows landing on session rows
    // ========================================================================

    /// One workspace, one session, at `cwd`, with a live pane.
    fn state_with_session_at(cwd: &str, tmux: Option<&str>) -> AppState {
        use crate::models::{Session, SessionStatus, Workspace};
        let mut state = AppState::new();
        state.workspaces.clear();
        let mut workspace = Workspace::new("proj".to_string(), PathBuf::from(cwd));
        let mut session = Session::new("proj".to_string(), cwd.to_string());
        session.status = SessionStatus::Idle;
        session.tmux_session_name = tmux.map(str::to_string);
        workspace.add_session(session);
        state.workspaces.push(workspace);
        state
    }

    /// Install a daemon snapshot with one row at `cwd`.
    fn install_daemon_row(
        state: &AppState,
        cwd: &str,
        chip: crate::fleet::attention::SessionAttention,
    ) {
        use crate::fleet::attention::DaemonAttention;
        let mut by_cwd = std::collections::HashMap::new();
        by_cwd.insert(cwd.to_string(), vec![chip]);
        *state.daemon_attention.lock().unwrap() = DaemonAttention::up(by_cwd);
    }

    #[test]
    fn a_daemon_row_lands_on_its_session_even_with_no_notifications_store() {
        use crate::fleet::attention::{AttentionKind, AttentionSource, SessionAttention};
        // The local producer is the FLOOR, not a gate. A host that never ran
        // notifyd has no notifications.db at all; before this the whole refresh
        // returned early there and the daemon's rows never landed.
        let cwd = "/work/daemon-only";
        let mut state = state_with_session_at(cwd, Some("tmux_proj"));
        install_daemon_row(
            &state,
            cwd,
            SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-1".into())
                .with_detail("Decide the sqlite path"),
        );

        state.refresh_attention_markers(2_000);

        let chips = &state.workspaces[0].sessions[0].live_attention;
        assert_eq!(chips.len(), 1, "the daemon row must land: {chips:?}");
        assert_eq!(chips[0].kind, AttentionKind::Ask);
        assert_eq!(chips[0].source, AttentionSource::Daemon);
        assert_eq!(chips[0].detail.as_deref(), Some("Decide the sqlite path"));
    }

    #[test]
    fn a_daemon_row_is_routed_through_the_daemon_while_it_is_up() {
        use crate::fleet::attention::{Answerable, AttentionKind, SessionAttention};
        let cwd = "/work/routed";
        let mut state = state_with_session_at(cwd, Some("tmux_proj"));
        install_daemon_row(
            &state,
            cwd,
            SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-9".into()),
        );

        state.refresh_attention_markers(2_000);

        assert_eq!(
            state.workspaces[0].sessions[0].live_attention[0].answerable,
            Answerable::Daemon {
                attention_id: "att-9".into()
            }
        );
    }

    #[test]
    fn a_daemon_row_greys_out_with_a_reason_once_the_daemon_goes() {
        use crate::fleet::attention::{AttentionKind, DaemonAttention, SessionAttention};
        let cwd = "/work/gone";
        let mut state = state_with_session_at(cwd, Some("tmux_proj"));
        // The row is present but the daemon is NOT reachable — the shape a
        // client sees between a cached row and a failed poll.
        let mut by_cwd = std::collections::HashMap::new();
        by_cwd.insert(
            cwd.to_string(),
            vec![SessionAttention::daemon(
                AttentionKind::Ask,
                1_000,
                "att-1".into(),
            )],
        );
        *state.daemon_attention.lock().unwrap() = DaemonAttention {
            by_cwd,
            reachable: false,
            error: Some("attention/list via /x/hangar.sock: refused".into()),
        };

        state.refresh_attention_markers(2_000);

        let chip = &state.workspaces[0].sessions[0].live_attention[0];
        assert!(
            !chip.answerable.is_answerable(),
            "an ACP-backed row with no daemon must not look answerable"
        );
        assert!(
            chip.answerable.refusal().is_some_and(|r| r.contains("attention/answer")),
            "and it must say which call is unavailable: {:?}",
            chip.answerable
        );
    }

    #[test]
    fn a_daemon_row_for_a_cwd_on_no_row_is_counted_elsewhere() {
        use crate::fleet::attention::{AttentionKind, SessionAttention};
        let mut state = state_with_session_at("/work/on-screen", Some("tmux_proj"));
        install_daemon_row(
            &state,
            "/work/somewhere-else",
            SessionAttention::daemon(AttentionKind::Approve, 1_000, "att-x".into()),
        );

        state.refresh_attention_markers(2_000);

        assert!(state.workspaces[0].sessions[0].live_attention.is_empty());
        assert_eq!(
            state.attention_elsewhere, 1,
            "a request the screen cannot place is still a request"
        );
    }

    #[test]
    fn an_attached_session_claims_its_cwd_so_it_is_not_reported_elsewhere() {
        use crate::fleet::attention::{AttentionKind, SessionAttention};
        let cwd = "/work/attached";
        let mut state = state_with_session_at(cwd, Some("tmux_proj"));
        state.workspaces[0].sessions[0].is_attached = true;
        install_daemon_row(
            &state,
            cwd,
            SessionAttention::daemon(AttentionKind::Ask, 1_000, "att-1".into()),
        );

        state.refresh_attention_markers(2_000);

        assert!(
            state.workspaces[0].sessions[0].live_attention.is_empty(),
            "an attached session never nags — the operator is looking at it"
        );
        assert_eq!(
            state.attention_elsewhere, 0,
            "the session under the cursor must not be reported as waiting elsewhere"
        );
    }

    #[test]
    fn a_daemon_row_survives_a_generating_session() {
        use crate::fleet::attention::{AttentionKind, SessionAttention};
        use crate::models::SessionStatus;
        // The generating gate exists because the LOCAL producer infers waiting
        // from a quiet pane, which is a guess. A daemon row is a request the
        // agent actually raised, and an agent can be mid-turn and blocked on an
        // approval at once — suppressing it there is how an operator ends up
        // watching a spinner that is waiting on them.
        let cwd = "/work/busy";
        let mut state = state_with_session_at(cwd, Some("tmux_proj"));
        state.workspaces[0].sessions[0].status = SessionStatus::Running;
        install_daemon_row(
            &state,
            cwd,
            SessionAttention::daemon(AttentionKind::Approve, 1_000, "att-1".into()),
        );

        state.refresh_attention_markers(2_000);

        assert_eq!(
            state.workspaces[0].sessions[0].live_attention.len(),
            1,
            "a generating session can still be blocked on an approval"
        );
    }

    #[test]
    fn agent_hook_name_maps_supported_hook_agents() {
        assert_eq!(
            AppState::agent_hook_name(SessionAgentType::Claude),
            Some("claude")
        );
        assert_eq!(
            AppState::agent_hook_name(SessionAgentType::Codex),
            Some("codex")
        );
        assert_eq!(
            AppState::agent_hook_name(SessionAgentType::Copilot),
            Some("copilot")
        );
        assert_eq!(AppState::agent_hook_name(SessionAgentType::Shell), None);
        assert_eq!(AppState::agent_hook_name(SessionAgentType::Gemini), None);
    }

    // ---- bulk-resume selection (Enter/r on multi-selected sessions) ----

    fn resumable_session(
        name: &str,
        mode: SessionMode,
        agent: SessionAgentType,
        status: crate::models::SessionStatus,
    ) -> crate::models::Session {
        let mut s = crate::models::Session::new(name.to_string(), "/tmp/ws".to_string());
        s.mode = mode;
        s.agent_type = agent;
        s.status = status;
        s
    }

    /// Bulk resume must start every selected *stopped interactive* session and
    /// exclude everything else — Running interactive (would kill+recreate a live
    /// tmux), Boss-mode, and non-agent (Shell) sessions. Regression for the bug
    /// where Enter only resumed the highlighted row.
    #[test]
    fn selected_resumable_session_ids_keeps_only_stopped_interactive() {
        use crate::models::SessionStatus;

        let stopped_claude = resumable_session(
            "stopped-claude",
            SessionMode::Interactive,
            SessionAgentType::Claude,
            SessionStatus::Stopped,
        );
        let stopped_codex = resumable_session(
            "stopped-codex",
            SessionMode::Interactive,
            SessionAgentType::Codex,
            SessionStatus::Stopped,
        );
        let running_claude = resumable_session(
            "running-claude",
            SessionMode::Interactive,
            SessionAgentType::Claude,
            SessionStatus::Running,
        );
        let boss_stopped = resumable_session(
            "boss-stopped",
            SessionMode::Boss,
            SessionAgentType::Claude,
            SessionStatus::Stopped,
        );
        let shell_stopped = resumable_session(
            "shell-stopped",
            SessionMode::Interactive,
            SessionAgentType::Shell,
            SessionStatus::Stopped,
        );

        let resumable_ids = [stopped_claude.id, stopped_codex.id];
        let all_ids = [
            stopped_claude.id,
            stopped_codex.id,
            running_claude.id,
            boss_stopped.id,
            shell_stopped.id,
        ];

        let mut ws = crate::models::Workspace::new("ws".to_string(), PathBuf::from("/tmp/ws"));
        ws.add_session(stopped_claude);
        ws.add_session(stopped_codex);
        ws.add_session(running_claude);
        ws.add_session(boss_stopped);
        ws.add_session(shell_stopped);

        let mut state = AppState::new();
        state.workspaces.push(ws);
        // Mark all five as multi-selected.
        for id in all_ids {
            state.selected_sessions.insert(id);
        }

        let mut got = state.selected_resumable_session_ids();
        got.sort();
        let mut want = resumable_ids.to_vec();
        want.sort();
        assert_eq!(got, want, "only stopped interactive agent sessions resume");
    }

    #[test]
    fn selected_resumable_session_ids_empty_when_nothing_selected() {
        let state = AppState::new();
        assert!(state.selected_resumable_session_ids().is_empty());
    }

    // ========================================================================
    // Startup discovery: the fast loader must hand off to a full refresh so
    // stopped sessions (which `load_workspaces_async` does not surface) appear
    // without the user having to manually refresh.
    // ========================================================================

    #[test]
    fn initial_background_load_enqueues_full_refresh_for_stopped_sessions() {
        use crate::app::state::WorkspaceLoadResult;

        let mut state = AppState::new();
        state.pending_async_action = None;

        // Simulate the startup background load completing.
        let tx = state.start_background_workspace_loading();
        tx.send(WorkspaceLoadResult::Success(Vec::new())).expect("send load result");

        let updated = state.check_workspace_loading_complete();

        assert!(updated, "applying the background result reports an update");
        assert_eq!(
            state.pending_async_action,
            Some(AsyncAction::RefreshWorkspaces),
            "fast startup load must enqueue a full refresh so stopped sessions surface"
        );
    }

    #[test]
    fn initial_background_load_does_not_clobber_a_queued_action() {
        use crate::app::state::WorkspaceLoadResult;

        let mut state = AppState::new();
        // A user-queued action is already pending when the load completes.
        state.pending_async_action = Some(AsyncAction::CleanupOrphaned);

        let tx = state.start_background_workspace_loading();
        tx.send(WorkspaceLoadResult::Success(Vec::new())).expect("send load result");
        state.check_workspace_loading_complete();

        assert_eq!(
            state.pending_async_action,
            Some(AsyncAction::CleanupOrphaned),
            "an already-queued action must not be overwritten by the refresh hand-off"
        );
    }

    // ========================================================================
    // Onboarding completion: State -> Config mapping
    // ========================================================================

    /// The take-effect seam for the first-run questionnaire: the
    /// source/role/use-case selections held in `OnboardingState` must land in
    /// the `OnboardingConfig` that `complete_onboarding` persists. This drives
    /// the mapping through the same `save_to`/`load_from` round-trip the real
    /// `save` uses, but against a tempdir so no real `~/.agents-in-a-box` is
    /// touched. Dropping any of the three assignments (e.g. `config.source =
    /// None`) at the seam makes the read-back assertions fail.
    #[test]
    fn complete_onboarding_persists_questionnaire_answers() {
        use crate::components::onboarding::OnboardingState;
        use crate::config::OnboardingConfig;

        // Build a finished wizard state with explicit, non-default selections
        // so a mapping that silently drops a field can't accidentally pass.
        let mut state = OnboardingState::new();
        state.selected_source_index = 2; // "Friend or colleague"
        state.selected_role_index = 3; // "Researcher"
        state.selected_use_case_index = 1; // "Automate repetitive tasks"

        // Sanity: confirm the indices resolve to the answers we expect, so the
        // assertions below pin real content rather than whatever index 0 is.
        assert_eq!(
            state.selected_source().as_deref(),
            Some("Friend or colleague")
        );
        assert_eq!(state.selected_role().as_deref(), Some("Researcher"));
        assert_eq!(
            state.selected_use_case().as_deref(),
            Some("Automate repetitive tasks")
        );

        // Map State -> Config via the exact seam `complete_onboarding` uses,
        // then persist + reload through an injected tempdir path.
        let config = AppState::onboarding_config_from_state(&state);

        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("config/onboarding.toml");
        config.save_to(&path).expect("save onboarding config to tempdir");

        let loaded = OnboardingConfig::load_from(&path).expect("reload onboarding config");
        assert!(loaded.completed, "onboarding must be marked completed");
        assert_eq!(
            loaded.source.as_deref(),
            Some("Friend or colleague"),
            "source selection must survive the State -> Config -> disk mapping"
        );
        assert_eq!(
            loaded.role.as_deref(),
            Some("Researcher"),
            "role selection must survive the State -> Config -> disk mapping"
        );
        assert_eq!(
            loaded.use_case.as_deref(),
            Some("Automate repetitive tasks"),
            "use_case selection must survive the State -> Config -> disk mapping"
        );
    }

    // ========================================================================
    // Bulk delete confirmation
    //
    // Pressing `d` (or Shift+D) with rows checked used to queue
    // BulkDeleteSessions immediately: no dialog, no Stop option, no
    // uncommitted-work warning, and every selected worktree was removed. These
    // tests pin the confirmation step in place.
    // ========================================================================

    struct RestoreAinbHome {
        previous: Option<String>,
        _guard: std::sync::MutexGuard<'static, ()>,
        _dir: tempfile::TempDir,
    }

    impl Drop for RestoreAinbHome {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(v) => std::env::set_var("AINB_HOME", v),
                None => std::env::remove_var("AINB_HOME"),
            }
        }
    }

    /// Take the crate-wide env lock and point `AINB_HOME` at a fresh tempdir
    /// until the returned guard drops.
    fn pin_ainb_home() -> RestoreAinbHome {
        let dir = tempfile::tempdir().expect("tempdir");
        RestoreAinbHome {
            _guard: crate::headroom::HEADROOM_ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            previous: {
                let previous = std::env::var("AINB_HOME").ok();
                std::env::set_var("AINB_HOME", dir.path());
                previous
            },
            _dir: dir,
        }
    }

    /// Point `AINB_HOME` at a tempdir for the duration of `body`.
    ///
    /// Opening the bulk dialog probes worktrees, which builds a
    /// `WorktreeManager` and so reads `AINB_HOME` and creates directories under
    /// it. Without this these tests would touch the developer's real
    /// `~/.agents-in-a-box`. The lock is the crate-wide env lock, not a private
    /// one, so this serialises against EVERY `AINB_HOME`-mutating test in the
    /// binary rather than just other callers here. The restore runs from `Drop`,
    /// so a failing assertion inside `body` cannot leave a deleted tempdir path
    /// in the env for every later test in the binary.
    fn with_ainb_home<R>(body: impl FnOnce() -> R) -> R {
        let _restore = pin_ainb_home();
        body()
    }

    /// `with_ainb_home` for an async body. `stop_interactive_session` falls back
    /// to `SessionStore::load()`, which reads `AINB_HOME`, so the bulk-stop tests
    /// need the same isolation the synchronous ones get.
    // The env guard is deliberately held across the await: that is what makes the
    // isolation cover the whole body. Test-only, single-threaded.
    #[allow(clippy::future_not_send)]
    async fn with_ainb_home_async<F, Fut, R>(body: F) -> R
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = R>,
    {
        let _restore = pin_ainb_home();
        body().await
    }

    /// Two checked, running interactive sessions, plus their ids in list order.
    fn state_with_checked_sessions(names: &[&str]) -> (AppState, Vec<uuid::Uuid>) {
        use crate::models::SessionStatus;

        let mut ws = crate::models::Workspace::new("ws".to_string(), PathBuf::from("/tmp/ws"));
        let mut ids = Vec::new();
        for name in names {
            let session = resumable_session(
                name,
                SessionMode::Interactive,
                SessionAgentType::Claude,
                SessionStatus::Running,
            );
            ids.push(session.id);
            ws.add_session(session);
        }

        let mut state = AppState::new();
        state.workspaces.push(ws);
        for id in &ids {
            state.selected_sessions.insert(*id);
        }
        (state, ids)
    }

    /// The regression guard: `d` with rows checked must ask first. If the
    /// confirmation is removed this goes red on `pending_async_action`.
    #[test]
    fn bulk_delete_asks_before_destroying_anything() {
        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["alpha", "beta"]);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            assert!(
                state.pending_async_action.is_none(),
                "bulk delete must not queue any action before the user confirms"
            );
            let dialog = state.confirmation_dialog.as_ref().expect("bulk confirmation dialog");
            let opts = dialog.options.as_ref().expect("tri-option dialog");
            assert_eq!(opts.len(), 3, "Stop all / Delete all / Cancel");
            assert_eq!(opts[0].label, "Stop all");
            assert_eq!(opts[1].label, "Delete all");
            assert_eq!(opts[2].label, "Cancel");
            assert_eq!(dialog.selected_index, 0, "Default = Stop all (safe option)");
            assert!(matches!(
                &opts[0].action,
                ConfirmAction::BulkStopSessions(got) if got == &ids
            ));
            assert!(matches!(
                &opts[1].action,
                ConfirmAction::BulkDeleteSessions(got) if got == &ids
            ));
            assert!(matches!(opts[2].action, ConfirmAction::Cancel));
            assert_eq!(
                state.selected_sessions.len(),
                2,
                "selection survives until the user picks an outcome"
            );
        });
    }

    /// Shift+D (the explicit bulk key) takes the same confirmed path as `d`.
    #[test]
    fn bulk_delete_selected_sessions_asks_before_destroying_anything() {
        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["alpha", "beta"]);

            EventHandler::process_event(AppEvent::DeleteSelectedSessions, &mut state);

            assert!(state.pending_async_action.is_none());
            let dialog = state.confirmation_dialog.as_ref().expect("bulk confirmation dialog");
            assert!(matches!(
                &dialog.options.as_ref().expect("tri-option dialog")[0].action,
                ConfirmAction::BulkStopSessions(got) if got == &ids
            ));
        });
    }

    /// The dialog names the sessions and says how many are affected.
    #[test]
    fn bulk_delete_dialog_names_the_affected_sessions() {
        with_ainb_home(|| {
            let (mut state, _ids) = state_with_checked_sessions(&["alpha", "beta"]);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            let dialog = state.confirmation_dialog.as_ref().expect("bulk confirmation dialog");
            assert_eq!(dialog.title, "Stop or Delete 2 Session(s)");
            assert!(
                dialog.message.contains("2 session(s): alpha, beta"),
                "{}",
                dialog.message
            );
            assert!(
                dialog.message.contains("Stop keeps every worktree"),
                "{}",
                dialog.message
            );
        });
    }

    /// Accepting the default (Stop all) must queue a stop, never a delete.
    #[test]
    fn bulk_dialog_default_option_queues_stop_not_delete() {
        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["alpha", "beta"]);
            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            EventHandler::process_event(AppEvent::ConfirmationConfirm, &mut state);

            assert!(matches!(
                state.pending_async_action,
                Some(AsyncAction::BulkStopSessions(ref got)) if got == &ids
            ));
            assert!(state.selected_sessions.is_empty(), "selection consumed");
            assert!(state.confirmation_dialog.is_none());
        });
    }

    /// Explicitly choosing Delete all still deletes, so the fix does not remove
    /// the capability, only the surprise.
    #[test]
    fn bulk_dialog_delete_option_queues_bulk_delete() {
        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["alpha", "beta"]);
            EventHandler::process_event(AppEvent::DeleteSession, &mut state);
            EventHandler::process_event(AppEvent::ConfirmationToggle, &mut state);

            EventHandler::process_event(AppEvent::ConfirmationConfirm, &mut state);

            assert!(matches!(
                state.pending_async_action,
                Some(AsyncAction::BulkDeleteSessions(ref got)) if got == &ids
            ));
        });
    }

    /// Cancel (either the option or Esc) leaves the selection and every session
    /// exactly as it was.
    #[test]
    fn bulk_dialog_cancel_does_nothing() {
        with_ainb_home(|| {
            let (mut state, _ids) = state_with_checked_sessions(&["alpha", "beta"]);
            EventHandler::process_event(AppEvent::DeleteSession, &mut state);
            EventHandler::process_event(AppEvent::ConfirmationPrev, &mut state); // wrap to Cancel

            EventHandler::process_event(AppEvent::ConfirmationConfirm, &mut state);

            assert!(
                state.pending_async_action.is_none(),
                "Cancel queues nothing"
            );
            assert_eq!(state.selected_sessions.len(), 2, "selection untouched");

            // Esc on a freshly-opened dialog is equally inert.
            EventHandler::process_event(AppEvent::DeleteSession, &mut state);
            EventHandler::process_event(AppEvent::ConfirmationCancel, &mut state);
            assert!(state.confirmation_dialog.is_none());
            assert!(state.pending_async_action.is_none());
            assert_eq!(state.selected_sessions.len(), 2);
        });
    }

    /// Long selections stay readable: three names, then a count.
    #[test]
    fn bulk_session_summary_truncates_long_selections() {
        let names: Vec<(uuid::Uuid, String)> =
            (1..=12).map(|i| (uuid::Uuid::new_v4(), format!("s{i}"))).collect();
        assert_eq!(
            AppState::format_bulk_session_summary(&names),
            "12 session(s): s1, s2, s3, and 9 more"
        );
        assert_eq!(
            AppState::format_bulk_session_summary(&[(uuid::Uuid::new_v4(), "only".to_string())]),
            "1 session(s): only"
        );
    }

    /// The uncommitted-work warning is the whole point of the dialog: it names
    /// the sessions whose work "Delete all" would destroy.
    #[test]
    fn bulk_uncommitted_warning_names_the_dirty_sessions() {
        assert_eq!(AppState::format_bulk_uncommitted_warning(&[], 0, 2), None);

        let dirty = vec![("alpha".to_string(), 3), ("beta".to_string(), 1)];
        let warning =
            AppState::format_bulk_uncommitted_warning(&dirty, 0, 2).expect("dirty sessions warn");
        assert!(warning.contains("4 uncommitted file(s)"), "{}", warning);
        assert!(warning.contains("2 session(s)"), "{}", warning);
        assert!(warning.contains("alpha (3)"), "{}", warning);
        assert!(warning.contains("beta (1)"), "{}", warning);

        let many: Vec<(String, usize)> = (1usize..=5).map(|i| (format!("s{i}"), i)).collect();
        let warning =
            AppState::format_bulk_uncommitted_warning(&many, 0, 5).expect("dirty sessions warn");
        assert!(warning.contains("and 2 more"), "{}", warning);
    }

    /// A worktree whose status cannot be read must never read as clean: the
    /// dialog says it could not be checked, so "no warning" keeps meaning
    /// "nothing to lose".
    #[test]
    fn bulk_uncommitted_warning_reports_what_could_not_be_checked() {
        let warning =
            AppState::format_bulk_uncommitted_warning(&[], 3, 3).expect("unchecked sessions warn");
        assert!(
            warning.contains("could not check 3 session(s)"),
            "{}",
            warning
        );

        let dirty = vec![("alpha".to_string(), 2)];
        let warning =
            AppState::format_bulk_uncommitted_warning(&dirty, 2, 3).expect("dirty sessions warn");
        assert!(warning.contains("alpha (2)"), "{}", warning);
        assert!(
            warning.contains("2 more could not be checked"),
            "{}",
            warning
        );
    }

    /// Stop is meaningless for a Boss (Docker) session: killing tmux leaves the
    /// container running. A selection of only such sessions must fall back to
    /// the binary delete confirmation, defaulting to No, rather than offering a
    /// Stop button that would lie about what happened.
    #[test]
    fn bulk_dialog_without_any_stop_path_falls_back_to_delete_confirmation() {
        use crate::models::SessionStatus;

        with_ainb_home(|| {
            let mut state = AppState::new();
            let mut ws = crate::models::Workspace::new("ws".to_string(), PathBuf::from("/tmp/ws"));
            for name in ["boss-a", "boss-b"] {
                let session = resumable_session(
                    name,
                    SessionMode::Boss,
                    SessionAgentType::Claude,
                    SessionStatus::Running,
                );
                state.selected_sessions.insert(session.id);
                ws.add_session(session);
            }
            state.workspaces.push(ws);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            assert!(state.pending_async_action.is_none(), "still asks first");
            let dialog = state.confirmation_dialog.as_ref().expect("confirmation dialog");
            assert!(dialog.options.is_none(), "no Stop option for Boss sessions");
            assert!(!dialog.selected_option, "Default = No");
            assert!(matches!(
                dialog.confirm_action,
                ConfirmAction::BulkDeleteSessions(_)
            ));
        });
    }

    /// One Boss row in the selection must not strip the safe option from the
    /// rows that can use it: Stop covers the stoppable subset, Delete still
    /// covers everything, and Stop stays the default.
    #[test]
    fn bulk_dialog_offers_stop_for_the_stoppable_subset_of_a_mixed_selection() {
        use crate::models::SessionStatus;

        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["alpha", "beta"]);
            let boss = resumable_session(
                "boss",
                SessionMode::Boss,
                SessionAgentType::Claude,
                SessionStatus::Running,
            );
            let boss_id = boss.id;
            state.workspaces[0].add_session(boss);
            state.selected_sessions.insert(boss_id);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            let dialog = state.confirmation_dialog.as_ref().expect("confirmation dialog");
            let opts = dialog.options.as_ref().expect("tri-option dialog");
            assert_eq!(dialog.selected_index, 0, "Stop is still the default");
            assert_eq!(opts[0].label, "Stop 2", "names how many Stop covers");
            assert!(
                matches!(&opts[0].action, ConfirmAction::BulkStopSessions(got) if got == &ids),
                "Stop covers only the two interactive sessions"
            );
            let ConfirmAction::BulkDeleteSessions(delete_ids) = &opts[1].action else {
                panic!("second option must delete");
            };
            assert_eq!(delete_ids.len(), 3, "Delete still covers everything");
            assert!(delete_ids.contains(&boss_id));
            assert!(
                dialog.message.contains("the other one cannot be stopped"),
                "a Boss row is excluded because it has no stop path, not because it \
                 is already stopped: {}",
                dialog.message
            );
        });
    }

    /// When the excluded rows are merely already stopped, the message must say
    /// so rather than claiming they cannot be stopped at all.
    #[test]
    fn bulk_dialog_names_already_stopped_rows_as_the_reason_they_are_excluded() {
        use crate::models::SessionStatus;

        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["running"]);
            let stopped = resumable_session(
                "stopped",
                SessionMode::Interactive,
                SessionAgentType::Claude,
                SessionStatus::Stopped,
            );
            state.selected_sessions.insert(stopped.id);
            state.workspaces[0].add_session(stopped);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            let dialog = state.confirmation_dialog.as_ref().expect("confirmation dialog");
            let opts = dialog.options.as_ref().expect("tri-option dialog");
            assert!(
                matches!(&opts[0].action, ConfirmAction::BulkStopSessions(got) if got == &ids),
                "Stop covers only the running session"
            );
            assert!(
                dialog.message.contains("the other one is already stopped"),
                "{}",
                dialog.message
            );
        });
    }

    /// A checked id that no longer resolves to a session still takes part in the
    /// bulk delete (nothing silently drops out), but the dialog shows a short
    /// label rather than a full 36-character uuid.
    #[test]
    fn bulk_dialog_labels_unresolvable_ids_without_dumping_a_uuid() {
        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["alpha"]);
            let stale = uuid::Uuid::new_v4();
            state.selected_sessions.insert(stale);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            let dialog = state.confirmation_dialog.as_ref().expect("confirmation dialog");
            assert!(dialog.message.contains("unknown ("), "{}", dialog.message);
            assert!(
                !dialog.message.contains(&stale.to_string()),
                "the full uuid would eat three rows of the dialog: {}",
                dialog.message
            );
            let opts = dialog.options.as_ref().expect("tri-option dialog");
            let ConfirmAction::BulkDeleteSessions(delete_ids) = &opts[1].action else {
                panic!("second option must delete");
            };
            assert!(delete_ids.contains(&stale), "stale ids stay in the delete");
            assert!(delete_ids.contains(&ids[0]));
        });
    }

    /// A bulk stop must not claim to have stopped sessions that were already
    /// stopped: the count is what the user reads to know the operation worked.
    #[tokio::test]
    async fn bulk_stop_reports_already_stopped_sessions_separately() {
        with_ainb_home_async(|| async {
            use crate::models::SessionStatus;

            // No tmux name on these, so the stop never shells out and the test
            // does not need a tmux binary.
            let mut ws = crate::models::Workspace::new("ws".to_string(), PathBuf::from("/tmp/ws"));
            let mut ids = Vec::new();
            for (name, status) in [
                ("running", SessionStatus::Running),
                ("already", SessionStatus::Stopped),
            ] {
                let mut session = resumable_session(
                    name,
                    SessionMode::Interactive,
                    SessionAgentType::Claude,
                    status,
                );
                session.tmux_session_name = None;
                ids.push(session.id);
                ws.add_session(session);
            }
            let mut state = AppState::new();
            state.workspaces.push(ws);

            state.bulk_stop_sessions(ids).await;

            let message = state
                .notifications
                .iter()
                .map(|n| n.message.clone())
                .collect::<Vec<_>>()
                .join(" | ");
            assert!(message.contains("Stopped 1 session(s)"), "{message}");
            assert!(message.contains("1 already stopped"), "{message}");
        })
        .await;
    }

    /// A selection that is already entirely stopped has nothing to stop, so
    /// offering "Stop all" as the default would make Enter a no-op that also
    /// throws the selection away.
    #[test]
    fn bulk_dialog_offers_no_stop_when_everything_is_already_stopped() {
        use crate::models::SessionStatus;

        with_ainb_home(|| {
            let mut state = AppState::new();
            let mut ws = crate::models::Workspace::new("ws".to_string(), PathBuf::from("/tmp/ws"));
            for name in ["a", "b"] {
                let session = resumable_session(
                    name,
                    SessionMode::Interactive,
                    SessionAgentType::Claude,
                    SessionStatus::Stopped,
                );
                state.selected_sessions.insert(session.id);
                ws.add_session(session);
            }
            state.workspaces.push(ws);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);

            let dialog = state.confirmation_dialog.as_ref().expect("confirmation dialog");
            assert!(dialog.options.is_none(), "nothing left to stop");
            assert!(!dialog.selected_option, "Default = No");
            assert!(
                dialog.message.contains("already stopped"),
                "the message must say why Stop is absent, and these sessions are \
                 resumable, so claiming they cannot be stopped and resumed would be \
                 the opposite of the truth: {}",
                dialog.message
            );
            assert!(
                !dialog.message.contains("None of these sessions can be stopped"),
                "{}",
                dialog.message
            );
        });
    }

    /// Stopping the stoppable subset must leave the rows it did not touch
    /// checked, or the user has to find and re-select them.
    #[test]
    fn bulk_stop_of_a_subset_keeps_the_untouched_rows_selected() {
        use crate::models::SessionStatus;

        with_ainb_home(|| {
            let (mut state, ids) = state_with_checked_sessions(&["alpha", "beta"]);
            let boss = resumable_session(
                "boss",
                SessionMode::Boss,
                SessionAgentType::Claude,
                SessionStatus::Running,
            );
            let boss_id = boss.id;
            state.workspaces[0].add_session(boss);
            state.selected_sessions.insert(boss_id);

            EventHandler::process_event(AppEvent::DeleteSession, &mut state);
            EventHandler::process_event(AppEvent::ConfirmationConfirm, &mut state);

            assert!(matches!(
                state.pending_async_action,
                Some(AsyncAction::BulkStopSessions(ref got)) if got == &ids
            ));
            assert_eq!(
                state.selected_sessions.iter().copied().collect::<Vec<_>>(),
                vec![boss_id],
                "the row Stop did not touch stays checked"
            );
        });
    }

    /// An empty selection has nothing to confirm, so it must not open a
    /// "Stop or Delete 0 Session(s)" dialog whose Delete deletes nothing.
    #[test]
    fn bulk_dialog_refuses_an_empty_selection() {
        with_ainb_home(|| {
            let mut state = AppState::new();
            state.show_bulk_delete_or_stop_confirmation(Vec::new());
            assert!(state.confirmation_dialog.is_none());
            assert!(state.pending_async_action.is_none());
        });
    }

    /// Two sessions pointing at the same tree must be probed once: reporting
    /// its four modified files twice would tell the user eight are at risk, and
    /// "Delete removes N worktree(s)" would count the tree twice too.
    #[test]
    fn bulk_worktree_status_counts_a_shared_tree_once() {
        use crate::models::SessionStatus;

        with_ainb_home(|| {
            if std::process::Command::new("git").arg("--version").status().is_err() {
                eprintln!("SKIP: git unavailable");
                return;
            }

            // One real repo with an uncommitted file, two sessions symlinked to it.
            let home = std::env::var("AINB_HOME").expect("pinned by with_ainb_home");
            let by_session = std::path::PathBuf::from(&home)
                .join(".agents-in-a-box")
                .join("worktrees")
                .join("by-session");
            std::fs::create_dir_all(&by_session).expect("by-session");
            let tree = std::path::PathBuf::from(&home).join("shared-tree");
            std::fs::create_dir_all(&tree).expect("tree");
            assert!(
                std::process::Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(&tree)
                    .status()
                    .expect("git init")
                    .success()
            );
            std::fs::write(tree.join("dirty.txt"), b"work").expect("write");

            let mut ws = crate::models::Workspace::new("ws".to_string(), tree.clone());
            let mut ids = Vec::new();
            for name in ["alpha", "beta"] {
                let session = resumable_session(
                    name,
                    SessionMode::Interactive,
                    SessionAgentType::Claude,
                    SessionStatus::Running,
                );
                std::os::unix::fs::symlink(&tree, by_session.join(session.id.to_string()))
                    .expect("symlink");
                ids.push(session.id);
                ws.add_session(session);
            }
            let mut state = AppState::new();
            state.workspaces.push(ws);

            let id_names: Vec<(uuid::Uuid, String)> = ids
                .iter()
                .zip(["alpha", "beta"])
                .map(|(id, name)| (*id, name.to_string()))
                .collect();
            let status = AppState::bulk_uncommitted_counts(&id_names);

            assert_eq!(status.with_worktree, 1, "one tree, not two");
            assert_eq!(status.dirty.len(), 1, "probed once");
            assert_eq!(status.dirty[0].1, 1, "one dirty file, not two");
            assert_eq!(
                status.dirty[0].0, "alpha, beta",
                "both sessions map to the dirty tree, so both are named"
            );
            assert_eq!(status.unchecked, 0);
        });
    }

    /// A Shell session's row is deletable like any other, and delete removes
    /// whatever its `by-session` symlink points at, so it must be counted and
    /// probed rather than assumed to own nothing.
    #[test]
    fn bulk_worktree_status_counts_a_shell_session_with_a_tree() {
        use crate::models::SessionStatus;

        with_ainb_home(|| {
            let home = std::env::var("AINB_HOME").expect("pinned by with_ainb_home");
            let by_session = std::path::PathBuf::from(&home)
                .join(".agents-in-a-box")
                .join("worktrees")
                .join("by-session");
            std::fs::create_dir_all(&by_session).expect("by-session");
            let tree = std::path::PathBuf::from(&home).join("shell-tree");
            std::fs::create_dir_all(&tree).expect("tree");

            let session = resumable_session(
                "shell",
                SessionMode::Interactive,
                SessionAgentType::Shell,
                SessionStatus::Running,
            );
            std::os::unix::fs::symlink(&tree, by_session.join(session.id.to_string()))
                .expect("symlink");
            let id = session.id;
            let mut ws = crate::models::Workspace::new("ws".to_string(), tree);
            ws.add_session(session);
            let mut state = AppState::new();
            state.workspaces.push(ws);

            let status = AppState::bulk_uncommitted_counts(&[(id, "shell".to_string())]);

            assert_eq!(
                status.with_worktree, 1,
                "delete would remove this directory"
            );
            assert_eq!(
                status.unchecked, 1,
                "not a git tree, so its contents are unknown, whether or not the \
                 temp directory happens to sit inside some other repository"
            );
            assert!(status.dirty.is_empty(), "no ancestor repository's files");
        });
    }

    /// A session directory that is not a checkout must never be answered for by
    /// an ancestor repository: `git status` walks up, so probing a plain folder
    /// nested inside a repo would report hundreds of files the session does not
    /// own, on the dialog whose entire job is to state what is at risk.
    #[test]
    fn a_plain_directory_inside_a_repo_is_unknown_not_dirty() {
        if std::process::Command::new("git").arg("--version").status().is_err() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let outer = tempfile::tempdir().expect("tempdir");
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(outer.path())
                .status()
                .expect("git init")
                .success()
        );
        std::fs::write(outer.path().join("dirty.txt"), b"work").expect("write");
        let nested = outer.path().join("not-a-checkout");
        std::fs::create_dir_all(&nested).expect("nested");

        let err = crate::git::WorktreeManager::uncommitted_file_count_at(&nested)
            .expect_err("a plain directory is not a checkout");
        assert!(format!("{err}").contains("no .git"), "{err}");
    }

    /// In a bulk selection the name is the point of the warning, even when only
    /// one of the selected sessions turns out to be dirty. Only a one-row dialog
    /// drops it, because there the name is the row the user is looking at.
    #[test]
    fn bulk_warning_names_the_dirty_session_even_when_only_one_is_dirty() {
        let dirty = vec![("beta".to_string(), 3)];

        let bulk = AppState::format_bulk_uncommitted_warning(&dirty, 0, 12)
            .expect("one dirty session in a bulk selection still warns");
        assert!(bulk.contains("beta (3)"), "{bulk}");

        let single = AppState::format_bulk_uncommitted_warning(&dirty, 0, 1)
            .expect("a single-row dialog still warns");
        assert_eq!(single, "⚠️ 3 uncommitted file(s) in worktree");
    }

    /// A single-row dialog must not hedge in the plural about "1 session(s)"
    /// when its own worktree could not be read.
    #[test]
    fn single_row_unchecked_warning_reads_singly() {
        let single = AppState::format_bulk_uncommitted_warning(&[], 1, 1)
            .expect("an unreadable worktree warns");
        assert_eq!(
            single,
            "⚠️ could not check this worktree for uncommitted work"
        );

        let bulk = AppState::format_bulk_uncommitted_warning(&[], 2, 6)
            .expect("unreadable worktrees warn");
        assert!(bulk.contains("could not check 2 session(s)"), "{bulk}");
    }

    /// Multi-select ids come out in list order, once each.
    #[test]
    fn selected_session_ids_in_order_dedups_and_follows_the_list() {
        let (state, ids) = state_with_checked_sessions(&["alpha", "beta", "gamma"]);
        assert_eq!(state.selected_session_ids_in_order(), ids);
    }

    /// End to end for the safe path: check the rows, press `d`, accept the
    /// default, and every worktree is still on disk afterwards.
    ///
    /// The keypress and the confirmation are driven through the real event
    /// handler, so re-introducing the original bug (queuing a bulk delete from
    /// `d`) fails this test at the action assertion before any directory is
    /// touched. Only the stop is executed; the delete action is never run,
    /// because running it in a test would remove real directories.
    #[tokio::test]
    async fn bulk_stop_preserves_every_worktree() {
        with_ainb_home_async(|| async {
            use crate::models::SessionStatus;

            // The stop path shells out to tmux; without the binary every stop
            // reports failure and the status assertions below are meaningless.
            if std::process::Command::new("tmux").arg("-V").status().is_err() {
                eprintln!("SKIP: tmux unavailable");
                return;
            }

            let tmp = tempfile::tempdir().expect("tempdir");
            let mut ws = crate::models::Workspace::new("ws".to_string(), tmp.path().to_path_buf());
            let mut ids = Vec::new();
            let mut worktrees = Vec::new();

            for name in ["alpha", "beta", "gamma"] {
                let worktree = tmp.path().join(name);
                std::fs::create_dir_all(&worktree).expect("create worktree");
                std::fs::write(worktree.join("uncommitted.txt"), b"work").expect("write file");

                let mut session = resumable_session(
                    name,
                    SessionMode::Interactive,
                    SessionAgentType::Claude,
                    SessionStatus::Running,
                );
                session.workspace_path = worktree.to_string_lossy().to_string();
                // A name that exists nowhere, so the real kill path runs and
                // reports "can't find session". Safe only because the kill
                // targets `=name`: a bare `-t` would prefix-match and could
                // reach a developer's live session.
                session.tmux_session_name = Some(format!("ainb-test-{}", session.id));
                ids.push(session.id);
                worktrees.push(worktree);
                ws.add_session(session);
            }

            let mut state = AppState::new();
            state.workspaces.push(ws);
            for id in &ids {
                state.selected_sessions.insert(*id);
            }

            // `d` with rows checked, then Enter on the default option.
            EventHandler::process_event(AppEvent::DeleteSession, &mut state);
            assert!(
                state.pending_async_action.is_none(),
                "the keypress must not queue anything before the user confirms"
            );
            EventHandler::process_event(AppEvent::ConfirmationConfirm, &mut state);

            let queued = state.pending_async_action.take();
            let stop_ids = match queued {
                Some(AsyncAction::BulkStopSessions(stop_ids)) => stop_ids,
                other => panic!("the default must be a stop, not {other:?}"),
            };
            assert_eq!(stop_ids, ids, "every checked session is stopped");

            state.bulk_stop_sessions(stop_ids).await;

            for worktree in &worktrees {
                assert!(worktree.is_dir(), "Stop must keep {}", worktree.display());
                assert!(
                    worktree.join("uncommitted.txt").exists(),
                    "Stop must keep the uncommitted work in {}",
                    worktree.display()
                );
            }
            for id in &ids {
                let session = state.find_session(*id).expect("session still registered");
                // The notification carries WHY when a stop failed. Without it
                // this assertion says only "not Stopped", which on a machine
                // whose tmux answers differently than the developer's is a
                // failure with nothing to act on.
                assert!(
                    matches!(session.status, SessionStatus::Stopped),
                    "session {id} is {:?}, not Stopped. Notifications: {:?}",
                    session.status,
                    state.notifications.iter().map(|n| &n.message).collect::<Vec<_>>()
                );
            }
        })
        .await;
    }
}

#[cfg(test)]
mod mcp_pool_config_screen_tests {
    use crate::app::state::{ConfigCategory, ConfigScreenState, ConfigValue};
    use crate::config::AppConfig;

    /// Edit a row by its dotted key, as the popup confirm does. The per-server
    /// `shared` rows now file under McpServers rather than McpPool, so the
    /// lookup has to span categories.
    fn set_bool(screen: &mut ConfigScreenState, key: &str, value: bool) {
        assert!(
            screen.settings.values().flatten().any(|s| s.key == key),
            "missing setting {key}"
        );
        screen.set_row_value(key, ConfigValue::Bool(value));
    }

    #[test]
    fn mcp_pool_settings_round_trip() {
        let mut config = AppConfig::default();
        config.mcp_servers = crate::config::McpServerConfig::defaults();
        config.mcp_pool.idle_grace_secs = 120;

        let mut screen = ConfigScreenState::from_app_config(&config);

        // Loaded values reflect config.
        let settings = screen.settings.get(&ConfigCategory::McpPool).unwrap();
        let grace = settings.iter().find(|s| s.key == "mcp_pool.idle_grace_secs").unwrap();
        assert_eq!(grace.value.display(), "120");
        // Per-server toggles exist for the built-in defaults.
        assert!(
            settings.iter().any(|s| s.key == "mcp_servers.context7.shared"),
            "expected per-server toggle, got: {:?}",
            settings.iter().map(|s| &s.key).collect::<Vec<_>>()
        );

        // Edit: disable pool + opt context7 out of sharing.
        set_bool(&mut screen, "mcp_pool.enabled", false);
        set_bool(&mut screen, "mcp_servers.context7.shared", false);
        screen.apply_to_app_config(&mut config).expect("edits apply");

        assert!(!config.mcp_pool.enabled);
        assert!(!config.mcp_servers["context7"].shared);
        assert!(
            config.mcp_servers["serena"].shared,
            "untouched server keeps default"
        );

        // Reopen → edited values shown.
        let reopened = ConfigScreenState::from_app_config(&config);
        let settings = reopened.settings.get(&ConfigCategory::McpPool).unwrap();
        let enabled = settings.iter().find(|s| s.key == "mcp_pool.enabled").unwrap();
        assert_eq!(enabled.value.display(), "✗ Disabled");
        let ctx = settings.iter().find(|s| s.key == "mcp_servers.context7.shared").unwrap();
        assert_eq!(ctx.value.display(), "✗ Disabled");
    }
}
