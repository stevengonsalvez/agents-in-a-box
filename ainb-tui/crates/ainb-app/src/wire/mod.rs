// ABOUTME: The per-section wire seam. `section_json` is the ONLY way a section
// becomes JSON, so every mirror host, test and doctor check sees the same frame.
//
// No section and not `AppState` derives `Serialize` (issue #983). Instead each
// section has a borrowed view here that names the fields a frame carries and
// leaves out the live handles (channels, `Instant`s, tmux clients, chat hosts).
// Everything BELOW a section serialises through its own derive, which is where
// the redaction lives: `#[serde(skip)]` on credential buffers, `SecretInput`
// split off the generic popup, redacting serializers on env maps and the
// `[fleet.bridge]` table, and `redact::scrub` on captured terminal and diff
// text. The views are what a later `#[derive(Serialize)]` on the sections turns
// into `#[serde(skip)]` lists; `tests/state_serde.rs` locks the shape so that
// swap cannot widen the frame without a failing fixture.
//
// The four leak checks and the key-path fixture all read the frame through
// [`serialize_section`], so they judge exactly what a host receives.

#[cfg(feature = "typescript-bindings")]
pub mod bindings;
pub mod fields;
pub mod frame;
pub mod shape;
pub mod store;
pub mod trace;
pub mod web;

use crate::app::AppState;
use crate::app::sections::{
    ClaudeChatSection, ConfigSection, FleetSection, GitViewSection, HangarSection, LogsSection,
    McpPoolSection, NewSessionSection, OnboardingSection, PluginsHostSection, RecoverySection,
    SessionLabelsSection, SessionsSection, ShellSection, SkillsSection, SshSection, TmuxSection,
    WorkspaceLoadSection,
};
use crate::app::versioned::SectionId;
use serde::{Serialize, Serializer};
use std::sync::Mutex;

/// The JSON a mirror host receives for one section, sent as `host`: every host
/// field inside the body names `host`, the same id the frame carrying it names
/// (#1066). There is no default host, so no caller can send rows that disagree
/// with their frame.
///
/// Reads the poller-published cells (`FleetSection.daemon_attention`,
/// `fleet_snapshot`, the Daemons snapshot) under their locks, so a caller must
/// not hold any of those locks across this call.
///
/// # Panics
///
/// Never in practice: every type reachable from a view serialises to JSON
/// without a non-string map key, which `state_serde.rs` proves per section.
#[must_use]
pub fn section_json(state: &AppState, id: SectionId, host: &frame::HostId) -> serde_json::Value {
    serialize_section(state, id, host, serde_json::value::Serializer)
        .expect("a section view always serialises to JSON")
}

/// The daemon read behind a section's content, for [`frame::Mirror`].
///
/// Section 20 is fed by one joined daemon read and says so. The Fleet poller's
/// reads carry no revision or daemon clock yet, so its section has none.
#[must_use]
pub fn daemon_read(state: &AppState, id: SectionId) -> Option<frame::DaemonRead> {
    match id {
        // Section 20 is one daemon read: its revision, and the daemon's clock
        // at the read, so a renderer ages a card on the daemon's clock.
        SectionId::AgentStatus => state.agent_status.view.as_ref().map(|view| frame::DaemonRead {
            revision: view.read_revision,
            clock_ms: view.read_at_ms,
        }),
        // The Fleet rows' stamps (`attention_updated_at`, `last_observed_at`) are
        // on the same daemon's clock. The snapshot poll that fills them carries
        // no clock of its own, so the frame names the newest daemon clock this
        // host holds: section 20's last read, and the newest revision it saw. A
        // renderer ages a row against that, never against its own now (#1044).
        SectionId::Fleet => state.agent_status.view.as_ref().map(|view| frame::DaemonRead {
            revision: view.head_revision.max(view.read_revision),
            clock_ms: view.read_at_ms,
        }),
        SectionId::Sessions
        | SectionId::SessionLabels
        | SectionId::Tmux
        | SectionId::Ssh
        | SectionId::GitView
        | SectionId::WorkspaceLoad
        | SectionId::NewSession
        | SectionId::Logs
        | SectionId::ClaudeChat
        | SectionId::Hangar
        | SectionId::McpPool
        | SectionId::Inbox
        | SectionId::PluginsHost
        | SectionId::Config
        | SectionId::Skills
        | SectionId::Recovery
        | SectionId::Onboarding
        | SectionId::Shell => None,
    }
}

/// Stable wire name of a section, used as the frame key and the fixture root.
#[must_use]
pub const fn section_name(id: SectionId) -> &'static str {
    match id {
        SectionId::Sessions => "sessions",
        SectionId::SessionLabels => "session_labels",
        SectionId::Tmux => "tmux",
        SectionId::Ssh => "ssh",
        SectionId::GitView => "git_view",
        SectionId::WorkspaceLoad => "workspace_load",
        SectionId::NewSession => "new_session",
        SectionId::Logs => "logs",
        SectionId::ClaudeChat => "claude_chat",
        SectionId::Fleet => "fleet",
        SectionId::Hangar => "hangar",
        SectionId::McpPool => "mcp_pool",
        SectionId::Inbox => "inbox",
        SectionId::PluginsHost => "plugins_host",
        SectionId::Config => "config",
        SectionId::Skills => "skills",
        SectionId::Recovery => "recovery",
        SectionId::Onboarding => "onboarding",
        SectionId::Shell => "shell",
        SectionId::AgentStatus => "agent_status",
    }
}

