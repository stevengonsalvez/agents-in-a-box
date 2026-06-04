// ABOUTME: Event handling system for keyboard input and app actions

#![allow(dead_code)]

use crate::app::{
    AppState,
    screens::ids as screen_ids,
    state::{AsyncAction, AuthMethod, ConfigPane},
};
use crate::cli::statusline_install::{InstallOutcome, StatuslineStatus, install_statusline};
use crate::credentials;
use crate::models::live_window::Source as LiveSource;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;
use tracing::info;

// Layout configuration - sessions pane width as percentage of terminal width
const SESSIONS_PANE_WIDTH_PERCENTAGE: f32 = 0.4;

#[derive(Debug, Clone)]
pub enum AppEvent {
    Quit,
    /// Plugin-scoped event — opaque rmp-serde payload destined for the plugin
    /// identified by `plugin_id`. Phase 2c added this variant; the
    /// `usage_event_bridge` module decodes legacy `Usage*` variants through
    /// it pre-Phase-3 (when burndown is extracted into a real plugin).
    Plugin {
        plugin_id: String,
        payload: Vec<u8>,
    },
    /// Navigate to a registered screen by id. Phase 2c added this variant to
    /// collapse the per-screen `GoTo*` variants behind one dispatch path —
    /// existing `GoTo*` variants are kept for now and translate through this
    /// at the layout layer.
    NavigateTo(String),
    GoToHomeScreen, // Return to home screen from any view
    NextSession,
    PreviousSession,
    NextWorkspace,
    PreviousWorkspace,
    ToggleHelp,
    RefreshWorkspaces,  // Manual refresh of workspace data
    CycleSessionFilter, // Cycle Interactive session filter (Shift+F): All → ActiveOnly → StoppedOnly
    ToggleClaudeChat,   // Toggle Claude chat visibility
    NewSession,         // Create session in current directory
    SearchWorkspace,    // Search all workspaces
    AttachSession,
    DetachSession,
    KillContainer,
    ReauthenticateCredentials,
    RestartSession,
    DeleteSession,
    ResumeSession(String), // Resume a Stopped interactive session (carries trigger key: "Enter" or "r")
    OpenInEditor,          // Open selected session's workspace in preferred editor
    OpenQuickShell,        // Open shell in selected workspace/session directory
    CleanupOrphaned,       // Clean up orphaned containers
    SwitchToLogs,
    SwitchToTerminal,
    GoToTop,
    GoToBottom,
    // Pane focus management
    SwitchPaneFocus,
    // Log scrolling events
    ScrollLogsUp,
    ScrollLogsDown,
    ScrollLogsToTop,
    ScrollLogsToBottom,
    ToggleAutoScroll, // Toggle auto-scroll mode in live logs
    // Mouse events
    MouseClick {
        x: u16,
        y: u16,
    },
    MouseDragStart {
        x: u16,
        y: u16,
    },
    MouseDragEnd {
        x: u16,
        y: u16,
    },
    MouseDragging {
        x: u16,
        y: u16,
    },
    MouseMove {
        x: u16,
        y: u16,
    },
    // New session creation events. Phase 6 (new-session redesign) retired
    // the legacy 13-step variants; only `NewSessionCancel` survives as the
    // host-level Esc handler for the `Creating` step.
    NewSessionCancel,
    // Notification events
    ShowNotification(String), // Display a notification message to the user
    // File finder events for @ symbol trigger
    FileFinderNavigateUp,
    FileFinderNavigateDown,
    FileFinderSelectFile,
    FileFinderCancel,
    // Search workspace events
    // Phase 6 (new-session redesign): SearchWorkspaceInputChar /
    // SearchWorkspaceBackspace retired — the SearchWorkspace screen no
    // longer hosts a text-filter input; PickRepo absorbed that role.
    // Confirmation dialog events
    ConfirmationToggle, // Switch between Yes/No (binary) or cycle forward (tri-option)
    ConfirmationPrev,   // Cycle backwards through tri-option dialog
    ConfirmationConfirm, // Confirm action
    ConfirmationCancel, // Cancel dialog
    // Auth setup events
    AuthSetupNext,            // Next auth method
    AuthSetupPrevious,        // Previous auth method
    AuthSetupSelect,          // Select current method
    AuthSetupCancel,          // Cancel auth setup (skip)
    AuthSetupInputChar(char), // Input character for API key
    AuthSetupBackspace,       // Backspace in API key input
    AuthSetupCheckStatus,     // Check authentication status
    AuthSetupRefresh,         // Manual refresh to check auth completion
    AuthSetupShowCommand,     // Show manual CLI command
    // Git view events
    ShowGitView,           // Show git view for selected session
    GitViewSwitchTab,      // Switch between Files and Diff tabs
    GitViewNextFile,       // Navigate to next file
    GitViewPrevFile,       // Navigate to previous file
    GitViewScrollUp,       // Scroll diff up
    GitViewScrollDown,     // Scroll diff down
    GitViewNextCommit,     // Navigate to next commit in commits tab
    GitViewPrevCommit,     // Navigate to previous commit in commits tab
    GitViewShowCommitDiff, // Show diff for selected commit (Enter on Commits tab)
    GitViewCommitPush,     // Commit and push changes
    GitViewBack,           // Return to session list
    GitCommitAndPush,      // Direct commit and push from main view (p key)
    // Quick commit dialog events (for home screen [p] key)
    QuickCommitStart,           // Start quick commit dialog
    QuickCommitInputChar(char), // Character input for quick commit
    QuickCommitBackspace,       // Backspace in quick commit
    QuickCommitCursorLeft,      // Move cursor left
    QuickCommitCursorRight,     // Move cursor right
    QuickCommitConfirm,         // Confirm quick commit (Enter)
    QuickCommitCancel,          // Cancel quick commit (Escape)
    // Commit message input events
    GitViewStartCommit,           // Start commit message input (p key)
    GitViewCommitInputChar(char), // Character input for commit message
    GitViewCommitBackspace,       // Backspace in commit message
    GitViewCommitCursorLeft,      // Move cursor left in commit message
    GitViewCommitCursorRight,     // Move cursor right in commit message
    GitViewCommitCancel,          // Cancel commit message input (Esc)
    GitViewCommitConfirm,         // Confirm and execute commit (Enter)
    GitCommitSuccess(String),     // Commit was successful with message
    // File tree navigation events
    GitViewToggleFolder, // Toggle folder expand/collapse
    GitViewExpandAll,    // Expand all folders
    GitViewCollapseAll,  // Collapse all folders
    // Tmux integration events
    AttachTmuxSession, // Attach to tmux session
    DetachTmuxSession, // Detach from tmux session
    EnterScrollMode,   // Enter scroll mode in tmux preview
    ExitScrollMode,    // Exit scroll mode in tmux preview
    ScrollPreviewUp,   // Scroll tmux preview up
    ScrollPreviewDown, // Scroll tmux preview down
    ToggleExpandAll,   // Toggle expand/collapse all workspaces
    // Other tmux rename events
    OtherTmuxStartRename, // Start rename mode for selected "Other tmux" session
    OtherTmuxRenameChar(char), // Character input for rename
    OtherTmuxRenameBackspace, // Backspace in rename
    OtherTmuxConfirmRename, // Confirm rename (Enter)
    OtherTmuxCancelRename, // Cancel rename (Escape)
    // SSH session rename events
    SshSessionStartRename,      // Start rename mode for selected SSH session
    SshSessionRenameChar(char), // Character input for SSH rename
    SshSessionRenameBackspace,  // Backspace in SSH rename
    SshSessionConfirmRename,    // Confirm SSH rename (Enter)
    SshSessionCancelRename,     // Cancel SSH rename (Escape)
    // AINB 2.0: Home screen events
    HomeScreenSelectTile,    // Select current tile (Enter)
    HomeScreenNavigateUp,    // Navigate up in tile grid
    HomeScreenNavigateDown,  // Navigate down in tile grid
    HomeScreenNavigateLeft,  // Navigate left in tile grid
    HomeScreenNavigateRight, // Navigate right in tile grid
    // AINB 2.0: Home screen V2 events (sidebar navigation)
    HomeScreenSidebarUp,     // Navigate up in sidebar
    HomeScreenSidebarDown,   // Navigate down in sidebar
    HomeScreenSidebarSelect, // Select current sidebar item (Enter)
    HomeScreenToggleFocus,   // Toggle focus between sidebar and content panel (Tab)
    StarSelectedWorkspace,   // Star/unstar the currently selected workspace
    // AINB 2.0: Home screen V2 welcome panel events
    WelcomePanelScrollUp,    // Scroll welcome panel up
    WelcomePanelScrollDown,  // Scroll welcome panel down
    WelcomePanelPageUp,      // Page up in welcome panel
    WelcomePanelPageDown,    // Page down in welcome panel
    WelcomePanelCopyContent, // Copy welcome panel content to clipboard (y)
    GoToAgentSelection,      // Navigate to agent selection view
    GoToCatalog,             // Navigate to catalog view (coming soon)
    GoToConfig,              // Navigate to config view
    GoToSessionList,         // Navigate to session list view
    GoToStats,               // Navigate to stats view
    GoToSkills,              // Navigate to skills view
    GoToSkillManager,        // Navigate to skill-manager view (spec §10.1)
    SkillManagerBack,        // Return to home screen from SkillManager (Esc/q)
    /// Discovery banner: import all detected units into the manifest
    /// (Enter on the §User Flow 1 banner).
    SkillManagerDiscoveryImport,
    /// Discovery banner: toggle the compact / expanded view.
    SkillManagerDiscoveryToggleDetails,
    /// Discovery banner: skip + persist marker so the banner does
    /// not re-show on subsequent opens.
    SkillManagerDiscoverySkip,
    /// Units panel: flip `shadowed_by` between the currently-selected
    /// unit and its conflict peer (spec §User Flow 3, hdt.8). No-op
    /// when the selected unit is not part of a conflict pair.
    SkillManagerConflictFlip,
    /// Units panel: run `ainb skill sync` for the selected unit
    /// (Phase D bidirectional content sync, bead v12.D.5). Routed
    /// when `[s]` is pressed and the selected unit is NOT part of a
    /// conflict pair — otherwise [`Self::SkillManagerConflictFlip`]
    /// fires instead.
    SkillManagerSync,
    /// Units panel: move selection up one row (k / Up arrow). Wraps
    /// to last row when at top. Recomputes detail pane on move.
    SkillManagerSelectPrev,
    /// Units panel: move selection down one row (j / Down arrow).
    /// Wraps to first row when at bottom. Recomputes detail pane.
    SkillManagerSelectNext,
    /// Units panel: jump selection to first row (g / Home).
    SkillManagerSelectFirst,
    /// Units panel: jump selection to last row (G / End).
    SkillManagerSelectLast,
    /// `[m]` on the SkillManager screen — re-run the discovery
    /// walkers and force the banner to re-appear (ignores any prior
    /// skip-marker). Fixes the empty-state "press [m] to refresh"
    /// hint that previously did nothing.
    SkillManagerRefreshDiscovery,
    /// `[c]` — re-trigger the background drift scan so the Units
    /// status column refreshes (✓ / ⚠ / ▲ / ⟷).
    SkillManagerCheck,
    /// `[u]` — update the selected unit: re-fetch its source, diff,
    /// apply. Runs the `ainb skill update <uri>` flow in-process and
    /// surfaces the result as a notification.
    SkillManagerUpdate,
    /// `[r]` — remove (uninstall) the selected unit from its target
    /// tools via the `ainb skill remove <uri>` flow.
    SkillManagerRemove,
    /// `[i]` — open the add-source input prompt (type a `gh:owner/repo`
    /// URI). On submit, runs `ainb source add` then re-discovers.
    SkillManagerOpenAddSource,
    /// `[/]` — open the search/filter input prompt.
    SkillManagerOpenSearch,
    /// A character typed while an input prompt is active.
    SkillManagerInputChar(char),
    /// Backspace in the active input prompt.
    SkillManagerInputBackspace,
    /// Enter — submit the active input prompt.
    SkillManagerInputSubmit,
    /// Esc — cancel the active input prompt.
    SkillManagerInputCancel,
    /// `[l]` — open the own-skill Library view, sourced from
    /// `library.yaml` (bead ai-lgk).
    SkillManagerOpenLibrary,
    /// Move the Library-view selection up one row.
    SkillManagerLibrarySelectPrev,
    /// Move the Library-view selection down one row.
    SkillManagerLibrarySelectNext,
    /// Enter — expand the selected Library row into its Detail band.
    SkillManagerLibraryEnter,
    /// Esc/q — close the Library view, returning to the Units screen.
    SkillManagerLibraryClose,
    /// `[b]` — open the catalog browse modal (bead ai-a20). Starts in
    /// Query mode; type a query then Enter to search via a
    /// `CatalogBackend` (mock under `AINB_CATALOG_MOCK=1`).
    SkillManagerOpenBrowse,
    /// A character typed into the browse query buffer (Query mode).
    SkillManagerBrowseInputChar(char),
    /// Backspace in the browse query buffer (Query mode).
    SkillManagerBrowseInputBackspace,
    /// Enter in Query mode — run the catalog search.
    SkillManagerBrowseSearch,
    /// Move the browse result selection up (Results mode).
    SkillManagerBrowseSelectPrev,
    /// Move the browse result selection down (Results mode).
    SkillManagerBrowseSelectNext,
    /// Enter on a selected result (Results mode) — install it through the
    /// existing install flow (add source + skill install).
    SkillManagerBrowseInstall,
    /// `/` in Results mode — return to Query mode to refine the search.
    SkillManagerBrowseEditQuery,
    /// Esc — close the browse modal, discarding the ephemeral results.
    SkillManagerBrowseClose,
    GoToRecovery,            // Navigate to session recovery view
    GoToInbox,               // Navigate to ainb-hooks notification inbox
    InboxMoveUp,             // Inbox: move selection up one row
    InboxMoveDown,           // Inbox: move selection down one row
    InboxPageUp,             // Inbox: jump 10 rows up
    InboxPageDown,           // Inbox: jump 10 rows down
    InboxOpenSelected,       // Inbox: mark selected row read (Enter)
    InboxDismissSelected,    // Inbox: dismiss selected row (d)
    InboxDismissVisible,     // Inbox: dismiss every visible row (Shift+C)
    InboxToggleArchived,     // Inbox: toggle dismissed filter (a)
    InboxCycleAgent,         // Inbox: cycle agent filter (p)
    InboxRefresh,            // Inbox: force-refresh from store (r)
    // AINB 2.0: Agent selection events
    AgentSelectionBack,         // Return to home screen (Esc)
    AgentSelectionNextProvider, // Navigate to next provider
    AgentSelectionPrevProvider, // Navigate to previous provider
    AgentSelectionNextModel,    // Navigate to next model
    AgentSelectionPrevModel,    // Navigate to previous model
    AgentSelectionToggleExpand, // Toggle provider expand
    AgentSelectionSelect,       // Select current agent (Enter)
    // AINB 2.0: Config screen events
    ConfigBack,            // Return to home screen (Esc)
    ConfigNextCategory,    // Navigate to next category
    ConfigPrevCategory,    // Navigate to previous category
    ConfigNextSetting,     // Navigate to next setting
    ConfigPrevSetting,     // Navigate to previous setting
    ConfigSwitchPane,      // Toggle focus between category and settings pane (Tab)
    ConfigNavigateUp,      // Navigate up within current focused pane
    ConfigNavigateDown,    // Navigate down within current focused pane
    ConfigFocusCategories, // Switch focus to categories pane (Left)
    ConfigFocusSettings,   // Switch focus to settings pane (Right)
    ConfigEditSetting,     // Start editing current setting (Enter)
    ConfigSaveEdit,        // Save current edit (Enter while editing)
    ConfigCancelEdit,      // Cancel current edit (Esc while editing)
    ConfigEditChar(char),  // Input character while editing
    ConfigEditBackspace,   // Backspace while editing
    ConfigSaveAll,         // Save all settings (S)
    // API Key configuration
    ConfigApiKeyStart,  // Start API key input mode (when on API Key Status)
    ConfigApiKeySave,   // Save the entered API key to keychain
    ConfigApiKeyDelete, // Delete stored API key
    // Auth provider popup
    AuthProviderPopupOpen,            // Open the auth provider popup
    AuthProviderPopupClose,           // Close the popup (Esc)
    AuthProviderPopupNext,            // Navigate to next provider
    AuthProviderPopupPrev,            // Navigate to previous provider
    AuthProviderPopupSelect,          // Select current provider (Enter)
    AuthProviderPopupInputChar(char), // Input character for API key
    AuthProviderPopupBackspace,       // Backspace in API key input
    AuthProviderPopupDeleteKey,       // Delete stored API key (D)
    // Config popup events (for choice/text input popups)
    ConfigPopupNavigateUp,      // Navigate up in choice list
    ConfigPopupNavigateDown,    // Navigate down in choice list
    ConfigPopupConfirm,         // Confirm selection/save text (Enter)
    ConfigPopupCancel,          // Cancel popup (Esc)
    ConfigPopupInputChar(char), // Input character in text/number input
    ConfigPopupBackspace,       // Backspace in text/number input
    // Log history viewer events
    LogHistoryBack,          // Return to home screen (Esc)
    LogHistoryNextSession,   // Navigate to next session
    LogHistoryPrevSession,   // Navigate to previous session
    LogHistorySelectSession, // Select/load session logs (Enter)
    LogHistoryToggleFocus,   // Toggle focus between sessions and logs (Tab)
    LogHistoryScrollUp,      // Scroll log entries up
    LogHistoryScrollDown,    // Scroll log entries down
    LogHistoryPageUp,        // Page up in log entries
    LogHistoryPageDown,      // Page down in log entries
    LogHistoryCycleFilter,   // Cycle through filter levels (f)
    LogHistoryRefresh,       // Refresh session list (r)
    LogHistoryCopySelection, // Copy selected text to clipboard (y or Ctrl+c)
    LogHistoryScrollLeft,    // Scroll log content left (←)
    LogHistoryScrollRight,   // Scroll log content right (→)
    LogHistoryScrollHome,    // Reset horizontal scroll to start (Home)
    LogHistoryCleanup,       // Delete all log files (C)
    // Onboarding wizard events
    OnboardingNext,            // Go to next step (Enter/Right Arrow)
    OnboardingBack,            // Go to previous step (Backspace/Left Arrow)
    OnboardingCancel,          // Cancel onboarding (Esc)
    OnboardingInputChar(char), // Input character for git directories
    OnboardingBackspace,       // Backspace in git directories input
    OnboardingDelete,          // Delete character in input
    OnboardingCursorLeft,      // Move cursor left in input
    OnboardingCursorRight,     // Move cursor right in input
    OnboardingCursorHome,      // Move cursor to start of input
    OnboardingCursorEnd,       // Move cursor to end of input
    OnboardingCheckDeps,       // Run dependency check
    OnboardingSkipAuth,        // Skip authentication step
    OnboardingEditorUp,        // Move editor selection up
    OnboardingEditorDown,      // Move editor selection down
    OnboardingFinish,          // Complete onboarding
    OnboardingInstallConfig,   // Install recommended config (I key)
    // Setup menu events
    SetupMenuBack,   // Return to home screen (Esc)
    SetupMenuSelect, // Select menu item (Enter)
    SetupMenuUp,     // Navigate up
    SetupMenuDown,   // Navigate down
    StartOnboarding, // Start onboarding wizard (from setup menu)
    FactoryReset,    // Factory reset AINB
    // Changelog viewer events
    ShowChangelog,       // Navigate to changelog view (v key)
    ChangelogBack,       // Return to home screen (Esc)
    ChangelogScrollUp,   // Scroll up one line
    ChangelogScrollDown, // Scroll down one line
    ChangelogPageUp,     // Page up
    ChangelogPageDown,   // Page down
    ChangelogToTop,      // Jump to top (g)
    ChangelogToBottom,   // Jump to bottom (G)
    // Usage analytics: variants removed. The burndown plugin owns these
    // events now; future host→plugin key forwarding flows through
    // AppEvent::Plugin{plugin_id="burndown", payload}.
    //
    // Exception: UsageWireStatusline stays in core. It's a host-side
    // helper that installs the Claude Code statusline (mutates
    // ~/.claude/settings.json) — it has nothing to do with the
    // analytics plugin and is reachable via the global `W` shortcut
    // and the slash command palette.
    UsageWireStatusline,
    // Skills browser events
    SkillsBack,             // Return to home screen (Esc)
    SkillsNextProvider,     // Next provider (Right arrow)
    SkillsPrevProvider,     // Previous provider (Left arrow)
    SkillsNextTab,          // Next sub-tab (Tab)
    SkillsPrevTab,          // Previous sub-tab (Shift+Tab)
    SkillsScrollUp,         // Move selection up (k/Up)
    SkillsScrollDown,       // Move selection down (j/Down)
    SkillsPageUp,           // Page up
    SkillsPageDown,         // Page down
    SkillsToTop,            // Jump to top (g)
    SkillsToBottom,         // Jump to bottom (G)
    SkillsRefresh,          // Reload data (r)
    SkillsSearchStart,      // Enter search mode (/)
    SkillsSearchChar(char), // Append char to search query
    SkillsSearchBackspace,  // Remove last char from search query
    SkillsSearchClose,      // Exit search mode (Esc)
    // Session recovery events
    SessionRecoveryBack,           // Return to home screen (Esc)
    SessionRecoveryNext,           // Navigate to next session (Down/j)
    SessionRecoveryPrev,           // Navigate to previous session (Up/k)
    SessionRecoveryResume,         // Resume selected session (r)
    SessionRecoveryArchive,        // Archive/delete selected item (d)
    SessionRecoveryRefresh,        // Refresh session list (R)
    SessionRecoveryToggleView,     // Toggle view mode: Sessions/Worktrees/All (Tab)
    SessionRecoveryRecoverAll,     // Recover all orphaned worktrees (Shift+A)
    SessionRecoveryToggleSelect,   // Toggle multi-select on current item (Space)
    SessionRecoveryDeleteSelected, // Delete all multi-selected items (Shift+D)
    ToggleSelectSession,           // Toggle multi-select on current session (Space)
    DeleteSelectedSessions,        // Bulk delete all multi-selected sessions (Shift+D)
    // Phase 5 (new-session redesign) Configure-screen events. Emitted by the
    // `configure::handle_key` outcome plumbing in `handle_new_session_keys`.
    /// Enter on Configure → record launch + start session. Carries the
    /// `LaunchSpec` already built by the Configure component so the
    /// dispatcher / async path doesn't have to re-derive the same fields
    /// (finding #7).
    ConfigureLaunch(crate::components::new_session::configure::LaunchSpec),
    ConfigureBack,      // Esc on Configure → return to PickRepo
    ConfigureOpenPresetManager, // ^P stub until Phase 7 polish
}

/// Translate a `RepoSource` variant into the `(SourceType, source_string)`
/// pair that `session-defaults.per_repo[].source_type/source` accepts
/// (finding #1). `None` for unparseable / non-clonable variants — the
/// picker's `recent_source` will fall back to favorites or `parse_with`.
fn source_provenance(
    source: &crate::git::repo_source::RepoSource,
) -> (Option<crate::config::favorites_store::SourceType>, Option<String>) {
    use crate::config::favorites_store::SourceType;
    use crate::git::repo_source::RepoSource;
    match source {
        RepoSource::LocalPath(p) => (
            Some(SourceType::LocalPath),
            Some(p.display().to_string()),
        ),
        RepoSource::HttpsUrl(u) => (Some(SourceType::HttpsUrl), Some(u.clone())),
        RepoSource::SshUrl(u) => (Some(SourceType::SshUrl), Some(u.clone())),
        RepoSource::GithubShorthand { owner, repo } => (
            Some(SourceType::GithubShorthand),
            Some(format!("{owner}/{repo}")),
        ),
        // SshSession and Filter have no clean SourceType mapping — leave
        // both columns blank so a future open falls back to favorites /
        // parse_with.
        RepoSource::SshSession(_) | RepoSource::Filter(_) => (None, None),
    }
}

/// Compute a stable display label for a `RepoSource` — drives the Configure
/// screen's title bar and the persistence key in `session-defaults.yaml`.
/// Phase 5 (new-session redesign).
fn derive_repo_label(source: &crate::git::repo_source::RepoSource) -> String {
    use crate::git::repo_source::RepoSource;
    match source {
        RepoSource::LocalPath(p) => p
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| p.display().to_string()),
        RepoSource::GithubShorthand { repo, .. } => repo.clone(),
        RepoSource::HttpsUrl(u) | RepoSource::SshUrl(u) => {
            // Pull the last path segment.
            u.rsplit('/').next().unwrap_or(u).trim_end_matches(".git").to_string()
        }
        RepoSource::SshSession(s) => {
            // `ssh://user@host` -> `host` for the title bar.
            let rest = s.strip_prefix("ssh://").unwrap_or(s);
            let host_part = rest.split('@').next_back().unwrap_or(rest);
            host_part.split('/').next().unwrap_or(host_part).to_string()
        }
        RepoSource::Filter(s) => s.clone(),
    }
}

/// Resolve the repo-picker's local candidate paths.
///
/// Prefers the `WorkspaceScanner` cache, filtered to directories that still
/// exist so a repo deleted or moved since the last scan cannot appear as a
/// selectable local-scan row. (Favorite and recent rows are built from
/// separate stores by `build_rows` and are not existence-checked here.)
/// Falls back to active-session workspace paths when no cache exists yet
/// (first run) or when every cached entry has been filtered out.
fn picker_local_paths(
    cache: Option<crate::git::RepositoryCache>,
    workspaces: &[crate::models::Workspace],
) -> Vec<std::path::PathBuf> {
    let cached_paths: Vec<std::path::PathBuf> = cache
        .map(|c| {
            c.repositories
                .into_iter()
                .filter(|r| r.path.is_dir())
                .map(|r| r.path)
                .collect()
        })
        .unwrap_or_default();
    // An empty filtered cache (no cache file yet, or every cached repo has
    // been deleted/moved) falls back to active-session workspaces rather than
    // leaving New Session with no local rows.
    if cached_paths.is_empty() {
        workspaces.iter().map(|w| w.path.clone()).collect()
    } else {
        cached_paths
    }
}

#[cfg(test)]
mod picker_local_paths_tests {
    use super::picker_local_paths;
    use crate::git::workspace_scanner::CachedRepository;
    use crate::git::RepositoryCache;
    use crate::models::Workspace;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn cache_with(paths: Vec<PathBuf>) -> RepositoryCache {
        RepositoryCache {
            version: 1,
            last_scan: chrono::Utc::now(),
            scan_paths: Vec::new(),
            scan_paths_mtime: HashMap::new(),
            repositories: paths
                .into_iter()
                .map(|p| CachedRepository {
                    name: p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("repo")
                        .to_string(),
                    path: p,
                })
                .collect(),
        }
    }

    #[test]
    fn keeps_only_cache_entries_whose_dir_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let real = dir.path().to_path_buf();
        let gone = real.join("gone");
        let out = picker_local_paths(Some(cache_with(vec![real.clone(), gone])), &[]);
        assert_eq!(out, vec![real]);
    }

    #[test]
    fn falls_back_to_workspace_paths_when_cache_absent() {
        let ws = vec![Workspace::new("w".to_string(), PathBuf::from("/ws/p"))];
        assert_eq!(picker_local_paths(None, &ws), vec![PathBuf::from("/ws/p")]);
    }

    #[test]
    fn falls_back_to_workspace_paths_when_cache_filters_to_empty() {
        // Cache present but every entry filtered out (here: empty) -> fall back.
        let ws = vec![Workspace::new("w".to_string(), PathBuf::from("/ws/p"))];
        assert_eq!(
            picker_local_paths(Some(cache_with(vec![])), &ws),
            vec![PathBuf::from("/ws/p")]
        );
    }
}

pub struct EventHandler;

impl EventHandler {
    fn persist_sessions_pane_preferences(state: &mut AppState) {
        state.app_config.ui_preferences.sessions_sidebar_width =
            Some(state.sessions_pane_state.preferred_width);
        state.app_config.ui_preferences.sessions_sidebar_collapsed =
            Some(state.sessions_pane_state.collapsed);
        if let Err(e) = state.app_config.save() {
            tracing::warn!("Failed to persist Sessions pane preferences: {}", e);
        }
    }

