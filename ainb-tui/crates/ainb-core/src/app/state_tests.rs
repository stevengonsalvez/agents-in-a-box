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
        };

        let session = AppState::stopped_session_from_metadata(&metadata);
        assert!(
            !session.skip_permissions,
            "Some(false) must be preserved, not defaulted to yolo"
        );
        assert_eq!(session.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[tokio::test]
    async fn idle_restart_respawns_a_dead_codex_pane_with_launch_settings() {
        use crate::models::{Session, SessionStatus, Workspace};
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
                let _ = Command::new("tmux").args(["kill-session", "-t", &self.0]).output();
            }
        }

        let home = tempfile::tempdir().expect("temp home");

        let tmux_name = format!("ainb-idle-restart-{}", uuid::Uuid::new_v4());
        let _tmux = ExactTmuxSession(tmux_name.clone());
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

        let session_id = uuid::Uuid::new_v4();
        let worktree = home.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let model = "gpt-5.6-luna".to_string();

        let mut session = Session::new_with_options(
            "idle-restart".to_string(),
            worktree.to_string_lossy().to_string(),
            true,
            SessionMode::Interactive,
            None,
            SessionAgentType::Codex,
            Some(model),
        );
        session.id = session_id;
        session.status = SessionStatus::Idle;
        session.tmux_session_name = Some(tmux_name.clone());

        let mut workspace = Workspace::new("idle-restart".to_string(), worktree);
        workspace.add_session(session);
        let mut state = AppState::new();
        state.workspaces.push(workspace);

        state.restart_cli_in_tmux(session_id).await.expect("idle restart");

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
        assert!(
            pane_command.contains(
                "codex resume --last --model gpt-5.6-luna --dangerously-bypass-approvals-and-sandbox"
            ),
            "idle restart must replace dead pane with persisted Codex argv, got: {pane_command}"
        );
    }

    // -- SessionFilter tests ------------------------------------------------

    use crate::app::state::SessionFilter;
    use crate::models::{Session, SessionStatus as Status};

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
        assert_eq!(state.session_filter, SessionFilter::All);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::ActiveOnly);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::StoppedOnly);
        state.cycle_session_filter();
        assert_eq!(state.session_filter, SessionFilter::All);
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

    fn set_default_workspace(screen: &mut ConfigScreenState, value: &str) {
        let settings = screen
            .settings
            .get_mut(&ConfigCategory::Workspace)
            .expect("Workspace category present");
        let setting = settings
            .iter_mut()
            .find(|s| s.key == "default_workspace")
            .expect("default_workspace setting present");
        setting.value = ConfigValue::Text(value.to_string());
    }

    fn displayed_default_workspace(screen: &ConfigScreenState) -> String {
        screen
            .settings
            .get(&ConfigCategory::Workspace)
            .unwrap()
            .iter()
            .find(|s| s.key == "default_workspace")
            .unwrap()
            .value
            .display()
    }

    #[test]
    fn default_workspace_edit_replaces_primary_and_round_trips() {
        let mut config = AppConfig::default();
        config.workspace_defaults.workspace_scan_paths = vec![PathBuf::from("/a/git")];

        let mut screen = ConfigScreenState::from_app_config(&config);
        set_default_workspace(&mut screen, "/a/projects");
        screen.apply_to_app_config(&mut config);

        // Primary scan path is the edited value, not appended to the tail.
        assert_eq!(
            config.workspace_defaults.workspace_scan_paths.first(),
            Some(&PathBuf::from("/a/projects"))
        );
        // Reopening the screen now shows the saved value.
        let reopened = ConfigScreenState::from_app_config(&config);
        assert_eq!(displayed_default_workspace(&reopened), "/a/projects");
    }

    #[test]
    fn default_workspace_edit_preserves_other_scan_dirs() {
        let mut config = AppConfig::default();
        config.workspace_defaults.workspace_scan_paths =
            vec![PathBuf::from("/a/git"), PathBuf::from("/a/work")];

        let mut screen = ConfigScreenState::from_app_config(&config);
        set_default_workspace(&mut screen, "/a/projects");
        screen.apply_to_app_config(&mut config);

        // Old primary replaced; the secondary scan dir is kept.
        assert_eq!(
            config.workspace_defaults.workspace_scan_paths,
            vec![PathBuf::from("/a/projects"), PathBuf::from("/a/work")]
        );
    }

    #[test]
    fn default_workspace_noop_confirm_keeps_secondary_dirs() {
        // Opening the popup and confirming without changing the value must NOT
        // drop other configured scan dirs (regression: a de-dup that stripped
        // the unchanged primary then overwrote slot 0 lost the secondary).
        let mut config = AppConfig::default();
        config.workspace_defaults.workspace_scan_paths =
            vec![PathBuf::from("/a/git"), PathBuf::from("/a/work")];

        let mut screen = ConfigScreenState::from_app_config(&config);
        // first() is /a/git — re-confirm it unchanged.
        set_default_workspace(&mut screen, "/a/git");
        screen.apply_to_app_config(&mut config);

        assert_eq!(
            config.workspace_defaults.workspace_scan_paths,
            vec![PathBuf::from("/a/git"), PathBuf::from("/a/work")]
        );
    }

    #[test]
    fn default_workspace_edit_does_not_duplicate_existing_path() {
        let mut config = AppConfig::default();
        config.workspace_defaults.workspace_scan_paths =
            vec![PathBuf::from("/a/git"), PathBuf::from("/a/work")];

        let mut screen = ConfigScreenState::from_app_config(&config);
        // Promote an already-present path to primary.
        set_default_workspace(&mut screen, "/a/work");
        screen.apply_to_app_config(&mut config);

        assert_eq!(
            config.workspace_defaults.workspace_scan_paths.first(),
            Some(&PathBuf::from("/a/work"))
        );
        let dupes = config
            .workspace_defaults
            .workspace_scan_paths
            .iter()
            .filter(|p| *p == &PathBuf::from("/a/work"))
            .count();
        assert_eq!(dupes, 1, "edited path must not be duplicated");
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

        // The original enable/disable placeholder row is preserved.
        assert!(
            rows.iter().any(|s| s.key == "installed_plugins"),
            "existing enable/disable placeholder row must be kept"
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
        {
            let rows = screen.settings.get_mut(&ConfigCategory::Plugins).unwrap();
            let row = rows.iter_mut().find(|s| s.key == row_key).expect("row present");
            row.value = ConfigValue::Text("/tmp/edited-kb".to_string());
        }

        let mut app = AppConfig::default();
        screen.apply_to_app_config(&mut app);

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

    #[test]
    fn attention_permission_event_marks_needs_permission() {
        use ainb_plugin_notifyd::AlertKind;
        let recent = vec![rec("claude", CWD, "PermissionRequest", NOW - 1000)];
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &recent),
            Some(AlertKind::NeedsPermission),
        );
    }

    #[test]
    fn attention_notification_event_marks_waiting() {
        use ainb_plugin_notifyd::AlertKind;
        let recent = vec![rec("claude", CWD, "Notification:idle_prompt", NOW - 1000)];
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &recent),
            Some(AlertKind::WaitingOnUser),
        );
    }

    #[test]
    fn attention_fresh_stop_marks_finished_stale_stop_clears() {
        use ainb_plugin_notifyd::AlertKind;
        let fresh = vec![rec("claude", CWD, "Stop", NOW - 1000)];
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &fresh),
            Some(AlertKind::Finished),
        );
        // Older than the 5-minute Finished TTL → retired, no marker.
        let stale = vec![rec("claude", CWD, "Stop", NOW - 6 * 60 * 1000)];
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &stale),
            None,
        );
    }

    #[test]
    fn attention_suppressed_while_generating() {
        let recent = vec![rec("claude", CWD, "PermissionRequest", NOW - 1000)];
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), true, 0, NOW, &recent),
            None,
            "a generating session shows the busy dot, not an attention marker",
        );
    }

    #[test]
    fn attention_blank_without_a_matching_event() {
        // No events at all → blank (this is the common idle case the old
        // "[?] on everything" behaviour got wrong).
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &[]),
            None,
        );
        // Events exist, but for a different cwd or a different agent —
        // must not bleed across sessions.
        let other = vec![
            rec("codex", CWD, "Notification", NOW - 50),
            rec("claude", "/work/other", "Notification", NOW - 100),
        ];
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &other),
            None,
        );
    }

    #[test]
    fn attention_ignores_events_at_or_before_baseline() {
        use ainb_plugin_notifyd::AlertKind;
        let recent = vec![rec("claude", CWD, "Notification", 500)];
        // Baseline at/after the event (e.g. user just attached) → cleared.
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 500, NOW, &recent),
            None,
        );
        // Baseline just before the event → still marks.
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 499, NOW, &recent),
            Some(AlertKind::WaitingOnUser),
        );
    }

    #[test]
    fn attention_newest_event_wins() {
        use ainb_plugin_notifyd::AlertKind;
        // Question asked, then the turn finished — newest (Stop) supersedes
        // the older Notification.
        let recent = vec![
            rec("claude", CWD, "Stop", NOW - 1000),
            rec("claude", CWD, "Notification", NOW - 5000),
        ];
        assert_eq!(
            AppState::attention_for_session(CWD, Some("claude"), false, 0, NOW, &recent),
            Some(AlertKind::Finished),
        );
    }

    #[test]
    fn attention_none_agent_never_marks() {
        // Shell / SSH session (no hook agent) → never a marker, even with
        // a permission event sitting in the same cwd.
        let recent = vec![rec("claude", CWD, "PermissionRequest", NOW - 1000)];
        assert_eq!(
            AppState::attention_for_session(CWD, None, false, 0, NOW, &recent),
            None,
        );
    }

    #[test]
    fn attention_matches_cwd_ignoring_trailing_slash() {
        use ainb_plugin_notifyd::AlertKind;
        let recent = vec![rec("claude", "/work/feat-x/", "Notification", NOW - 100)];
        assert_eq!(
            AppState::attention_for_session("/work/feat-x", Some("claude"), false, 0, NOW, &recent),
            Some(AlertKind::WaitingOnUser),
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
}

#[cfg(test)]
mod mcp_pool_config_screen_tests {
    use crate::app::state::{ConfigCategory, ConfigScreenState, ConfigValue};
    use crate::config::AppConfig;

    fn set_bool(screen: &mut ConfigScreenState, key: &str, value: bool) {
        let setting = screen
            .settings
            .get_mut(&ConfigCategory::McpPool)
            .unwrap()
            .iter_mut()
            .find(|s| s.key == key)
            .unwrap_or_else(|| panic!("missing setting {key}"));
        setting.value = ConfigValue::Bool(value);
    }

    #[test]
    fn mcp_pool_settings_round_trip() {
        let mut config = AppConfig::default();
        config.mcp_servers = crate::config::McpServerConfig::defaults();
        config.mcp_pool.idle_grace_secs = 120;

        let mut screen = ConfigScreenState::from_app_config(&config);

        // Loaded values reflect config.
        let settings = screen.settings.get(&ConfigCategory::McpPool).unwrap();
        let grace = settings.iter().find(|s| s.key == "idle_grace_secs").unwrap();
        assert_eq!(grace.value.display(), "120");
        // Per-server toggles exist for the built-in defaults.
        assert!(
            settings.iter().any(|s| s.key == "shared.context7"),
            "expected per-server toggle, got: {:?}",
            settings.iter().map(|s| &s.key).collect::<Vec<_>>()
        );

        // Edit: disable pool + opt context7 out of sharing.
        set_bool(&mut screen, "pool_enabled", false);
        set_bool(&mut screen, "shared.context7", false);
        screen.apply_to_app_config(&mut config);

        assert!(!config.mcp_pool.enabled);
        assert!(!config.mcp_servers["context7"].shared);
        assert!(
            config.mcp_servers["serena"].shared,
            "untouched server keeps default"
        );

        // Reopen → edited values shown.
        let reopened = ConfigScreenState::from_app_config(&config);
        let settings = reopened.settings.get(&ConfigCategory::McpPool).unwrap();
        let enabled = settings.iter().find(|s| s.key == "pool_enabled").unwrap();
        assert_eq!(enabled.value.display(), "✗ Disabled");
        let ctx = settings.iter().find(|s| s.key == "shared.context7").unwrap();
        assert_eq!(ctx.value.display(), "✗ Disabled");
    }
}