/// Serialise one section's view, sent as `host`, into any serde `Serializer`;
/// see [`section_json`].
///
/// Generic so the type tracer in [`trace`] walks the same values `section_json`
/// emits, with the declared Rust type of every field in hand.
///
/// # Errors
///
/// Whatever `serializer` reports.
pub fn serialize_section<S: Serializer>(
    state: &AppState,
    id: SectionId,
    host: &frame::HostId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    // Frame-only redaction on persisted types (see `fields`) is live for
    // exactly this call, and so is the sending host the fleet rows name.
    let _frame = fields::FrameScope::enter();
    let _host = SendingHost::enter(host);
    match id {
        SectionId::Sessions => SessionsView::from(&*state.sessions).serialize(serializer),
        SectionId::SessionLabels => {
            SessionLabelsView::from(&*state.session_labels).serialize(serializer)
        }
        SectionId::Tmux => TmuxView::from(&*state.tmux).serialize(serializer),
        SectionId::Ssh => SshView::from(&*state.ssh).serialize(serializer),
        SectionId::GitView => GitViewView::from(&*state.git_view).serialize(serializer),
        SectionId::WorkspaceLoad => {
            WorkspaceLoadView::from(&*state.workspace_load).serialize(serializer)
        }
        SectionId::NewSession => NewSessionView::from(&*state.new_session).serialize(serializer),
        SectionId::Logs => LogsView::from(&*state.log_streams).serialize(serializer),
        SectionId::ClaudeChat => ClaudeChatView::from(&*state.claude_chat).serialize(serializer),
        SectionId::Fleet => FleetView::from(&*state.fleet).serialize(serializer),
        SectionId::Hangar => HangarView::from(&*state.hangar).serialize(serializer),
        SectionId::McpPool => McpPoolView::from(&*state.mcp_pool).serialize(serializer),
        SectionId::Inbox => InboxView {}.serialize(serializer),
        SectionId::PluginsHost => PluginsHostView::from(&*state.plugins_host).serialize(serializer),
        SectionId::Config => ConfigView::from(&*state.config).serialize(serializer),
        SectionId::Skills => SkillsView::from(&*state.skills).serialize(serializer),
        SectionId::Recovery => RecoveryView::from(&*state.recovery).serialize(serializer),
        SectionId::Onboarding => OnboardingView::from(&*state.onboarding).serialize(serializer),
        SectionId::Shell => ShellView::from(&*state.shell).serialize(serializer),
        SectionId::AgentStatus => AgentStatusView::from(&*state.agent_status).serialize(serializer),
    }
}

/// The host the section being serialised is sent as, for the view fields that
/// name a host (#1066). Serde gives a `serialize_with` function only the field,
/// so the host rides a scope on this thread for exactly one
/// [`serialize_section`] call; serialisation never leaves the thread.
struct SendingHost {
    previous: Option<frame::HostId>,
}

thread_local! {
    static SENDING_HOST: std::cell::RefCell<Option<frame::HostId>> =
        const { std::cell::RefCell::new(None) };
}

impl SendingHost {
    fn enter(host: &frame::HostId) -> Self {
        let previous = SENDING_HOST.with(|cell| cell.replace(Some(host.clone())));
        Self { previous }
    }

    /// The host of the section being serialised; `local` outside one.
    fn current() -> frame::HostId {
        SENDING_HOST
            .with(|cell| cell.borrow().clone())
            .unwrap_or_else(frame::HostId::local)
    }
}

impl Drop for SendingHost {
    fn drop(&mut self) {
        let previous = self.previous.take();
        SENDING_HOST.with(|cell| *cell.borrow_mut() = previous);
    }
}

/// Serialise the value behind a shared cell the poller threads write through.
/// A poisoned lock still holds the last complete value, so it is read anyway.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn locked<T: Serialize, S: Serializer>(cell: &&Mutex<T>, serializer: S) -> Result<S::Ok, S::Error> {
    let guard = cell.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.serialize(serializer)
}