    /// Handle mouse events and convert to appropriate app events
    pub fn handle_mouse_event(event: AppEvent, state: &mut AppState) -> Option<AppEvent> {
        match event {
            AppEvent::MouseClick { x, y } => {
                if state.current_screen == screen_ids::HOME && !state.help_visible {
                    if state.home_screen_v2_state.begin_sidebar_resize(x, y) {
                        return None;
                    }

                    if let Some(outcome) =
                        state.home_screen_v2_state.click_sidebar_item_at(x, y, Instant::now())
                    {
                        if outcome.double_click {
                            return Some(AppEvent::HomeScreenSidebarSelect);
                        }
                    }

                    return None;
                }

                // Determine which pane was clicked based on terminal dimensions
                // The layout splits at 40% for sessions, 60% for logs
                let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0;
                let split_point = (term_width as f32 * SESSIONS_PANE_WIDTH_PERCENTAGE) as u16;

                // Check if we're in the main view (not in overlays)
                if state.current_screen == screen_ids::SESSION_LIST && !state.help_visible {
                    if state.sessions_pane_state.is_on_toggle(x, y) {
                        state.sessions_pane_state.toggle_collapsed();
                        Self::persist_sessions_pane_preferences(state);
                        return None;
                    }

                    if state.sessions_pane_state.begin_resize(x, y) {
                        return None;
                    }

                    if let Some(target) = state.session_list_row_at_mouse(x, y) {
                        let double_click =
                            state.sessions_pane_state.record_row_click(target, Instant::now());
                        state.select_session_list_row(target);
                        if double_click {
                            return Some(AppEvent::AttachTmuxSession);
                        }
                        return None;
                    }

                    if state.sessions_pane_state.contains_sessions_point(x, y) {
                        state.focused_pane = crate::app::state::FocusedPane::Sessions;
                        return None;
                    }

                    if state.sessions_pane_state.contains_preview_point(x, y) {
                        state.focused_pane = crate::app::state::FocusedPane::LiveLogs;
                        return None;
                    }

                    if x < split_point {
                        state.focused_pane = crate::app::state::FocusedPane::Sessions;
                    } else {
                        state.focused_pane = crate::app::state::FocusedPane::LiveLogs;
                    }
                    None
                } else {
                    None
                }
            }
            AppEvent::MouseDragStart { x: _, y: _ } => {
                // Start text selection in logs pane
                if state.focused_pane == crate::app::state::FocusedPane::LiveLogs {
                    // This will be handled in Phase 2
                    None
                } else {
                    None
                }
            }
            AppEvent::MouseDragging { x, y: _ } => {
                if state.current_screen == screen_ids::HOME && !state.help_visible {
                    let term_width = crossterm::terminal::size().unwrap_or((80, 24)).0;
                    state.home_screen_v2_state.drag_sidebar_resize(x, term_width);
                    return None;
                }

                if state.current_screen == screen_ids::SESSION_LIST && !state.help_visible {
                    let width = state
                        .sessions_pane_state
                        .last_content_width()
                        .unwrap_or_else(|| crossterm::terminal::size().unwrap_or((80, 24)).0);
                    state.sessions_pane_state.drag_resize(x, width);
                    return None;
                }

                // Update selection during drag
                if state.focused_pane == crate::app::state::FocusedPane::LiveLogs {
                    // This will be handled in Phase 2
                    None
                } else {
                    None
                }
            }
            AppEvent::MouseDragEnd { x, y } => {
                if state.current_screen == screen_ids::HOME && !state.help_visible {
                    state.home_screen_v2_state.update_sidebar_edge_hover(x, y);
                    if state.home_screen_v2_state.finish_sidebar_resize() {
                        let width = state.home_screen_v2_state.sidebar.preferred_width;
                        state.app_config.ui_preferences.home_sidebar_width = Some(width);
                        if let Err(e) = state.app_config.save() {
                            tracing::warn!("Failed to persist HomeScreen sidebar width: {}", e);
                        }
                    }
                    return None;
                }

                if state.current_screen == screen_ids::SESSION_LIST && !state.help_visible {
                    state.sessions_pane_state.update_hover(x, y);
                    if state.sessions_pane_state.finish_resize() {
                        Self::persist_sessions_pane_preferences(state);
                    }
                    return None;
                }

                // Finalize text selection
                if state.focused_pane == crate::app::state::FocusedPane::LiveLogs {
                    // This will be handled in Phase 2
                    None
                } else {
                    None
                }
            }
            AppEvent::MouseMove { x, y } => {
                if state.current_screen == screen_ids::HOME && !state.help_visible {
                    state.home_screen_v2_state.update_sidebar_edge_hover(x, y);
                }
                if state.current_screen == screen_ids::SESSION_LIST && !state.help_visible {
                    state.sessions_pane_state.update_hover(x, y);
                }
                None
            }
            _ => None,
        }
    }
    /// Get text from system clipboard
    fn get_clipboard_text() -> Result<String, Box<dyn std::error::Error>> {
        use arboard::Clipboard;
        let mut clipboard = Clipboard::new()?;
        let text = clipboard.get_text()?;
        Ok(text)
    }

    /// Dispatch a bracketed-paste event to the right New Session text-entry step.
    /// Returns `None` when the user isn't currently in a text-entry step that
    /// accepts paste, so the text is dropped silently rather than typed literally.
    ///
    /// Phase 6 (new-session redesign): the legacy 13-step flow is gone — the
    /// only text-entry surfaces are the smart-parse picker (PickRepo) and the
    /// Configure prompt textarea. Both own their own paste handling via the
    /// component-local `handle_key` arms, so paste events never need to be
    /// dispatched at the host event-router level.
    pub fn handle_paste_event(_text: String, _state: &AppState) -> Option<AppEvent> {
        None
    }

    /// True when the user is currently focused on any free-form text input.
    ///
    /// Single-character global shortcuts (`H`, `W`, future ones) must NOT
    /// fire while this is true — the keystroke belongs to the field, not
    /// the app. This is the single source of truth for "is the user
    /// typing right now"; every global character shortcut consults it so
    /// new shortcuts can't accidentally re-introduce the bug where
    /// pasting `SHOTClubhouse/SHOTid` becomes `SOTid` because `H`
    /// toggled the help overlay mid-paste.
    ///
    /// Includes:
    /// * Modal text-entry overlays (confirmation, OtherTmux/SSH rename,
    ///   onboarding, setup menu) — these already early-return higher up
    ///   in `handle_key_event`, but they're listed here so the answer to
    ///   "am I in a text input?" is correct even before those returns.
    /// * Quick-commit dialog (`quick_commit_message.is_some()`).
    /// * `View::NewSession` text-entry steps (`InputRepoSource`,
    ///   `InputBranch`, `InputPrompt`, `ConfigureSsh`). Non-text steps in
    ///   the same view (agent picker, branch list, etc.) do not count.
    /// * `View::SearchWorkspace`, `View::ClaudeChat`, `View::AuthSetup`,
    ///   `View::Config`, `View::AttachedTerminal` — views whose whole
    ///   purpose is text entry / pass-through.
    /// * The auth-provider popup, Analytics input/zoom-search,
    ///   Skills search overlay, and GitView commit-message mode —
    ///   text-entry overlays toggled inside otherwise navigable screens.
    /// Public wrapper so the main event loop can suppress globals like the
    /// slash-palette while the user is typing into a free-form input.
    pub fn is_in_text_input_context(state: &AppState) -> bool {
        Self::is_text_input_context(state)
    }

    fn is_text_input_context(state: &AppState) -> bool {
        use crate::app::screens::ids as screen_ids;
        use crate::app::state::NewSessionStep;

        // Modal text-entry overlays. These early-return higher up in
        // handle_key_event, but listing them keeps this helper a
        // complete predicate.
        if state.other_tmux_rename_mode
            || state.ssh_session_rename_mode
            || state.is_in_quick_commit_mode()
        {
            return true;
        }

        // NewSession (post-Phase-6) has only two text-entry steps —
        // PickRepo's smart-parse filter and Configure's Boss-mode prompt
        // textarea. Both accept colon-bearing input (URLs, prompts), so
        // global single-character shortcuts must be suppressed while they
        // own focus.
        let new_session_text_active = state.current_screen == screen_ids::NEW_SESSION
            && state
                .new_session_state
                .as_ref()
                .map(|s| {
                    matches!(s.step, NewSessionStep::PickRepo | NewSessionStep::Configure)
                })
                .unwrap_or(false);

        // Analytics is plugin-owned post-Phase 7; the host can't
        // introspect the burndown plugin's input modes (zoom search,
        // custom-period input). The plugin's own handle_key path
        // consumes character keystrokes before they reach this
        // helper, so we treat the analytics screen as always-non-text
        // here. If a plugin ever needs the host to suppress global
        // shortcuts while it's in text-entry mode, add a wire signal
        // (e.g. publish on `host.input_mode`) and read it here.
        let skills_text_active =
            state.current_screen == screen_ids::SKILLS && state.skills_state.search_active;
        // SkillManager add-source / search prompt — when its input
        // overlay is open the user is typing a URI or filter, which
        // routinely contains `:` (e.g. `gh:owner/repo`,
        // `git:file://…`). Without this, the global `:` slash-command
        // palette would open mid-URI and swallow the rest of the
        // keystrokes — exactly the bug that made `[i] add source`
        // appear broken.
        let skill_manager_input_active = state.current_screen == screen_ids::SKILL_MANAGER
            && (state.skill_manager_state.input.is_some()
                || state.skill_manager_state.browse.as_ref().is_some_and(|b| {
                    b.mode == crate::components::skill_manager_screen::BrowseMode::Query
                }));
        let git_view_text_active = state.current_screen == screen_ids::GIT_VIEW
            && state.git_view_state.as_ref().map(|gv| gv.is_in_commit_mode()).unwrap_or(false);

        // Config is multi-mode — only the states that accept free-form
        // character input count as text-entry. Plain navigation of
        // settings categories should NOT suppress global shortcuts
        // like `H`; that would be a UX regression. The modal popup
        // opened via `ConfigEditSetting` is included only for its
        // `TextInput` / `NumberInput` variants (via
        // `ConfigPopupState::is_text_entry`); `Choice` and `Boolean`
        // popups are navigation-only, so `H` is still allowed there.
        let config_text_active = state.current_screen == screen_ids::CONFIG
            && (state.config_screen_state.editing
                || state.config_screen_state.api_key_input_mode
                || state.config_popup_state.is_text_entry());

        new_session_text_active
            || matches!(
                state.current_screen.as_str(),
                screen_ids::SEARCH_WORKSPACE
                    | screen_ids::CLAUDE_CHAT
                    | screen_ids::AUTH_SETUP
                    | screen_ids::ATTACHED_TERMINAL
            )
            || config_text_active
            || state.auth_provider_popup_state.show_popup
            || skills_text_active
            || skill_manager_input_active
            || git_view_text_active
    }

    /// Pure decision logic shared between the production global-`W`
    /// shortcut and tests. Wiring is productive when live data isn't
    /// already flowing from the Tier1 cache *and* the user's
    /// `~/.claude/settings.json` doesn't already carry our block.
    fn should_wire_statusline_inner(
        live_source: LiveSource,
        statusline_status: Option<&StatuslineStatus>,
    ) -> bool {
        if live_source == LiveSource::Tier1Cache {
            return false;
        }
        matches!(
            statusline_status,
            Some(StatuslineStatus::NotConfigured) | Some(StatuslineStatus::Other(_))
        )
    }

    /// True when wiring the Claude Code statusline would be productive.
    /// Drives the global `W` shortcut. When this is `false` the keystroke
    /// is ignored at the global layer and falls through to the active
    /// view's normal handling.
    ///
    /// The settings.json read goes through [`AppState::statusline_status_cached`]
    /// so that holding `W` (or rapid keystrokes elsewhere) doesn't hammer
    /// the filesystem.
    fn should_wire_statusline(state: &mut AppState) -> bool {
        // Read from the background watcher's snapshot — never call
        // live_window::current() inline; the Tier 2 fallback walks JSONL
        // transcripts and would stall input handling on every keystroke.
        let live_source = state.live_window_watcher.snapshot().source;
        let status = state.statusline_status_cached();
        Self::should_wire_statusline_inner(live_source, status.as_ref())
    }

