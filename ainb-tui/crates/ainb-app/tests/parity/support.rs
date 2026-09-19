//! Parity fixtures: one scenario per screen, built into an `AppState`.
//!
//! A fixture is JSON with a schema owned by this module, not a serialised
//! `AppState`: state is built through its public API, so a fixture keeps
//! describing the same screen while the state behind it is reshaped. The
//! renderer test in `ainb-core` draws each fixture to a text snapshot, and
//! every staged change must leave those snapshots byte-identical.
//!
//! Callers point `HOME` at an empty scratch directory first, so nothing the
//! machine's own config holds reaches the frame.
//!
//! Shared by path between `ainb-app/tests/parity.rs` (fixtures build) and
//! `ainb-core/tests/parity_snapshots.rs` (fixtures render), so it names the
//! crate as `ainb_app` in both.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Deserialize;
use uuid::Uuid;

use ainb_app::app::AppState;
use ainb_app::app::screens::ids;
use ainb_app::components::code_review::model::{DiffRow, Hunk, ReviewFile, ReviewModel, RowKind};
use ainb_app::components::daemons::Snapshot;
use ainb_app::components::git_view::{GitFileStatus, GitTab, GitViewState};
use ainb_app::fleet::daemons::probe::{DaemonKind, DaemonState, DaemonStatus};
use ainb_app::models::{Session, SessionStatus, Workspace};

/// One committed scenario.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParityFixture {
    /// Terminal size the snapshot is drawn at.
    pub width: u16,
    pub height: u16,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceFixture>,
    /// `[workspace, session]` selected in the session list.
    #[serde(default)]
    pub selected: Option<[usize; 2]>,
    #[serde(default)]
    pub help_visible: bool,
    /// Why this fixture has no ratatui half, when it has none. The terminal
    /// renderer and its snapshot checks skip it; the frames dump and the
    /// webview half still take it. `stats` is the one: the terminal's stats is
    /// burndown's plugin paint, not a built-in screen (D3-prime).
    #[serde(default)]
    pub dom_only: Option<String>,
    pub screen: ScreenFixture,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceFixture {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub sessions: Vec<SessionFixture>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionFixture {
    pub name: String,
    #[serde(default)]
    pub status: StatusFixture,
}

#[derive(Debug, Default, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusFixture {
    Running,
    #[default]
    Stopped,
    Idle,
}

/// The screen the fixture opens, with whatever that screen needs.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ScreenFixture {
    Home,
    SessionList,
    NewSessionPickRepo,
    Config,
    /// The daemons screen over a collected snapshot, so both renderers draw
    /// rows and not only a table's headings: each `DaemonFixture` is one
    /// daemon as the collector would report it.
    Daemons {
        #[serde(default)]
        daemons: Vec<DaemonFixture>,
    },
    GitView {
        files: Vec<ReviewFileFixture>,
        /// Sidebar directories the person has collapsed. A fixture carries one
        /// so the frame's own copy of them is drawn, and so the frame-twice
        /// check sees a set with something in it.
        #[serde(default)]
        collapsed_dirs: Vec<String>,
    },
    SessionRecovery,
    SkillManager,
    LogHistory,
    Onboarding,
    SetupMenu,
    /// The inbox screen over the `inbox` section (D3-prime), folded through
    /// the section's own `apply_read` so the fixture carries what a daemon
    /// read would: `filler` unread rows are appended after `entries` so a
    /// fixture can sit past the fold's row cap without listing 100 rows.
    Inbox {
        entries: Vec<InboxRowFixture>,
        #[serde(default)]
        filler: usize,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxRowFixture {
    pub kind: String,
    pub summary: String,
    #[serde(default)]
    pub read: bool,
    /// Section 21 holding one `fleet/usage_summary` reply, as the daemon
    /// sends it, on the terminal's `analytics` screen (burndown's).
    Stats {
        usage: Box<ainb_hangar_proto::fleet::FleetUsageSummaryResult>,
    },
    /// A plugin screen with nothing painted yet, which draws one of the three
    /// fallback placeholders (D3p-f): not registered, registered with a render
    /// error, or registered with no frame yet.
    Plugin {
        screen: PluginScreenFixture,
        /// The host's runtime has the plugin registered.
        #[serde(default)]
        registered: bool,
        /// The render error the host recorded for the screen.
        #[serde(default)]
        render_error: Option<String>,
    },
}

/// A screen `PLUGIN_SCREENS` hands to a plugin.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginScreenFixture {
    Analytics,
    Witr,
    Learnings,
    Abtop,
    Hangar,
}