/// The Hangar fleet snapshot as a frame carries it.
///
/// `FleetSession` is the daemon's protocol row, so its own `Serialize` keeps
/// every field for the RPC it comes from. A frame projects it: off go
/// `current_request` (the complete tool input of a pending approval, unbounded
/// and shaped by the agent; the fingerprint stays), `cwd` and `display_name`
/// (the operator's paths and labels, #983 M19, as section 20 does). On goes
/// `host_id`: these rows are the daemon's, named with the host the section is
/// sent as ([`serialize_section`], #1066), so a row and the frame
/// carrying it name one host, and a row stays addressable once it is mirrored
/// next to another host's.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn fleet_rows<S: Serializer>(
    cell: &&Mutex<Vec<ainb_hangar_proto::fleet::FleetSession>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let host_id = SendingHost::current();
    let rows: Vec<FleetRowFrame> = cell
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .map(|row| FleetRowFrame::from_row(row, host_id.as_str()))
        .collect();
    rows.serialize(serializer)
}

/// One fleet row on the wire; see [`fleet_rows`].
#[derive(Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
struct FleetRowFrame<'a> {
    host_id: &'a str,
    session_key: String,
    provider: ainb_hangar_proto::fleet::FleetProvider,
    provider_session_id: Option<String>,
    tmux_target: Option<String>,
    pane_binding: ainb_hangar_proto::fleet::PaneBinding,
    process_start_fingerprint: Option<String>,
    lifecycle: ainb_hangar_proto::fleet::LifecycleState,
    active_work_count: i64,
    attention: ainb_hangar_proto::fleet::AttentionState,
    current_request_fingerprint: Option<String>,
    management: ainb_hangar_proto::fleet::ManagementState,
    transport_health: ainb_hangar_proto::fleet::TransportHealth,
    capabilities: ainb_hangar_proto::fleet::FleetCapabilities,
    provenance: ainb_hangar_proto::fleet::FleetProvenance,
    confidence: ainb_hangar_proto::fleet::FleetConfidence,
    discovered_at: i64,
    last_observed_at: i64,
    lifecycle_updated_at: i64,
    attention_updated_at: i64,
    model: Option<String>,
    reasoning_effort: Option<String>,
    model_updated_at: i64,
    version: i64,
    updated_revision: i64,
}

impl<'a> FleetRowFrame<'a> {
    /// One row, named with the host the daemon gave in `auth/hello` (#1066).
    fn from_row(row: &ainb_hangar_proto::fleet::FleetSession, host_id: &'a str) -> Self {
        // Destructured, so a field added to the protocol row fails to compile
        // here until someone decides whether a frame carries it.
        let ainb_hangar_proto::fleet::FleetSession {
            session_key,
            provider,
            provider_session_id,
            tmux_target,
            pane_binding,
            process_start_fingerprint,
            cwd: _,
            display_name: _,
            lifecycle,
            active_work_count,
            attention,
            current_request_fingerprint,
            current_request: _,
            management,
            transport_health,
            capabilities,
            provenance,
            confidence,
            discovered_at,
            last_observed_at,
            lifecycle_updated_at,
            attention_updated_at,
            model,
            reasoning_effort,
            model_updated_at,
            version,
            updated_revision,
        } = row;
        Self {
            host_id,
            session_key: session_key.clone(),
            provider: *provider,
            provider_session_id: provider_session_id.clone(),
            tmux_target: tmux_target.clone(),
            pane_binding: *pane_binding,
            process_start_fingerprint: process_start_fingerprint.clone(),
            lifecycle: *lifecycle,
            active_work_count: *active_work_count,
            attention: *attention,
            current_request_fingerprint: current_request_fingerprint.clone(),
            management: *management,
            transport_health: *transport_health,
            capabilities: capabilities.clone(),
            provenance: *provenance,
            confidence: *confidence,
            discovered_at: *discovered_at,
            last_observed_at: *last_observed_at,
            lifecycle_updated_at: *lifecycle_updated_at,
            attention_updated_at: *attention_updated_at,
            model: model.clone(),
            reasoning_effort: reasoning_effort.clone(),
            model_updated_at: *model_updated_at,
            version: *version,
            updated_revision: *updated_revision,
        }
    }
}

/// Section 20 (agent status) on the wire (#1015, #983).
///
/// `AgentStatusSection` and its `StatusView` deliberately do not derive
/// `Serialize`, so this is the whole frame. It carries what a remote surface
/// needs to draw a card (the state, its evidence, the wait kind, the
/// attachment) and leaves OFF three roster fields: `current_request` (the
/// full tool input of a pending approval), `cwd` and `display_name` (the
/// operator's paths and labels, #983 M19). The failure reason and the unbound
/// detail are free text, so they are scrubbed as the frame is built.
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Serialize)]
struct AgentStatusView<'a> {
    absent: Option<String>,
    head_revision: i64,
    view: Option<StatusViewFrame<'a>>,
}

#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Serialize)]
struct StatusViewFrame<'a> {
    host_id: &'a str,
    read_revision: i64,
    received_at_ms: i64,
    head_revision: i64,
    health: HealthFrame,
    cards: Vec<AgentCardFrame<'a>>,
}

#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum HealthFrame {
    Live,
    Stale {
        read_revision: i64,
        head_revision: i64,
    },
    Unreachable {
        stale_since_ms: i64,
        reason: String,
    },
}