    pub fn handle_key_event(key_event: KeyEvent, state: &mut AppState) -> Option<AppEvent> {
        use crate::app::screens::ids as screen_ids;

        // Handle confirmation dialog first (highest priority)
        if let Some(ref dialog) = state.confirmation_dialog {
            // Tri-option dialogs cycle backwards on Left so users can navigate
            // both directions; binary dialogs keep the simple Toggle behaviour.
            let is_tri = dialog.options.is_some();
            match key_event.code {
                KeyCode::Left if is_tri => return Some(AppEvent::ConfirmationPrev),
                KeyCode::Right | KeyCode::Tab => {
                    return Some(AppEvent::ConfirmationToggle);
                }
                KeyCode::Left => {
                    return Some(AppEvent::ConfirmationToggle);
                }
                KeyCode::Enter => {
                    return Some(AppEvent::ConfirmationConfirm);
                }
                KeyCode::Esc => {
                    return Some(AppEvent::ConfirmationCancel);
                }
                _ => return None,
            }
        }

        // Handle "Other tmux" rename mode (high priority)
        if state.other_tmux_rename_mode {
            match key_event.code {
                KeyCode::Enter => return Some(AppEvent::OtherTmuxConfirmRename),
                KeyCode::Esc => return Some(AppEvent::OtherTmuxCancelRename),
                KeyCode::Backspace => return Some(AppEvent::OtherTmuxRenameBackspace),
                KeyCode::Char(c) => return Some(AppEvent::OtherTmuxRenameChar(c)),
                _ => return None,
            }
        }

        // Handle SSH session rename mode (high priority)
        if state.ssh_session_rename_mode {
            match key_event.code {
                KeyCode::Enter => return Some(AppEvent::SshSessionConfirmRename),
                KeyCode::Esc => return Some(AppEvent::SshSessionCancelRename),
                KeyCode::Backspace => return Some(AppEvent::SshSessionRenameBackspace),
                KeyCode::Char(c) => return Some(AppEvent::SshSessionRenameChar(c)),
                _ => return None,
            }
        }

        // Handle onboarding wizard view FIRST (before any other handlers)
        // Onboarding is a modal experience that should not be interrupted by global keybinds
        if state.current_screen == screen_ids::ONBOARDING {
            return Self::handle_onboarding_keys(key_event, state);
        }

        // Handle setup menu view (same priority as onboarding)
        if state.current_screen == screen_ids::SETUP_MENU {
            return Self::handle_setup_menu_keys(key_event, state);
        }

        // ------------------------------------------------------------
        // Single-character global shortcuts.
        //
        // Contract: a single `KeyCode::Char(_)` with no modifier MUST
        // NOT trigger any app-level action while the user is in a
        // text-input context. If you need a binding that fires inside
        // text inputs, use an explicit modifier (`Ctrl+`, `Alt+`,
        // function keys) — never a bare `KeyCode::Char`.
        //
        // Previously each global shortcut maintained its own suppress
        // list of text-input views (the `W` shortcut had one; the
        // `H`/`?` shortcut did not). That was easy to forget and caused
        // pasted text containing `H` to be partially swallowed because
        // `H` toggled the help overlay mid-paste (e.g. `SHOTClubhouse/SHOTid`
        // → `SOTid`). The single `in_text_input` predicate replaces all
        // those lists. It now gates *three* places: (a) the explicit
        // `?`/`H` and `W` globals immediately below, (b) the help-visible
        // swallow guard (so the field still consumes keys if help is
        // somehow open inside a text input), and (c) the SessionList
        // fallthrough match later in this function (defense-in-depth
        // for future text-input views that forget an early-return
        // handler).
        let in_text_input = Self::is_text_input_context(state);

        if state.help_visible {
            tracing::debug!("Help is visible, handling key: {:?}", key_event.code);
            if !in_text_input {
                match key_event.code {
                    KeyCode::Char('?' | 'H') | KeyCode::Esc => {
                        tracing::info!("Toggling help off via {:?}", key_event.code);
                        return Some(AppEvent::ToggleHelp);
                    }
                    _ => {
                        tracing::debug!("Ignoring key {:?} while help visible", key_event.code);
                        return None;
                    }
                }
            } else if matches!(key_event.code, KeyCode::Esc) {
                // Help is visible while the user is in a text input.
                // (Reachable via `HomeTile::Help` / `SidebarItem::Help`
                // followed by view navigation — `H`/`?` itself can no
                // longer toggle help inside a text input.) Treat Esc as
                // "close help" rather than letting it fall through to
                // the view's cancel handler, which would otherwise
                // close the form. Any printable key falls through so
                // the field still consumes it.
                tracing::info!("Closing help via Esc from text-input context");
                return Some(AppEvent::ToggleHelp);
            }
        }

        if !in_text_input {
            // Global help toggle: `?` or `Shift+H` from any non-text view.
            if matches!(key_event.code, KeyCode::Char('?' | 'H')) {
                return Some(AppEvent::ToggleHelp);
            }

            // Global `W`: wire Claude Code statusline. Active from any
            // non-text-input context when the statusline is unwired or
            // stale (live data isn't coming from the Tier1 cache). The CTA
            // in the top status bar points users here, so the shortcut
            // must work everywhere — not just from the Burndown panel
            // where it originally lived.
            //
            // The suppress list below covers two kinds of context:
            //   (a) views that fundamentally accept free-form character
            //       input (NewSession's prompt/branch/repo entry,
            //       SearchWorkspace, ClaudeChat, AuthSetup, the Config
            //       editor, the AttachedTerminal pass-through, the auth
            //       provider popup),
            //   (b) per-view text-entry overlays toggled inside otherwise
            //       navigable screens (GitView's commit message, the
            //       Skills search overlay).
            //
            // Modal text inputs that already early-return at the top of
            // `handle_key_event` (confirmation dialog, OtherTmux/SshSession
            // rename, onboarding/setup menus, quick-commit) don't reach
            // this block, so they don't need entries here.
            // Analytics is plugin-owned now; host can't introspect the
            // burndown plugin's input modes. The plugin must handle its
            // own W-suppression by intercepting key events before they
            // reach this global handler. Until plugin key forwarding is
            // wired (Phase 4+), `W` on the analytics screen does fire the
            // host install path — the plugin's input modes don't conflict
            // with it because they're modal and consume Escape/Enter, not
            // capital W.
            let analytics_text_active = false;
            let skills_text_active =
                state.current_screen == screen_ids::SKILLS && state.skills_state.search_active;
            let git_view_text_active = state.current_screen == screen_ids::GIT_VIEW
                && state.git_view_state.as_ref().map(|gv| gv.is_in_commit_mode()).unwrap_or(false);
            let suppress_global_w = matches!(
                state.current_screen.as_str(),
                screen_ids::NEW_SESSION
                    | screen_ids::SEARCH_WORKSPACE
                    | screen_ids::CLAUDE_CHAT
                    | screen_ids::AUTH_SETUP
                    | screen_ids::CONFIG
                    | screen_ids::ATTACHED_TERMINAL
            ) || state.auth_provider_popup_state.show_popup
                || analytics_text_active
                || skills_text_active
                || git_view_text_active;
            if !suppress_global_w
                && matches!(key_event.code, KeyCode::Char('W'))
                && Self::should_wire_statusline(state)
            {
                return Some(AppEvent::UsageWireStatusline);
            }
        }

        // AINB 2.0: Handle home screen view
        if state.current_screen == screen_ids::HOME {
            return Self::handle_home_screen_keys(key_event, state);
        }

        // ainb-hooks Inbox screen
        if state.current_screen == screen_ids::INBOX {
            return Self::handle_inbox_keys(key_event, state);
        }

        // AINB 2.0: Handle agent selection view
        if state.current_screen == screen_ids::AGENT_SELECTION {
            return Self::handle_agent_selection_keys(key_event, state);
        }

        // AINB 2.0: Handle auth provider popup (overlays config screen)
        if state.auth_provider_popup_state.show_popup {
            return Self::handle_auth_provider_popup_keys(key_event, state);
        }

        // AINB 2.0: Handle config screen view
        if state.current_screen == screen_ids::CONFIG {
            return Self::handle_config_screen_keys(key_event, state);
        }

        // Handle new session creation view
        if state.current_screen == screen_ids::NEW_SESSION {
            return Self::handle_new_session_keys(key_event, state);
        }

        // Handle search workspace view
        if state.current_screen == screen_ids::SEARCH_WORKSPACE {
            return Self::handle_search_workspace_keys(key_event, state);
        }

        // Handle non-git notification view
        if state.current_screen == screen_ids::NON_GIT_NOTIFICATION {
            return Self::handle_non_git_notification_keys(key_event, state);
        }

        // Handle Claude chat popup view
        if state.current_screen == screen_ids::CLAUDE_CHAT {
            return Self::handle_claude_chat_keys(key_event, state);
        }

        // Handle attached terminal view
        if state.current_screen == screen_ids::ATTACHED_TERMINAL {
            return Self::handle_attached_terminal_keys(key_event, state);
        }

        // Handle auth setup view
        if state.current_screen == screen_ids::AUTH_SETUP {
            return Self::handle_auth_setup_keys(key_event, state);
        }

        // Handle quick commit dialog input
        if state.is_in_quick_commit_mode() {
            return match key_event.code {
                KeyCode::Enter => Some(AppEvent::QuickCommitConfirm),
                KeyCode::Esc => Some(AppEvent::QuickCommitCancel),
                KeyCode::Backspace => Some(AppEvent::QuickCommitBackspace),
                KeyCode::Left => Some(AppEvent::QuickCommitCursorLeft),
                KeyCode::Right => Some(AppEvent::QuickCommitCursorRight),
                KeyCode::Char(ch) => Some(AppEvent::QuickCommitInputChar(ch)),
                _ => None,
            };
        }

        // Handle git view
        if state.current_screen == screen_ids::GIT_VIEW {
            tracing::debug!("In git view, handling git view keys");
            return Self::handle_git_view_keys(key_event, state);
        }

        // Handle log history view
        if state.current_screen == screen_ids::LOG_HISTORY {
            tracing::debug!("In log history view, handling log history keys");
            return Self::handle_log_history_keys(key_event, state);
        }

        // Handle changelog view
        if state.current_screen == screen_ids::CHANGELOG {
            tracing::debug!("In changelog view, handling changelog keys");
            return Self::handle_changelog_keys(key_event, state);
        }

        // Plugin-owned screens (Analytics → burndown today) forward
        // keystrokes down `plugin/handle_key` so the plugin's own UI
        // state (period chip, focused panel, zoom, filter stack) can
        // react. The forwarder returns `Handled` for non-reserved
        // keys; reserved keys (`Ctrl+C`, `?`, `H`) and screens with
        // no associated plugin fall through to the global handler
        // below. See `screens::builtin::forward_key_to_focused_plugin`
        // for the reservation list and the crossterm → wire
        // translation.
        if let crate::app::screens::EventOutcome::Handled =
            crate::app::screens::builtin::forward_key_to_focused_plugin(state, &key_event)
        {
            return None;
        }

        // Handle skills browser view
        if state.current_screen == screen_ids::SKILLS {
            tracing::debug!("In skills view, handling skills keys");
            return Self::handle_skills_keys(key_event, state);
        }

        // Handle skill-manager view (spec §10.1)
        if state.current_screen == screen_ids::SKILL_MANAGER {
            // Text-input prompt (add-source URI or search) takes
            // priority over every other key — while it's open the
            // user is typing, so chars must reach the buffer rather
            // than trigger shortcuts.
            if state.skill_manager_state.input.is_some() {
                return match key_event.code {
                    KeyCode::Enter => Some(AppEvent::SkillManagerInputSubmit),
                    KeyCode::Esc => Some(AppEvent::SkillManagerInputCancel),
                    KeyCode::Backspace => Some(AppEvent::SkillManagerInputBackspace),
                    KeyCode::Char(c) => Some(AppEvent::SkillManagerInputChar(c)),
                    _ => None,
                };
            }

            // Catalog browse overlay (`[b]`, bead ai-a20): two phases.
            //   * Query mode — every char goes into the query buffer
            //     (so `/`, `:`, spaces all reach it); Enter searches.
            //   * Results mode — arrows select; Enter installs the
            //     selected hit; `/` returns to Query mode to refine.
            // Esc closes from either mode. Intercepts before the banner
            // + normal keymap, just like the Library overlay.
            if let Some(browse) = &state.skill_manager_state.browse {
                use crate::components::skill_manager_screen::BrowseMode;
                return match browse.mode {
                    BrowseMode::Query => match key_event.code {
                        KeyCode::Enter => Some(AppEvent::SkillManagerBrowseSearch),
                        KeyCode::Esc => Some(AppEvent::SkillManagerBrowseClose),
                        KeyCode::Backspace => Some(AppEvent::SkillManagerBrowseInputBackspace),
                        KeyCode::Char(c) => Some(AppEvent::SkillManagerBrowseInputChar(c)),
                        _ => None,
                    },
                    BrowseMode::Results => match key_event.code {
                        KeyCode::Up | KeyCode::Char('k') => {
                            Some(AppEvent::SkillManagerBrowseSelectPrev)
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            Some(AppEvent::SkillManagerBrowseSelectNext)
                        }
                        KeyCode::Enter => Some(AppEvent::SkillManagerBrowseInstall),
                        KeyCode::Char('/') => Some(AppEvent::SkillManagerBrowseEditQuery),
                        KeyCode::Esc | KeyCode::Char('q') => {
                            Some(AppEvent::SkillManagerBrowseClose)
                        }
                        _ => None,
                    },
                };
            }

            // Own-skill Library overlay (`[l]`, bead ai-lgk): when
            // open, arrows / j-k move the selection, Enter expands the
            // selected row's Detail band, and Esc/q closes the overlay
            // (back to the Units screen — NOT home, so the user doesn't
            // lose the SkillManager context). Intercepts before the
            // banner + normal keymap.
            if state.skill_manager_state.library.is_some() {
                return match key_event.code {
                    KeyCode::Up | KeyCode::Char('k') => {
                        Some(AppEvent::SkillManagerLibrarySelectPrev)
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        Some(AppEvent::SkillManagerLibrarySelectNext)
                    }
                    KeyCode::Enter => Some(AppEvent::SkillManagerLibraryEnter),
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('l') => {
                        Some(AppEvent::SkillManagerLibraryClose)
                    }
                    _ => None,
                };
            }

            // Discovery banner (spec §User Flow 1 / P5): when the
            // overlay is visible, Enter/d/s drive its state machine
            // instead of the normal Skills shortcuts. Esc/q still
            // returns to Home so the user can always escape.
            if state.skill_manager_state.banner.is_active() {
                return match key_event.code {
                    KeyCode::Enter => Some(AppEvent::SkillManagerDiscoveryImport),
                    KeyCode::Char('d') => Some(AppEvent::SkillManagerDiscoveryToggleDetails),
                    KeyCode::Char('s') => Some(AppEvent::SkillManagerDiscoverySkip),
                    KeyCode::Esc | KeyCode::Char('q') => Some(AppEvent::SkillManagerBack),
                    _ => None,
                };
            }
            tracing::debug!("In skill-manager view, handling full keymap");
            return match key_event.code {
                KeyCode::Esc | KeyCode::Char('q') => Some(AppEvent::SkillManagerBack),
                // Units panel `[s]` — dual-purpose:
                //   * if the selected unit is part of a conflict pair,
                //     flip the shadowed_by edge (legacy behaviour);
                //   * otherwise, fire `SkillManagerSync` to run the
                //     Phase D bidirectional content sync on the
                //     selected unit (bead v12.D.5).
                // The banner branch above intercepts `s` first when
                // the discovery overlay is visible (skip-banner).
                KeyCode::Char('s') => {
                    let ainb_home = ainb_skill_core::default_ainb_home();
                    if selected_unit_has_conflict_peer(state, &ainb_home) {
                        Some(AppEvent::SkillManagerConflictFlip)
                    } else {
                        Some(AppEvent::SkillManagerSync)
                    }
                }
                // Help-bar shortcuts — now wired (were advertised but
                // dropped before this change):
                KeyCode::Char('i') => Some(AppEvent::SkillManagerOpenAddSource),
                KeyCode::Char('u') => Some(AppEvent::SkillManagerUpdate),
                KeyCode::Char('c') => Some(AppEvent::SkillManagerCheck),
                KeyCode::Char('r') => Some(AppEvent::SkillManagerRemove),
                KeyCode::Char('b') => Some(AppEvent::SkillManagerOpenBrowse),
                KeyCode::Char('l') => Some(AppEvent::SkillManagerOpenLibrary),
                KeyCode::Char('/') => Some(AppEvent::SkillManagerOpenSearch),
                // `[m]` re-runs discovery (the empty-state hint
                // finally tells the truth).
                KeyCode::Char('m') => Some(AppEvent::SkillManagerRefreshDiscovery),
                // Selection navigation — arrows + vim-style j/k +
                // Home/End/g/G. Wraps at list ends. Detail pane
                // recomputed on every move so the right-hand pane
                // mirrors the cursor without an extra keystroke.
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::SkillManagerSelectPrev),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::SkillManagerSelectNext),
                KeyCode::Home | KeyCode::Char('g') => Some(AppEvent::SkillManagerSelectFirst),
                KeyCode::End | KeyCode::Char('G') => Some(AppEvent::SkillManagerSelectLast),
                _ => None,
            };
        }

        // Handle session recovery view
        if state.current_screen == screen_ids::SESSION_RECOVERY {
            tracing::debug!("In session recovery view, handling session recovery keys");
            return Self::handle_session_recovery_keys(key_event, state);
        }

        // Handle key events based on focused pane (the SessionList view
        // reaches this block via fallthrough — it has no explicit early
        // return above). Defense-in-depth guard: every text-input view
        // listed in `is_text_input_context` already has its own
        // early-return handler higher up, so reaching here while
        // `in_text_input` is true would only happen if someone adds a
        // new text-input view to the predicate but forgets to wire a
        // handler. Short-circuit so the bare-char shortcuts below
        // (`c`, `n`, `a`, `q`, …) can't steal a keystroke from the field.
        if in_text_input {
            return None;
        }

        use crate::app::state::FocusedPane;

        match key_event.code {
            // Return to home screen (quit only available from HomeScreen)
            KeyCode::Char('q') | KeyCode::Esc => Some(AppEvent::GoToHomeScreen),
            KeyCode::Tab => {
                tracing::debug!(
                    "Tab key pressed, current focused_pane: {:?}",
                    state.focused_pane
                );
                Some(AppEvent::SwitchPaneFocus)
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(AppEvent::Quit)
            }
            KeyCode::Char('c') => Some(AppEvent::ToggleClaudeChat),
            KeyCode::Char('f') => Some(AppEvent::RefreshWorkspaces), // Manual refresh
            KeyCode::Char('F') => Some(AppEvent::CycleSessionFilter), // Cycle session filter (active/stopped/all)
            KeyCode::Char('n') => Some(AppEvent::NewSession),
            KeyCode::Char('s') => {
                // Star/unstar the selected workspace (only if a workspace is selected)
                if state.selected_workspace_index.is_some() {
                    Some(AppEvent::StarSelectedWorkspace)
                } else {
                    Some(AppEvent::ShowNotification(
                        "Select a workspace first to star it".to_string(),
                    ))
                }
            }
            KeyCode::Char('a') => {
                tracing::info!("[ACTION] 'a' key pressed - AttachTmuxSession requested");
                Some(AppEvent::AttachTmuxSession)
            }
            // The badge-to-position mapping is recomputed on every render —
            // digit N attaches to whatever is at that position *now*, not a
            // fixed session ID.
            KeyCode::Char(d)
                if matches!(d, '1'..='9')
                    && !key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && !key_event.modifiers.contains(KeyModifiers::ALT) =>
            {
                let n = (d as u8 - b'0') as usize;
                let items = state.attachable_items_in_order();
                if let Some(target) = items.get(n - 1).copied() {
                    tracing::info!(
                        "[ACTION] digit '{}' pressed - attach to position {} ({:?})",
                        d,
                        n,
                        target
                    );
                    state.select_attachable(target);
                    Some(AppEvent::AttachTmuxSession)
                } else {
                    Some(AppEvent::ShowNotification(format!(
                        "No session at position {}",
                        n
                    )))
                }
            }
            KeyCode::Enter => {
                // Enter on a Stopped interactive session = resume it.
                // Enter on a Running session = attach (mirrors 'a').
                // Other selection types fall through to None to preserve prior behaviour.
                use crate::models::{SessionAgentType, SessionMode, SessionStatus};
                if let Some(session) = state.selected_session() {
                    let is_interactive = matches!(session.mode, SessionMode::Interactive)
                        && matches!(
                            session.agent_type,
                            SessionAgentType::Claude
                                | SessionAgentType::Codex
                                | SessionAgentType::Gemini
                                | SessionAgentType::Copilot
                        );
                    if is_interactive && matches!(session.status, SessionStatus::Stopped) {
                        Some(AppEvent::ResumeSession("Enter".to_string()))
                    } else {
                        Some(AppEvent::AttachTmuxSession)
                    }
                } else {
                    None
                }
            }
            KeyCode::Char('r') => {
                // 'r' resumes a Stopped interactive session; otherwise it falls
                // back to the existing reauthenticate-credentials shortcut.
                use crate::models::{SessionAgentType, SessionMode, SessionStatus};
                if let Some(session) = state.selected_session() {
                    let is_interactive = matches!(session.mode, SessionMode::Interactive)
                        && matches!(
                            session.agent_type,
                            SessionAgentType::Claude
                                | SessionAgentType::Codex
                                | SessionAgentType::Gemini
                                | SessionAgentType::Copilot
                        );
                    if is_interactive && matches!(session.status, SessionStatus::Stopped) {
                        Some(AppEvent::ResumeSession("r".to_string()))
                    } else {
                        Some(AppEvent::ReauthenticateCredentials)
                    }
                } else {
                    Some(AppEvent::ReauthenticateCredentials)
                }
            }
            KeyCode::F(2) => {
                // F2 for rename - works in "SSH Sessions" and "Other tmux" sections
                if state.is_ssh_session_selected() {
                    Some(AppEvent::SshSessionStartRename)
                } else if state.is_other_tmux_selected() {
                    Some(AppEvent::OtherTmuxStartRename)
                } else {
                    Some(AppEvent::ShowNotification(
                        "F2 rename only works on SSH and 'Other tmux' sessions".to_string(),
                    ))
                }
            }
            KeyCode::Char('e') => Some(AppEvent::RestartSession),
            KeyCode::Char(' ') => Some(AppEvent::ToggleSelectSession),
            KeyCode::Char('D') => Some(AppEvent::DeleteSelectedSessions),
            KeyCode::Char('d') => Some(AppEvent::DeleteSession),
            KeyCode::Char('x') => Some(AppEvent::CleanupOrphaned),
            KeyCode::Char('g') => Some(AppEvent::ShowGitView), // Show git view
            KeyCode::Char('p') => Some(AppEvent::QuickCommitStart), // Start quick commit dialog
            KeyCode::Char('o') => Some(AppEvent::OpenInEditor), // Open in editor
            KeyCode::Char('E') => Some(AppEvent::ToggleExpandAll), // Toggle expand/collapse all workspaces
            KeyCode::Char('$') => Some(AppEvent::OpenQuickShell), // Quick shell in current workspace/session

            // Tmux preview scroll mode (Shift + Up/Down)
            KeyCode::Up if key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(AppEvent::ScrollPreviewUp)
            }
            KeyCode::Down if key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                Some(AppEvent::ScrollPreviewDown)
            }

            // Navigation keys depend on focused pane (arrow keys only)
            KeyCode::Down => {
                tracing::debug!("Down key pressed, focused_pane: {:?}", state.focused_pane);
                match state.focused_pane {
                    FocusedPane::Sessions => {
                        tracing::debug!("Sessions pane focused, triggering NextSession");
                        Some(AppEvent::NextSession)
                    }
                    FocusedPane::LiveLogs => {
                        tracing::debug!("LiveLogs pane focused, triggering ScrollLogsDown");
                        Some(AppEvent::ScrollLogsDown)
                    }
                }
            }
            KeyCode::Up => {
                tracing::debug!("Up key pressed, focused_pane: {:?}", state.focused_pane);
                match state.focused_pane {
                    FocusedPane::Sessions => {
                        tracing::debug!("Sessions pane focused, triggering PreviousSession");
                        Some(AppEvent::PreviousSession)
                    }
                    FocusedPane::LiveLogs => {
                        tracing::debug!("LiveLogs pane focused, triggering ScrollLogsUp");
                        Some(AppEvent::ScrollLogsUp)
                    }
                }
            }
            KeyCode::Left => {
                tracing::debug!("Left key pressed, focused_pane: {:?}", state.focused_pane);
                match state.focused_pane {
                    FocusedPane::Sessions => {
                        tracing::debug!("Sessions pane focused, triggering PreviousWorkspace");
                        Some(AppEvent::PreviousWorkspace)
                    }
                    FocusedPane::LiveLogs => {
                        tracing::debug!("LiveLogs pane focused, no left/right scrolling");
                        None // No left/right scrolling in logs
                    }
                }
            }
            KeyCode::Right => {
                tracing::debug!("Right key pressed, focused_pane: {:?}", state.focused_pane);
                match state.focused_pane {
                    FocusedPane::Sessions => {
                        tracing::debug!("Sessions pane focused, triggering NextWorkspace");
                        Some(AppEvent::NextWorkspace)
                    }
                    FocusedPane::LiveLogs => {
                        tracing::debug!("LiveLogs pane focused, no left/right scrolling");
                        None // No left/right scrolling in logs
                    }
                }
            }
            KeyCode::Home => match state.focused_pane {
                FocusedPane::Sessions => Some(AppEvent::GoToTop),
                FocusedPane::LiveLogs => Some(AppEvent::ScrollLogsToTop),
            },
            KeyCode::End => match state.focused_pane {
                FocusedPane::Sessions => Some(AppEvent::GoToBottom),
                FocusedPane::LiveLogs => Some(AppEvent::ScrollLogsToBottom),
            },
            KeyCode::Char(' ') => match state.focused_pane {
                FocusedPane::Sessions => None, // Space does nothing in sessions pane
                FocusedPane::LiveLogs => Some(AppEvent::ToggleAutoScroll),
            },
            _ => None,
        }
    }

    fn handle_search_workspace_keys(
        key_event: KeyEvent,
        _state: &mut AppState,
    ) -> Option<AppEvent> {
        // Phase 6 (new-session redesign): the search-workspace screen used to
        // host the legacy `SelectRepo` repo picker. The redesigned flow
        // routes that responsibility into PickRepo, so this handler now only
        // honors Esc to back out.
        match key_event.code {
            KeyCode::Esc => Some(AppEvent::NewSessionCancel),
            _ => None,
        }
    }

    fn handle_new_session_keys(key_event: KeyEvent, state: &mut AppState) -> Option<AppEvent> {
        use crate::app::state::NewSessionStep;
        use crate::components::new_session::configure::{self, ConfigureOutcome};
        use crate::components::new_session::pick_repo::{self, PickRepoOutcome};

        // Phase 5 (new-session redesign): Configure screen — own key handler.
        // Process BEFORE PickRepo so the step check stays linear.
        let on_configure = state
            .new_session_state
            .as_ref()
            .map(|s| s.step == NewSessionStep::Configure)
            .unwrap_or(false);
        if on_configure {
            let outcome = state
                .new_session_state
                .as_mut()
                .and_then(|s| s.configure_state.as_mut())
                .map(|cfg| configure::handle_key(cfg, key_event))
                .unwrap_or(ConfigureOutcome::Stay);

            return match outcome {
                ConfigureOutcome::Stay => None,
                ConfigureOutcome::BackToPickRepo => Some(AppEvent::ConfigureBack),
                ConfigureOutcome::Launch(spec) => Some(AppEvent::ConfigureLaunch(spec)),
                ConfigureOutcome::OpenPresetManager => {
                    Some(AppEvent::ConfigureOpenPresetManager)
                }
            };
        }

        // Phase 4 (new-session redesign): screen-1 has its own self-contained
        // key handler. Process it BEFORE the match below so we can take a
        // `&mut` borrow on `pick_repo_state` without fighting the immutable
        // borrow used by the legacy match arms.
        let on_pick_repo = state
            .new_session_state
            .as_ref()
            .map(|s| s.step == NewSessionStep::PickRepo)
            .unwrap_or(false);
        if on_pick_repo {
            let outcome = state
                .new_session_state
                .as_mut()
                .and_then(|s| s.pick_repo_state.as_mut())
                .map(|pick| pick_repo::handle_key(pick, key_event))
                .unwrap_or(PickRepoOutcome::Stay);

            return match outcome {
                PickRepoOutcome::Stay => None,
                PickRepoOutcome::BackToHome => {
                    // Persist session-defaults at the screen boundary
                    // (finding #3) so arrow/Esc no longer write on every
                    // keypress. Best-effort — non-fatal IO error.
                    use crate::config::session_defaults::SessionDefaults;
                    if let Some(pick) = state
                        .new_session_state
                        .as_ref()
                        .and_then(|ns| ns.pick_repo_state.as_ref())
                    {
                        let path = SessionDefaults::default_path();
                        if let Err(err) = pick.defaults.save_to(&path) {
                            tracing::warn!(error = %err, "PickRepo BackToHome: persist session-defaults failed");
                        }
                    }
                    // Return to whichever screen the user invoked `n` from
                    // (Sessions, Home, …). Stevie hit Esc-on-PickRepo
                    // dropping him on Home even when he opened it from
                    // Sessions (2026-05-22). Fall back to Home if no
                    // previous screen recorded.
                    state.new_session_state = None;
                    let prev = state
                        .previous_screen
                        .take()
                        .unwrap_or_else(|| crate::app::screens::ids::HOME.to_string());
                    state.current_screen = prev;
                    None
                }
                PickRepoOutcome::AdvanceTo(source)
                | PickRepoOutcome::StartClone(source) => {
                    // Phase 5: transition into Configure. StartClone for now
                    // skips the real async clone (Phase 6+ wires it) and
                    // advances straight in — the tripwires don't depend on
                    // network I/O. Build `configure_state` from `source` +
                    // session-defaults, set step = Configure.
                    //
                    // The dispatcher (not the UI layer) computes:
                    //   - the HEAD branch via `git::repo_source::head_branch`
                    //     (finding #9 — keeps git2 out of components/);
                    //   - the configured `branch_prefix` (finding #5);
                    //   - the existing-branch list for collision-disamb
                    //     (finding #16).
                    // PickRepo persistence (finding #3): write
                    // session-defaults ONCE here, not on every arrow keypress.
                    use crate::components::new_session::configure::ConfigureState;
                    use crate::config::session_defaults::SessionDefaults;
                    use crate::git::repo_source::head_branch;
                    use crate::git::worktree_manager::WorktreeManager;
                    if let Some(pick) = state
                        .new_session_state
                        .as_ref()
                        .and_then(|ns| ns.pick_repo_state.as_ref())
                    {
                        let path = SessionDefaults::default_path();
                        if let Err(err) = pick.defaults.save_to(&path) {
                            tracing::warn!(error = %err, "PickRepo advance: persist session-defaults failed");
                        }
                    }
                    let defaults =
                        SessionDefaults::load_from(&SessionDefaults::default_path());
                    let label = derive_repo_label(&source);
                    let branch_source = match &source {
                        crate::git::repo_source::RepoSource::LocalPath(p) => head_branch(p),
                        _ => None,
                    };
                    let branch_prefix = state
                        .app_config
                        .workspace_defaults
                        .branch_prefix
                        .clone();
                    // Use `list_all_worktrees` (scans by-session symlinks →
                    // real git branch via head.shorthand()), NOT
                    // `list_worktrees` which only finds legacy UUID-named
                    // top-level dirs and misses every modern by-name worktree.
                    // The latter returned empty → collision never detected
                    // (Stevie 2026-05-27: feat/blog re-launch slipped through).
                    let existing_branches: Vec<String> = WorktreeManager::new()
                        .ok()
                        .and_then(|m| m.list_all_worktrees().ok())
                        .map(|infos| {
                            infos.into_iter().map(|(_, i)| i.branch_name).collect()
                        })
                        .unwrap_or_default();
                    let cfg = ConfigureState::from_pick_repo(
                        source.clone(),
                        label,
                        &defaults,
                        branch_source,
                        &branch_prefix,
                        existing_branches,
                    );
                    if let Some(ns) = state.new_session_state.as_mut() {
                        ns.configure_state = Some(cfg);
                        ns.step = NewSessionStep::Configure;
                    }
                    tracing::debug!(?source, "PickRepo advance → Configure");
                    None
                }
            };
        }

        // Phase 6 (new-session redesign): the only steps remaining are
        // PickRepo (handled above), Configure (handled above), and Creating —
        // the in-flight state which only accepts Esc to cancel.
        if let Some(ref session_state) = state.new_session_state {
            match session_state.step {
                NewSessionStep::Configure => None, // handled above
                NewSessionStep::PickRepo => None,  // handled above
                NewSessionStep::Creating => match key_event.code {
                    KeyCode::Esc => Some(AppEvent::NewSessionCancel),
                    _ => None,
                },
            }
        } else {
            None
        }
    }

    fn handle_non_git_notification_keys(
        key_event: KeyEvent,
        _state: &mut AppState,
    ) -> Option<AppEvent> {
        match key_event.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(AppEvent::GoToHomeScreen),
            // 's' key removed - use 'n' to access local repo search via source selection
            _ => None,
        }
    }

    fn handle_attached_terminal_keys(
        key_event: KeyEvent,
        _state: &mut AppState,
    ) -> Option<AppEvent> {
        match key_event.code {
            KeyCode::Char('d') => Some(AppEvent::DetachSession),
            KeyCode::Char('q') | KeyCode::Esc => Some(AppEvent::DetachSession),
            KeyCode::Char('k') => Some(AppEvent::KillContainer),
            _ => None, // All other keys are passed through to the terminal
        }
    }

    fn handle_claude_chat_keys(key_event: KeyEvent, _state: &mut AppState) -> Option<AppEvent> {
        match key_event.code {
            // Escape closes the Claude chat popup
            KeyCode::Esc => Some(AppEvent::ToggleClaudeChat),
            // Enter sends the message
            KeyCode::Enter => {
                // TODO: Add send message event
                None
            }
            // Backspace for editing input
            KeyCode::Backspace => {
                // TODO: Add backspace handling
                None
            }
            // All other characters are input to the chat
            KeyCode::Char(_ch) => {
                // TODO: Add character input handling
                None
            }
            _ => None,
        }
    }

    fn handle_auth_setup_keys(key_event: KeyEvent, state: &mut AppState) -> Option<AppEvent> {
        if let Some(ref auth_state) = state.auth_setup_state {
            // If we're inputting API key, handle text input
            if auth_state.selected_method == AuthMethod::ApiKey
                && !auth_state.api_key_input.is_empty()
            {
                match key_event.code {
                    KeyCode::Enter => Some(AppEvent::AuthSetupSelect),
                    KeyCode::Backspace => Some(AppEvent::AuthSetupBackspace),
                    KeyCode::Esc => Some(AppEvent::AuthSetupBackspace), // Clear input
                    KeyCode::Char(ch) => Some(AppEvent::AuthSetupInputChar(ch)),
                    _ => None,
                }
            } else {
                // Method selection mode or waiting for auth completion
                match key_event.code {
                    KeyCode::Esc => Some(AppEvent::AuthSetupCancel),
                    KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::AuthSetupPrevious),
                    KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::AuthSetupNext),
                    KeyCode::Enter => Some(AppEvent::AuthSetupSelect),
                    KeyCode::Char('r') => Some(AppEvent::AuthSetupRefresh), // Manual refresh
                    KeyCode::Char('c') => Some(AppEvent::AuthSetupShowCommand), // Show CLI command
                    _ => None,
                }
            }
        } else {
            None
        }
    }

    fn handle_onboarding_keys(key_event: KeyEvent, state: &mut AppState) -> Option<AppEvent> {
        use crate::components::onboarding::OnboardingStep;

        if let Some(ref onboarding_state) = state.onboarding_state {
            // Different handling based on current step
            match onboarding_state.current_step {
                OnboardingStep::GitDirectories => {
                    // Text input mode for git directories
                    // Note: Left/Backspace used for text editing, use Up arrow to go back
                    match key_event.code {
                        KeyCode::Enter => Some(AppEvent::OnboardingNext),
                        KeyCode::Esc => Some(AppEvent::OnboardingCancel),
                        KeyCode::Up => Some(AppEvent::OnboardingBack), // Go back (since Left is cursor)
                        KeyCode::Backspace => Some(AppEvent::OnboardingBackspace),
                        KeyCode::Delete => Some(AppEvent::OnboardingDelete),
                        KeyCode::Left => Some(AppEvent::OnboardingCursorLeft),
                        KeyCode::Right => Some(AppEvent::OnboardingCursorRight),
                        KeyCode::Home => Some(AppEvent::OnboardingCursorHome),
                        KeyCode::End => Some(AppEvent::OnboardingCursorEnd),
                        KeyCode::Char(ch) => Some(AppEvent::OnboardingInputChar(ch)),
                        _ => None,
                    }
                }
                OnboardingStep::DependencyCheck => {
                    match key_event.code {
                        KeyCode::Enter => {
                            // If deps not checked yet, check them; otherwise advance
                            if onboarding_state.dependency_status.is_none() {
                                Some(AppEvent::OnboardingCheckDeps)
                            } else {
                                Some(AppEvent::OnboardingNext)
                            }
                        }
                        KeyCode::Esc => Some(AppEvent::OnboardingCancel),
                        KeyCode::Left | KeyCode::Backspace | KeyCode::Up => {
                            Some(AppEvent::OnboardingBack)
                        }
                        KeyCode::Char('r') => Some(AppEvent::OnboardingCheckDeps), // Re-check
                        KeyCode::Char('i') | KeyCode::Char('I') => {
                            Some(AppEvent::OnboardingInstallConfig)
                        } // Install config
                        _ => None,
                    }
                }
                OnboardingStep::Authentication => match key_event.code {
                    KeyCode::Enter => Some(AppEvent::OnboardingNext),
                    KeyCode::Esc => Some(AppEvent::OnboardingCancel),
                    KeyCode::Left | KeyCode::Backspace | KeyCode::Up => {
                        Some(AppEvent::OnboardingBack)
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') => Some(AppEvent::OnboardingSkipAuth),
                    _ => None,
                },
                OnboardingStep::EditorSelection => match key_event.code {
                    KeyCode::Enter => Some(AppEvent::OnboardingNext),
                    KeyCode::Esc => Some(AppEvent::OnboardingCancel),
                    KeyCode::Left | KeyCode::Backspace => Some(AppEvent::OnboardingBack),
                    KeyCode::Up => Some(AppEvent::OnboardingEditorUp),
                    KeyCode::Down => Some(AppEvent::OnboardingEditorDown),
                    KeyCode::Char('k') => Some(AppEvent::OnboardingEditorUp),
                    KeyCode::Char('j') => Some(AppEvent::OnboardingEditorDown),
                    _ => None,
                },
                OnboardingStep::Summary => match key_event.code {
                    KeyCode::Enter => Some(AppEvent::OnboardingFinish),
                    KeyCode::Esc => Some(AppEvent::OnboardingCancel),
                    KeyCode::Left | KeyCode::Backspace | KeyCode::Up => {
                        Some(AppEvent::OnboardingBack)
                    }
                    _ => None,
                },
                _ => {
                    // Welcome and other steps - basic navigation
                    match key_event.code {
                        KeyCode::Enter | KeyCode::Right => Some(AppEvent::OnboardingNext),
                        KeyCode::Esc => Some(AppEvent::OnboardingCancel),
                        KeyCode::Left | KeyCode::Backspace | KeyCode::Up => {
                            Some(AppEvent::OnboardingBack)
                        }
                        _ => None,
                    }
                }
            }
        } else {
            None
        }
    }

    fn handle_setup_menu_keys(key_event: KeyEvent, state: &mut AppState) -> Option<AppEvent> {
        // Handle confirmation dialog keys
        if state.setup_menu_state.showing_confirmation {
            match key_event.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    Some(AppEvent::SetupMenuSelect) // Confirm
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    Some(AppEvent::SetupMenuBack) // Cancel
                }
                _ => None,
            }
        } else {
            // Normal menu navigation
            match key_event.code {
                KeyCode::Esc => Some(AppEvent::SetupMenuBack),
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::SetupMenuUp),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::SetupMenuDown),
                KeyCode::Enter => Some(AppEvent::SetupMenuSelect),
                _ => None,
            }
        }
    }

    fn handle_git_view_keys(key_event: KeyEvent, state: &mut AppState) -> Option<AppEvent> {
        tracing::debug!("Git view key pressed: {:?}", key_event);

        // Check if we're in commit message input mode
        let in_commit_mode = if let Some(ref git_state) = state.git_view_state {
            git_state.is_in_commit_mode()
        } else {
            tracing::warn!("No git state available in handle_git_view_keys");
            false
        };

        if in_commit_mode {
            // Handle commit message input
            match key_event.code {
                KeyCode::Esc => Some(AppEvent::GitViewCommitCancel),
                KeyCode::Enter => Some(AppEvent::GitViewCommitConfirm),
                KeyCode::Backspace => Some(AppEvent::GitViewCommitBackspace),
                KeyCode::Left => Some(AppEvent::GitViewCommitCursorLeft),
                KeyCode::Right => Some(AppEvent::GitViewCommitCursorRight),
                KeyCode::Char(ch) => Some(AppEvent::GitViewCommitInputChar(ch)),
                _ => None,
            }
        } else {
            // Normal git view navigation
            match key_event.code {
                KeyCode::Esc => Some(AppEvent::GitViewBack),
                KeyCode::Tab => Some(AppEvent::GitViewSwitchTab),
                KeyCode::Char('j') | KeyCode::Down => {
                    if let Some(ref git_state) = state.git_view_state {
                        match git_state.active_tab {
                            crate::components::git_view::GitTab::Files => {
                                Some(AppEvent::GitViewNextFile)
                            }
                            crate::components::git_view::GitTab::Diff => {
                                Some(AppEvent::GitViewScrollDown)
                            }
                            crate::components::git_view::GitTab::Commits => {
                                Some(AppEvent::GitViewNextCommit)
                            }
                            crate::components::git_view::GitTab::Markdown => {
                                Some(AppEvent::GitViewScrollDown)
                            }
                        }
                    } else {
                        None
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    if let Some(ref git_state) = state.git_view_state {
                        match git_state.active_tab {
                            crate::components::git_view::GitTab::Files => {
                                Some(AppEvent::GitViewPrevFile)
                            }
                            crate::components::git_view::GitTab::Diff => {
                                Some(AppEvent::GitViewScrollUp)
                            }
                            crate::components::git_view::GitTab::Commits => {
                                Some(AppEvent::GitViewPrevCommit)
                            }
                            crate::components::git_view::GitTab::Markdown => {
                                Some(AppEvent::GitViewScrollUp)
                            }
                        }
                    } else {
                        None
                    }
                }
                KeyCode::Enter => {
                    // Toggle folder on Enter key in Files tab, show commit diff in Commits tab
                    if let Some(ref git_state) = state.git_view_state {
                        match git_state.active_tab {
                            crate::components::git_view::GitTab::Files => {
                                Some(AppEvent::GitViewToggleFolder)
                            }
                            crate::components::git_view::GitTab::Commits => {
                                Some(AppEvent::GitViewShowCommitDiff)
                            }
                            _ => None,
                        }
                    } else {
                        None
                    }
                }
                KeyCode::Char('e') => {
                    // Expand all folders
                    if let Some(ref git_state) = state.git_view_state {
                        if git_state.active_tab == crate::components::git_view::GitTab::Files {
                            Some(AppEvent::GitViewExpandAll)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                KeyCode::Char('E') => {
                    // Collapse all folders
                    if let Some(ref git_state) = state.git_view_state {
                        if git_state.active_tab == crate::components::git_view::GitTab::Files {
                            Some(AppEvent::GitViewCollapseAll)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                KeyCode::Char('p') => {
                    tracing::info!("Git view 'p' key pressed - starting commit");
                    Some(AppEvent::GitViewStartCommit)
                }
                _ => None,
            }
        }
    }

    /// Handle key events for the log history viewer
    fn handle_log_history_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        use crate::components::log_history_viewer::LogViewerFocus;

        tracing::debug!("Log history key handler: {:?}", key_event.code);

        // Global shortcuts
        match key_event.code {
            KeyCode::Esc => return Some(AppEvent::LogHistoryBack),
            KeyCode::Char('f') => return Some(AppEvent::LogHistoryCycleFilter),
            KeyCode::Char('r') => return Some(AppEvent::LogHistoryRefresh),
            KeyCode::Char('y') => return Some(AppEvent::LogHistoryCopySelection),
            KeyCode::Char('c')
                if key_event.modifiers.contains(crossterm::event::KeyModifiers::CONTROL) =>
            {
                return Some(AppEvent::LogHistoryCopySelection);
            }
            KeyCode::Char('c') | KeyCode::Char('C') => return Some(AppEvent::LogHistoryCleanup),
            KeyCode::Tab => return Some(AppEvent::LogHistoryToggleFocus),
            KeyCode::Home => return Some(AppEvent::LogHistoryScrollHome),
            _ => {}
        }

        // Focus-specific navigation
        match state.log_history_state.focus {
            LogViewerFocus::SessionList => match key_event.code {
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::LogHistoryPrevSession),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::LogHistoryNextSession),
                KeyCode::Enter => Some(AppEvent::LogHistorySelectSession),
                _ => None,
            },
            LogViewerFocus::LogEntries => match key_event.code {
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::LogHistoryScrollUp),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::LogHistoryScrollDown),
                KeyCode::PageUp => Some(AppEvent::LogHistoryPageUp),
                KeyCode::PageDown => Some(AppEvent::LogHistoryPageDown),
                KeyCode::Left | KeyCode::Char('h') => Some(AppEvent::LogHistoryScrollLeft),
                KeyCode::Right | KeyCode::Char('l') => Some(AppEvent::LogHistoryScrollRight),
                _ => None,
            },
        }
    }

    // Skills browser key handling
    fn handle_skills_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        tracing::debug!("Skills key handler: {:?}", key_event.code);

        // Search mode eats most keys: typing feeds the query, Esc exits.
        if state.skills_state.search_active {
            return match key_event.code {
                KeyCode::Esc => Some(AppEvent::SkillsSearchClose),
                KeyCode::Enter => Some(AppEvent::SkillsSearchClose),
                KeyCode::Backspace => Some(AppEvent::SkillsSearchBackspace),
                KeyCode::Char(c) => Some(AppEvent::SkillsSearchChar(c)),
                _ => None,
            };
        }

        match key_event.code {
            KeyCode::Esc => Some(AppEvent::SkillsBack),
            KeyCode::Right | KeyCode::Char('l') => Some(AppEvent::SkillsNextProvider),
            KeyCode::Left | KeyCode::Char('h') => Some(AppEvent::SkillsPrevProvider),
            KeyCode::Tab => Some(AppEvent::SkillsNextTab),
            KeyCode::BackTab => Some(AppEvent::SkillsPrevTab),
            KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::SkillsScrollUp),
            KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::SkillsScrollDown),
            KeyCode::PageUp => Some(AppEvent::SkillsPageUp),
            KeyCode::PageDown => Some(AppEvent::SkillsPageDown),
            KeyCode::Char('g') => Some(AppEvent::SkillsToTop),
            KeyCode::Char('G') => Some(AppEvent::SkillsToBottom),
            KeyCode::Char('r') => Some(AppEvent::SkillsRefresh),
            KeyCode::Char('/') => Some(AppEvent::SkillsSearchStart),
            _ => None,
        }
    }

    // Changelog viewer key handling
    fn handle_changelog_keys(key_event: KeyEvent, _state: &AppState) -> Option<AppEvent> {
        tracing::debug!("Changelog key handler: {:?}", key_event.code);

        match key_event.code {
            KeyCode::Esc => Some(AppEvent::ChangelogBack),
            KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::ChangelogScrollUp),
            KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::ChangelogScrollDown),
            KeyCode::PageUp => Some(AppEvent::ChangelogPageUp),
            KeyCode::PageDown => Some(AppEvent::ChangelogPageDown),
            KeyCode::Char('g') => Some(AppEvent::ChangelogToTop),
            KeyCode::Char('G') => Some(AppEvent::ChangelogToBottom),
            _ => None,
        }
    }

    // Session recovery key handling
    fn handle_session_recovery_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        tracing::debug!("Session recovery key handler: {:?}", key_event.code);

        // If overlay is showing, Esc dismisses it; all other keys ignored
        if state.session_recovery_state.recovery_overlay.is_some() {
            return match key_event.code {
                KeyCode::Esc | KeyCode::Enter => Some(AppEvent::SessionRecoveryBack), // reused to dismiss
                _ => None,
            };
        }

        match key_event.code {
            KeyCode::Esc => Some(AppEvent::SessionRecoveryBack),
            KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::SessionRecoveryPrev),
            KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::SessionRecoveryNext),
            KeyCode::Char('r') => Some(AppEvent::SessionRecoveryResume),
            KeyCode::Char('d') => Some(AppEvent::SessionRecoveryArchive),
            KeyCode::Char('R') => Some(AppEvent::SessionRecoveryRefresh),
            KeyCode::Tab => Some(AppEvent::SessionRecoveryToggleView),
            KeyCode::Char('A') => Some(AppEvent::SessionRecoveryRecoverAll),
            KeyCode::Char(' ') => Some(AppEvent::SessionRecoveryToggleSelect),
            KeyCode::Char('D') => Some(AppEvent::SessionRecoveryDeleteSelected),
            _ => None,
        }
    }

    /// Inbox screen key dispatcher. Keys follow the spec:
    ///
    ///   - ↑/↓ k/j         move selection
    ///   - PageUp/PageDown jump 10 rows
    ///   - Enter           open + mark read
    ///   - d               dismiss selected
    ///   - C               dismiss every visible row (Shift+C)
    ///   - a               toggle archived filter
    ///   - p               cycle agent filter
    ///   - r               refresh
    ///   - q / Esc         back to previous screen (home if none)
    fn handle_inbox_keys(key_event: KeyEvent, _state: &mut AppState) -> Option<AppEvent> {
        match key_event.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(AppEvent::GoToHomeScreen),
            KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::InboxMoveUp),
            KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::InboxMoveDown),
            KeyCode::PageUp => Some(AppEvent::InboxPageUp),
            KeyCode::PageDown => Some(AppEvent::InboxPageDown),
            KeyCode::Enter => Some(AppEvent::InboxOpenSelected),
            KeyCode::Char('d') => Some(AppEvent::InboxDismissSelected),
            KeyCode::Char('C') => Some(AppEvent::InboxDismissVisible),
            KeyCode::Char('a') => Some(AppEvent::InboxToggleArchived),
            KeyCode::Char('p') => Some(AppEvent::InboxCycleAgent),
            KeyCode::Char('r') => Some(AppEvent::InboxRefresh),
            _ => None,
        }
    }

    // AINB 2.0: Home screen key handling (V2 with sidebar and card grid)
    fn handle_home_screen_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        use crate::components::home_screen_v2::HomeScreenFocus;

        tracing::debug!("HomeScreen V2 key handler: {:?}", key_event.code);

        // Global shortcuts that work regardless of focus (matches HomeTile shortcuts)
        // Inbox shortcut FIRST so the Shift+i path beats the plain
        // 'i' arm (GoToStats) on terminals where crossterm delivers
        // shifted letters as KeyCode::Char('i') + SHIFT modifier
        // instead of KeyCode::Char('I'). The Linux tmux runner on
        // GitHub Actions hits the modifier path; macOS hits the
        // uppercase code-point path. Both must reach the Inbox.
        if let KeyCode::Char(c) = key_event.code {
            let shift_pressed = key_event.modifiers.contains(KeyModifiers::SHIFT);
            if c == 'I' || (c == 'i' && shift_pressed) {
                return Some(AppEvent::GoToInbox);
            }
        }
        match key_event.code {
            KeyCode::Char('a') => return Some(AppEvent::GoToAgentSelection),
            KeyCode::Char('c') => return Some(AppEvent::GoToCatalog),
            KeyCode::Char('C') => return Some(AppEvent::GoToConfig),
            KeyCode::Char('s') => return Some(AppEvent::GoToSessionList),
            KeyCode::Char('i') => return Some(AppEvent::GoToStats),
            KeyCode::Char('k') => return Some(AppEvent::GoToSkills),
            KeyCode::Char('m') => return Some(AppEvent::GoToSkillManager),
            KeyCode::Char('R') => return Some(AppEvent::GoToRecovery),
            KeyCode::Char('v') => return Some(AppEvent::ShowChangelog),
            KeyCode::Char('?') => return Some(AppEvent::ToggleHelp),
            KeyCode::Char('q') => return Some(AppEvent::Quit),
            // Phase 4 (new-session redesign): `n` opens the unified picker
            // directly from home. The spec's 90% flow is `n -> Enter` (2
            // keystrokes) — previously users had to land on session-list
            // first. See plans/new-session-redesign-spec.md flow 1.
            KeyCode::Char('n') => return Some(AppEvent::NewSession),
            _ => {}
        }

        // Tab to toggle focus between sidebar and content panel
        if key_event.code == KeyCode::Tab {
            return Some(AppEvent::HomeScreenToggleFocus);
        }

        // Focus-specific navigation
        let focus = &state.home_screen_v2_state.focus;
        let event = match focus {
            HomeScreenFocus::Sidebar => match key_event.code {
                KeyCode::Up => Some(AppEvent::HomeScreenSidebarUp),
                KeyCode::Down => Some(AppEvent::HomeScreenSidebarDown),
                KeyCode::Enter => Some(AppEvent::HomeScreenSidebarSelect),
                _ => None,
            },
            HomeScreenFocus::ContentPanel => match key_event.code {
                KeyCode::Up => Some(AppEvent::WelcomePanelScrollUp),
                KeyCode::Down => Some(AppEvent::WelcomePanelScrollDown),
                KeyCode::PageUp => Some(AppEvent::WelcomePanelPageUp),
                KeyCode::PageDown => Some(AppEvent::WelcomePanelPageDown),
                KeyCode::Char('y') => Some(AppEvent::WelcomePanelCopyContent),
                _ => None,
            },
        };

        tracing::debug!("HomeScreen V2 key handler returning: {:?}", event);
        event
    }

    // AINB 2.0: Agent selection key handling
    fn handle_agent_selection_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        let agent_state = &state.agent_selection_state;

        // Check if a provider is expanded (showing models)
        if agent_state.expanded_provider.is_some() {
            match key_event.code {
                KeyCode::Esc => Some(AppEvent::AgentSelectionBack),
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::AgentSelectionPrevModel),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::AgentSelectionNextModel),
                KeyCode::Tab => Some(AppEvent::AgentSelectionNextProvider),
                KeyCode::BackTab => Some(AppEvent::AgentSelectionPrevProvider),
                KeyCode::Enter => Some(AppEvent::AgentSelectionSelect),
                KeyCode::Char(' ') => Some(AppEvent::AgentSelectionToggleExpand),
                _ => None,
            }
        } else {
            match key_event.code {
                KeyCode::Esc => Some(AppEvent::AgentSelectionBack),
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::AgentSelectionPrevProvider),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::AgentSelectionNextProvider),
                KeyCode::Enter | KeyCode::Char(' ') => Some(AppEvent::AgentSelectionToggleExpand),
                _ => None,
            }
        }
    }

    fn handle_config_screen_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        // Check if config popup is showing first
        if state.config_popup_state.show_popup {
            return Self::handle_config_popup_keys(key_event, state);
        }

        let config_state = &state.config_screen_state;
        tracing::debug!(
            "Config screen key handler: {:?}, editing: {}, api_key_mode: {}",
            key_event.code,
            config_state.editing,
            config_state.api_key_input_mode
        );

        // API key input mode - special handling (saves to keychain)
        if config_state.api_key_input_mode {
            match key_event.code {
                KeyCode::Enter => Some(AppEvent::ConfigApiKeySave),
                KeyCode::Esc => Some(AppEvent::ConfigCancelEdit),
                KeyCode::Backspace => Some(AppEvent::ConfigEditBackspace),
                KeyCode::Char(c) => Some(AppEvent::ConfigEditChar(c)),
                _ => None,
            }
        } else if config_state.editing {
            // Normal editing mode - handle text input
            match key_event.code {
                KeyCode::Enter => Some(AppEvent::ConfigSaveEdit),
                KeyCode::Esc => Some(AppEvent::ConfigCancelEdit),
                KeyCode::Backspace => Some(AppEvent::ConfigEditBackspace),
                KeyCode::Char(c) => Some(AppEvent::ConfigEditChar(c)),
                _ => None,
            }
        } else {
            // Navigation mode - check if we're on auth settings
            let is_auth_category = config_state.selected_category == 0; // Authentication category
            let on_claude_auth = is_auth_category && config_state.selected_setting == 0; // Claude Authentication

            match key_event.code {
                KeyCode::Esc => Some(AppEvent::ConfigBack),
                KeyCode::Tab => Some(AppEvent::ConfigSwitchPane),
                // Up/Down navigate within the current focused pane
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::ConfigNavigateUp),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::ConfigNavigateDown),
                // Left/Right switch focus between panes
                KeyCode::Left | KeyCode::Char('h') => Some(AppEvent::ConfigFocusCategories),
                KeyCode::Right | KeyCode::Char('l') => Some(AppEvent::ConfigFocusSettings),
                KeyCode::Enter => {
                    if on_claude_auth {
                        // Open the auth provider popup
                        Some(AppEvent::AuthProviderPopupOpen)
                    } else {
                        Some(AppEvent::ConfigEditSetting)
                    }
                }
                KeyCode::Char('s' | 'S') => Some(AppEvent::ConfigSaveAll),
                _ => None,
            }
        }
    }

    // AINB 2.0: Auth provider popup key handling
    fn handle_auth_provider_popup_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        let popup_state = &state.auth_provider_popup_state;

        if popup_state.is_entering_key {
            // API key input mode
            match key_event.code {
                KeyCode::Enter => Some(AppEvent::AuthProviderPopupSelect),
                KeyCode::Esc => Some(AppEvent::AuthProviderPopupClose),
                KeyCode::Backspace => Some(AppEvent::AuthProviderPopupBackspace),
                KeyCode::Char(c) => Some(AppEvent::AuthProviderPopupInputChar(c)),
                _ => None,
            }
        } else {
            // Navigation mode
            match key_event.code {
                KeyCode::Esc => Some(AppEvent::AuthProviderPopupClose),
                KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::AuthProviderPopupPrev),
                KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::AuthProviderPopupNext),
                KeyCode::Enter => Some(AppEvent::AuthProviderPopupSelect),
                KeyCode::Char('d' | 'D') => Some(AppEvent::AuthProviderPopupDeleteKey),
                _ => None,
            }
        }
    }

    // AINB 2.0: Config popup key handling (for choice/text input popups)
    fn handle_config_popup_keys(key_event: KeyEvent, state: &AppState) -> Option<AppEvent> {
        use crate::components::config_popup::ConfigPopupType;

        let popup_state = &state.config_popup_state;

        match &popup_state.popup_type {
            ConfigPopupType::Choice { .. } | ConfigPopupType::Boolean { .. } => {
                // Choice/Boolean navigation mode
                match key_event.code {
                    KeyCode::Esc => Some(AppEvent::ConfigPopupCancel),
                    KeyCode::Up | KeyCode::Char('k') => Some(AppEvent::ConfigPopupNavigateUp),
                    KeyCode::Down | KeyCode::Char('j') => Some(AppEvent::ConfigPopupNavigateDown),
                    KeyCode::Enter => Some(AppEvent::ConfigPopupConfirm),
                    _ => None,
                }
            }
            ConfigPopupType::TextInput { .. } | ConfigPopupType::NumberInput { .. } => {
                // Text/Number input mode
                match key_event.code {
                    KeyCode::Esc => Some(AppEvent::ConfigPopupCancel),
                    KeyCode::Enter => Some(AppEvent::ConfigPopupConfirm),
                    KeyCode::Backspace => Some(AppEvent::ConfigPopupBackspace),
                    KeyCode::Char(c) => Some(AppEvent::ConfigPopupInputChar(c)),
                    _ => None,
                }
            }
        }
    }

    pub fn process_event(event: AppEvent, state: &mut AppState) {
        match event {
            AppEvent::Quit => state.quit(),
            AppEvent::GoToHomeScreen => {
                tracing::info!("Navigating to HomeScreen");
                state.current_screen = screen_ids::HOME.to_string();
            }
            AppEvent::ToggleHelp => state.toggle_help(),
            AppEvent::ToggleClaudeChat => state.toggle_claude_chat(),
            AppEvent::ToggleExpandAll => state.toggle_expand_all_workspaces(),
            // Other tmux rename events
            AppEvent::OtherTmuxStartRename => state.start_other_tmux_rename(),
            AppEvent::OtherTmuxRenameChar(c) => state.other_tmux_rename_char(c),
            AppEvent::OtherTmuxRenameBackspace => state.other_tmux_rename_backspace(),
            AppEvent::OtherTmuxCancelRename => state.cancel_other_tmux_rename(),
            AppEvent::OtherTmuxConfirmRename => {
                state.pending_async_action = Some(AsyncAction::ConfirmOtherTmuxRename);
            }
            // SSH session rename events
            AppEvent::SshSessionStartRename => state.start_ssh_session_rename(),
            AppEvent::SshSessionRenameChar(c) => state.ssh_session_rename_char(c),
            AppEvent::SshSessionRenameBackspace => state.ssh_session_rename_backspace(),
            AppEvent::SshSessionCancelRename => state.cancel_ssh_session_rename(),
            AppEvent::SshSessionConfirmRename => state.confirm_ssh_session_rename(),
            AppEvent::RefreshWorkspaces => {
                // Mark for async processing to reload workspace data
                state.pending_async_action = Some(AsyncAction::RefreshWorkspaces);
            }
            AppEvent::CycleSessionFilter => {
                state.cycle_session_filter();
                let label = match state.session_filter {
                    crate::app::state::SessionFilter::All => "all sessions",
                    crate::app::state::SessionFilter::ActiveOnly => "active only",
                    crate::app::state::SessionFilter::StoppedOnly => "stopped only",
                };
                state.add_success_notification(format!("Filter: {}", label));
                state.ui_needs_refresh = true;
            }
            AppEvent::NextSession => {
                state.next_session();
                state.last_preview_update = None;
            }
            AppEvent::PreviousSession => {
                state.previous_session();
                state.last_preview_update = None;
            }
            AppEvent::NextWorkspace => {
                state.next_workspace();
                state.last_preview_update = None;
            }
            AppEvent::PreviousWorkspace => {
                state.previous_workspace();
                state.last_preview_update = None;
            }
            AppEvent::GoToTop => {
                if state.selected_workspace_index.is_some() {
                    state.selected_session_index = Some(0);
                }
            }
            AppEvent::GoToBottom => {
                if let Some(workspace_idx) = state.selected_workspace_index {
                    if let Some(workspace) = state.workspaces.get(workspace_idx) {
                        if !workspace.sessions.is_empty() {
                            state.selected_session_index = Some(workspace.sessions.len() - 1);
                        }
                    }
                }
            }
            AppEvent::NewSession => {
                // Phase 4 (new-session redesign): open the screen-1 unified
                // picker synchronously, then route the user to the
                // NEW_SESSION screen. Track `previous_screen` so Esc on
                // PickRepo returns to wherever the user invoked `n` from
                // (Home, Sessions, etc.) rather than hardcoding HOME.
                use crate::components::new_session::pick_repo::PickRepoState;
                use crate::git::{RepositoryCache, WorkspaceScanner};

                // Local-repo candidates come from the WorkspaceScanner cache
                // (a cheap JSON read of
                // ~/.agents-in-a-box/cache/repositories.json), NOT a
                // synchronous filesystem walk — reading the cache surfaces
                // every scanned repo to the fuzzy filter without the
                // event-loop freeze that motivated dropping the inline scan
                // here (2026-05-22). `picker_local_paths` also drops entries
                // whose directory no longer exists, so a repo deleted since
                // the last scan can't appear as a selectable dead row.
                let local_paths =
                    picker_local_paths(RepositoryCache::load(), &state.workspaces);

                // Refresh the cache off the UI thread so a newly-created repo
                // surfaces on a later open. `scan()` is read-through: instant
                // while the cache is valid, full walk + atomic persist only
                // once it goes stale. (A repo created directly under a scan
                // root bumps that root's mtime and invalidates the cache
                // immediately; one nested deeper is picked up on the 1h TTL.)
                // `spawn_blocking` keeps this on tokio's managed blocking pool
                // — consistent with every other blocking offload, and unlike a
                // detached `std::thread` it is not torn down mid-write at
                // shutdown.
                let scan_paths =
                    state.app_config.workspace_defaults.workspace_scan_paths.clone();
                let exclude_paths =
                    state.app_config.workspace_defaults.exclude_paths.clone();
                tokio::task::spawn_blocking(move || {
                    let scanner = WorkspaceScanner::with_additional_paths(scan_paths)
                        .with_exclude_paths(exclude_paths);
                    if let Err(err) = scanner.scan() {
                        tracing::warn!(error = %err, "pick_repo: background repo rescan failed");
                    }
                });
                let ns = crate::app::state::NewSessionState {
                    step: crate::app::state::NewSessionStep::PickRepo,
                    pick_repo_state: Some(PickRepoState::from_disk(&local_paths)),
                    ..Default::default()
                };
                state.new_session_state = Some(ns);
                state.previous_screen = Some(state.current_screen.clone());
                state.current_screen =
                    crate::app::screens::ids::NEW_SESSION.to_string();
                tracing::debug!(
                    previous = %state.previous_screen.as_deref().unwrap_or(""),
                    "AppEvent::NewSession -> PickRepo opened"
                );
            }
            AppEvent::SearchWorkspace => {
                // Phase 6 (new-session redesign): SearchWorkspace is a no-op —
                // the legacy workspace-search flow was wired to the deleted
                // `SelectRepo` step. The redesigned PickRepo screen absorbed
                // that role; nothing in the host should fire this anymore.
                tracing::debug!("AppEvent::SearchWorkspace: legacy no-op (Phase 6)");
            }
            AppEvent::NewSessionCancel => {
                state.cancel_new_session();
            }
            AppEvent::ConfigureBack => {
                // Phase 5: Esc on Configure persists the half-typed prompt to
                // session-defaults so it's restored on re-entry, then routes
                // the user back to PickRepo without losing the highlighted
                // row. Persistence error is non-fatal (best-effort).
                use crate::app::state::NewSessionStep;
                use crate::config::session_defaults::SessionDefaults;
                let (repo_label, prompt_text) = state
                    .new_session_state
                    .as_ref()
                    .and_then(|ns| ns.configure_state.as_ref())
                    .map(|cfg| (cfg.repo_label.clone(), cfg.prompt.to_string()))
                    .unwrap_or_default();
                if !repo_label.is_empty() {
                    let path = SessionDefaults::default_path();
                    let mut defaults = SessionDefaults::load_from(&path);
                    let entry = defaults
                        .per_repo
                        .entry(repo_label.clone())
                        .or_default();
                    entry.last_prompt = if prompt_text.is_empty() {
                        None
                    } else {
                        Some(prompt_text)
                    };
                    if let Err(err) = defaults.save_to(&path) {
                        tracing::warn!(error = %err, "ConfigureBack: persist failed");
                    }
                    // Refresh PickRepo's in-memory snapshot so a later Enter
                    // on PickRepo doesn't clobber the prompt we just wrote.
                    // The picker carries its own `defaults` copy from open
                    // time; mutations elsewhere are invisible to it.
                    if let Some(pick) = state
                        .new_session_state
                        .as_mut()
                        .and_then(|ns| ns.pick_repo_state.as_mut())
                    {
                        pick.defaults = defaults;
                    }
                }
                if let Some(ns) = state.new_session_state.as_mut() {
                    ns.configure_state = None;
                    ns.step = NewSessionStep::PickRepo;
                }
            }
            AppEvent::ConfigureLaunch(spec) => {
                // Phase 6 (new-session redesign): persist launch into
                // session-defaults BEFORE the async dispatch so tripwires
                // observe the YAML mutation synchronously, transition to the
                // Creating step so the legacy render dispatcher draws the
                // in-flight banner, then fire
                // `AsyncAction::CreateSessionFromConfigure` — the new
                // configure-state-aware sibling of `CreateNewSession`.
                //
                // The `LaunchSpec` payload is the same one the Configure
                // component built (finding #7); we use it as the single
                // source of truth instead of reaching back into
                // `configure_state` a second time.
                use crate::config::session_defaults::SessionDefaults;
                let path = SessionDefaults::default_path();
                let mut defaults = SessionDefaults::load_from(&path);
                let (st, src) = source_provenance(&spec.repo_source);
                let branch_override = spec.branch_override();
                defaults.record_launch(
                    &spec.repo_label,
                    &spec.preset_name,
                    branch_override.as_deref(),
                    spec.prompt.as_deref(),
                    st,
                    src.as_deref(),
                );
                if let Err(err) = defaults.save_to(&path) {
                    tracing::warn!(error = %err, "ConfigureLaunch: persist failed");
                }
                // Move into the Creating step so the in-flight UI is shown
                // until the async create resolves. Keep `configure_state`
                // intact — `create_session_from_configure` reads it.
                if let Some(ns) = state.new_session_state.as_mut() {
                    ns.step = crate::app::state::NewSessionStep::Creating;
                }
                state.pending_async_action =
                    Some(AsyncAction::CreateSessionFromConfigure(spec));
            }
            AppEvent::ConfigureOpenPresetManager => {
                // Phase 7 polish — stub for now.
                tracing::warn!("ConfigureOpenPresetManager — stub until Phase 7");
            }
            AppEvent::ShowNotification(message) => {
                tracing::info!("Event: ShowNotification - {}", message);
                state.add_warning_notification(message);
            }
            AppEvent::AttachSession => {
                if let Some(session_id) = state.get_selected_session_id() {
                    state.pending_async_action = Some(AsyncAction::AttachToContainer(session_id));
                }
            }
            AppEvent::AttachTmuxSession => {
                tracing::info!("[ACTION] Processing AttachTmuxSession event");
                tracing::debug!(
                    "[ACTION] State: workspace_idx={:?}, session_idx={:?}, shell_selected={}, is_ssh={}, ssh_idx={:?}, is_other_tmux={}, other_tmux_idx={:?}",
                    state.selected_workspace_index,
                    state.selected_session_index,
                    state.shell_selected,
                    state.is_ssh_session_selected(),
                    state.selected_ssh_session_index,
                    state.is_other_tmux_selected(),
                    state.selected_other_tmux_index
                );

                // Check if we're in the "SSH Sessions" section
                if state.is_ssh_session_selected() {
                    if let Some(ssh_session) = state.selected_ssh_session() {
                        if let Some(tmux_name) = &ssh_session.tmux_session_name {
                            let session_name = tmux_name.clone();
                            tracing::info!("[ACTION] Attaching to SSH session: {}", session_name);
                            state.pending_async_action =
                                Some(AsyncAction::AttachToOtherTmux(session_name));
                        } else {
                            tracing::warn!("[ACTION] SSH session has no tmux session name");
                            state.add_error_notification(
                                "SSH session has no tmux session".to_string(),
                            );
                        }
                    } else {
                        tracing::warn!("[ACTION] SSH session selected but no session found");
                    }
                // Check if we're in the "Other tmux" section
                } else if state.is_other_tmux_selected() {
                    if let Some(other_session) = state.selected_other_tmux_session() {
                        let session_name = other_session.name.clone();
                        tracing::info!(
                            "[ACTION] Attaching to other tmux session: {}",
                            session_name
                        );
                        state.pending_async_action =
                            Some(AsyncAction::AttachToOtherTmux(session_name));
                    } else {
                        tracing::warn!("[ACTION] Other tmux selected but no session found");
                    }
                } else if state.shell_selected {
                    // Shell session selected - attach to its tmux session
                    if let Some(workspace_idx) = state.selected_workspace_index {
                        if let Some(workspace) = state.workspaces.get(workspace_idx) {
                            if let Some(shell) = &workspace.shell_session {
                                let session_name = shell.tmux_session_name.clone();
                                tracing::info!(
                                    "[ACTION] Attaching to workspace shell: {}",
                                    session_name
                                );
                                state.pending_async_action =
                                    Some(AsyncAction::AttachToOtherTmux(session_name));
                            } else {
                                tracing::warn!(
                                    "[ACTION] Shell selected but no shell session found in workspace"
                                );
                                state.add_error_notification("No shell session found".to_string());
                            }
                        }
                    }
                } else if let Some(session_id) = state.get_selected_session_id() {
                    // Get more info about the session for logging
                    if let Some(session) = state.get_selected_session() {
                        tracing::info!(
                            "[ACTION] Attaching to session: id={}, name={}, tmux_name={:?}, status={:?}",
                            session_id,
                            session.name,
                            session.tmux_session_name,
                            session.status
                        );
                    }
                    state.pending_async_action = Some(AsyncAction::AttachToTmuxSession(session_id));
                } else {
                    tracing::warn!(
                        "[ACTION] AttachTmuxSession: No session selected (workspace_idx={:?}, session_idx={:?})",
                        state.selected_workspace_index,
                        state.selected_session_index
                    );
                    state.add_error_notification("No session selected to attach".to_string());
                }
            }
            AppEvent::DetachSession => {
                // Clear attached session and return to home screen
                state.attached_session_id = None;
                state.current_screen = screen_ids::HOME.to_string();
                state.ui_needs_refresh = true;
            }
            AppEvent::DetachTmuxSession => {
                // Detaching from tmux is handled by AttachHandler (Ctrl+Q)
                // This event is a no-op placeholder
                tracing::debug!("DetachTmuxSession event received (no-op)");
            }
            AppEvent::ScrollPreviewUp => {
                // Scroll events are handled by the LayoutComponent's tmux_preview
                // This is a signal that should be processed in main loop
                tracing::debug!("ScrollPreviewUp event (handled by layout component)");
                state.ui_needs_refresh = true;
            }
            AppEvent::ScrollPreviewDown => {
                // Scroll events are handled by the LayoutComponent's tmux_preview
                // This is a signal that should be processed in main loop
                tracing::debug!("ScrollPreviewDown event (handled by layout component)");
                state.ui_needs_refresh = true;
            }
            AppEvent::EnterScrollMode => {
                tracing::debug!("EnterScrollMode event (handled by layout component)");
                state.ui_needs_refresh = true;
            }
            AppEvent::ExitScrollMode => {
                tracing::debug!("ExitScrollMode event (handled by layout component)");
                state.ui_needs_refresh = true;
            }
            AppEvent::KillContainer => {
                if let Some(session_id) = state.attached_session_id {
                    state.pending_async_action = Some(AsyncAction::KillContainer(session_id));
                }
            }
            AppEvent::ReauthenticateCredentials => {
                info!("Queueing re-authentication request");
                state.pending_async_action = Some(AsyncAction::ReauthenticateCredentials);
            }
            AppEvent::RestartSession => {
                if let Some(session_id) = state.get_selected_session_id() {
                    state.pending_async_action = Some(AsyncAction::RestartSession(session_id));
                }
            }
            AppEvent::DeleteSession => {
                tracing::info!("[ACTION] Processing DeleteSession event");
                tracing::debug!(
                    "[ACTION] Delete state: workspace_idx={:?}, session_idx={:?}, shell_selected={}, is_other_tmux={}, other_tmux_idx={:?}",
                    state.selected_workspace_index,
                    state.selected_session_index,
                    state.shell_selected,
                    state.is_other_tmux_selected(),
                    state.selected_other_tmux_index
                );

                let managed_count = state.selected_sessions.len();
                let other_names = state.selected_other_tmux_names_in_order();
                let other_count = other_names.len();

                // Checked rows win over cursor delete, so pressing `d` after
                // multi-select cannot accidentally delete only the highlighted row.
                if managed_count > 0 && other_count > 0 {
                    state.add_warning_notification(
                        "Delete managed and Other tmux sessions separately.".to_string(),
                    );
                } else if other_count > 0 {
                    state.show_kill_other_tmux_sessions_confirmation(other_names);
                } else if managed_count > 0 {
                    let ids: Vec<uuid::Uuid> = state.selected_sessions.iter().copied().collect();
                    state.add_success_notification(format!(
                        "Deleting {} selected session(s)...",
                        managed_count
                    ));
                    state.pending_async_action = Some(AsyncAction::BulkDeleteSessions(ids));
                    state.selected_sessions.clear();
                // Check if we're in the SSH Sessions section
                } else if state.is_ssh_session_selected() {
                    if let Some(ssh_session) = state.selected_ssh_session() {
                        // SSH sessions are tmux sessions - use the tmux session name for kill
                        if let Some(tmux_name) = ssh_session.tmux_session_name.clone() {
                            tracing::info!(
                                "[ACTION] Showing kill confirmation for SSH session: {}",
                                tmux_name
                            );
                            state.show_kill_ssh_session_confirmation(tmux_name);
                        } else {
                            tracing::warn!("[ACTION] SSH session has no tmux_session_name");
                            state.add_warning_notification(
                                "Cannot delete SSH session: no tmux session name".to_string(),
                            );
                        }
                    } else {
                        tracing::warn!(
                            "[ACTION] SSH session selected but no session found at index {:?}",
                            state.selected_ssh_session_index
                        );
                    }
                // Check if we're in the "Other tmux" section
                } else if state.is_other_tmux_selected() {
                    if let Some(other_session) = state.selected_other_tmux_session() {
                        tracing::info!(
                            "[ACTION] Showing kill confirmation for other tmux session: {}",
                            other_session.name
                        );
                        state.show_kill_other_tmux_confirmation(other_session.name.clone());
                    } else {
                        tracing::warn!(
                            "[ACTION] Other tmux selected but no session found at index {:?}",
                            state.selected_other_tmux_index
                        );
                    }
                } else if state.shell_selected {
                    // Shell session selected - show kill shell confirmation
                    if let Some(workspace_idx) = state.selected_workspace_index {
                        if state
                            .workspaces
                            .get(workspace_idx)
                            .and_then(|w| w.shell_session.as_ref())
                            .is_some()
                        {
                            state.show_kill_shell_confirmation(workspace_idx);
                        }
                    }
                } else if let Some(session) = state.selected_session() {
                    // Interactive sessions (Claude/Codex/Gemini/Copilot) get the
                    // tri-option Stop / Delete / Cancel dialog so the user can
                    // soft-stop without losing the worktree. Boss/Docker, SSH,
                    // and Shell sessions stick with the binary delete flow.
                    use crate::models::{SessionAgentType, SessionMode};
                    let is_interactive_agent = matches!(session.mode, SessionMode::Interactive)
                        && matches!(
                            session.agent_type,
                            SessionAgentType::Claude
                                | SessionAgentType::Codex
                                | SessionAgentType::Gemini
                                | SessionAgentType::Copilot
                        );
                    let session_id = session.id;
                    if is_interactive_agent {
                        state.show_delete_or_stop_confirmation(session_id);
                    } else {
                        state.show_delete_confirmation(session_id);
                    }
                } else {
                    tracing::warn!(
                        "[ACTION] DeleteSession: No item to delete (workspace_idx={:?}, session_idx={:?}, shell={}, other_tmux_idx={:?})",
                        state.selected_workspace_index,
                        state.selected_session_index,
                        state.shell_selected,
                        state.selected_other_tmux_index
                    );
                    state.add_warning_notification("No session selected to delete".to_string());
                }
            }
            AppEvent::ToggleSelectSession => {
                state.toggle_select_session();
                let count =
                    state.selected_sessions.len() + state.selected_other_tmux_sessions.len();
                if count > 0 {
                    state.add_success_notification(format!(
                        "{} session(s) selected — Shift+D to delete",
                        count
                    ));
                }
            }
            AppEvent::DeleteSelectedSessions => {
                let managed_count = state.selected_sessions.len();
                let other_names = state.selected_other_tmux_names_in_order();
                let other_count = other_names.len();
                if managed_count == 0 && other_count == 0 {
                    state.add_warning_notification(
                        "No sessions selected. Use Space to select sessions first.".to_string(),
                    );
                } else if managed_count > 0 && other_count > 0 {
                    state.add_warning_notification(
                        "Delete managed and Other tmux sessions separately.".to_string(),
                    );
                } else if other_count > 0 {
                    state.show_kill_other_tmux_sessions_confirmation(other_names);
                } else {
                    let ids: Vec<uuid::Uuid> = state.selected_sessions.iter().copied().collect();
                    state.add_success_notification(format!(
                        "Deleting {} selected session(s)...",
                        managed_count
                    ));
                    state.pending_async_action = Some(AsyncAction::BulkDeleteSessions(ids));
                    state.selected_sessions.clear();
                }
            }
            AppEvent::ResumeSession(trigger) => {
                if let Some(session_id) = state.get_selected_session_id() {
                    tracing::info!(
                        "[ACTION] Resuming stopped session: {} (trigger={})",
                        session_id,
                        trigger
                    );
                    state.pending_async_action =
                        Some(AsyncAction::ResumeSession(session_id, trigger));
                } else {
                    state.add_warning_notification("No session selected to resume".to_string());
                }
            }
            AppEvent::OpenInEditor => {
                // Open session's workspace in preferred editor
                if let Some(session) = state.selected_session() {
                    let workspace_path = std::path::PathBuf::from(&session.workspace_path);
                    state.pending_async_action = Some(AsyncAction::OpenInEditor(workspace_path));
                } else {
                    state.add_warning_notification("⚠️ No session selected".to_string());
                }
            }
            AppEvent::CleanupOrphaned => {
                // Queue cleanup of orphaned containers
                state.pending_async_action = Some(AsyncAction::CleanupOrphaned);
            }
            AppEvent::OpenQuickShell => {
                // Open workspace shell and optionally cd to session's worktree
                if let Some(workspace_idx) = state.selected_workspace_index {
                    // Get target directory - session worktree if selected, otherwise workspace root
                    let target_dir = if let Some(session) = state.selected_session() {
                        // Session selected - cd to its worktree
                        Some(std::path::PathBuf::from(&session.workspace_path))
                    } else {
                        // Just workspace selected - cd to workspace root (or None to stay where we are)
                        None
                    };

                    tracing::info!("Opening workspace shell, target_dir: {:?}", target_dir);
                    state.pending_async_action = Some(AsyncAction::OpenWorkspaceShell {
                        workspace_index: workspace_idx,
                        target_dir,
                    });
                } else {
                    state.add_warning_notification("No workspace selected".to_string());
                }
            }
            AppEvent::SwitchToLogs => {
                // TODO: Implement view switching
            }
            AppEvent::SwitchToTerminal => {
                // TODO: Implement terminal view
            }
            AppEvent::SwitchPaneFocus => {
                use crate::app::state::FocusedPane;
                let old_pane = state.focused_pane.clone();
                state.focused_pane = match state.focused_pane {
                    FocusedPane::Sessions => FocusedPane::LiveLogs,
                    FocusedPane::LiveLogs => FocusedPane::Sessions,
                };
                tracing::debug!(
                    "Switched focus from {:?} to {:?}",
                    old_pane,
                    state.focused_pane
                );
            }
            AppEvent::ScrollLogsUp => {
                // Handled in main.rs to access layout component
            }
            AppEvent::ScrollLogsDown => {
                // Handled in main.rs to access layout component
            }
            AppEvent::ScrollLogsToTop => {
                // Handled in main.rs to access layout component
            }
            AppEvent::ScrollLogsToBottom => {
                // Handled in main.rs to access layout component
            }
            AppEvent::ToggleAutoScroll => {
                // Handled in main.rs to access layout component
            }
            AppEvent::ConfirmationToggle => {
                if let Some(ref mut dialog) = state.confirmation_dialog {
                    if let Some(ref options) = dialog.options {
                        let len = options.len().max(1);
                        dialog.selected_index = (dialog.selected_index + 1) % len;
                    } else {
                        dialog.selected_option = !dialog.selected_option;
                    }
                }
            }
            AppEvent::ConfirmationPrev => {
                if let Some(ref mut dialog) = state.confirmation_dialog {
                    if let Some(ref options) = dialog.options {
                        let len = options.len().max(1);
                        dialog.selected_index = (dialog.selected_index + len - 1) % len;
                    } else {
                        dialog.selected_option = !dialog.selected_option;
                    }
                }
            }
            AppEvent::ConfirmationConfirm => {
                if let Some(dialog) = state.confirmation_dialog.take() {
                    let action = if let Some(options) = dialog.options.as_ref() {
                        // Tri-option mode: pick the highlighted option's action.
                        options.get(dialog.selected_index).map(|o| o.action.clone())
                    } else if dialog.selected_option {
                        Some(dialog.confirm_action.clone())
                    } else {
                        None
                    };

                    if let Some(action) = action {
                        match action {
                            crate::app::state::ConfirmAction::DeleteSession(session_id) => {
                                state.pending_async_action =
                                    Some(AsyncAction::DeleteSession(session_id));
                            }
                            crate::app::state::ConfirmAction::StopSession(session_id) => {
                                state.pending_async_action =
                                    Some(AsyncAction::StopSession(session_id));
                            }
                            crate::app::state::ConfirmAction::KillOtherTmux(session_name) => {
                                state.pending_async_action =
                                    Some(AsyncAction::KillOtherTmux(session_name));
                            }
                            crate::app::state::ConfirmAction::KillOtherTmuxSessions(
                                session_names,
                            ) => {
                                state.selected_other_tmux_sessions.clear();
                                state.pending_async_action =
                                    Some(AsyncAction::KillOtherTmuxSessions(session_names));
                            }
                            crate::app::state::ConfirmAction::KillWorkspaceShell(workspace_idx) => {
                                state.pending_async_action =
                                    Some(AsyncAction::KillWorkspaceShell(workspace_idx));
                            }
                            crate::app::state::ConfirmAction::Cancel => {
                                // Explicit Cancel: dialog already taken; nothing to do.
                            }
                        }
                    }
                }
            }
            AppEvent::ConfirmationCancel => {
                state.confirmation_dialog = None;
            }
            AppEvent::AuthSetupNext => {
                if let Some(ref mut auth_state) = state.auth_setup_state {
                    auth_state.selected_method = match auth_state.selected_method {
                        AuthMethod::OAuth => AuthMethod::ApiKey,
                        AuthMethod::ApiKey => AuthMethod::Skip,
                        AuthMethod::Skip => AuthMethod::OAuth,
                    };
                }
            }
            AppEvent::AuthSetupPrevious => {
                if let Some(ref mut auth_state) = state.auth_setup_state {
                    auth_state.selected_method = match auth_state.selected_method {
                        AuthMethod::OAuth => AuthMethod::Skip,
                        AuthMethod::ApiKey => AuthMethod::OAuth,
                        AuthMethod::Skip => AuthMethod::ApiKey,
                    };
                }
            }
            AppEvent::AuthSetupSelect => {
                if let Some(ref auth_state) = state.auth_setup_state {
                    match auth_state.selected_method {
                        AuthMethod::OAuth => {
                            // Mark for async OAuth processing
                            state.pending_async_action = Some(AsyncAction::AuthSetupOAuth);
                        }
                        AuthMethod::ApiKey => {
                            if auth_state.api_key_input.is_empty() {
                                // Enter API key input mode
                                if let Some(ref mut auth_state) = state.auth_setup_state {
                                    auth_state.api_key_input = "sk-".to_string();
                                    auth_state.show_cursor = true;
                                }
                            } else {
                                // Save the API key
                                state.pending_async_action = Some(AsyncAction::AuthSetupApiKey);
                            }
                        }
                        AuthMethod::Skip => {
                            // Skip auth setup and go to home screen
                            state.auth_setup_state = None;
                            state.current_screen = screen_ids::HOME.to_string();
                            state.check_current_directory_status();
                            state.pending_async_action = Some(AsyncAction::RefreshWorkspaces);
                        }
                    }
                }
            }
            AppEvent::AuthSetupCancel => {
                // Same as skip - go to home screen without auth
                state.auth_setup_state = None;
                state.current_screen = screen_ids::HOME.to_string();
                state.check_current_directory_status();
                state.pending_async_action = Some(AsyncAction::RefreshWorkspaces);
            }
            AppEvent::AuthSetupInputChar(ch) => {
                if let Some(ref mut auth_state) = state.auth_setup_state {
                    auth_state.api_key_input.push(ch);
                }
            }
            AppEvent::AuthSetupBackspace => {
                if let Some(ref mut auth_state) = state.auth_setup_state {
                    if auth_state.api_key_input.is_empty() {
                        // Exit API key input mode
                        auth_state.show_cursor = false;
                    } else {
                        auth_state.api_key_input.pop();
                    }
                }
            }
            AppEvent::AuthSetupCheckStatus => {
                // Check if authentication was completed and transition if so
                if state.auth_setup_state.is_some() && !AppState::is_first_time_setup() {
                    // Authentication completed!
                    state.auth_setup_state = None;
                    state.current_screen = screen_ids::HOME.to_string();
                    state.check_current_directory_status();
                    state.pending_async_action = Some(AsyncAction::RefreshWorkspaces);
                }
            }
            AppEvent::AuthSetupRefresh => {
                // Manual refresh - check authentication status immediately
                if let Some(ref mut auth_state) = state.auth_setup_state {
                    if !AppState::is_first_time_setup() {
                        // Authentication completed!
                        state.auth_setup_state = None;
                        state.current_screen = screen_ids::HOME.to_string();
                        state.check_current_directory_status();
                        state.pending_async_action = Some(AsyncAction::RefreshWorkspaces);
                    } else {
                        // Still waiting - update message
                        auth_state.error_message = Some("Still waiting for authentication. Complete the process in the terminal window.\n\nPress 'r' to refresh or 'Esc' to cancel.".to_string());
                    }
                }
            }
            AppEvent::AuthSetupShowCommand => {
                // Show alternative authentication methods
                if let Some(ref mut auth_state) = state.auth_setup_state {
                    auth_state.error_message = Some(
                        "📋 Alternative Authentication Methods:\n\n\
                         1. If the OAuth URL didn't appear, check the container logs\n\n\
                         2. Use API Key authentication instead (press Up/Down to switch)\n\n\
                         3. Run authentication manually in a terminal:\n\
                            docker exec -it agents-box-auth /bin/bash\n\
                            claude auth login\n\n\
                         Press 'Esc' to go back."
                            .to_string(),
                    );
                }
            }
            // Phase 6 (new-session redesign): the FileFinder events (@-trigger
            // for the legacy Boss-prompt textarea) have been removed. The new
            // Configure screen owns its own prompt textarea and doesn't host
            // the @-finder yet — Phase 7 polish will reintroduce it if needed.
            AppEvent::FileFinderNavigateUp
            | AppEvent::FileFinderNavigateDown
            | AppEvent::FileFinderSelectFile
            | AppEvent::FileFinderCancel => {
                tracing::debug!("FileFinder event in NewSession: legacy no-op (Phase 6)");
            }
            // Git view events
            AppEvent::ShowGitView => {
                tracing::info!("Showing git view");
                state.show_git_view();
                tracing::info!(
                    "Git view state after show: current_screen = {:?}, git_state = {}",
                    state.current_screen,
                    state.git_view_state.is_some()
                );
            }
            AppEvent::GitViewSwitchTab => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.switch_tab();
                }
            }
            AppEvent::GitViewNextFile => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.next_file();
                }
            }
            AppEvent::GitViewPrevFile => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.previous_file();
                }
            }
            AppEvent::GitViewScrollUp => {
                if let Some(ref mut git_state) = state.git_view_state {
                    match git_state.active_tab {
                        crate::components::git_view::GitTab::Diff => git_state.scroll_diff_up(),
                        crate::components::git_view::GitTab::Markdown => {
                            git_state.scroll_markdown_up()
                        }
                        _ => {}
                    }
                }
            }
            AppEvent::GitViewScrollDown => {
                if let Some(ref mut git_state) = state.git_view_state {
                    match git_state.active_tab {
                        crate::components::git_view::GitTab::Diff => git_state.scroll_diff_down(),
                        crate::components::git_view::GitTab::Markdown => {
                            git_state.scroll_markdown_down()
                        }
                        _ => {}
                    }
                }
            }
            AppEvent::GitViewNextCommit => {
                if let Some(ref mut git_state) = state.git_view_state {
                    if git_state.selected_commit_index < git_state.commits.len().saturating_sub(1) {
                        git_state.selected_commit_index += 1;
                    }
                }
            }
            AppEvent::GitViewPrevCommit => {
                if let Some(ref mut git_state) = state.git_view_state {
                    if git_state.selected_commit_index > 0 {
                        git_state.selected_commit_index -= 1;
                    }
                }
            }
            AppEvent::GitViewShowCommitDiff => {
                if let Some(ref mut git_state) = state.git_view_state {
                    // Get the selected commit hash
                    if let Some(commit) = git_state.commits.get(git_state.selected_commit_index) {
                        let commit_hash = commit.hash_short.clone();
                        // Load the commit diff
                        match crate::git::operations::get_commit_diff(
                            &git_state.worktree_path,
                            &commit_hash,
                        ) {
                            Ok(diff_lines) => {
                                git_state.diff_content = diff_lines;
                                git_state.diff_scroll_offset = 0;
                                // Switch to Diff tab to show the commit diff
                                git_state.active_tab = crate::components::git_view::GitTab::Diff;
                            }
                            Err(e) => {
                                tracing::error!("Failed to get commit diff: {}", e);
                                state.add_error_notification(format!(
                                    "Failed to load commit diff: {}",
                                    e
                                ));
                            }
                        }
                    }
                }
            }
            AppEvent::GitViewToggleFolder => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.toggle_folder();
                }
            }
            AppEvent::GitViewExpandAll => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.expand_all_folders();
                }
            }
            AppEvent::GitViewCollapseAll => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.collapse_all_folders();
                }
            }
            AppEvent::GitViewCommitPush => {
                state.git_commit_and_push();
            }
            AppEvent::GitViewBack => {
                // Return to the previous view (where user was before opening Git view)
                state.current_screen = state
                    .previous_screen
                    .take()
                    .unwrap_or(crate::app::screens::ids::SESSION_LIST.to_string());
                state.git_view_state = None;
            }
            // Commit message input events
            AppEvent::GitViewStartCommit => {
                tracing::info!("Processing GitViewStartCommit event");
                if let Some(ref mut git_state) = state.git_view_state {
                    tracing::info!("Git state found, starting commit message input");
                    git_state.start_commit_message_input();
                    state.add_info_notification(
                        "📝 Enter commit message and press Enter to commit & push".to_string(),
                    );
                } else {
                    tracing::warn!("No git state available for GitViewStartCommit");
                }
            }
            AppEvent::GitViewCommitInputChar(ch) => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.add_char_to_commit_message(ch);
                }
            }
            AppEvent::GitViewCommitBackspace => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.backspace_commit_message();
                }
            }
            AppEvent::GitViewCommitCursorLeft => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.move_commit_cursor_left();
                }
            }
            AppEvent::GitViewCommitCursorRight => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.move_commit_cursor_right();
                }
            }
            AppEvent::GitViewCommitCancel => {
                if let Some(ref mut git_state) = state.git_view_state {
                    git_state.cancel_commit_message_input();
                }
            }
            AppEvent::GitViewCommitConfirm => {
                state.git_commit_and_push();
            }
            AppEvent::GitCommitAndPush => {
                tracing::info!("Direct git commit and push from main view");
                state.git_commit_and_push();
            }
            AppEvent::QuickCommitStart => {
                tracing::info!("Starting quick commit dialog");
                state.start_quick_commit();
            }
            AppEvent::QuickCommitInputChar(ch) => {
                state.add_char_to_quick_commit(ch);
            }
            AppEvent::QuickCommitBackspace => {
                state.backspace_quick_commit();
            }
            AppEvent::QuickCommitCursorLeft => {
                state.move_quick_commit_cursor_left();
            }
            AppEvent::QuickCommitCursorRight => {
                state.move_quick_commit_cursor_right();
            }
            AppEvent::QuickCommitConfirm => {
                state.confirm_quick_commit();
            }
            AppEvent::QuickCommitCancel => {
                state.cancel_quick_commit();
            }
            AppEvent::GitCommitSuccess(message) => {
                tracing::info!("Git commit successful: {}", message);
                // Add success notification
                state.add_success_notification(format!("✅ {}", message));
                // Exit git view and return to home screen
                state.current_screen = crate::app::screens::ids::HOME.to_string();
                state.git_view_state = None;
                tracing::info!("Returned to home screen after successful commit");
            }
            // AINB 2.0: Home screen events
            AppEvent::HomeScreenSelectTile => {
                use crate::app::state::HomeTile;
                tracing::info!("HomeScreenSelectTile event - processing tile selection");
                if let Some(tile) = state.home_screen_state.selected().cloned() {
                    tracing::info!("Selected tile: {:?}", tile);
                    match tile {
                        HomeTile::Agents => {
                            tracing::info!("Navigating to AgentSelection view");
                            state.current_screen = screen_ids::AGENT_SELECTION.to_string();
                        }
                        HomeTile::Sessions => {
                            tracing::info!("Navigating to SessionList view");
                            state.current_screen = screen_ids::SESSION_LIST.to_string();
                        }
                        HomeTile::Help => {
                            tracing::info!("Toggling help overlay visible");
                            state.help_visible = true;
                        }
                        HomeTile::Config => {
                            tracing::info!("Navigating to Config view");
                            state.current_screen = screen_ids::CONFIG.to_string();
                        }
                        HomeTile::Recovery => {
                            tracing::info!("Navigating to SessionRecovery view");
                            state.current_screen = screen_ids::SESSION_RECOVERY.to_string();
                        }
                        HomeTile::SkillManager => {
                            tracing::info!("Navigating to SkillManager view (spec §10.1)");
                            state.current_screen = screen_ids::SKILL_MANAGER.to_string();
                        }
                        HomeTile::Catalog | HomeTile::Stats => {
                            tracing::info!("Tile {:?} - Coming Soon", tile);
                            // Coming soon - show notification
                            state.add_info_notification(format!(
                                "{} {} - Coming Soon!",
                                tile.icon(),
                                tile.label()
                            ));
                        }
                    }
                } else {
                    tracing::warn!("No tile selected in HomeScreenState");
                }
            }
            AppEvent::HomeScreenNavigateUp => {
                tracing::debug!("HomeScreen navigate up");
                state.home_screen_state.select_up();
            }
            AppEvent::HomeScreenNavigateDown => {
                tracing::debug!("HomeScreen navigate down");
                state.home_screen_state.select_down();
            }
            AppEvent::HomeScreenNavigateLeft => {
                tracing::debug!("HomeScreen navigate left");
                state.home_screen_state.select_left();
            }
            AppEvent::HomeScreenNavigateRight => {
                tracing::debug!("HomeScreen navigate right");
                state.home_screen_state.select_right();
            }
            // AINB 2.0: Home screen V2 events
            AppEvent::HomeScreenSidebarUp => {
                tracing::debug!("HomeScreen V2 sidebar up");
                state.home_screen_v2_state.sidebar.move_up();
            }
            AppEvent::HomeScreenSidebarDown => {
                tracing::debug!("HomeScreen V2 sidebar down");
                state.home_screen_v2_state.sidebar.move_down();
            }
            AppEvent::HomeScreenSidebarSelect => {
                use crate::components::sidebar::SidebarItem;
                tracing::debug!("HomeScreen V2 sidebar select");
                let selected = state.home_screen_v2_state.sidebar.selected_item();
                match selected {
                    SidebarItem::Agents => {
                        state.current_screen = screen_ids::AGENT_SELECTION.to_string();
                    }
                    SidebarItem::Catalog => {
                        state.add_info_notification("Skill catalog coming soon!".to_string());
                    }
                    SidebarItem::Config => {
                        state.current_screen = screen_ids::CONFIG.to_string();
                    }
                    SidebarItem::Sessions => {
                        state.current_screen = screen_ids::SESSION_LIST.to_string();
                    }
                    SidebarItem::Inbox => {
                        state.previous_screen = Some(state.current_screen.clone());
                        state.current_screen = screen_ids::INBOX.to_string();
                        state.inbox_state.refresh();
                    }
                    SidebarItem::Recovery => {
                        state.session_recovery_state.refresh();
                        state.current_screen = screen_ids::SESSION_RECOVERY.to_string();
                    }
                    SidebarItem::Logs => {
                        // Initialize log history viewer with log directory
                        if let Some(log_dir) = state.log_dir() {
                            state.log_history_state.set_log_dir(log_dir);
                        }
                        state.log_history_state.show();
                        state.current_screen = screen_ids::LOG_HISTORY.to_string();
                    }
                    SidebarItem::Stats => {
                        tracing::info!("Navigating to Usage Analytics from sidebar");
                        state.current_screen = screen_ids::ANALYTICS.to_string();
                        // Data load lives inside the burndown plugin
                        // now (Phase 3 cutover); host no longer
                        // pre-populates state for the analytics screen.
                    }
                    SidebarItem::Skills => {
                        tracing::info!("Navigating to Skills from sidebar");
                        state.current_screen = screen_ids::SKILLS.to_string();
                        state.start_background_skills_load(false);
                    }
                    SidebarItem::SkillManager => {
                        tracing::info!("Navigating to SkillManager from sidebar (spec §10.1)");
                        state.current_screen = screen_ids::SKILL_MANAGER.to_string();
                        // Mirror the discovery flow from the `m` keybind
                        // handler (AppEvent::GoToSkillManager) — sidebar entry
                        // must trigger the same hdt.9 live-data rehydrate +
                        // hdt.6 banner overlay, otherwise the screen opens
                        // empty and the user never sees their orphan units.
                        let ainb_home = ainb_skill_core::default_ainb_home();
                        state.skill_manager_state.reload_from_disk(&ainb_home);
                        // Also start the drift poll (bead v12.E.4).
                        let backend: std::sync::Arc<
                            dyn ainb_skill_core::drift::DriftBackend + Send + Sync,
                        > = std::sync::Arc::new(
                            ainb_skill_core::drift::GitLsRemoteBackend::new(),
                        );
                        state.start_background_drift_load(&ainb_home, backend);
                        let claude_home = std::env::var_os("HOME")
                            .map(std::path::PathBuf::from)
                            .map(|h| h.join(".claude"))
                            .unwrap_or_else(|| std::path::PathBuf::from(".claude"));
                        let walker =
                            crate::components::skill_manager_screen::run_discovery_walkers(
                                &claude_home,
                            );
                        crate::components::skill_manager_screen::maybe_show_discovery_banner(
                            &mut state.skill_manager_state,
                            &ainb_home,
                            walker,
                        );
                    }
                    SidebarItem::Changelog => {
                        state.current_screen = screen_ids::CHANGELOG.to_string();
                    }
                    SidebarItem::Setup => {
                        state.current_screen = screen_ids::SETUP_MENU.to_string();
                    }
                    SidebarItem::Help => {
                        state.help_visible = true;
                    }
                }
            }
            AppEvent::HomeScreenToggleFocus => {
                tracing::debug!("HomeScreen V2 toggle focus");
                state.home_screen_v2_state.toggle_focus();
            }
            AppEvent::StarSelectedWorkspace => {
                tracing::info!("StarSelectedWorkspace event triggered");
                if let Some(workspace_idx) = state.selected_workspace_index {
                    if let Some(workspace) = state.workspaces.get(workspace_idx) {
                        let workspace_name = workspace.name.clone();

                        // Load favorites store
                        let mut favorites_store = crate::config::FavoritesStore::load();

                        // Try to get the remote URL from the git repository
                        // This allows favoriting the REMOTE repo, not just the local path
                        let (source, source_type, display_source) = if let Ok(git_repo) =
                            crate::git::RepositoryManager::open(&workspace.path)
                        {
                            if let Ok(Some(remote_url)) = git_repo.get_remote_url() {
                                // Parse the remote URL to get owner/repo.
                                // `from_input` is deprecated for new
                                // free-form input (finding #14), but the
                                // URL here is already validated by
                                // `get_remote_url()` so the legacy
                                // fallible contract is fine.
                                #[allow(deprecated)]
                                if let Ok(repo_source) =
                                    crate::git::RepoSource::from_input(&remote_url)
                                {
                                    if let Ok(parsed) = repo_source.parse_components() {
                                        // Use GitHub shorthand if it's a GitHub repo
                                        if parsed.host == "github.com" {
                                            let shorthand =
                                                format!("{}/{}", parsed.owner, parsed.repo_name);
                                            (
                                                shorthand.clone(),
                                                crate::config::FavoriteSourceType::GithubShorthand,
                                                shorthand,
                                            )
                                        } else {
                                            // For other hosts, use the full URL
                                            let source_type = if remote_url.starts_with("git@") {
                                                crate::config::FavoriteSourceType::SshUrl
                                            } else {
                                                crate::config::FavoriteSourceType::HttpsUrl
                                            };
                                            let display =
                                                format!("{}/{}", parsed.owner, parsed.repo_name);
                                            (remote_url, source_type, display)
                                        }
                                    } else {
                                        // Couldn't parse, use raw URL
                                        let source_type = if remote_url.starts_with("git@") {
                                            crate::config::FavoriteSourceType::SshUrl
                                        } else {
                                            crate::config::FavoriteSourceType::HttpsUrl
                                        };
                                        (remote_url.clone(), source_type, remote_url)
                                    }
                                } else {
                                    // Fallback to local path
                                    let path_str = workspace.path.display().to_string();
                                    (
                                        path_str.clone(),
                                        crate::config::FavoriteSourceType::LocalPath,
                                        path_str,
                                    )
                                }
                            } else {
                                // No remote, use local path
                                let path_str = workspace.path.display().to_string();
                                (
                                    path_str.clone(),
                                    crate::config::FavoriteSourceType::LocalPath,
                                    path_str,
                                )
                            }
                        } else {
                            // Not a git repo, use local path
                            let path_str = workspace.path.display().to_string();
                            (
                                path_str.clone(),
                                crate::config::FavoriteSourceType::LocalPath,
                                path_str,
                            )
                        };

                        // Toggle: remove if exists, add if not
                        // Check both source and local path for existing favorites
                        let local_path_str = workspace.path.display().to_string();
                        let existing = favorites_store
                            .favorites
                            .iter()
                            .find(|f| f.source == source || f.source == local_path_str);

                        if let Some(existing) = existing {
                            let alias = existing.alias.clone();
                            let removed_source = existing.source.clone();
                            favorites_store.remove(&alias);
                            if let Err(e) = favorites_store.save() {
                                tracing::error!("Failed to save favorites: {}", e);
                            }
                            tracing::info!("Removed from favorites: {}", removed_source);
                            state.add_success_notification(format!(
                                "★ Removed '{}' from favorites",
                                workspace_name
                            ));
                        } else {
                            // Generate alias from workspace name
                            let alias = workspace_name.to_lowercase().replace(' ', "-");
                            let favorite = crate::config::Favorite::new(
                                alias.clone(),
                                source.clone(),
                                source_type.clone(),
                            );
                            if favorites_store.add(favorite).is_ok() {
                                if let Err(e) = favorites_store.save() {
                                    tracing::error!("Failed to save favorites: {}", e);
                                }
                                tracing::info!("Added to favorites: {} as {}", source, alias);
                                state.add_success_notification(format!(
                                    "⭐ Added '{}' to favorites",
                                    display_source
                                ));
                            } else {
                                // Alias already exists, try with a suffix
                                let alias_with_suffix =
                                    format!("{}-{}", alias, chrono::Utc::now().timestamp() % 1000);
                                let favorite = crate::config::Favorite::new(
                                    alias_with_suffix.clone(),
                                    source.clone(),
                                    source_type,
                                );
                                let _ = favorites_store.add(favorite);
                                if let Err(e) = favorites_store.save() {
                                    tracing::error!("Failed to save favorites: {}", e);
                                }
                                tracing::info!(
                                    "Added to favorites: {} as {}",
                                    source,
                                    alias_with_suffix
                                );
                                state.add_success_notification(format!(
                                    "⭐ Added '{}' to favorites",
                                    display_source
                                ));
                            }
                        }
                    }
                }
            }
            AppEvent::WelcomePanelScrollUp => {
                tracing::debug!("Welcome panel scroll up");
                state.home_screen_v2_state.welcome.scroll_up();
            }
            AppEvent::WelcomePanelScrollDown => {
                tracing::debug!("Welcome panel scroll down");
                state.home_screen_v2_state.welcome.scroll_down();
            }
            AppEvent::WelcomePanelPageUp => {
                tracing::debug!("Welcome panel page up");
                state.home_screen_v2_state.welcome.page_up();
            }
            AppEvent::WelcomePanelPageDown => {
                tracing::debug!("Welcome panel page down");
                state.home_screen_v2_state.welcome.page_down();
            }
            AppEvent::WelcomePanelCopyContent => {
                tracing::debug!("Welcome panel copy content");
                match state.home_screen_v2_state.welcome.copy_content_to_clipboard() {
                    Ok(()) => {
                        state.add_success_notification("Content copied to clipboard".to_string());
                    }
                    Err(e) => {
                        state.add_error_notification(format!("Failed to copy: {}", e));
                    }
                }
            }
            AppEvent::GoToAgentSelection => {
                tracing::info!("Navigating to AgentSelection");
                state.current_screen = screen_ids::AGENT_SELECTION.to_string();
            }
            AppEvent::GoToCatalog => {
                state.add_info_notification("Skill catalog coming soon!".to_string());
            }
            AppEvent::GoToConfig => {
                tracing::info!("Navigating to Config");
                state.current_screen = screen_ids::CONFIG.to_string();
            }
            AppEvent::GoToSessionList => {
                tracing::info!("Navigating to SessionList");
                state.current_screen = screen_ids::SESSION_LIST.to_string();
            }
            AppEvent::GoToStats => {
                tracing::info!("Navigating to Usage Analytics");
                state.current_screen = screen_ids::ANALYTICS.to_string();
                // Plugin owns its own data load; host no longer
                // pre-populates analytics state.
            }
            AppEvent::GoToSkills => {
                tracing::info!("Navigating to Skills");
                state.current_screen = screen_ids::SKILLS.to_string();
                state.start_background_skills_load(false);
            }
            AppEvent::GoToSkillManager => {
                tracing::info!("Navigating to SkillManager (spec §10.1)");
                state.current_screen = screen_ids::SKILL_MANAGER.to_string();
                let ainb_home = ainb_skill_core::default_ainb_home();
                // P8 live-data binding (hdt.9): rehydrate Sources /
                // Units / Detail panels from $AINB_HOME/manifest.yaml
                // + lock.yaml on every screen-open so out-of-band
                // edits (e.g. `ainb migrate --discover`,
                // `ainb skill install`) are reflected without
                // requiring a TUI restart. Banner state is preserved
                // by `reload_from_disk` — the subsequent
                // `maybe_show_discovery_banner` call only flips
                // banner to Visible when the manifest is empty AND
                // walkers find candidates, so the two steps compose
                // cleanly.
                state.skill_manager_state.reload_from_disk(&ainb_home);
                // Bead v12.E.4: kick off a background drift scan so
                // the Units panel's `status` column fills in (`✓` /
                // `⚠` / `▲` / `⟷`) on the next tick. Until results
                // land, the column shows the muted "…" placeholder.
                // `start_background_drift_load` coalesces if a
                // previous scan is still in flight.
                let backend: std::sync::Arc<
                    dyn ainb_skill_core::drift::DriftBackend + Send + Sync,
                > = std::sync::Arc::new(
                    ainb_skill_core::drift::GitLsRemoteBackend::new(),
                );
                state.start_background_drift_load(&ainb_home, backend);
                // Spec §User Flow 1: on screen-enter, when the
                // manifest is empty AND we have not been told to
                // skip, run the discovery walkers and pop the
                // banner overlay. Idempotent — re-entering an
                // already-Visible banner is a no-op (the user
                // sees the same counts they did first time, per
                // spec edge case "Banner re-appears next open
                // until dismissed via [s]").
                let claude_home = std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|h| h.join(".claude"))
                    .unwrap_or_else(|| std::path::PathBuf::from(".claude"));
                let walker = crate::components::skill_manager_screen::run_discovery_walkers(
                    &claude_home,
                );
                crate::components::skill_manager_screen::maybe_show_discovery_banner(
                    &mut state.skill_manager_state,
                    &ainb_home,
                    walker,
                );
            }
            AppEvent::SkillManagerBack => {
                tracing::info!("Returning to home from SkillManager (Esc/q)");
                state.current_screen = screen_ids::HOME.to_string();
            }
            AppEvent::SkillManagerDiscoveryImport => {
                tracing::info!("Discovery banner: import all");
                let ainb_home = ainb_skill_core::default_ainb_home();
                if let Err(e) =
                    crate::components::skill_manager_screen::apply_discovery_import(
                        &mut state.skill_manager_state,
                        &ainb_home,
                    )
                {
                    tracing::warn!(error = %e, "discovery import failed");
                }
            }
            AppEvent::SkillManagerDiscoveryToggleDetails => {
                crate::components::skill_manager_screen::toggle_discovery_details(
                    &mut state.skill_manager_state,
                );
            }
            AppEvent::SkillManagerDiscoverySkip => {
                tracing::info!("Discovery banner: skip + persist marker");
                let ainb_home = ainb_skill_core::default_ainb_home();
                if let Err(e) =
                    crate::components::skill_manager_screen::apply_discovery_skip(
                        &mut state.skill_manager_state,
                        &ainb_home,
                    )
                {
                    tracing::warn!(error = %e, "discovery skip failed");
                }
            }
            AppEvent::SkillManagerSync => {
                // Phase D (v12.D.5): run `ainb skill sync` for the
                // selected unit. The actual sync runs out-of-band via
                // the CLI surface; here we only fire-and-forget the
                // intent + reload the screen state so a successful
                // sync surfaces fresh deployed paths / usage on next
                // paint. Tests assert routing-only behaviour against
                // the dispatch table; integration tests for the CLI
                // path live in `ainb-cli/tests/skill_sync_*`.
                //
                // Surface a `sync: <unit>` info notification so the user
                // sees that `[s]` routed to Sync (not ConflictFlip) and
                // so the live tmux tripwire (v12.1.T3) can observe the
                // routing decision in the captured pane.
                tracing::info!("Units panel: sync selected unit");
                let unit_name = state
                    .skill_manager_state
                    .units
                    .get(state.skill_manager_state.selected)
                    .map(|u| u.name.clone());
                let ainb_home = ainb_skill_core::default_ainb_home();
                state.skill_manager_state.reload_from_disk(&ainb_home);
                if let Some(name) = unit_name {
                    state.add_info_notification(format!("sync: {name}"));
                }
            }
            AppEvent::SkillManagerConflictFlip => {
                tracing::info!("Units panel: flip shadowed_by on selected unit");
                let ainb_home = ainb_skill_core::default_ainb_home();
                let unit_name = state
                    .skill_manager_state
                    .units
                    .get(state.skill_manager_state.selected)
                    .map(|u| u.name.clone());
                match crate::components::skill_manager_screen::apply_conflict_flip(
                    &mut state.skill_manager_state,
                    &ainb_home,
                ) {
                    // `[s]` on a conflict-peer unit flips which side wins.
                    // Surface a toast so the keystroke isn't a silent no-op
                    // (the alternative, non-conflict, branch fires Sync).
                    Ok(()) => {
                        if let Some(name) = unit_name {
                            state.add_info_notification(format!("shadow flipped: {name}"));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "conflict flip failed");
                        state.add_error_notification(format!("conflict flip failed: {e}"));
                    }
                }
            }
            AppEvent::SkillManagerRefreshDiscovery => {
                // `[m]` — explicit discovery refresh. Re-walk the tool
                // homes + force the banner even past a prior skip-marker.
                tracing::info!("SkillManager: refresh discovery (m)");
                let ainb_home = ainb_skill_core::default_ainb_home();
                state.skill_manager_state.reload_from_disk(&ainb_home);
                let claude_home = std::env::var_os("HOME")
                    .map(std::path::PathBuf::from)
                    .map(|h| h.join(".claude"))
                    .unwrap_or_else(|| std::path::PathBuf::from(".claude"));
                let walker = crate::components::skill_manager_screen::run_discovery_walkers(
                    &claude_home,
                );
                crate::components::skill_manager_screen::force_show_discovery_banner(
                    &mut state.skill_manager_state,
                    &ainb_home,
                    walker,
                );
                if !state.skill_manager_state.banner.is_active() {
                    state.add_info_notification(
                        "discovery: no un-adopted units found".to_string(),
                    );
                }
            }
            AppEvent::SkillManagerCheck => {
                // `[c]` — re-run the background drift scan so the Units
                // status column (✓ / ⚠ / ▲ / ⟷) refreshes.
                tracing::info!("SkillManager: check drift (c)");
                let ainb_home = ainb_skill_core::default_ainb_home();
                let backend: std::sync::Arc<
                    dyn ainb_skill_core::drift::DriftBackend + Send + Sync,
                > = std::sync::Arc::new(
                    ainb_skill_core::drift::GitLsRemoteBackend::new(),
                );
                state.start_background_drift_load(&ainb_home, backend);
                state.add_info_notification(
                    "drift check running — status column refreshes shortly".to_string(),
                );
            }
            AppEvent::SkillManagerUpdate => {
                // `[u]` — re-fetch + apply for the selected unit.
                let ainb_home = ainb_skill_core::default_ainb_home();
                let uri = state
                    .skill_manager_state
                    .units
                    .get(state.skill_manager_state.selected)
                    .map(|u| u.declared_uri.clone());
                match uri {
                    None => {
                        state.add_warning_notification(
                            "update: no unit selected".to_string(),
                        );
                    }
                    Some(uri) => {
                        let cmd = ainb_cli::SkillCommand::Update(ainb_cli::UpdateArgs {
                            uri: Some(uri.clone()),
                            all: false,
                            check: false,
                            yes: true,
                            dry_run: false,
                        });
                        let (ok, msg) = run_skill_cli(&ainb_home, cmd);
                        state.skill_manager_state.reload_from_disk(&ainb_home);
                        if ok {
                            state.add_success_notification(format!("updated: {msg}"));
                        } else {
                            state.add_error_notification(format!("update failed: {msg}"));
                        }
                    }
                }
            }
            AppEvent::SkillManagerRemove => {
                // `[r]` — uninstall the selected unit from its tools.
                let ainb_home = ainb_skill_core::default_ainb_home();
                let uri = state
                    .skill_manager_state
                    .units
                    .get(state.skill_manager_state.selected)
                    .map(|u| u.declared_uri.clone());
                match uri {
                    None => {
                        state.add_warning_notification(
                            "remove: no unit selected".to_string(),
                        );
                    }
                    Some(uri) => {
                        // Two-step uninstall:
                        //   1. `skill remove --yes` tears down any deployed
                        //      tool files recorded in the lockfile.
                        //   2. drop the unit from the *manifest* so the
                        //      Units table (which is manifest-driven) loses
                        //      the row.
                        // A manifest-declared unit that was never installed
                        // has no lockfile entry, so step 1 reports "not in
                        // the lockfile" — that's not a user-facing failure,
                        // the unit still vanishes from the table. We only
                        // surface an error when neither step removed anything.
                        let cmd = ainb_cli::SkillCommand::Remove(ainb_cli::RemoveSkillArgs {
                            uri: uri.clone(),
                            targets: None,
                            yes: true,
                            dry_run: false,
                        });
                        let (lockfile_ok, msg) = run_skill_cli(&ainb_home, cmd);
                        let manifest_dropped =
                            drop_unit_from_manifest(&ainb_home, &uri);
                        state.skill_manager_state.reload_from_disk(&ainb_home);
                        if lockfile_ok {
                            state.add_success_notification(format!("removed: {msg}"));
                        } else if manifest_dropped {
                            state.add_success_notification(format!("removed: {uri}"));
                        } else {
                            state.add_error_notification(format!("remove failed: {msg}"));
                        }
                    }
                }
            }
            AppEvent::SkillManagerOpenAddSource => {
                state.skill_manager_state.input = Some(
                    crate::components::skill_manager_screen::InputState::new(
                        crate::components::skill_manager_screen::InputKind::AddSource,
                    ),
                );
            }
            AppEvent::SkillManagerOpenSearch => {
                // Pre-fill the prompt with the current filter so the
                // user can edit rather than retype.
                let mut input = crate::components::skill_manager_screen::InputState::new(
                    crate::components::skill_manager_screen::InputKind::Search,
                );
                if let Some(existing) = &state.skill_manager_state.search {
                    input.buffer = existing.clone();
                }
                state.skill_manager_state.input = Some(input);
            }
            AppEvent::SkillManagerInputChar(c) => {
                if let Some(input) = state.skill_manager_state.input.as_mut() {
                    input.buffer.push(c);
                }
            }
            AppEvent::SkillManagerInputBackspace => {
                if let Some(input) = state.skill_manager_state.input.as_mut() {
                    input.buffer.pop();
                }
            }
            AppEvent::SkillManagerInputCancel => {
                state.skill_manager_state.input = None;
            }
            AppEvent::SkillManagerInputSubmit => {
                let Some(input) = state.skill_manager_state.input.take() else {
                    return;
                };
                use crate::components::skill_manager_screen::InputKind;
                match input.kind {
                    InputKind::Search => {
                        let q = input.buffer.trim().to_lowercase();
                        state.skill_manager_state.search =
                            if q.is_empty() { None } else { Some(q) };
                    }
                    InputKind::AddSource => {
                        let uri = input.buffer.trim().to_string();
                        tracing::info!(uri = %uri, "SkillManager: add-source submit");
                        if uri.is_empty() {
                            return;
                        }
                        let ainb_home = ainb_skill_core::default_ainb_home();
                        let cmd = ainb_cli::SourceCommand::Add(ainb_cli::AddArgs {
                            uri: uri.clone(),
                            name: None,
                            kind: None,
                        });
                        let (ok, msg) = run_source_cli(&ainb_home, cmd);
                        tracing::info!(ok, msg = %msg, "SkillManager: add-source result");
                        state.skill_manager_state.reload_from_disk(&ainb_home);
                        if ok {
                            state.add_success_notification(format!("source added: {msg}"));
                        } else {
                            state.add_error_notification(format!("add source failed: {msg}"));
                        }
                    }
                }
            }
            AppEvent::SkillManagerSelectPrev => {
                let ainb_home = ainb_skill_core::default_ainb_home();
                crate::components::skill_manager_screen::move_selection(
                    &mut state.skill_manager_state,
                    &ainb_home,
                    crate::components::skill_manager_screen::SelectionMove::Prev,
                );
            }
            AppEvent::SkillManagerSelectNext => {
                let ainb_home = ainb_skill_core::default_ainb_home();
                crate::components::skill_manager_screen::move_selection(
                    &mut state.skill_manager_state,
                    &ainb_home,
                    crate::components::skill_manager_screen::SelectionMove::Next,
                );
            }
            AppEvent::SkillManagerSelectFirst => {
                let ainb_home = ainb_skill_core::default_ainb_home();
                crate::components::skill_manager_screen::move_selection(
                    &mut state.skill_manager_state,
                    &ainb_home,
                    crate::components::skill_manager_screen::SelectionMove::First,
                );
            }
            AppEvent::SkillManagerSelectLast => {
                let ainb_home = ainb_skill_core::default_ainb_home();
                crate::components::skill_manager_screen::move_selection(
                    &mut state.skill_manager_state,
                    &ainb_home,
                    crate::components::skill_manager_screen::SelectionMove::Last,
                );
            }
            AppEvent::SkillManagerOpenLibrary => {
                // `[l]` — open the own-skill Library view, sourced from
                // `library.yaml` (bead ai-lgk). Built fresh on open so
                // out-of-band `ainb skill library` edits are reflected.
                tracing::info!("SkillManager: open own-skill Library (l)");
                let ainb_home = ainb_skill_core::default_ainb_home();
                state.skill_manager_state.library = Some(
                    crate::components::skill_manager_screen::LibraryViewState::load_from_disk(
                        &ainb_home,
                    ),
                );
            }
            AppEvent::SkillManagerLibrarySelectPrev => {
                if let Some(lib) = state.skill_manager_state.library.as_mut() {
                    lib.select_prev();
                }
            }
            AppEvent::SkillManagerLibrarySelectNext => {
                if let Some(lib) = state.skill_manager_state.library.as_mut() {
                    lib.select_next();
                }
            }
            AppEvent::SkillManagerLibraryEnter => {
                // Enter expands the selected own-skill into its Detail
                // band (idempotent — pressing again keeps it open).
                if let Some(lib) = state.skill_manager_state.library.as_mut() {
                    if lib.selected_row().is_some() {
                        lib.show_detail = true;
                    }
                }
            }
            AppEvent::SkillManagerLibraryClose => {
                state.skill_manager_state.library = None;
            }
            AppEvent::SkillManagerOpenBrowse => {
                // `[b]` — open the catalog browse modal in Query mode.
                tracing::info!("SkillManager: open catalog browse (b)");
                state.skill_manager_state.browse = Some(
                    crate::components::skill_manager_screen::BrowseViewState::new(),
                );
            }
            AppEvent::SkillManagerBrowseInputChar(c) => {
                if let Some(b) = state.skill_manager_state.browse.as_mut() {
                    b.query.push(c);
                    b.status = None;
                }
            }
            AppEvent::SkillManagerBrowseInputBackspace => {
                if let Some(b) = state.skill_manager_state.browse.as_mut() {
                    b.query.pop();
                    b.status = None;
                }
            }
            AppEvent::SkillManagerBrowseSearch => {
                // Enter in Query mode — run the catalog search via the
                // production backend (mock under AINB_CATALOG_MOCK=1, so
                // the live tmux tripwire stays offline).
                let ainb_home = ainb_skill_core::default_ainb_home();
                let query = state
                    .skill_manager_state
                    .browse
                    .as_ref()
                    .map(|b| b.query.clone())
                    .unwrap_or_default();
                if query.trim().is_empty() {
                    if let Some(b) = state.skill_manager_state.browse.as_mut() {
                        b.set_error("type a query to search the catalog");
                    }
                } else {
                    let result = run_catalog_search(&ainb_home, query.trim());
                    if let Some(b) = state.skill_manager_state.browse.as_mut() {
                        match result {
                            Ok(rows) => b.set_results(rows),
                            Err(msg) => b.set_error(msg),
                        }
                    }
                }
            }
            AppEvent::SkillManagerBrowseSelectPrev => {
                if let Some(b) = state.skill_manager_state.browse.as_mut() {
                    b.select_prev();
                }
            }
            AppEvent::SkillManagerBrowseSelectNext => {
                if let Some(b) = state.skill_manager_state.browse.as_mut() {
                    b.select_next();
                }
            }
            AppEvent::SkillManagerBrowseEditQuery => {
                // `/` in Results mode — back to Query mode to refine.
                if let Some(b) = state.skill_manager_state.browse.as_mut() {
                    b.mode = crate::components::skill_manager_screen::BrowseMode::Query;
                    b.status = None;
                }
            }
            AppEvent::SkillManagerBrowseInstall => {
                // Enter on a selected result — route the hit's install_uri
                // through the existing install flow (add source + skill
                // install), exactly like the CLI does.
                let ainb_home = ainb_skill_core::default_ainb_home();
                let install_uri = state
                    .skill_manager_state
                    .browse
                    .as_ref()
                    .and_then(|b| b.selected_row().map(|r| r.install_uri.clone()));
                match install_uri {
                    None => {
                        state.add_warning_notification(
                            "browse: no result selected".to_string(),
                        );
                    }
                    Some(uri) => {
                        let (ok, msg) = install_catalog_hit(&ainb_home, &uri);
                        state.skill_manager_state.reload_from_disk(&ainb_home);
                        if ok {
                            // Close the modal on a successful install so
                            // the user lands back on the (now-updated)
                            // Units table.
                            state.skill_manager_state.browse = None;
                            state.add_success_notification(format!("installed: {msg}"));
                        } else {
                            state.add_error_notification(format!("install failed: {msg}"));
                        }
                    }
                }
            }
            AppEvent::SkillManagerBrowseClose => {
                state.skill_manager_state.browse = None;
            }
            AppEvent::GoToInbox => {
                tracing::info!("Navigating to Inbox");
                state.previous_screen = Some(state.current_screen.clone());
                state.current_screen = screen_ids::INBOX.to_string();
                state.inbox_state.refresh();
            }
            AppEvent::InboxMoveUp => state.inbox_state.move_up(1),
            AppEvent::InboxMoveDown => state.inbox_state.move_down(1),
            AppEvent::InboxPageUp => state.inbox_state.move_up(10),
            AppEvent::InboxPageDown => state.inbox_state.move_down(10),
            AppEvent::InboxOpenSelected => {
                // Capture the cwd before mark_selected_read invalidates
                // selection ordering on refresh.
                let row_cwd = state
                    .inbox_state
                    .selected_row()
                    .map(|r| r.cwd.clone())
                    .unwrap_or_default();
                state.inbox_state.mark_selected_read();
                // cwd-based jump-to-tmux: find the ainb session whose
                // workspace_path matches the notification's cwd (exact
                // or prefix). If found, surface its tmux session name
                // for the existing AttachToOtherTmux async action so
                // ainb's tmux subsystem owns the attach itself.
                if !row_cwd.is_empty() {
                    let target = state
                        .workspaces
                        .iter()
                        .find(|ws| {
                            let p = ws.path.to_string_lossy().to_string();
                            row_cwd == p
                                || row_cwd.starts_with(&format!("{p}/"))
                        })
                        .and_then(|ws| {
                            // Prefer a non-shell session (an agent-running
                            // one) since hook events come from agents,
                            // not shells. Fall back to the workspace
                            // shell if no agent session has tmux.
                            ws.sessions
                                .iter()
                                .find_map(|s| s.tmux_session_name.clone())
                                .or_else(|| {
                                    ws.shell_session
                                        .as_ref()
                                        .map(|s| s.tmux_session_name.clone())
                                })
                        });
                    if let Some(tmux_name) = target {
                        tracing::info!(
                            cwd = %row_cwd,
                            tmux = %tmux_name,
                            "inbox: jumping to tmux session"
                        );
                        state.pending_async_action =
                            Some(crate::app::state::AsyncAction::AttachToOtherTmux(
                                tmux_name,
                            ));
                    } else {
                        state.add_info_notification(format!(
                            "no ainb session matches cwd {row_cwd}"
                        ));
                    }
                }
            }
            AppEvent::InboxDismissSelected => {
                state.inbox_state.dismiss_selected();
            }
            AppEvent::InboxDismissVisible => {
                let n = state.inbox_state.dismiss_visible();
                state.add_info_notification(format!("dismissed {n} row(s)"));
            }
            AppEvent::InboxToggleArchived => state.inbox_state.toggle_archived(),
            AppEvent::InboxCycleAgent => state.inbox_state.cycle_agent_filter(),
            AppEvent::InboxRefresh => state.inbox_state.refresh(),
            AppEvent::GoToRecovery => {
                tracing::info!("Navigating to Session Recovery");
                state.session_recovery_state.refresh();
                state.current_screen = screen_ids::SESSION_RECOVERY.to_string();
            }
            // AINB 2.0: Agent selection events
            AppEvent::AgentSelectionBack => {
                state.current_screen = screen_ids::HOME.to_string();
            }
            AppEvent::AgentSelectionNextProvider => {
                state.agent_selection_state.select_next_provider();
            }
            AppEvent::AgentSelectionPrevProvider => {
                state.agent_selection_state.select_prev_provider();
            }
            AppEvent::AgentSelectionNextModel => {
                state.agent_selection_state.select_next_model();
            }
            AppEvent::AgentSelectionPrevModel => {
                state.agent_selection_state.select_prev_model();
            }
            AppEvent::AgentSelectionToggleExpand => {
                state.agent_selection_state.toggle_expand();
            }
            AppEvent::AgentSelectionSelect => {
                if state.agent_selection_state.is_current_available() {
                    // Store selected agent and proceed to session creation
                    state.add_success_notification(format!(
                        "Selected: {} - {}",
                        state
                            .agent_selection_state
                            .current_provider()
                            .map(|p| p.name.as_str())
                            .unwrap_or("Unknown"),
                        state
                            .agent_selection_state
                            .current_model()
                            .map(|m| m.name.as_str())
                            .unwrap_or("Unknown")
                    ));
                    // Go to session list or new session
                    state.current_screen = screen_ids::SESSION_LIST.to_string();
                } else {
                    state.add_warning_notification("This agent is not available yet.".to_string());
                }
            }
            // AINB 2.0: Config screen events
            AppEvent::ConfigBack => {
                tracing::info!("Navigating back from Config to HomeScreen");
                state.current_screen = screen_ids::HOME.to_string();
            }
            AppEvent::ConfigNextCategory => {
                let num_categories = state.config_screen_state.categories.len();
                if num_categories > 0 {
                    state.config_screen_state.selected_category =
                        (state.config_screen_state.selected_category + 1) % num_categories;
                    state.config_screen_state.selected_setting = 0;
                }
            }
            AppEvent::ConfigPrevCategory => {
                let num_categories = state.config_screen_state.categories.len();
                if num_categories > 0 {
                    state.config_screen_state.selected_category = state
                        .config_screen_state
                        .selected_category
                        .checked_sub(1)
                        .unwrap_or(num_categories - 1);
                    state.config_screen_state.selected_setting = 0;
                }
            }
            AppEvent::ConfigNextSetting => {
                let current_category = &state.config_screen_state.categories
                    [state.config_screen_state.selected_category];
                if let Some(settings) = state.config_screen_state.settings.get(current_category) {
                    if !settings.is_empty() {
                        state.config_screen_state.selected_setting =
                            (state.config_screen_state.selected_setting + 1) % settings.len();
                    }
                }
            }
            AppEvent::ConfigPrevSetting => {
                let current_category = &state.config_screen_state.categories
                    [state.config_screen_state.selected_category];
                if let Some(settings) = state.config_screen_state.settings.get(current_category) {
                    if !settings.is_empty() {
                        state.config_screen_state.selected_setting = state
                            .config_screen_state
                            .selected_setting
                            .checked_sub(1)
                            .unwrap_or(settings.len() - 1);
                    }
                }
            }
            AppEvent::ConfigSwitchPane => {
                // Toggle focus between categories and settings panes
                state.config_screen_state.focused_pane =
                    match state.config_screen_state.focused_pane {
                        ConfigPane::Categories => ConfigPane::Settings,
                        ConfigPane::Settings => ConfigPane::Categories,
                    };
                tracing::debug!(
                    "Config switch pane - focus is now on {:?}",
                    state.config_screen_state.focused_pane
                );
            }
            AppEvent::ConfigNavigateUp => {
                // Navigate up within the currently focused pane
                match state.config_screen_state.focused_pane {
                    ConfigPane::Categories => {
                        // Navigate to previous category
                        let num_categories = state.config_screen_state.categories.len();
                        if num_categories > 0 {
                            state.config_screen_state.selected_category = state
                                .config_screen_state
                                .selected_category
                                .checked_sub(1)
                                .unwrap_or(num_categories - 1);
                            state.config_screen_state.selected_setting = 0;
                        }
                    }
                    ConfigPane::Settings => {
                        // Navigate to previous setting
                        let current_category = &state.config_screen_state.categories
                            [state.config_screen_state.selected_category];
                        if let Some(settings) =
                            state.config_screen_state.settings.get(current_category)
                        {
                            if !settings.is_empty() {
                                state.config_screen_state.selected_setting = state
                                    .config_screen_state
                                    .selected_setting
                                    .checked_sub(1)
                                    .unwrap_or(settings.len() - 1);
                            }
                        }
                    }
                }
            }
            AppEvent::ConfigNavigateDown => {
                // Navigate down within the currently focused pane
                match state.config_screen_state.focused_pane {
                    ConfigPane::Categories => {
                        // Navigate to next category
                        let num_categories = state.config_screen_state.categories.len();
                        if num_categories > 0 {
                            state.config_screen_state.selected_category =
                                (state.config_screen_state.selected_category + 1) % num_categories;
                            state.config_screen_state.selected_setting = 0;
                        }
                    }
                    ConfigPane::Settings => {
                        // Navigate to next setting
                        let current_category = &state.config_screen_state.categories
                            [state.config_screen_state.selected_category];
                        if let Some(settings) =
                            state.config_screen_state.settings.get(current_category)
                        {
                            if !settings.is_empty() {
                                state.config_screen_state.selected_setting =
                                    (state.config_screen_state.selected_setting + 1)
                                        % settings.len();
                            }
                        }
                    }
                }
            }
            AppEvent::ConfigFocusCategories => {
                state.config_screen_state.focused_pane = ConfigPane::Categories;
                tracing::debug!("Config focus switched to Categories pane");
            }
            AppEvent::ConfigFocusSettings => {
                state.config_screen_state.focused_pane = ConfigPane::Settings;
                tracing::debug!("Config focus switched to Settings pane");
            }
            AppEvent::ConfigEditSetting => {
                let current_category = state.config_screen_state.categories
                    [state.config_screen_state.selected_category];
                if let Some(settings) = state.config_screen_state.settings.get(&current_category) {
                    if let Some(setting) = settings.get(state.config_screen_state.selected_setting)
                    {
                        // Open popup based on setting type
                        let title = setting.label.clone();
                        let description = setting.description.clone();
                        let key = setting.key.clone();

                        match &setting.value {
                            crate::app::state::ConfigValue::Choice(options, selected_idx) => {
                                state.config_popup_state.open_choice(
                                    &title,
                                    &description,
                                    &key,
                                    options.clone(),
                                    *selected_idx,
                                );
                            }
                            crate::app::state::ConfigValue::Text(text) => {
                                state.config_popup_state.open_text(
                                    &title,
                                    &description,
                                    &key,
                                    text,
                                );
                            }
                            crate::app::state::ConfigValue::Secret(_) => {
                                // For secrets, show empty input (don't reveal existing value)
                                state.config_popup_state.open_text(&title, &description, &key, "");
                            }
                            crate::app::state::ConfigValue::Bool(value) => {
                                state.config_popup_state.open_boolean(
                                    &title,
                                    &description,
                                    &key,
                                    *value,
                                );
                            }
                            crate::app::state::ConfigValue::Number(value) => {
                                state.config_popup_state.open_number(
                                    &title,
                                    &description,
                                    &key,
                                    *value,
                                );
                            }
                        }
                        tracing::info!("Opened popup for setting: {}", setting.label);
                    }
                }
            }
            AppEvent::ConfigSaveEdit => {
                let current_category = state.config_screen_state.categories
                    [state.config_screen_state.selected_category];
                if let Some(settings) =
                    state.config_screen_state.settings.get_mut(&current_category)
                {
                    if let Some(setting) =
                        settings.get_mut(state.config_screen_state.selected_setting)
                    {
                        let new_value = state.config_screen_state.edit_buffer.clone();
                        // Update the value based on the type
                        setting.value = match &setting.value {
                            crate::app::state::ConfigValue::Text(_) => {
                                crate::app::state::ConfigValue::Text(new_value)
                            }
                            crate::app::state::ConfigValue::Secret(_) => {
                                crate::app::state::ConfigValue::Secret(new_value)
                            }
                            crate::app::state::ConfigValue::Bool(_) => {
                                crate::app::state::ConfigValue::Bool(
                                    new_value.to_lowercase() == "true",
                                )
                            }
                            crate::app::state::ConfigValue::Number(_) => {
                                crate::app::state::ConfigValue::Number(
                                    new_value.parse().unwrap_or(0),
                                )
                            }
                            crate::app::state::ConfigValue::Choice(options, _) => {
                                // Try to find the index of the entered value
                                let idx = options.iter().position(|o| o == &new_value).unwrap_or(0);
                                crate::app::state::ConfigValue::Choice(options.clone(), idx)
                            }
                        };
                        tracing::info!(
                            "Saved setting: {} = {}",
                            setting.label,
                            setting.value.display()
                        );
                    }
                }
                state.config_screen_state.editing = false;
                state.config_screen_state.edit_buffer.clear();
            }
            AppEvent::ConfigCancelEdit => {
                state.config_screen_state.editing = false;
                state.config_screen_state.edit_buffer.clear();
                tracing::info!("Cancelled editing");
            }
            AppEvent::ConfigEditChar(c) => {
                state.config_screen_state.edit_buffer.push(c);
            }
            AppEvent::ConfigEditBackspace => {
                state.config_screen_state.edit_buffer.pop();
            }
            AppEvent::ConfigSaveAll => {
                tracing::info!("Saving all settings to config file");

                // Apply ConfigScreenState settings to AppConfig
                state.config_screen_state.apply_to_app_config(&mut state.app_config);

                // Save to disk
                match state.app_config.save() {
                    Ok(()) => {
                        state.add_success_notification("Settings saved to config.toml".to_string());
                        tracing::info!("Settings saved to ~/.agents-in-a-box/config/config.toml");
                    }
                    Err(e) => {
                        state.add_error_notification(format!("Failed to save settings: {}", e));
                        tracing::error!("Failed to save config: {}", e);
                    }
                }
            }
            // API Key configuration events
            AppEvent::ConfigApiKeyStart => {
                tracing::info!("Starting API key input mode");
                state.config_screen_state.api_key_input_mode = true;
                state.config_screen_state.edit_buffer.clear();
                state.add_info_notification(
                    "Enter your Anthropic API key (starts with sk-ant-)".to_string(),
                );
            }
            AppEvent::ConfigApiKeySave => {
                let api_key = state.config_screen_state.edit_buffer.clone();
                tracing::info!("Saving API key to keychain");

                match credentials::store_anthropic_api_key(&api_key) {
                    Ok(()) => {
                        state.add_success_notification(
                            "API key saved to system keychain".to_string(),
                        );
                        tracing::info!("API key successfully stored in keychain");

                        // Update auth status to show API key configured
                        let masked = credentials::get_anthropic_api_key_masked();
                        let status = format!("API Key ({})", masked);
                        let auth_category = crate::app::state::ConfigCategory::Authentication;
                        if let Some(settings) =
                            state.config_screen_state.settings.get_mut(&auth_category)
                        {
                            if let Some(status_setting) =
                                settings.iter_mut().find(|s| s.key == "claude_auth")
                            {
                                status_setting.value = crate::app::state::ConfigValue::Text(status);
                            }
                        }
                    }
                    Err(e) => {
                        state.add_error_notification(format!("Failed to save API key: {}", e));
                        tracing::error!("Failed to store API key: {}", e);
                    }
                }

                state.config_screen_state.api_key_input_mode = false;
                state.config_screen_state.edit_buffer.clear();
            }
            AppEvent::ConfigApiKeyDelete => {
                tracing::info!("Deleting API key from keychain");

                match credentials::delete_anthropic_api_key() {
                    Ok(()) => {
                        state.add_success_notification(
                            "API key removed from system keychain".to_string(),
                        );
                        tracing::info!("API key successfully deleted from keychain");

                        // Update auth status to show system auth
                        let auth_category = crate::app::state::ConfigCategory::Authentication;
                        if let Some(settings) =
                            state.config_screen_state.settings.get_mut(&auth_category)
                        {
                            if let Some(status_setting) =
                                settings.iter_mut().find(|s| s.key == "claude_auth")
                            {
                                status_setting.value = crate::app::state::ConfigValue::Text(
                                    "System Auth (Pro/Max Plan)".to_string(),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        state.add_error_notification(format!("Failed to delete API key: {}", e));
                        tracing::error!("Failed to delete API key: {}", e);
                    }
                }
            }
            // Auth provider popup events
            AppEvent::AuthProviderPopupOpen => {
                tracing::info!("Opening auth provider popup");
                state.auth_provider_popup_state.show_popup = true;
                state.auth_provider_popup_state.refresh_providers();
            }
            AppEvent::AuthProviderPopupClose => {
                tracing::info!("Closing auth provider popup");
                state.auth_provider_popup_state.show_popup = false;
                state.auth_provider_popup_state.is_entering_key = false;
                state.auth_provider_popup_state.api_key_input.clear();
            }
            AppEvent::AuthProviderPopupNext => {
                state.auth_provider_popup_state.select_next();
            }
            AppEvent::AuthProviderPopupPrev => {
                state.auth_provider_popup_state.select_prev();
            }
            AppEvent::AuthProviderPopupSelect => {
                let popup_state = &state.auth_provider_popup_state;

                if popup_state.is_entering_key {
                    // Save the API key
                    let api_key = popup_state.api_key_input.clone();
                    tracing::info!("Saving API key from popup");

                    match credentials::store_anthropic_api_key(&api_key) {
                        Ok(()) => {
                            state.add_success_notification(
                                "API key saved to system keychain".to_string(),
                            );

                            // Update config screen status
                            let masked = credentials::get_anthropic_api_key_masked();
                            let status = format!("API Key ({})", masked);
                            let auth_category = crate::app::state::ConfigCategory::Authentication;
                            if let Some(settings) =
                                state.config_screen_state.settings.get_mut(&auth_category)
                            {
                                if let Some(status_setting) =
                                    settings.iter_mut().find(|s| s.key == "claude_auth")
                                {
                                    status_setting.value =
                                        crate::app::state::ConfigValue::Text(status);
                                }
                            }

                            // Persist auth provider to config.toml
                            state.app_config.authentication.claude_provider =
                                crate::config::ClaudeAuthProvider::ApiKey;
                            if let Err(e) = state.app_config.save() {
                                tracing::warn!("Failed to save config: {}", e);
                            }

                            // Close popup and refresh
                            state.auth_provider_popup_state.show_popup = false;
                            state.auth_provider_popup_state.is_entering_key = false;
                            state.auth_provider_popup_state.api_key_input.clear();
                            state.auth_provider_popup_state.refresh_providers();
                        }
                        Err(e) => {
                            state.add_error_notification(format!("Failed to save API key: {}", e));
                        }
                    }
                } else {
                    // Check what's selected
                    if let Some(provider) = popup_state.current_provider() {
                        if !provider.available {
                            state
                                .add_info_notification(format!("{} - Coming Soon!", provider.name));
                        } else if provider.id == "api_key" {
                            // Start API key input mode
                            state.auth_provider_popup_state.start_key_input();
                        } else if provider.id == "system" {
                            // System auth - just close and confirm
                            state.add_success_notification(
                                "Using system authentication (Pro/Max plan)".to_string(),
                            );

                            // Delete any stored API key to switch to system auth
                            let _ = credentials::delete_anthropic_api_key();

                            // Update config screen status
                            let auth_category = crate::app::state::ConfigCategory::Authentication;
                            if let Some(settings) =
                                state.config_screen_state.settings.get_mut(&auth_category)
                            {
                                if let Some(status_setting) =
                                    settings.iter_mut().find(|s| s.key == "claude_auth")
                                {
                                    status_setting.value = crate::app::state::ConfigValue::Text(
                                        "System Auth (Pro/Max Plan)".to_string(),
                                    );
                                }
                            }

                            // Persist auth provider to config.toml
                            state.app_config.authentication.claude_provider =
                                crate::config::ClaudeAuthProvider::SystemAuth;
                            if let Err(e) = state.app_config.save() {
                                tracing::warn!("Failed to save config: {}", e);
                            }

                            state.auth_provider_popup_state.show_popup = false;
                            state.auth_provider_popup_state.refresh_providers();
                        }
                    }
                }
            }
            AppEvent::AuthProviderPopupInputChar(c) => {
                state.auth_provider_popup_state.api_key_input.push(c);
            }
            AppEvent::AuthProviderPopupBackspace => {
                if state.auth_provider_popup_state.api_key_input.is_empty() {
                    // Exit key input mode
                    state.auth_provider_popup_state.cancel_key_input();
                } else {
                    state.auth_provider_popup_state.api_key_input.pop();
                }
            }
            AppEvent::AuthProviderPopupDeleteKey => {
                tracing::info!("Deleting API key from popup");
                match credentials::delete_anthropic_api_key() {
                    Ok(()) => {
                        state.add_success_notification("API key removed".to_string());
                        state.auth_provider_popup_state.refresh_providers();

                        // Update config screen
                        let auth_category = crate::app::state::ConfigCategory::Authentication;
                        if let Some(settings) =
                            state.config_screen_state.settings.get_mut(&auth_category)
                        {
                            if let Some(status_setting) =
                                settings.iter_mut().find(|s| s.key == "claude_auth")
                            {
                                status_setting.value = crate::app::state::ConfigValue::Text(
                                    "System Auth (Pro/Max Plan)".to_string(),
                                );
                            }
                        }

                        // Persist switch to system auth in config.toml
                        state.app_config.authentication.claude_provider =
                            crate::config::ClaudeAuthProvider::SystemAuth;
                        if let Err(e) = state.app_config.save() {
                            tracing::warn!("Failed to save config: {}", e);
                        }
                    }
                    Err(e) => {
                        state.add_error_notification(format!("Failed to delete: {}", e));
                    }
                }
            }
            // Config popup events (choice/text input popups)
            AppEvent::ConfigPopupNavigateUp => {
                state.config_popup_state.navigate_up();
            }
            AppEvent::ConfigPopupNavigateDown => {
                state.config_popup_state.navigate_down();
            }
            AppEvent::ConfigPopupConfirm => {
                use crate::components::config_popup::ConfigPopupValue;

                if let Some(value) = state.config_popup_state.get_value() {
                    let setting_key = state.config_popup_state.setting_key.clone();
                    let current_category = state.config_screen_state.categories
                        [state.config_screen_state.selected_category];

                    // Update the setting value
                    if let Some(settings) =
                        state.config_screen_state.settings.get_mut(&current_category)
                    {
                        if let Some(setting) = settings.iter_mut().find(|s| s.key == setting_key) {
                            match value {
                                ConfigPopupValue::Choice(text, idx) => {
                                    if let crate::app::state::ConfigValue::Choice(opts, _) =
                                        &setting.value
                                    {
                                        setting.value = crate::app::state::ConfigValue::Choice(
                                            opts.clone(),
                                            idx,
                                        );
                                    }
                                    tracing::info!(
                                        "Config setting {} changed to: {}",
                                        setting_key,
                                        text
                                    );
                                }
                                ConfigPopupValue::Text(text) => {
                                    setting.value =
                                        crate::app::state::ConfigValue::Text(text.clone());
                                    tracing::info!(
                                        "Config setting {} changed to: {}",
                                        setting_key,
                                        text
                                    );
                                }
                                ConfigPopupValue::Boolean(b) => {
                                    setting.value = crate::app::state::ConfigValue::Bool(b);
                                    tracing::info!(
                                        "Config setting {} changed to: {}",
                                        setting_key,
                                        b
                                    );
                                }
                                ConfigPopupValue::Number(n) => {
                                    setting.value = crate::app::state::ConfigValue::Number(n);
                                    tracing::info!(
                                        "Config setting {} changed to: {}",
                                        setting_key,
                                        n
                                    );
                                }
                            }
                        }
                    }
                }
                state.config_popup_state.close();
            }
            AppEvent::ConfigPopupCancel => {
                tracing::debug!("Config popup cancelled");
                state.config_popup_state.close();
            }
            AppEvent::ConfigPopupInputChar(c) => {
                state.config_popup_state.input_char(c);
            }
            AppEvent::ConfigPopupBackspace => {
                state.config_popup_state.backspace();
            }
            // Log history viewer events
            AppEvent::LogHistoryBack => {
                tracing::debug!("Log history back");
                state.log_history_state.hide();
                state.current_screen = screen_ids::HOME.to_string();
            }
            AppEvent::LogHistoryNextSession => {
                tracing::debug!("Log history next session");
                state.log_history_state.select_next_session();
            }
            AppEvent::LogHistoryPrevSession => {
                tracing::debug!("Log history prev session");
                state.log_history_state.select_prev_session();
            }
            AppEvent::LogHistorySelectSession => {
                tracing::debug!("Log history select session");
                state.log_history_state.load_selected_session();
            }
            AppEvent::LogHistoryToggleFocus => {
                tracing::debug!("Log history toggle focus");
                state.log_history_state.toggle_focus();
            }
            AppEvent::LogHistoryScrollUp => {
                tracing::debug!("Log history scroll up");
                state.log_history_state.scroll_up();
            }
            AppEvent::LogHistoryScrollDown => {
                tracing::debug!("Log history scroll down");
                state.log_history_state.scroll_down();
            }
            AppEvent::LogHistoryPageUp => {
                tracing::debug!("Log history page up");
                state.log_history_state.page_up(20);
            }
            AppEvent::LogHistoryPageDown => {
                tracing::debug!("Log history page down");
                state.log_history_state.page_down(20);
            }
            AppEvent::LogHistoryCycleFilter => {
                tracing::debug!("Log history cycle filter");
                state.log_history_state.cycle_filter();
            }
            AppEvent::LogHistoryRefresh => {
                tracing::debug!("Log history refresh");
                state.log_history_state.refresh_sessions();
            }
            AppEvent::LogHistoryCopySelection => {
                tracing::debug!("Log history copy selection");
                if let Err(e) = state.log_history_state.copy_selection_to_clipboard() {
                    tracing::warn!("Failed to copy to clipboard: {}", e);
                } else {
                    tracing::info!("Copied selection to clipboard");
                }
            }
            AppEvent::LogHistoryScrollLeft => {
                tracing::debug!("Log history scroll left");
                state.log_history_state.scroll_left(4);
            }
            AppEvent::LogHistoryScrollRight => {
                tracing::debug!("Log history scroll right");
                state.log_history_state.scroll_right(4);
            }
            AppEvent::LogHistoryScrollHome => {
                tracing::debug!("Log history scroll home");
                state.log_history_state.scroll_home();
            }
            AppEvent::LogHistoryCleanup => {
                tracing::info!("Log history cleanup requested");
                match state.log_history_state.delete_all_logs() {
                    Ok(count) => {
                        tracing::info!("Deleted {} log files", count);
                        state.log_history_state.refresh_sessions();
                    }
                    Err(e) => {
                        tracing::error!("Failed to delete log files: {}", e);
                    }
                }
            }
            // Changelog viewer events
            AppEvent::ShowChangelog => {
                tracing::debug!("Show changelog");
                state.current_screen = screen_ids::CHANGELOG.to_string();
            }
            AppEvent::ChangelogBack => {
                tracing::debug!("Changelog back");
                state.current_screen = screen_ids::HOME.to_string();
            }
            AppEvent::ChangelogScrollUp => {
                tracing::debug!("Changelog scroll up");
                state.changelog_state.scroll_up();
            }
            AppEvent::ChangelogScrollDown => {
                tracing::debug!("Changelog scroll down");
                // Use a reasonable visible height for scrolling
                state.changelog_state.scroll_down(30);
            }
            AppEvent::ChangelogPageUp => {
                tracing::debug!("Changelog page up");
                state.changelog_state.page_up(30);
            }
            AppEvent::ChangelogPageDown => {
                tracing::debug!("Changelog page down");
                state.changelog_state.page_down(30);
            }
            AppEvent::ChangelogToTop => {
                tracing::debug!("Changelog scroll to top");
                state.changelog_state.scroll_to_top();
            }
            AppEvent::ChangelogToBottom => {
                tracing::debug!("Changelog scroll to bottom");
                state.changelog_state.scroll_to_bottom(30);
            }
            // Usage analytics events: removed. The burndown plugin owns
            // every Analytics-screen state mutation now (period, filters,
            // zoom, scroll, refresh). When AppEvent::Usage* variants land
            // here in future they'll forward to the plugin via
            // AppEvent::Plugin{plugin_id, payload} rather than mutate
            // host-side state. UsageWireStatusline (host CLI install
            // helper) remains in core via the slash command palette.
            AppEvent::UsageWireStatusline => {
                // Fire when the statusline isn't already serving fresh data
                // from the Tier1 cache *and* the user's settings.json doesn't
                // already carry our block. This event is reachable from the
                // global `W` shortcut as well as the legacy Burndown route,
                // so the guard lives here rather than at the keymap.
                if state.live_window_watcher.snapshot().source == LiveSource::Tier1Cache {
                    return;
                }
                match state.statusline_status_cached() {
                    Some(StatuslineStatus::Configured) => return,
                    Some(_) => {}
                    None => return,
                }
                let outcome = install_statusline();
                // Any successful install path mutates settings.json, so
                // drop the cached detection result before the next read.
                state.invalidate_statusline_status_cache();
                match outcome {
                    Ok(InstallOutcome::Installed) => {
                        state.app_config.ui_preferences.statusline_decision =
                            crate::config::StatuslineDecision::Installed;
                        let _ = state.app_config.save();
                        state.add_success_notification(
                            "Wired Claude Code statusline. Live data appears next prompt render."
                                .to_string(),
                        );
                    }
                    Ok(InstallOutcome::AlreadyInstalled) => {
                        state.app_config.ui_preferences.statusline_decision =
                            crate::config::StatuslineDecision::Installed;
                        let _ = state.app_config.save();
                        state.add_success_notification(
                            "Statusline already wired — waiting for first prompt render."
                                .to_string(),
                        );
                    }
                    Ok(InstallOutcome::Migrated) => {
                        // Legacy `ainb statusline` was rewritten in
                        // place to `ainb claudecode statusline`. The
                        // user already opted in; surface as a success.
                        state.app_config.ui_preferences.statusline_decision =
                            crate::config::StatuslineDecision::Installed;
                        let _ = state.app_config.save();
                        state.add_success_notification(
                            "Migrated existing ainb statusline → ainb claudecode statusline."
                                .to_string(),
                        );
                    }
                    Ok(InstallOutcome::ExistingDifferent { current_command }) => {
                        state.add_warning_notification(format!(
                            "Existing statusline detected: {current_command}. Run `ainb init` for keep/replace."
                        ));
                    }
                    Err(e) => {
                        state.add_error_notification(format!("Failed to install statusline: {e}"));
                    }
                }
            }
            // Skills browser events
            AppEvent::SkillsBack => {
                tracing::debug!("Skills back");
                state.current_screen = screen_ids::HOME.to_string();
            }
            AppEvent::SkillsNextProvider => {
                state.skills_state.next_provider();
                if state.skills_state.provider.has_data() {
                    state.start_background_skills_load(false);
                }
            }
            AppEvent::SkillsPrevProvider => {
                state.skills_state.prev_provider();
                if state.skills_state.provider.has_data() {
                    state.start_background_skills_load(false);
                }
            }
            AppEvent::SkillsNextTab => {
                state.skills_state.next_tab();
            }
            AppEvent::SkillsPrevTab => {
                state.skills_state.prev_tab();
            }
            AppEvent::SkillsScrollUp => {
                state.skills_state.scroll_up();
            }
            AppEvent::SkillsScrollDown => {
                let max = state.skills_state.row_count();
                state.skills_state.scroll_down(max);
            }
            AppEvent::SkillsPageUp => {
                state.skills_state.page_up(20);
            }
            AppEvent::SkillsPageDown => {
                let max = state.skills_state.row_count();
                state.skills_state.page_down(max, 20);
            }
            AppEvent::SkillsToTop => {
                state.skills_state.scroll_to_top();
            }
            AppEvent::SkillsToBottom => {
                let max = state.skills_state.row_count();
                state.skills_state.scroll_to_bottom(max);
            }
            AppEvent::SkillsRefresh => {
                tracing::info!("Refreshing skills data");
                let msg = if state.start_background_skills_load(true) {
                    "Refreshing skills data…"
                } else {
                    "Refresh already in progress"
                };
                state.add_success_notification(msg.to_string());
            }
            AppEvent::SkillsSearchStart => {
                state.skills_state.search_active = true;
                state.skills_state.search_query.clear();
                state.skills_state.selected_index = 0;
            }
            AppEvent::SkillsSearchChar(c) => {
                state.skills_state.search_push(c);
                let max = state.skills_state.row_count();
                state.skills_state.clamp_selection(max);
            }
            AppEvent::SkillsSearchBackspace => {
                state.skills_state.search_pop();
                let max = state.skills_state.row_count();
                state.skills_state.clamp_selection(max);
            }
            AppEvent::SkillsSearchClose => {
                state.skills_state.search_active = false;
                // Query is preserved so the filter stays applied after exit.
            }
            // Session recovery events
            AppEvent::SessionRecoveryBack => {
                // If overlay is showing, dismiss it first
                if state.session_recovery_state.recovery_overlay.is_some() {
                    tracing::debug!("Dismissing recovery overlay");
                    state.session_recovery_state.dismiss_overlay();
                } else {
                    tracing::debug!("Session recovery back");
                    state.current_screen = screen_ids::HOME.to_string();
                }
            }
            AppEvent::SessionRecoveryNext => {
                tracing::debug!("Session recovery next");
                state.session_recovery_state.next();
            }
            AppEvent::SessionRecoveryPrev => {
                tracing::debug!("Session recovery prev");
                state.session_recovery_state.previous();
            }
            AppEvent::SessionRecoveryResume => {
                tracing::debug!("Session recovery resume");
                if state.session_recovery_state.has_multi_selection() {
                    // Bulk resume all multi-selected items
                    let (resumed, failed) = state.session_recovery_state.resume_multi_selected();
                    if failed == 0 {
                        state.add_success_notification(format!("Resumed {} sessions", resumed));
                    } else {
                        state.add_info_notification(format!(
                            "Resumed {}, failed {}",
                            resumed, failed
                        ));
                    }
                } else {
                    // Single item resume (worktree or session)
                    let (name, result) = if state.session_recovery_state.is_worktree_selected() {
                        let name = state
                            .session_recovery_state
                            .selected_worktree()
                            .map(|w| w.name.clone())
                            .unwrap_or_default();
                        (name, state.session_recovery_state.resume_worktree())
                    } else {
                        let name = state
                            .session_recovery_state
                            .selected()
                            .map(|s| s.session.clone())
                            .unwrap_or_default();
                        (name, state.session_recovery_state.resume_selected())
                    };

                    let overlay_result = match result {
                        Ok(ref tmux_name) => {
                            crate::components::session_recovery::RecoveryResultLine {
                                name: name.clone(),
                                success: true,
                                detail: format!("→ {}", tmux_name),
                            }
                        }
                        Err(ref e) => crate::components::session_recovery::RecoveryResultLine {
                            name: name.clone(),
                            success: false,
                            detail: e.clone(),
                        },
                    };

                    let (title, succeeded) = match &result {
                        Ok(_) => (format!("Resumed: {}", name), true),
                        Err(e) => (format!("Failed: {}", e), false),
                    };

                    state.session_recovery_state.recovery_overlay =
                        Some(crate::components::session_recovery::RecoveryOverlay {
                            title,
                            results: vec![overlay_result],
                            scroll_offset: 0,
                        });

                    if succeeded {
                        state.add_success_notification("Session resumed".to_string());
                    }
                }
            }
            AppEvent::SessionRecoveryArchive => {
                tracing::debug!("Session recovery archive/delete");
                // Use delete_selected() which handles both sessions (archive) and worktrees (delete)
                let is_worktree = state.session_recovery_state.is_worktree_selected();
                match state.session_recovery_state.delete_selected() {
                    Ok(()) => {
                        if is_worktree {
                            state.add_info_notification("Worktree deleted".to_string());
                        } else {
                            state.add_info_notification("Session archived".to_string());
                        }
                    }
                    Err(e) => {
                        if is_worktree {
                            state.add_error_notification(format!("Failed to delete: {}", e));
                        } else {
                            state.add_error_notification(format!("Failed to archive: {}", e));
                        }
                    }
                }
            }
            AppEvent::SessionRecoveryRefresh => {
                tracing::debug!("Session recovery refresh");
                state.session_recovery_state.refresh();
            }
            AppEvent::SessionRecoveryToggleView => {
                tracing::debug!("Session recovery toggle view");
                state.session_recovery_state.toggle_view_mode();
            }
            AppEvent::SessionRecoveryRecoverAll => {
                tracing::info!("Session recovery: recovering all worktrees");
                let result = state.session_recovery_state.recover_all_worktrees();
                let total = result.succeeded.len() + result.failed.len();
                if result.failed.is_empty() {
                    state.add_info_notification(format!(
                        "Recovered all {} sessions successfully",
                        result.succeeded.len()
                    ));
                } else {
                    state.add_info_notification(format!(
                        "Recovered {}/{} sessions ({} failed)",
                        result.succeeded.len(),
                        total,
                        result.failed.len()
                    ));
                }
            }
            AppEvent::SessionRecoveryToggleSelect => {
                state.session_recovery_state.toggle_select();
                let count = state.session_recovery_state.selected_items.len();
                if count > 0 {
                    state.add_info_notification(format!("{} items selected", count));
                }
            }
            AppEvent::SessionRecoveryDeleteSelected => {
                let count = state.session_recovery_state.selected_items.len();
                if count == 0 {
                    state.add_info_notification(
                        "No items selected. Use Space to select items first.".to_string(),
                    );
                } else {
                    tracing::info!("Session recovery: deleting {} selected items", count);
                    let (deleted, failed) = state.session_recovery_state.delete_multi_selected();
                    if failed == 0 {
                        state.add_info_notification(format!("Deleted {} items", deleted));
                    } else {
                        state.add_info_notification(format!(
                            "Deleted {}/{} items ({} failed)",
                            deleted,
                            deleted + failed,
                            failed
                        ));
                    }
                }
            }
            // Onboarding wizard events
            AppEvent::OnboardingNext => {
                use crate::components::onboarding::OnboardingStep;
                tracing::debug!("Onboarding next step");
                let mut trigger_dep_check = false;
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    if onboarding_state.is_final_step() {
                        // On final step, finish onboarding
                        if let Err(e) = state.complete_onboarding() {
                            tracing::error!("Failed to complete onboarding: {}", e);
                        }
                    } else {
                        let (advanced, needs_dep_check) = onboarding_state.advance();
                        if !advanced {
                            tracing::debug!("Cannot advance: requirements not met");
                        }
                        trigger_dep_check = needs_dep_check;
                        // Initialize editors when entering EditorSelection step
                        if onboarding_state.current_step == OnboardingStep::EditorSelection {
                            onboarding_state.init_editors_if_needed();
                        }
                    }
                }
                // Auto-trigger dependency check if entering DependencyCheck step
                // Queue as async action so UI shows loading state immediately
                if trigger_dep_check {
                    tracing::debug!("Queuing dependency check as async action");
                    if let Some(ref mut onboarding_state) = state.onboarding_state {
                        onboarding_state.dependency_check_running = true;
                    }
                    state.pending_async_action = Some(AsyncAction::OnboardingCheckDeps);
                }
            }
            AppEvent::OnboardingBack => {
                tracing::debug!("Onboarding back step");
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.go_back();
                }
            }
            AppEvent::OnboardingCancel => {
                tracing::debug!("Onboarding cancelled");
                state.cancel_onboarding();
            }
            AppEvent::OnboardingInputChar(ch) => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.input_char(ch);
                }
            }
            AppEvent::OnboardingBackspace => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.backspace();
                }
            }
            AppEvent::OnboardingDelete => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.delete();
                }
            }
            AppEvent::OnboardingCursorLeft => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.cursor_left();
                }
            }
            AppEvent::OnboardingCursorRight => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.cursor_right();
                }
            }
            AppEvent::OnboardingCursorHome => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.cursor_home();
                }
            }
            AppEvent::OnboardingCursorEnd => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.cursor_end();
                }
            }
            AppEvent::OnboardingCheckDeps => {
                tracing::debug!("Queuing dependency check as async action");
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.dependency_check_running = true;
                }
                state.pending_async_action = Some(AsyncAction::OnboardingCheckDeps);
            }
            AppEvent::OnboardingSkipAuth => {
                tracing::debug!("Skipping authentication");
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    onboarding_state.auth_completed = true;
                    onboarding_state.auth_method = Some("skipped".to_string());
                    onboarding_state.advance();
                }
            }
            AppEvent::OnboardingEditorUp => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    if onboarding_state.selected_editor_index > 0 {
                        onboarding_state.selected_editor_index -= 1;
                    }
                }
            }
            AppEvent::OnboardingEditorDown => {
                if let Some(ref mut onboarding_state) = state.onboarding_state {
                    let max_idx = onboarding_state.available_editors.len().saturating_sub(1);
                    if onboarding_state.selected_editor_index < max_idx {
                        onboarding_state.selected_editor_index += 1;
                    }
                }
            }
            AppEvent::OnboardingInstallConfig => {
                use crate::components::onboarding::dependency_checker::DependencyChecker;
                tracing::debug!("Installing recommended tmux config");
                match DependencyChecker::install_tmux_config() {
                    Ok(()) => {
                        tracing::info!("Successfully installed tmux.conf");
                        // Re-run dependency check to update status
                        if let Some(ref mut onboarding_state) = state.onboarding_state {
                            onboarding_state.dependency_check_running = true;
                        }
                        state.pending_async_action = Some(AsyncAction::OnboardingCheckDeps);
                    }
                    Err(e) => {
                        tracing::error!("Failed to install tmux.conf: {}", e);
                        if let Some(ref mut onboarding_state) = state.onboarding_state {
                            onboarding_state.error_message = Some(format!("Install failed: {}", e));
                        }
                    }
                }
            }
            AppEvent::OnboardingFinish => {
                tracing::debug!("Finishing onboarding");
                if let Err(e) = state.complete_onboarding() {
                    tracing::error!("Failed to complete onboarding: {}", e);
                }
            }
            // Setup menu events
            AppEvent::SetupMenuBack => {
                tracing::debug!("Setup menu back");
                if state.setup_menu_state.showing_confirmation {
                    state.setup_menu_state.cancel_action();
                } else {
                    state.current_screen = screen_ids::HOME.to_string();
                }
            }
            AppEvent::SetupMenuSelect => {
                tracing::debug!("Setup menu select");
                use crate::components::setup_menu::SetupMenuItem;

                // Check if showing confirmation dialog
                if state.setup_menu_state.showing_confirmation {
                    // Confirmed action
                    if let Some(item) = state.setup_menu_state.confirm_action() {
                        match item {
                            SetupMenuItem::FactoryReset => {
                                use crate::config::OnboardingConfig;
                                if let Err(e) = OnboardingConfig::factory_reset() {
                                    tracing::error!("Factory reset failed: {}", e);
                                } else {
                                    tracing::info!("Factory reset completed");
                                    state.start_onboarding(true, None);
                                }
                            }
                            _ => {}
                        }
                    }
                } else {
                    // Request action (may show confirmation for dangerous actions)
                    use crate::components::onboarding::OnboardingStep;
                    if let Some(item) = state.setup_menu_state.request_action() {
                        match item {
                            SetupMenuItem::RerunWizard => {
                                state.start_onboarding(true, None);
                            }
                            SetupMenuItem::CheckDependencies => {
                                state.start_onboarding(true, Some(OnboardingStep::DependencyCheck));
                            }
                            SetupMenuItem::ConfigureGitPaths => {
                                state.start_onboarding(true, Some(OnboardingStep::GitDirectories));
                            }
                            SetupMenuItem::AuthenticationSettings => {
                                state.start_onboarding(true, Some(OnboardingStep::Authentication));
                            }
                            SetupMenuItem::EditorPreference => {
                                state.start_onboarding(true, Some(OnboardingStep::EditorSelection));
                            }
                            SetupMenuItem::FactoryReset => {
                                // This shouldn't happen as it's handled by confirmation
                            }
                        }
                    }
                }
            }
            AppEvent::SetupMenuUp => {
                tracing::debug!("Setup menu up");
                if !state.setup_menu_state.showing_confirmation {
                    state.setup_menu_state.move_up();
                }
            }
            AppEvent::SetupMenuDown => {
                tracing::debug!("Setup menu down");
                if !state.setup_menu_state.showing_confirmation {
                    state.setup_menu_state.move_down();
                }
            }
            AppEvent::StartOnboarding => {
                tracing::debug!("Starting onboarding from setup menu");
                state.start_onboarding(true, None);
            }
            AppEvent::FactoryReset => {
                tracing::debug!("Factory reset requested");
                use crate::config::OnboardingConfig;
                if let Err(e) = OnboardingConfig::factory_reset() {
                    tracing::error!("Factory reset failed: {}", e);
                } else {
                    tracing::info!("Factory reset completed");
                    state.start_onboarding(true, None);
                }
            }
            // Mouse events are handled directly in the main event loop
            AppEvent::MouseClick { .. }
            | AppEvent::MouseDragStart { .. }
            | AppEvent::MouseDragEnd { .. }
            | AppEvent::MouseDragging { .. }
            | AppEvent::MouseMove { .. } => {
                // These are processed by handle_mouse_event
            }
            // Phase 2c plugin-shaped variants. Today the in-core burndown
            // handlers still drive Analytics directly through the legacy
            // `Usage*` variants; the bridge module is responsible for
            // round-tripping `AppEvent::Plugin` payloads when Phase 3 swaps
            // the dispatch path into the burndown plugin.
            AppEvent::Plugin { plugin_id, payload } => {
                tracing::debug!(
                    target: "plugin_event",
                    plugin_id = %plugin_id,
                    payload_len = payload.len(),
                    "received AppEvent::Plugin (Phase 2c stub — bridge dispatch lands in Phase 3)",
                );
            }
            AppEvent::NavigateTo(screen_id) => {
                // Phase 2c integration step: route through the screen-id table
                // landed by Phase 2a. We validate against the built-in `ids`
                // constants statically; layout dispatch reads
                // `state.current_screen` and looks up the matching `Screen`
                // impl in `LayoutComponent::screens` (the in-tree
                // `ScreenRegistry`). Plugin-supplied screens (Phase 4) will
                // register additional ids into that same registry, at which
                // point this validation switches to a registry probe.
                if is_known_screen_id(&screen_id) {
                    state.previous_screen = Some(state.current_screen.clone());
                    state.current_screen = screen_id;
                } else {
                    tracing::warn!(
                        target: "navigation",
                        screen_id = %screen_id,
                        "AppEvent::NavigateTo for unknown screen id — ignoring",
                    );
                }
            }
        }
    }
}