/// One daemon row of a `daemons` fixture.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonFixture {
    /// `DaemonKind`'s lowercase id.
    pub kind: DaemonKind,
    pub state: DaemonState,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub error_count: u64,
    #[serde(default)]
    pub last_error: Option<String>,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewFileFixture {
    pub path: String,
    pub status: FileStatusFixture,
    /// Diff rows, each `"+text"`, `"-text"` or `" text"`.
    pub rows: Vec<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatusFixture {
    Added,
    Modified,
    Deleted,
}

impl ParityFixture {
    /// Read a fixture file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Every `*.json` fixture in `dir`, sorted by file name.
    pub fn all_in(dir: &Path) -> Vec<(String, PathBuf)> {
        let mut out = std::fs::read_dir(dir)
            .unwrap_or_else(|e| panic!("parity fixtures in {}: {e}", dir.display()))
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .map(|path| {
                let name =
                    path.file_stem().expect("fixture has a name").to_string_lossy().into_owned();
                (name, path)
            })
            .collect::<Vec<_>>();
        out.sort();
        out
    }

    /// The screen id this fixture leaves current.
    #[must_use]
    pub fn screen_id(&self) -> &'static str {
        match self.screen {
            ScreenFixture::Home => ids::HOME,
            ScreenFixture::SessionList => ids::SESSION_LIST,
            ScreenFixture::NewSessionPickRepo => ids::NEW_SESSION,
            ScreenFixture::Config => ids::CONFIG,
            ScreenFixture::Daemons { .. } => ids::DAEMONS,
            ScreenFixture::GitView { .. } => ids::GIT_VIEW,
            ScreenFixture::SessionRecovery => ids::SESSION_RECOVERY,
            ScreenFixture::SkillManager => ids::SKILL_MANAGER,
            ScreenFixture::LogHistory => ids::LOG_HISTORY,
            ScreenFixture::Onboarding => ids::ONBOARDING,
            ScreenFixture::SetupMenu => ids::SETUP_MENU,
            ScreenFixture::Inbox { .. } => ids::INBOX,
            ScreenFixture::Stats { .. } => ids::ANALYTICS,
            ScreenFixture::Plugin { screen, .. } => match screen {
                PluginScreenFixture::Analytics => ids::ANALYTICS,
                PluginScreenFixture::Witr => ids::WITR,
                PluginScreenFixture::Learnings => ids::LEARNINGS,
                PluginScreenFixture::Abtop => ids::ABTOP,
                PluginScreenFixture::Hangar => ids::HANGAR,
            },
        }
    }

    /// Build the state this fixture describes.
    #[must_use]
    pub fn build(&self) -> AppState {
        let mut state = AppState::new();
        state.sessions.workspaces =
            self.workspaces.iter().enumerate().map(build_workspace).collect();
        if let Some([workspace, session]) = self.selected {
            state.sessions.selected_workspace_index = Some(workspace);
            state.sessions.selected_session_index = Some(session);
        }
        state.shell.help_visible = self.help_visible;
        state.shell.current_screen = self.screen_id().to_string();

        match &self.screen {
            ScreenFixture::NewSessionPickRepo => {
                state.new_session.new_session_state = Some(ainb_app::app::state::NewSessionState {
                    step: ainb_app::app::state::NewSessionStep::PickRepo,
                    pick_repo_state: Some(
                        ainb_app::components::new_session::pick_repo::PickRepoState::from_disk_no_locals(),
                    ),
                    ..Default::default()
                });
            }
            ScreenFixture::GitView {
                files,
                collapsed_dirs,
            } => {
                let mut git = GitViewState::new(PathBuf::from("/parity/repo"));
                git.active_tab = GitTab::Review;
                git.review = ReviewModel {
                    files: files.iter().map(build_review_file).collect(),
                };
                git.review_ui.collapsed_dirs = collapsed_dirs.iter().cloned().collect();
                state.git_view.git_view_state = Some(git);
            }
            ScreenFixture::Onboarding => {
                state.onboarding.onboarding_state =
                    Some(ainb_app::components::onboarding::OnboardingState::new());
            }
            ScreenFixture::Stats { usage } => {
                state.apply_usage_read((**usage).clone(), 0);
            }
            ScreenFixture::Plugin {
                registered,
                render_error,
                ..
            } => {
                let screen = self.screen_id().to_string();
                let host = &mut state.plugins_host;
                host.plugin_presence.insert(
                    screen.clone(),
                    ainb_app::app::sections::PluginPresence {
                        registered: *registered,
                        ..Default::default()
                    },
                );
                if let Some(error) = render_error {
                    host.plugin_render_errors.insert(screen, error.clone());
                }
            }
            ScreenFixture::Daemons { daemons } => {
                // A collected snapshot, at a fixed clock so the relative
                // columns and the frame dump do not move between runs, and
                // not parked, so the render's touch spawns no collector.
                let snapshot = Snapshot {
                    rows: daemons.iter().map(build_daemon).collect(),
                    collected_at_ms: 1_700_000_000_000,
                    last_touch_ms: 1_700_000_000_000,
                    ..Snapshot::default()
                };
                state.hangar.daemons_state.shared = Some(Arc::new(Mutex::new(snapshot)));
            }
            ScreenFixture::Home
            | ScreenFixture::SessionList
            | ScreenFixture::Config
            | ScreenFixture::SessionRecovery
            | ScreenFixture::SkillManager
            | ScreenFixture::LogHistory
            | ScreenFixture::SetupMenu => {}
            ScreenFixture::Inbox { entries, filler } => {
                let row = |n: usize, kind: &str, summary: &str, read: bool| {
                    ainb_hangar_proto::events::InboxEntryRow {
                        id: format!("01J0PARITYINBOX{n:011}"),
                        kind: kind.to_string(),
                        event: format!("{kind}_created"),
                        subject_id: format!("{kind}-{n}"),
                        summary: summary.to_string(),
                        recipient: "member:me".to_string(),
                        created_at: 1_700_000_000_000 - n as i64,
                        read_at: read.then_some(1_700_000_000_500),
                    }
                };
                let mut rows: Vec<_> = entries
                    .iter()
                    .enumerate()
                    .map(|(n, e)| row(n, &e.kind, &e.summary, e.read))
                    .collect();
                for n in 0..*filler {
                    rows.push(row(
                        entries.len() + n,
                        "task",
                        &format!("Task queued: filler-{n}"),
                        false,
                    ));
                }
                let unread = rows.iter().filter(|r| r.read_at.is_none()).count() as i64;
                state.apply_inbox_read(
                    ainb_hangar_proto::snapshots::InboxListResult {
                        entries: rows,
                        unread,
                    },
                    1_700_000_001_000,
                );
            }
        }
        state
    }
}