#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
#[derive(Serialize)]
struct AgentCardFrame<'a> {
    session_key: &'a str,
    provider: ainb_hangar_proto::fleet::FleetProvider,
    lifecycle: ainb_hangar_proto::fleet::LifecycleState,
    management: ainb_hangar_proto::fleet::ManagementState,
    transport_health: ainb_hangar_proto::fleet::TransportHealth,
    state: ainb_hangar_proto::agent_status::AgentState,
    provenance: ainb_hangar_proto::agent_status::Provenance,
    tier: ainb_hangar_proto::agent_status::Tier,
    evidence_observed_at: i64,
    has_open_request: bool,
    pane_unbound: bool,
    pane_unbound_detail: Option<String>,
    host_id: &'a str,
    turn_complete: bool,
    wait_kind: Option<ainb_hangar_proto::agent_status::WaitKind>,
    attachment: ainb_hangar_proto::agent_status::Attachment,
}

impl<'a> From<&'a crate::app::sections::AgentStatusSection> for AgentStatusView<'a> {
    fn from(section: &'a crate::app::sections::AgentStatusSection) -> Self {
        use ainb_hangar_proto::status_view::ViewHealth;
        let scrub = |text: &str| crate::fleet::bridge::redact::scrub(text);
        Self {
            absent: section.absent.as_deref().map(scrub),
            head_revision: section.head_revision,
            view: section.view.as_ref().map(|view| StatusViewFrame {
                host_id: &view.host_id,
                read_revision: view.read_revision,
                received_at_ms: view.received_at_ms,
                head_revision: view.head_revision,
                health: match &view.health {
                    ViewHealth::Live => HealthFrame::Live,
                    ViewHealth::Stale {
                        read_revision,
                        head_revision,
                    } => HealthFrame::Stale {
                        read_revision: *read_revision,
                        head_revision: *head_revision,
                    },
                    ViewHealth::Unreachable {
                        stale_since_ms,
                        reason,
                    } => HealthFrame::Unreachable {
                        stale_since_ms: *stale_since_ms,
                        reason: scrub(reason),
                    },
                },
                cards: view
                    .cards()
                    .map(|card| AgentCardFrame {
                        session_key: &card.status.session_key,
                        provider: card.session.provider,
                        lifecycle: card.session.lifecycle,
                        management: card.session.management,
                        transport_health: card.session.transport_health,
                        state: card.status.state,
                        provenance: card.status.provenance,
                        tier: card.status.tier,
                        evidence_observed_at: card.status.evidence_observed_at,
                        has_open_request: card.status.has_open_request,
                        pane_unbound: card.status.pane_unbound,
                        pane_unbound_detail: card.status.pane_unbound_detail.as_deref().map(scrub),
                        host_id: &card.status.host_id,
                        turn_complete: card.status.turn_complete,
                        wait_kind: card.status.wait_kind,
                        attachment: card.status.attachment,
                    })
                    .collect(),
            }),
        }
    }
}

/// Plugin render failures, scrubbed: an error string can carry a URL or token.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn scrubbed_values<K: Serialize + std::hash::Hash + Eq, S: Serializer>(
    map: &&std::collections::HashMap<K, String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer
        .collect_map(map.iter().map(|(key, text)| (key, crate::fleet::bridge::redact::scrub(text))))
}

/// Subprocess error text, scrubbed.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn scrubbed_opt<S: Serializer>(value: &&Option<String>, serializer: S) -> Result<S::Ok, S::Error> {
    fields::scrub_opt(value, serializer)
}

/// The quick-commit composer as its length: operator prose mid-typing.
// serde's `serialize_with` hands the view's `&&T`, so the double reference is its signature.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn opt_len<S: Serializer>(value: &&Option<String>, serializer: S) -> Result<S::Ok, S::Error> {
    fields::opt_char_count(value, serializer)
}