/// `true` when the currently-selected SkillManager unit is part of a
/// conflict pair — i.e. either it carries `shadowed_by` or another
/// manifest entry points its `shadowed_by` back at it. Used by the
/// `[s]` keybind to route between conflict-flip (existing) and
/// `SkillManagerSync` (bead v12.D.5).
///
/// Loads the manifest from `ainb_home/manifest.yaml`. Missing /
/// invalid manifests, empty unit lists, or an out-of-range selection
/// all resolve to "no conflict peer" so the keybind falls through to
/// sync — that's the conservative default since the legacy
/// flip-on-no-pair behaviour was a silent no-op.
/// Run an `ainb skill ...` command in-process against `ainb_home`,
/// capturing its stdout. Returns `(success, message)` where `message`
/// is the last non-empty line of output (or the error string). Used by
/// the SkillManager update / remove keybinds so the TUI can surface a
/// notification without shelling out.
///
/// NOTE: these calls run synchronously on the UI thread. For a local
/// `file://` source (and the sandbox) they're instant; a real network
/// source could briefly block. The git no-prompt env (set inside the
/// skill-core git helpers) makes an unreachable remote fail fast rather
/// than hang, so the worst case is a short stall + an error toast.
fn run_skill_cli(ainb_home: &std::path::Path, cmd: ainb_cli::SkillCommand) -> (bool, String) {
    let mut buf: Vec<u8> = Vec::new();
    match ainb_cli::skill::dispatch(ainb_home, cmd, &mut buf) {
        Ok(()) => (true, last_meaningful_line(&buf)),
        Err(e) => (false, format!("{e}")),
    }
}