fn build_workspace((index, fixture): (usize, &WorkspaceFixture)) -> Workspace {
    let mut workspace = Workspace::new(fixture.name.clone(), PathBuf::from(&fixture.path));
    for (offset, session) in fixture.sessions.iter().enumerate() {
        let mut built = Session::new(session.name.clone(), fixture.path.clone());
        // Fixed ids, so anything that prints an id prefix draws the same text.
        built.id = Uuid::from_u128(((index as u128 + 1) << 64) | (offset as u128 + 1));
        built.status = match session.status {
            StatusFixture::Running => SessionStatus::Running,
            StatusFixture::Stopped => SessionStatus::Stopped,
            StatusFixture::Idle => SessionStatus::Idle,
        };
        workspace.add_session(built);
    }
    workspace
}

fn build_daemon(fixture: &DaemonFixture) -> DaemonStatus {
    DaemonStatus {
        kind: fixture.kind,
        state: fixture.state,
        pid: None,
        uptime_ms: None,
        version: fixture.version.clone(),
        // A version the fixture names is this binary's, so the terminal
        // prints it rather than "unknown".
        version_current: fixture.version.as_ref().map(|_| true),
        connected: fixture.connected,
        channel: None,
        last_activity_at: None,
        error_count: fixture.error_count,
        last_error: fixture.last_error.clone(),
        last_attention_poll_at: None,
        last_attention_error: None,
        inbound_expected: 0,
        inbound_live: 0,
        last_inbound_error: None,
        reason: fixture.reason.clone(),
        scheduler_orphan: None,
        atc_instance: None,
    }
}

fn build_review_file(fixture: &ReviewFileFixture) -> ReviewFile {
    let (mut old_line, mut new_line) = (1, 1);
    let mut rows = Vec::new();
    let (mut insertions, mut deletions) = (0, 0);
    for row in &fixture.rows {
        let (marker, text) = row.split_at(row.char_indices().nth(1).map_or(row.len(), |(i, _)| i));
        let (kind, old_lineno, new_lineno) = match marker {
            "+" => {
                insertions += 1;
                new_line += 1;
                (RowKind::Added, None, Some(new_line - 1))
            }
            "-" => {
                deletions += 1;
                old_line += 1;
                (RowKind::Removed, Some(old_line - 1), None)
            }
            _ => {
                old_line += 1;
                new_line += 1;
                (RowKind::Context, Some(old_line - 1), Some(new_line - 1))
            }
        };
        rows.push(DiffRow {
            kind,
            old_lineno,
            new_lineno,
            raw: text.to_string(),
            emphasis: Vec::new(),
        });
    }
    ReviewFile {
        path: fixture.path.clone(),
        status: match fixture.status {
            FileStatusFixture::Added => GitFileStatus::Added,
            FileStatusFixture::Modified => GitFileStatus::Modified,
            FileStatusFixture::Deleted => GitFileStatus::Deleted,
        },
        insertions,
        deletions,
        language: None,
        collapsed: false,
        binary: false,
        hunks: vec![Hunk {
            old_start: 1,
            new_start: 1,
            gap_before: 0,
            gap_after: 0,
            expanded_before: 0,
            expanded_after: 0,
            rows,
        }],
        new_lines: Vec::new(),
    }
}