/// Declares a borrowed view over a section: the listed fields, by reference,
/// with optional per-field serde attributes. Anything not listed is not on the
/// wire.
macro_rules! view {
    ($view:ident<$lt:lifetime> for $section:ty { $( $(#[$attr:meta])* $field:ident : $ty:ty ),* $(,)? }) => {
        #[derive(Serialize)]
        #[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
        // Field names mirror the section's, prefixes and all, so the frame
        // keys match the Rust fields a later derive would emit.
        #[allow(clippy::struct_field_names)]
        struct $view<$lt> {
            $( $(#[$attr])* $field: &$lt $ty, )*
        }

        impl<$lt> From<&$lt $section> for $view<$lt> {
            fn from(section: &$lt $section) -> Self {
                Self { $( $field: &section.$field, )* }
            }
        }
    };
}

/// The session list as a renderer draws it (#1180): every workspace, with only
/// the rows the session filter lets through, and the selected row by id.
///
/// The frame carries the rows a surface draws, not the filter: a renderer
/// never holds its own copy of the rule, and never a hidden set beside the
/// list. The section's `selected_session_index` is an index into its full list,
/// so it stays off the wire and the selection travels as `selected_session_id`.
#[derive(Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
// Field names mirror the section's, prefixes and all, so the frame keys match
// the Rust fields.
#[allow(clippy::struct_field_names)]
struct SessionsView<'a> {
    #[cfg_attr(feature = "typescript-bindings", specta(type = Vec<crate::models::Workspace>))]
    workspaces: VisibleWorkspaces<'a>,
    selected_workspace_index: &'a Option<usize>,
    selected_session_id: Option<uuid::Uuid>,
    shell_selected: &'a bool,
    selected_sessions: &'a std::collections::HashSet<uuid::Uuid>,
    expand_all_workspaces: &'a bool,
    session_filter: &'a crate::app::state::SessionFilter,
    attached_session_id: &'a Option<uuid::Uuid>,
    favorite_workspace_paths: &'a std::collections::HashSet<std::path::PathBuf>,
}

impl<'a> From<&'a SessionsSection> for SessionsView<'a> {
    fn from(section: &'a SessionsSection) -> Self {
        Self::showing(section, section.session_filter)
    }
}

impl<'a> SessionsView<'a> {
    /// The view with the rows `filter` shows. The frame passes the section's
    /// own filter; a surface that is not the TUI passes its own (#1180).
    fn showing(section: &'a SessionsSection, filter: crate::app::state::SessionFilter) -> Self {
        // A selected row the filter hides is not on the frame, so the frame
        // must not name it: cycling the filter does not move the selection.
        let selected_session_id = section
            .selected_workspace_index
            .and_then(|workspace| section.workspaces.get(workspace))
            .zip(section.selected_session_index)
            .and_then(|(workspace, session)| workspace.sessions.get(session))
            .filter(|session| filter.passes(session))
            .map(|session| session.id);
        Self {
            workspaces: VisibleWorkspaces {
                workspaces: &section.workspaces,
                filter,
            },
            selected_workspace_index: &section.selected_workspace_index,
            selected_session_id,
            shell_selected: &section.shell_selected,
            selected_sessions: &section.selected_sessions,
            expand_all_workspaces: &section.expand_all_workspaces,
            session_filter: &section.session_filter,
            attached_session_id: &section.attached_session_id,
            favorite_workspace_paths: &section.favorite_workspace_paths,
        }
    }
}

/// The Sessions section as its frame body would be under the `All` filter:
/// every session row, whatever filter the TUI's Shift+F left persisted.
///
/// The filter is a fact about the TUI's renderer, not about the sessions, so a
/// surface that lists sessions for another purpose (the web dashboard,
/// `ainb list --frame`, the web's attach lookup) reads this. It goes through
/// the same view as the frame, so nothing the frame withholds or scrubs
/// reaches it either.
#[must_use]
pub fn every_session_json(state: &AppState) -> serde_json::Value {
    serde_json::to_value(SessionsView::showing(
        &state.sessions,
        crate::app::state::SessionFilter::All,
    ))
    .expect("a section view always serialises to JSON")
}

/// The workspaces with only the session rows `filter` shows, in list order.
///
/// A workspace whose rows all pass is serialised as it stands; only one that
/// hides a row is copied without it.
struct VisibleWorkspaces<'a> {
    workspaces: &'a [crate::models::Workspace],
    filter: crate::app::state::SessionFilter,
}

impl Serialize for VisibleWorkspaces<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.workspaces.len()))?;
        for workspace in self.workspaces {
            if workspace.sessions.iter().all(|session| self.filter.passes(session)) {
                seq.serialize_element(workspace)?;
            } else {
                let mut visible = workspace.clone();
                visible.sessions.retain(|session| self.filter.passes(session));
                seq.serialize_element(&visible)?;
            }
        }
        seq.end()
    }
}

view!(SessionLabelsView<'a> for SessionLabelsSection {
    session_label_store: crate::config::SessionLabelStore,
    session_label_rename_mode: bool,
    session_label_rename_buffer: String,
    session_label_rename_target: Option<crate::app::state::AttachableRef>,
    session_context_menu: Option<crate::app::state::SessionContextMenu>,
});

view!(TmuxView<'a> for TmuxSection {
    embed_session: Option<crate::app::effect::TmuxSessionName>,
    other_tmux_sessions: Vec<crate::models::OtherTmuxSession>,
    other_tmux_expanded: bool,
    selected_other_tmux_index: Option<usize>,
    selected_other_tmux_sessions: std::collections::HashSet<String>,
    other_tmux_rename_mode: bool,
    other_tmux_rename_buffer: String,
});

view!(SshView<'a> for SshSection {
    ssh_sessions: Vec<crate::models::Session>,
    ssh_sessions_expanded: bool,
    selected_ssh_session_index: Option<usize>,
    ssh_session_rename_mode: bool,
    ssh_session_rename_buffer: String,
});

view!(GitViewView<'a> for GitViewSection {
    git_view_state: Option<crate::components::GitViewState>,
    #[serde(rename = "quick_commit_message_len", serialize_with = "opt_len")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = Option<u32>))]
    quick_commit_message: Option<String>,
    quick_commit_cursor: usize,
    is_current_dir_git_repo: bool,
});

