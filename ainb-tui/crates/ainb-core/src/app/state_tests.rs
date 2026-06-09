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

    /// Path encoding mirrors `find_transcript_path` in spawn-agent-lib.sh:30-69.
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
    }

    /// `find_latest_transcript_in` returns the most recently modified `.jsonl`
    /// under `<home>/.claude/projects/-{encoded}/`.
    #[test]
    fn test_find_latest_transcript_picks_newest_by_mtime() {
        use std::fs::{self, File};
        use std::path::PathBuf;
        use std::time::{Duration, SystemTime};

        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();

        let worktree = PathBuf::from("/tmp/work-abc-test");
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
        use std::path::PathBuf;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let fake_home = tmp.path().to_path_buf();
        let worktree = PathBuf::from("/tmp/empty-work");
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
    fn agent_hook_name_maps_claude_and_codex_only() {
        assert_eq!(
            AppState::agent_hook_name(SessionAgentType::Claude),
            Some("claude")
        );
        assert_eq!(
            AppState::agent_hook_name(SessionAgentType::Codex),
            Some("codex")
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
}