/// Run a catalog search via the production [`SkillsShHttpBackend`]
/// (mock under `AINB_CATALOG_MOCK=1`) and project the hits into the
/// TUI's `BrowseRow` view-model. Returns `Err(msg)` on a backend error
/// so the modal can surface it without panicking. Bead ai-a20.
fn run_catalog_search(
    ainb_home: &std::path::Path,
    query: &str,
) -> Result<Vec<crate::components::skill_manager_screen::BrowseRow>, String> {
    use ainb_skill_core::catalog::CatalogBackend;
    // The HTTP backend uses `reqwest::blocking`, which builds its own runtime
    // and PANICS when constructed on a thread that is already inside a tokio
    // runtime. The TUI event loop runs under `#[tokio::main]`, so run the
    // (synchronous) search on a dedicated OS thread — the blocking client is
    // then built off the runtime thread. The mock path returns before ever
    // touching reqwest, so this is a no-op cost in tests.
    let ainb_home = ainb_home.to_path_buf();
    let query = query.to_string();
    let hits = std::thread::spawn(move || {
        let backend = ainb_cli::catalog_http::SkillsShHttpBackend::from_env(&ainb_home);
        backend.search(&query).map_err(|e| e.to_string())
    })
    .join()
    .map_err(|_| "catalog search thread panicked".to_string())??;
    Ok(hits
        .into_iter()
        .map(|h| crate::components::skill_manager_screen::BrowseRow {
            name: h.name,
            repo: h.repo,
            stars: h.stars,
            install_uri: h.install_uri,
            description: h.description,
        })
        .collect())
}