view!(WorkspaceLoadView<'a> for WorkspaceLoadSection {
    is_loading_workspaces: bool,
    #[serde(serialize_with = "scrubbed_opt")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = Option<String>))]
    workspace_load_error: Option<String>,
});

view!(NewSessionView<'a> for NewSessionSection {
    new_session_state: Option<crate::app::state::NewSessionState>,
    branch_refresh_seq: u64,
    repo_check_seq: u64,
    repo_init_seq: u64,
});

view!(LogsView<'a> for LogsSection {
    live_logs: std::collections::HashMap<uuid::Uuid, Vec<crate::components::live_logs_stream::LogEntry>>,
    last_logs_session_id: Option<uuid::Uuid>,
    log_history_state: crate::components::LogHistoryViewerState,
});

view!(ClaudeChatView<'a> for ClaudeChatSection {
    claude_chat_visible: bool,
    claude_chat_state: Option<crate::app::state::ClaudeChatState>,
});

view!(FleetView<'a> for FleetSection {
    attention_baseline: std::collections::HashMap<uuid::Uuid, i64>,
    live_window: crate::models::live_window::LiveWindow,
    ask_state: crate::fleet::answer::AskState,
    broadcast: crate::fleet::broadcast::Broadcast,
    conversation: crate::fleet::conversation::Conversation,
    #[serde(serialize_with = "locked")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = crate::fleet::attention::DaemonAttention))]
    daemon_attention: Mutex<crate::fleet::attention::DaemonAttention>,
    #[serde(serialize_with = "fleet_rows")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = Vec<FleetRowFrame>))]
    fleet_snapshot: Mutex<Vec<ainb_hangar_proto::fleet::FleetSession>>,
    fleet_metadata: std::collections::HashMap<uuid::Uuid, crate::app::state::SessionFleetMetadata>,
    daemon_attention_seen: u64,
    attention_elsewhere: usize,
    attention_error_since: std::collections::HashMap<uuid::Uuid, i64>,
});

// `pending_daemon_config_edits` stays out: raw `(key, value)` edits queued by
// a keystroke and drained on the same app tick by `process_async_action`, so a
// host has nothing to draw from them.
view!(HangarView<'a> for HangarSection {
    hangar_daemon_config_loaded: bool,
    daemons_state: crate::components::daemons::DaemonsState,
});

view!(McpPoolView<'a> for McpPoolSection {
    mcp_overlay: Option<crate::app::state::McpOverlayState>,
});

#[derive(Serialize)]
#[cfg_attr(feature = "typescript-bindings", derive(specta::Type))]
struct InboxView {}

// `plugin_ui_states` stays out: each view is JSON its plugin wrote, with keys
// no key-path check can know in advance, so nothing proves it free of a secret.
// `watched_plugin_screens` stays out too: which screens other hosts watch is
// host bookkeeping, not something a host draws.
view!(PluginsHostView<'a> for PluginsHostSection {
    plugin_captures_text: std::collections::HashMap<crate::app::screens::ScreenId, bool>,
    plugin_presence: std::collections::BTreeMap<crate::app::screens::ScreenId, crate::app::sections::PluginPresence>,
    #[serde(serialize_with = "scrubbed_values")]
    #[cfg_attr(feature = "typescript-bindings", specta(type = std::collections::HashMap<crate::app::screens::ScreenId, String>))]
    plugin_render_errors: std::collections::HashMap<crate::app::screens::ScreenId, String>,
});

view!(ConfigView<'a> for ConfigSection {
    app_config: crate::config::AppConfig,
    config_screen_state: crate::app::state::ConfigScreenState,
    config_popup_state: crate::components::config_popup::ConfigPopupState,
    statusline_status: Option<crate::cli::statusline_install::StatuslineStatus>,
});

view!(SkillsView<'a> for SkillsSection {
    skills_state: crate::components::skills::SkillsViewState,
    skill_manager_state: crate::components::skill_manager_screen::SkillsScreenData,
});

view!(RecoveryView<'a> for RecoverySection {
    session_recovery_state: crate::components::SessionRecoveryState,
});

view!(OnboardingView<'a> for OnboardingSection {
    onboarding_state: Option<crate::components::onboarding::OnboardingState>,
    setup_menu_state: crate::components::setup_menu::SetupMenuState,
    auth_setup_state: Option<crate::app::state::AuthSetupState>,
    auth_provider_popup_state: crate::app::state::AuthProviderPopupState,
});