#[cfg(test)]
mod catalog_search_tokio_guard {
    use super::run_catalog_search;

    // Serialize env mutation against other env-touching tests in this binary.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Regression: `run_catalog_search` must run the `reqwest::blocking`
    /// search off the runtime thread. Building a blocking client inside a
    /// tokio runtime panics — before the thread-offload fix this test aborted
    /// with that panic instead of returning a network error.
    #[tokio::test]
    async fn search_from_tokio_context_does_not_panic() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_mock = std::env::var_os("AINB_CATALOG_MOCK");
        let prev_base = std::env::var_os("AINB_SKILLS_API_BASE");
        // Force the real (non-mock) path at an unreachable endpoint so the
        // search fails fast with a connection error rather than hitting a
        // real catalog.
        std::env::remove_var("AINB_CATALOG_MOCK");
        std::env::set_var("AINB_SKILLS_API_BASE", "http://127.0.0.1:1/nope");

        let res = run_catalog_search(std::path::Path::new("/nonexistent-ainb-home"), "react");

        match prev_mock {
            Some(v) => std::env::set_var("AINB_CATALOG_MOCK", v),
            None => std::env::remove_var("AINB_CATALOG_MOCK"),
        }
        match prev_base {
            Some(v) => std::env::set_var("AINB_SKILLS_API_BASE", v),
            None => std::env::remove_var("AINB_SKILLS_API_BASE"),
        }

        // The assertion that matters is "the call returned at all" (no panic
        // unwind). A connect to a dead port yields Err.
        assert!(res.is_err(), "expected a connection error, got: {res:?}");
    }
}

/// Install a catalog hit by its unit URI, routing through the existing
/// install flow: derive the source URI from the unit URI, `ainb source
/// add` it (idempotent — "already exists" is not a failure), then `ainb
/// skill install <uri> --yes`. Returns `(ok, last_line)`. Bead ai-a20.
fn install_catalog_hit(ainb_home: &std::path::Path, install_uri: &str) -> (bool, String) {
    use ainb_skill_core::Uri;
    let Ok(uri) = Uri::parse(install_uri) else {
        return (false, format!("invalid install URI `{install_uri}`"));
    };
    if !uri.is_unit() {
        return (false, format!("`{install_uri}` is not a unit URI"));
    }
    // Source URI = `<type>:<locator>[@<ref>]` with NO `/path`.
    let mut source_uri = format!("{}:{}", uri.source_type, uri.locator);
    if let Some(r) = &uri.ref_ {
        source_uri.push('@');
        source_uri.push_str(r);
    }

    // 1. Add the source. "already exists" is fine — the source may have
    //    been added by a previous browse / `source add`.
    let add_cmd = ainb_cli::SourceCommand::Add(ainb_cli::AddArgs {
        uri: source_uri.clone(),
        name: None,
        kind: None,
    });
    let (add_ok, add_msg) = run_source_cli(ainb_home, add_cmd);
    if !add_ok && !add_msg.contains("already exists") {
        return (false, format!("add source `{source_uri}`: {add_msg}"));
    }

    // 2. Install the unit (non-interactive).
    let install_cmd = ainb_cli::SkillCommand::Install(ainb_cli::InstallArgs {
        uri: install_uri.to_string(),
        targets: None,
        dry_run: false,
        yes: true,
    });
    run_skill_cli(ainb_home, install_cmd)
}