view!(ShellView<'a> for ShellSection {
    current_screen: crate::app::screens::ScreenId,
    previous_screen: Option<crate::app::screens::ScreenId>,
    should_quit: bool,
    help_visible: bool,
    ui_needs_refresh: bool,
    home_screen_state: crate::app::state::HomeScreenState,
    home_screen_v2_state: crate::components::home_screen_v2::HomeScreenV2State,
    notifications: Vec<crate::app::state::Notification>,
    confirmation_dialog: Option<crate::app::state::ConfirmationDialog>,
    async_operation_cancelled: bool,
    last_panel_close_version: Option<u64>,
    session_tab: crate::components::session_tabs::SessionTab,
    focused_pane: crate::app::state::FocusedPane,
});

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::ConfigValue;
    use crate::components::config_popup::ConfigPopupType;

    /// Build under a scratch `HOME`, holding the crate's env lock and putting
    /// the previous value back, the same way the reducer tests do.
    fn with_scratch_home<T>(body: impl FnOnce() -> T) -> T {
        let _guard = crate::config::tunables::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("scratch home");
        let previous = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());
        let out = body();
        match previous {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        out
    }

    /// #1131: a session's merged attention rides its row on the frame as
    /// `attention`, kind and scrubbed detail only, and never reaches the disk
    /// form of the session.
    #[test]
    fn the_sessions_frame_carries_each_rows_merged_attention_scrubbed() {
        use crate::fleet::attention::{AttentionKind, SessionAttention};
        // Assembled at runtime, so no credential-shaped literal is committed.
        let key = format!("sk-ant-{}", "api03-abcdefghijklmnopqrstuvwxyz");
        let mut session = crate::models::Session::new("s".to_string(), "/work/s".to_string());
        session.live_attention = vec![
            SessionAttention::local(AttentionKind::Ask, 1_000)
                .with_detail(format!("Paste {key} into the prompt?")),
        ];
        let body = with_scratch_home(|| {
            let mut state = AppState::new();
            let mut workspace =
                crate::models::Workspace::new("w".to_string(), std::path::PathBuf::from("/work/s"));
            workspace.add_session(session.clone());
            state.sessions.get_mut().workspaces = vec![workspace];
            section_json(&state, SectionId::Sessions, &frame::HostId::local())
        });

        let attention = &body["workspaces"][0]["sessions"][0]["attention"];
        assert_eq!(attention[0]["kind"], "Ask", "{attention}");
        let detail = attention[0]["detail"].as_str().expect("a detail");
        assert!(detail.starts_with("Paste "), "{detail}");
        assert!(!detail.contains(&key), "the detail is scrubbed: {detail}");
        assert_eq!(
            attention[0].as_object().map(serde_json::Map::len),
            Some(2),
            "kind and detail only: {attention}"
        );

        let disk = serde_json::to_value(&session).expect("serialises");
        assert!(disk.get("attention").is_none(), "{disk}");
        assert!(disk.get("live_attention").is_none(), "{disk}");
    }

    /// D2 seam 4: the open conversation rides the Fleet frame as a bounded,
    /// scrubbed window. A conversation is exactly where a pasted credential
    /// ends up, so neither a message body nor a held tool call's arguments
    /// reach a renderer verbatim, and the operator's unsent draft never leaves
    /// the process that is typing it.
    #[test]
    fn the_fleet_frame_carries_the_conversation_bounded_and_scrubbed() {
        use crate::fleet::conversation::{
            Conversation, ConversationActor, ConversationCard, ConversationCardState,
            ConversationKind, ConversationRow, MAX_ROWS,
        };
        // Assembled at runtime, so no credential-shaped literal is committed.
        let key = format!("sk-ant-{}", "api03-abcdefghijklmnopqrstuvwxyz");
        let rows: Vec<ConversationRow> = (0..MAX_ROWS + 10)
            .map(|index| ConversationRow {
                id: format!("m-{index}"),
                actor: ConversationActor::Session("claude:s-1".to_string()),
                kind: ConversationKind::Agent,
                reply: false,
                body: format!("Use {key} for the call"),
                truncated: false,
            })
            .collect();
        let body = with_scratch_home(|| {
            let mut state = AppState::new();
            state.fleet.get_mut().conversation = Conversation {
                rows,
                cards: vec![ConversationCard {
                    confirm_id: "c-1".to_string(),
                    tool: "shell".to_string(),
                    arguments: serde_json::json!({ "command": format!("curl -H {key}") }),
                    arguments_bytes: 40,
                    state: ConversationCardState::Open,
                    detail: String::new(),
                }],
                composer: "half a sentence nobody has sent".to_string(),
                ..Conversation::default()
            };
            section_json(&state, SectionId::Fleet, &frame::HostId::local())
        });

        let conversation = &body["conversation"];
        let framed = conversation["rows"].as_array().expect("rows");
        assert_eq!(
            framed.len(),
            MAX_ROWS + 10,
            "the projection bounds the list, not serde"
        );
        let first = framed[0]["body"].as_str().expect("a body");
        assert!(first.starts_with("Use "), "{first}");
        assert!(!first.contains(&key), "the body is scrubbed: {first}");

        let argument =
            conversation["cards"][0]["arguments"]["command"].as_str().expect("a command");
        assert!(
            !argument.contains(&key),
            "every string inside a tool call is scrubbed: {argument}"
        );

        assert_eq!(
            conversation["composer_len"], 31,
            "the draft crosses as its length: {conversation}"
        );
        assert!(
            conversation.get("composer").is_none(),
            "and never as its text: {conversation}"
        );
    }

    /// #1052: the changelog is static content and its scroll is renderer-local,
    /// so the Config frame names no changelog state and carries none of its text.
    #[test]
    fn the_config_frame_carries_no_changelog_state_or_text() {
        let body = with_scratch_home(|| {
            section_json(&AppState::new(), SectionId::Config, &frame::HostId::local())
        });
        let keys: Vec<&String> = body.as_object().expect("an object body").keys().collect();
        assert!(
            !keys.iter().any(|key| key.as_str() == "changelog_state"),
            "{keys:?}"
        );
        let text = serde_json::to_string(&body).expect("serialises");
        let opening = &crate::components::changelog::CHANGELOG_MARKDOWN[..64];
        assert!(
            !text.contains(opening),
            "the Config frame carries changelog text"
        );
    }

    #[test]
    fn enter_on_an_env_row_opens_a_popup_that_withholds_the_value() {
        with_scratch_home(|| {
            let mut state = shape::sample_state(&mut shape::PlainSeed);
            let screen = &mut state.config.get_mut().config_screen_state;
            let (category, index) = screen
                .settings
                .iter()
                .find_map(|(category, rows)| {
                    rows.iter()
                        .position(|row| row.key.ends_with(".environment.ANTHROPIC_API_KEY"))
                        .map(|index| (*category, index))
                })
                .expect("the sample template has an env row");
            if let ConfigValue::Text(text) =
                &mut screen.settings.get_mut(&category).unwrap()[index].value
            {
                *text = "env-value-marker".to_string();
            }
            screen.visible_rows = vec![(category, index)];
            screen.selected_setting = 0;
            crate::app::EventHandler::process_event(
                crate::app::AppEvent::ConfigEditSetting,
                &mut state,
            );

            assert!(matches!(
                state.config.config_popup_state.popup_type,
                ConfigPopupType::SecretInput { .. }
            ));
            let frame =
                section_json(&state, SectionId::Config, &frame::HostId::local()).to_string();
            assert!(!frame.contains("env-value-marker"), "{frame}");
        });
    }

    #[test]
    fn a_failed_answer_does_not_carry_the_typed_draft() {
        let frame = with_scratch_home(|| {
            let state = shape::sample_state(&mut shape::PlainSeed);
            section_json(&state, SectionId::Fleet, &frame::HostId::local()).to_string()
        });
        assert!(frame.contains("draft_len"), "{frame}");
        assert!(!frame.contains("typed answer"), "{frame}");
    }
}