/// Remove the unit whose `declared_uri` matches `uri` from the manifest
/// under `ainb_home`, persisting the change. Returns `true` when a unit
/// was found and the rewrite succeeded. Best-effort: a missing /
/// malformed manifest, or a save failure, returns `false` rather than
/// panicking — the caller surfaces the appropriate notification.
///
/// The Units table is rendered from the manifest, so this is what makes
/// the row vanish after `[r] remove`.
fn drop_unit_from_manifest(ainb_home: &std::path::Path, uri: &str) -> bool {
    use ainb_skill_core::manifest::Manifest;
    let manifest_path = ainb_home.join("manifest.yaml");
    let Ok(mut manifest) = Manifest::load_from(&manifest_path) else {
        return false;
    };
    let before = manifest.units.len();
    manifest.units.retain(|u| u.uri != uri);
    if manifest.units.len() == before {
        return false; // nothing matched — leave the file untouched
    }
    manifest.save_to(&manifest_path).is_ok()
}

/// Same shape as [`run_skill_cli`] for `ainb source ...` commands.
fn run_source_cli(ainb_home: &std::path::Path, cmd: ainb_cli::SourceCommand) -> (bool, String) {
    let mut buf: Vec<u8> = Vec::new();
    match ainb_cli::source::dispatch(ainb_home, cmd, &mut buf) {
        Ok(()) => (true, last_meaningful_line(&buf)),
        Err(e) => (false, format!("{e}")),
    }
}

/// Last non-empty, non-comment line of captured CLI output — the most
/// useful one-liner for a notification (CLI flows print a trailing
/// summary like `installed ... → 1 tool(s)`). Falls back to a generic
/// "done" when output is empty.
fn last_meaningful_line(buf: &[u8]) -> String {
    let text = String::from_utf8_lossy(buf);
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .next_back()
        .map(|l| l.to_string())
        .unwrap_or_else(|| "done".to_string())
}

fn selected_unit_has_conflict_peer(
    state: &AppState,
    ainb_home: &std::path::Path,
) -> bool {
    use ainb_skill_core::manifest::Manifest;
    let manifest_path = ainb_home.join("manifest.yaml");
    let Ok(manifest) = Manifest::load_from(&manifest_path) else {
        return false;
    };
    let sel = state.skill_manager_state.selected;
    let Some(unit) = manifest.units.get(sel) else {
        return false;
    };
    if unit.shadowed_by.is_some() {
        return true;
    }
    let sel_uri = unit.uri.clone();
    manifest
        .units
        .iter()
        .any(|u| u.shadowed_by.as_ref().map(|x| x.to_string()) == Some(sel_uri.clone()))
}

/// `true` if `id` matches one of the built-in screen ids declared in
/// `crate::app::screens::ids`. Phase 4 will replace this with a probe into
/// the live `ScreenRegistry` so plugin-supplied ids resolve too.
fn is_known_screen_id(id: &str) -> bool {
    use crate::app::screens::ids;
    matches!(
        id,
        ids::HOME
            | ids::AGENT_SELECTION
            | ids::CONFIG
            | ids::CATALOG
            | ids::ANALYTICS
            | ids::SESSION_LIST
            | ids::LOGS
            | ids::LOG_HISTORY
            | ids::TERMINAL
            | ids::HELP
            | ids::NEW_SESSION
            | ids::SEARCH_WORKSPACE
            | ids::NON_GIT_NOTIFICATION
            | ids::ATTACHED_TERMINAL
            | ids::AUTH_SETUP
            | ids::CLAUDE_CHAT
            | ids::GIT_VIEW
            | ids::ONBOARDING
            | ids::SETUP_MENU
            | ids::CHANGELOG
            | ids::SESSION_RECOVERY
            | ids::SKILLS
    )
}

#[cfg(test)]
mod navigate_to_tests {
    use super::*;
    use crate::app::screens::ids;

    fn fresh_state() -> AppState {
        AppState::default()
    }

    #[test]
    fn navigate_to_known_screen_updates_current() {
        let mut state = fresh_state();
        let starting = state.current_screen.clone();
        EventHandler::process_event(AppEvent::NavigateTo(ids::ANALYTICS.to_string()), &mut state);
        assert_eq!(state.current_screen, ids::ANALYTICS);
        assert_eq!(state.previous_screen.as_deref(), Some(starting.as_str()));
    }

    #[test]
    fn navigate_to_unknown_screen_does_not_change_current() {
        let mut state = fresh_state();
        let starting = state.current_screen.clone();
        EventHandler::process_event(
            AppEvent::NavigateTo("definitely-not-a-real-screen".to_string()),
            &mut state,
        );
        assert_eq!(state.current_screen, starting);
    }

    #[test]
    fn is_known_screen_id_accepts_all_builtin_ids() {
        for id in [
            ids::HOME,
            ids::AGENT_SELECTION,
            ids::CONFIG,
            ids::CATALOG,
            ids::ANALYTICS,
            ids::SESSION_LIST,
            ids::LOGS,
            ids::LOG_HISTORY,
            ids::TERMINAL,
            ids::HELP,
            ids::NEW_SESSION,
            ids::SEARCH_WORKSPACE,
            ids::NON_GIT_NOTIFICATION,
            ids::ATTACHED_TERMINAL,
            ids::AUTH_SETUP,
            ids::CLAUDE_CHAT,
            ids::GIT_VIEW,
            ids::ONBOARDING,
            ids::SETUP_MENU,
            ids::CHANGELOG,
            ids::SESSION_RECOVERY,
            ids::SKILLS,
        ] {
            assert!(is_known_screen_id(id), "{id} should be recognised");
        }
    }

    #[test]
    fn is_known_screen_id_rejects_garbage() {
        assert!(!is_known_screen_id(""));
        assert!(!is_known_screen_id("not-a-screen"));
        assert!(!is_known_screen_id("home2"));
    }
}

#[cfg(test)]
mod global_w_tests {
    use super::*;
    use crate::cli::statusline_install::StatuslineStatus;
    use crate::models::live_window::Source;

    #[test]
    fn fires_when_source_is_none_and_statusline_unconfigured() {
        assert!(EventHandler::should_wire_statusline_inner(
            Source::None,
            Some(&StatuslineStatus::NotConfigured),
        ));
    }

    #[test]
    fn fires_when_tier2_local_and_statusline_unconfigured() {
        // Tier2Local means ainb is reading JSONL fallback — not the
        // Tier1 cache the statusline would write to. Wiring is still
        // productive.
        assert!(EventHandler::should_wire_statusline_inner(
            Source::Tier2Local,
            Some(&StatuslineStatus::NotConfigured),
        ));
    }

    #[test]
    fn fires_when_other_command_present() {
        assert!(EventHandler::should_wire_statusline_inner(
            Source::None,
            Some(&StatuslineStatus::Other("ccusage statusline".into())),
        ));
    }

    #[test]
    fn no_op_when_tier1_cache_active() {
        // Already wired and fresh — `W` should fall through and not
        // re-trigger the install.
        assert!(!EventHandler::should_wire_statusline_inner(
            Source::Tier1Cache,
            Some(&StatuslineStatus::Configured),
        ));
        assert!(!EventHandler::should_wire_statusline_inner(
            Source::Tier1Cache,
            Some(&StatuslineStatus::NotConfigured),
        ));
    }

    #[test]
    fn no_op_when_already_configured_even_without_fresh_cache() {
        // Settings.json has our block but the cache hasn't been written
        // yet (statusline hasn't run). Re-installing wouldn't help.
        assert!(!EventHandler::should_wire_statusline_inner(
            Source::None,
            Some(&StatuslineStatus::Configured),
        ));
    }

    #[test]
    fn no_op_when_status_detection_failed() {
        // IO failure reading settings.json — refuse to install blindly.
        assert!(!EventHandler::should_wire_statusline_inner(
            Source::None,
            None,
        ));
    }
}

#[cfg(test)]
mod text_input_guard_tests {
    //! Regression tests for the global-shortcut guard.
    //!
    //! The bug these tests pin down: pasting `SHOTClubhouse/SHOTid` into
    //! the New Session repo URL field used to come out as `SOTid`
    //! because the unconditional global `H` shortcut toggled the help
    //! overlay mid-paste. Same hazard for every other text-input view
    //! and every other single-character global shortcut someone might
    //! add in future.

    use super::*;
    use crate::app::screens::ids as screen_ids;
    use crate::app::state::{AppState, NewSessionState, NewSessionStep};

    // Phase 6 (new-session redesign): the three legacy `InputRepoSource`
    // paste/keystroke regression tests were removed along with the step
    // itself. PickRepo and Configure own their own paste handling
    // component-locally; the cross-component "no global shortcut steals a
    // char" invariant is still covered by `is_text_input_context_covers_*`
    // tests below.
    fn char_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// Outside any text input, `Shift+H` must still toggle the global
    /// help overlay — we're only suppressing it inside text inputs, not
    /// removing it.
    #[test]
    fn global_h_still_toggles_help_outside_text_input() {
        let mut state = AppState::default();
        state.current_screen = screen_ids::HOME.to_string();

        let evt = EventHandler::handle_key_event(char_key('H'), &mut state)
            .expect("Shift+H outside text input must dispatch ToggleHelp");
        assert!(matches!(evt, AppEvent::ToggleHelp));
    }

    /// Config screen — `Shift+H` must still toggle help when the user
    /// is navigating settings (not actively editing a value).
    /// Regression test for the gemini-code-assist#MEDIUM finding on
    /// PR #130: blanket-including `screen_ids::CONFIG` in the text-input
    /// predicate broke the help shortcut for plain navigation.
    #[test]
    fn global_h_still_toggles_help_during_config_navigation() {
        let mut state = AppState::default();
        state.current_screen = screen_ids::CONFIG.to_string();
        // editing = false, api_key_input_mode = false by default

        let evt = EventHandler::handle_key_event(char_key('H'), &mut state)
            .expect("Shift+H in Config navigation must dispatch ToggleHelp");
        assert!(matches!(evt, AppEvent::ToggleHelp));
    }

    /// Esc inside a text input while help is visible must close help,
    /// NOT fall through to the view's cancel handler (which would close
    /// the form). Reachable when the user opens help from HomeScreen
    /// then navigates into a text-entry view. Phase 6 (new-session
    /// redesign) updates this to use the PickRepo step.
    #[test]
    fn esc_closes_help_inside_text_input_without_cancelling_form() {
        let mut state = AppState::default();
        state.current_screen = screen_ids::NEW_SESSION.to_string();
        state.new_session_state = Some(NewSessionState {
            step: NewSessionStep::PickRepo,
            ..NewSessionState::default()
        });
        state.help_visible = true;

        let evt = EventHandler::handle_key_event(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut state,
        )
        .expect("Esc in help-visible text-input must dispatch ToggleHelp");
        assert!(
            matches!(evt, AppEvent::ToggleHelp),
            "expected ToggleHelp, got {:?}",
            evt
        );
    }

    /// Every non-NewSession branch of `is_text_input_context` must be
    /// recognised as a text-input context. This is the belt-and-braces
    /// invariant — adding a new view that should accept free-form
    /// input requires both extending the helper *and* extending this
    /// test, so the two stay in sync.
    #[test]
    fn is_text_input_context_covers_every_other_branch() {
        use crate::components::GitViewState;
        use std::path::PathBuf;

        // Screen-only branches: switching `current_screen` is enough.
        // (Config is intentionally excluded — it's gated on edit state,
        // covered separately below.)
        for screen in &[
            screen_ids::SEARCH_WORKSPACE,
            screen_ids::CLAUDE_CHAT,
            screen_ids::AUTH_SETUP,
            screen_ids::ATTACHED_TERMINAL,
        ] {
            let mut state = AppState::default();
            state.current_screen = (*screen).to_string();
            assert!(
                EventHandler::is_text_input_context(&state),
                "screen `{}` must be treated as text input",
                screen
            );
        }

        // Config screen: only counts as text input when actively editing
        // a setting or entering an API key, NOT when navigating the
        // categories list. Suppressing globals during plain navigation
        // would regress the help shortcut UX.
        let mut state = AppState::default();
        state.current_screen = screen_ids::CONFIG.to_string();
        assert!(
            !EventHandler::is_text_input_context(&state),
            "Config without edit mode must NOT be treated as text input"
        );

        let mut state = AppState::default();
        state.current_screen = screen_ids::CONFIG.to_string();
        state.config_screen_state.editing = true;
        assert!(
            EventHandler::is_text_input_context(&state),
            "Config + editing = true must be treated as text input"
        );

        let mut state = AppState::default();
        state.current_screen = screen_ids::CONFIG.to_string();
        state.config_screen_state.api_key_input_mode = true;
        assert!(
            EventHandler::is_text_input_context(&state),
            "Config + api_key_input_mode = true must be treated as text input"
        );

        // Config edit popup (opened via ConfigEditSetting). The
        // predicate only flips for popup variants that actually
        // capture characters — `TextInput` and `NumberInput`. Use
        // the public `open_text` API so the test exercises a real
        // popup-open code path and stays valid if the popup_type
        // representation changes.
        let mut state = AppState::default();
        state.current_screen = screen_ids::CONFIG.to_string();
        state.config_popup_state.open_text("Title", "Desc", "key", "value");
        assert!(
            EventHandler::is_text_input_context(&state),
            "Config + config_popup TextInput must be treated as text input"
        );

        // Negative control: a Choice popup is navigation-only (arrow
        // keys / Enter), so `H` should still toggle help.
        let mut state = AppState::default();
        state.current_screen = screen_ids::CONFIG.to_string();
        state.config_popup_state.open_choice(
            "Title",
            "Desc",
            "key",
            vec!["A".into(), "B".into()],
            0,
        );
        assert!(
            !EventHandler::is_text_input_context(&state),
            "Config + Choice popup must NOT be treated as text input"
        );

        // Modal flags on AppState. Each must independently flip the
        // predicate to true.
        let cases: Vec<(&str, fn(&mut AppState))> = vec![
            ("other_tmux_rename_mode", |s| {
                s.other_tmux_rename_mode = true
            }),
            ("ssh_session_rename_mode", |s| {
                s.ssh_session_rename_mode = true
            }),
            ("quick_commit_message", |s| {
                s.quick_commit_message = Some(String::new())
            }),
            ("auth_provider_popup", |s| {
                s.auth_provider_popup_state.show_popup = true
            }),
        ];
        for (label, setup) in cases {
            let mut state = AppState::default();
            setup(&mut state);
            assert!(
                EventHandler::is_text_input_context(&state),
                "{} must be treated as text input",
                label
            );
        }

        // Analytics input mode + zoom-search assertions DROPPED in the
        // plugin migration — analytics is now owned by the burndown
        // subprocess plugin. The host can't read its text-entry modes;
        // see the comment in `is_text_input_context` for the host/
        // plugin boundary rationale and the wire-signal path forward
        // when a plugin needs the host to suppress globals during
        // text entry.

        // Skills search overlay.
        let mut state = AppState::default();
        state.current_screen = screen_ids::SKILLS.to_string();
        state.skills_state.search_active = true;
        assert!(
            EventHandler::is_text_input_context(&state),
            "Skills search_active must be treated as text input"
        );

        // GitView commit-message mode.
        let mut state = AppState::default();
        state.current_screen = screen_ids::GIT_VIEW.to_string();
        let mut git_state = GitViewState::new(PathBuf::from("/tmp"));
        git_state.start_commit_message_input();
        state.git_view_state = Some(git_state);
        assert!(
            EventHandler::is_text_input_context(&state),
            "GitView commit-message mode must be treated as text input"
        );

        // Negative control: GitView without commit mode active is NOT
        // a text input — it's a navigable screen.
        let mut state = AppState::default();
        state.current_screen = screen_ids::GIT_VIEW.to_string();
        state.git_view_state = Some(GitViewState::new(PathBuf::from("/tmp")));
        assert!(
            !EventHandler::is_text_input_context(&state),
            "GitView outside commit mode must NOT be treated as text input"
        );

        // Negative control: bare default state on HomeScreen is not a
        // text input.
        let state = AppState::default();
        assert!(
            !EventHandler::is_text_input_context(&state),
            "HomeScreen with no modal flags must NOT be treated as text input"
        );
    }

    /// `is_text_input_context` must return true for every text-entry
    /// step of the NewSession screen. Phase 6 (new-session redesign):
    /// the legacy 13-step flow was retired — only PickRepo (smart-parse
    /// filter) and Configure (Boss-mode prompt) accept free-form chars
    /// and must therefore be treated as text-input contexts. The
    /// `Creating` step is a render-only spinner with no text entry.
    #[test]
    fn is_text_input_context_covers_new_session_text_steps() {
        let text_steps = [NewSessionStep::PickRepo, NewSessionStep::Configure];
        for step in &text_steps {
            let mut state = AppState::default();
            state.current_screen = screen_ids::NEW_SESSION.to_string();
            state.new_session_state = Some(NewSessionState {
                step: step.clone(),
                ..NewSessionState::default()
            });
            assert!(
                EventHandler::is_text_input_context(&state),
                "step {:?} must be treated as text input",
                step
            );
        }

        // Sanity: the Creating step is a render-only spinner and must
        // NOT be treated as a text-input context.
        let mut state = AppState::default();
        state.current_screen = screen_ids::NEW_SESSION.to_string();
        state.new_session_state = Some(NewSessionState {
            step: NewSessionStep::Creating,
            ..NewSessionState::default()
        });
        assert!(
            !EventHandler::is_text_input_context(&state),
            "Creating is render-only, not a text input"
        );
    }
}

/// Bead v12.D.5 tripwire — `[s]` on the SkillManager Units panel
/// routes to `SkillManagerSync` when no conflict pair is present,
/// and to the legacy `SkillManagerConflictFlip` when the manifest
/// holds a shadowed_by edge for the selected unit.
#[cfg(test)]
mod skill_manager_sync_keybind_tests {
    use super::*;
    use crate::app::screens::ids as screen_ids;
    use ainb_skill_core::manifest::{Manifest, UnitEntry};
    use ainb_skill_core::Uri;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn press_s(state: &mut AppState) -> Option<AppEvent> {
        EventHandler::handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE), state)
    }

    fn switch_to_skill_manager(state: &mut AppState) {
        state.current_screen = screen_ids::SKILL_MANAGER.to_string();
    }

    /// AINB_HOME points at the supplied tempdir for the duration of
    /// the closure; `selected_unit_has_conflict_peer` is one of the
    /// few code paths that has to read the on-disk manifest, so we
    /// pin the env to a tempdir to keep the test hermetic.
    fn with_ainb_home<R>(dir: &std::path::Path, body: impl FnOnce() -> R) -> R {
        // The lock keeps parallel-running tests in the same process
        // from clobbering each other's AINB_HOME.
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("AINB_HOME").ok();
        std::env::set_var("AINB_HOME", dir);
        let r = body();
        match prev {
            Some(v) => std::env::set_var("AINB_HOME", v),
            None => std::env::remove_var("AINB_HOME"),
        }
        r
    }

    #[test]
    fn s_routes_to_sync_when_no_conflict_pair() {
        let tmp = tempfile::tempdir().unwrap();
        with_ainb_home(tmp.path(), || {
            // Empty manifest on disk — no conflict pair possible.
            Manifest::default()
                .save_to(&tmp.path().join("manifest.yaml"))
                .unwrap();

            let mut state = AppState::default();
            switch_to_skill_manager(&mut state);
            let ev = press_s(&mut state);
            assert!(
                matches!(ev, Some(AppEvent::SkillManagerSync)),
                "expected SkillManagerSync, got {ev:?}"
            );
        });
    }

    #[test]
    fn s_routes_to_conflict_flip_when_selected_carries_shadowed_by() {
        let tmp = tempfile::tempdir().unwrap();
        with_ainb_home(tmp.path(), || {
            let mut manifest = Manifest::default();
            manifest.units.push(UnitEntry {
                uri: "gh:owner/repo@main/skills/commit".into(),
                targets: None,
                // Selected unit IS shadowed → conflict pair present.
                shadowed_by: Some(
                    Uri::parse("local:/tmp/orphan@head/commit").unwrap(),
                ),
            });
            manifest.units.push(UnitEntry {
                uri: "local:/tmp/orphan@head/commit".into(),
                targets: None,
                shadowed_by: None,
            });
            manifest
                .save_to(&tmp.path().join("manifest.yaml"))
                .unwrap();

            let mut state = AppState::default();
            switch_to_skill_manager(&mut state);
            state.skill_manager_state.selected = 0; // unit with shadowed_by
            let ev = press_s(&mut state);
            assert!(
                matches!(ev, Some(AppEvent::SkillManagerConflictFlip)),
                "expected SkillManagerConflictFlip, got {ev:?}"
            );
        });
    }

    #[test]
    fn s_routes_to_conflict_flip_when_selected_is_shadowed_by_peer() {
        let tmp = tempfile::tempdir().unwrap();
        with_ainb_home(tmp.path(), || {
            let mut manifest = Manifest::default();
            // unit[0] is the active side; unit[1] points back at it.
            manifest.units.push(UnitEntry {
                uri: "gh:owner/repo@main/skills/commit".into(),
                targets: None,
                shadowed_by: None,
            });
            manifest.units.push(UnitEntry {
                uri: "local:/tmp/orphan@head/commit".into(),
                targets: None,
                shadowed_by: Some(
                    Uri::parse("gh:owner/repo@main/skills/commit").unwrap(),
                ),
            });
            manifest
                .save_to(&tmp.path().join("manifest.yaml"))
                .unwrap();

            let mut state = AppState::default();
            switch_to_skill_manager(&mut state);
            state.skill_manager_state.selected = 0; // active side
            let ev = press_s(&mut state);
            assert!(
                matches!(ev, Some(AppEvent::SkillManagerConflictFlip)),
                "expected SkillManagerConflictFlip, got {ev:?}"
            );
        });
    }
}