/// The body type of every section's frame, keyed by its wire name. Exists only
/// for the TypeScript export: a renderer narrows `frame.body` with
/// `SectionBodies[frame.section]`.
#[cfg(feature = "typescript-bindings")]
#[derive(specta::Type)]
#[allow(dead_code)]
struct SectionBodies<'a> {
    sessions: SessionsView<'a>,
    session_labels: SessionLabelsView<'a>,
    tmux: TmuxView<'a>,
    ssh: SshView<'a>,
    git_view: GitViewView<'a>,
    workspace_load: WorkspaceLoadView<'a>,
    new_session: NewSessionView<'a>,
    logs: LogsView<'a>,
    claude_chat: ClaudeChatView<'a>,
    fleet: FleetView<'a>,
    hangar: HangarView<'a>,
    mcp_pool: McpPoolView<'a>,
    inbox: InboxView,
    plugins_host: PluginsHostView<'a>,
    config: ConfigView<'a>,
    skills: SkillsView<'a>,
    recovery: RecoveryView<'a>,
    onboarding: OnboardingView<'a>,
    shell: ShellView<'a>,
    agent_status: AgentStatusView<'a>,
}

/// Register every section view with the TypeScript export, named for its section.
#[cfg(feature = "typescript-bindings")]
pub(crate) fn register_section_views(types: specta::Types) -> specta::Types {
    types
        .register::<SectionBodies<'static>>()
        .register::<SessionsView<'static>>()
        .register::<SessionLabelsView<'static>>()
        .register::<TmuxView<'static>>()
        .register::<SshView<'static>>()
        .register::<GitViewView<'static>>()
        .register::<WorkspaceLoadView<'static>>()
        .register::<NewSessionView<'static>>()
        .register::<LogsView<'static>>()
        .register::<ClaudeChatView<'static>>()
        .register::<FleetView<'static>>()
        .register::<HangarView<'static>>()
        .register::<McpPoolView<'static>>()
        .register::<InboxView>()
        .register::<PluginsHostView<'static>>()
        .register::<ConfigView<'static>>()
        .register::<SkillsView<'static>>()
        .register::<RecoveryView<'static>>()
        .register::<OnboardingView<'static>>()
        .register::<ShellView<'static>>()
        .register::<AgentStatusView<'static>>()
}
