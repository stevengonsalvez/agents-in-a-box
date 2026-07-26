//! P4.7 — Settings screen: the pure reducer + five-section width-aware render.
//!
//! The settings screen (hotkey `,`) has five j/k-navigable sections:
//!
//! 1. **Daemon connection** — socket path, PID, uptime, version, connection state
//!    (from the `hangar/health` RPC snapshot).
//! 2. **Providers** — registered LLM providers with online/offline status.
//! 3. **LLM keys** — a masked list; `n` opens the key-entry modal (writes to the
//!    OS keychain via the `host/secret_store_get` cap), `d` deletes.
//! 4. **Workspace** (P5.5) — a `slug · name [default]` table with the active
//!    workspace marked `▶` in `SELECTION_GREEN`. `s` sets the selected row
//!    active (emits a `SwitchWorkspace` intent → `host/workspace_set_active`),
//!    `d` toggles default, `n` creates a new workspace, `r` renames the
//!    selected one. `J`/`K` move the in-pane selection.
//! 5. **Members** (e38.11) — a render-only `email · role` list (the `owner` role
//!    in `SELECTION_GREEN`). The mutation surface is CLI-first
//!    (`ainb hangar member set-role|remove`); the pane lets the operator see who
//!    holds which role from the `hangar/members_list` snapshot.
//!
//! The reducer ([`reduce_settings`]) is **pure**. The crucial security property
//! (P4.7 risk register, "keychain write leaks key into log"): entered key
//! material is wrapped in [`KeyMaterial`], whose `Debug` impl **redacts** the
//! value, so neither a `{:?}` log nor a panic message can leak a secret. Only
//! [`KeyMaterial::expose`] reveals the raw bytes, at the single call site that
//! performs the actual keychain write.

use ainb_hangar_core::daemon_config::{ConfigKind, DAEMON_CONFIG_REGISTRY, parse_bool_token};
use ainb_hangar_proto::settings::{HealthSnapshot, KeyRow, ProviderRow, WorkspaceRow};
use ainb_hangar_proto::snapshots::{MemberWireRow, NotifyRuleWireRow};
use ainb_hangar_proto::{Channel, ChannelSet};
use ainb_plugin_sdk::WireBuffer;

use crate::widgets::key_entry::{render_key_entry_modal, render_workspace_name_modal};

/// A secret key value with a **redacting** `Debug` impl.
///
/// The whole point of this newtype is that `format!("{km:?}")` (and therefore any
/// `tracing` field, `dbg!`, or panic message that captures it) yields a redaction
/// marker, never the secret. The raw value is reachable only through
/// [`Self::expose`], which is called exactly once — at the keychain write.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyMaterial(String);

impl KeyMaterial {
    /// Wrap a raw secret value.
    #[must_use]
    pub const fn new(value: String) -> Self {
        Self(value)
    }

    /// Reveal the raw secret. The **only** unredacted accessor — call it solely
    /// at the keychain write site, never in a log.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for KeyMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the value. A length hint is safe and aids debugging.
        write!(f, "KeyMaterial(REDACTED, {} chars)", self.0.chars().count())
    }
}

/// The five settings sections, in j/k navigation order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSection {
    /// Daemon connection (socket / pid / uptime / version).
    Daemon,
    /// Registered LLM providers.
    Providers,
    /// Stored LLM keys (masked).
    Keys,
    /// Workspace switcher.
    Workspaces,
    /// Workspace members (render-only list; e38.11). The mutation surface is
    /// CLI-first (`ainb hangar member set-role|remove`) — the pane lists each
    /// member with their role so the operator can see who can do what.
    Members,
    /// Per-attention-kind notification routing rules (tcp T5): a grid of kind ×
    /// channel toggles. `]`/`[` move the kind row, `h`/`l` move the channel column,
    /// `space` toggles the selected cell (emitting a `SetNotifyRule` intent →
    /// `hangar/notify_rule_set`), and `g` flips the edit scope between the
    /// host-wide GLOBAL rule and the active workspace override
    /// (agents-in-a-box-cqh).
    Notifications,
}

impl SettingsSection {
    /// The next section down (j), clamped at [`Self::Notifications`].
    #[must_use]
    const fn next(self) -> Self {
        match self {
            Self::Daemon => Self::Providers,
            Self::Providers => Self::Keys,
            Self::Keys => Self::Workspaces,
            Self::Workspaces => Self::Members,
            Self::Members | Self::Notifications => Self::Notifications,
        }
    }

    /// The previous section up (k), clamped at [`Self::Daemon`].
    #[must_use]
    const fn prev(self) -> Self {
        match self {
            Self::Daemon | Self::Providers => Self::Daemon,
            Self::Keys => Self::Providers,
            Self::Workspaces => Self::Keys,
            Self::Members => Self::Workspaces,
            Self::Notifications => Self::Members,
        }
    }

    /// The section header label.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Daemon => "Daemon",
            Self::Providers => "Providers",
            Self::Keys => "LLM Keys",
            Self::Workspaces => "Workspaces",
            Self::Members => "Members",
            Self::Notifications => "Notifications",
        }
    }
}

/// The daemon-connection status, driving the section's colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionStatus {
    /// Connected (green).
    Connected,
    /// Disconnected (red).
    Disconnected,
}

/// The scope the Notifications routing grid edits (agents-in-a-box-cqh).
///
/// GLOBAL edits the host-wide default rule (`workspace_id IS NULL`) — the rule a
/// HOOK-raised attention resolves, because hook sessions are workspace-less
/// (`attention_ingest` resolves `workspace=None`). So editing in `Global` scope
/// is what actually re-routes the live ASK / error notifications a user sees.
/// WORKSPACE edits the active workspace's OVERRIDE, which only governs attentions
/// raised WITH that workspace attached (escalations, standups). The grid defaults
/// to [`Self::Global`] so the common edit lands on the rule the fleet-wide
/// attentions honour — "what you edit is what applies".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyScope {
    /// The host-wide default rule (`workspace_id IS NULL`) — what hook-raised
    /// attentions resolve.
    Global,
    /// The active workspace's override rule.
    Workspace,
}

impl NotifyScope {
    /// Flip to the other scope.
    #[must_use]
    const fn toggled(self) -> Self {
        match self {
            Self::Global => Self::Workspace,
            Self::Workspace => Self::Global,
        }
    }

    /// The scope label rendered in the grid header + hint.
    #[must_use]
    const fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace => "workspace",
        }
    }

    /// The `workspace_id` a `notify_rules_list` for this scope carries: `None` for
    /// GLOBAL (the host-wide default), `Some(ws_id)` for WORKSPACE.
    #[must_use]
    pub fn scoped_workspace_id<'a>(self, ws_id: &'a str) -> Option<&'a str> {
        match self {
            Self::Global => None,
            Self::Workspace => Some(ws_id),
        }
    }
}

/// Whether a `hangar/notify_rules_list` reply that answered `reply_workspace_id`
/// (the scope it echoes) should be applied to a grid whose CURRENT scope is
/// `scope` over workspace `ws_id` (agents-in-a-box-cqh).
///
/// A reply for the scope the grid already LEFT (the human flipped `g` while the
/// list was in flight) is DROPPED so it can't briefly repopulate the grid with the
/// wrong scope's rows before the in-scope reply lands.
#[must_use]
pub fn notify_reply_matches_scope(
    scope: NotifyScope,
    ws_id: &str,
    reply_workspace_id: Option<&str>,
) -> bool {
    reply_workspace_id == scope.scoped_workspace_id(ws_id)
}

/// The render-state cache for the settings screen.
///
/// Holds the four daemon snapshots, the active section, the in-section list
/// selection, the connection status, and the key-entry modal flag. The
/// in-flight key-entry buffer is held as [`KeyMaterial`] so it can never leak
/// through a `Debug`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsState {
    health: HealthSnapshot,
    providers: Vec<ProviderRow>,
    keys: Vec<KeyRow>,
    workspaces: Vec<WorkspaceRow>,
    /// The current workspace's members (render-only; e38.11).
    members: Vec<MemberWireRow>,
    /// The notification routing grid rows, one per attention kind (tcp T5).
    notify_rules: Vec<NotifyRuleWireRow>,
    /// The selected kind row in the Notifications grid.
    notify_kind_sel: usize,
    /// The selected channel column (0..[`Channel::all`]) in the Notifications grid.
    notify_channel_sel: usize,
    /// The scope the grid edits — GLOBAL (host-wide default, what hook-raised
    /// attentions honour) or the active WORKSPACE override (agents-in-a-box-cqh).
    notify_scope: NotifyScope,
    section: SettingsSection,
    /// Selection within the active section's list (workspaces / keys).
    list_selected: usize,
    connection: ConnectionStatus,
    /// The key-entry modal's in-flight value, present while the modal is open.
    key_entry: Option<KeyMaterial>,
    /// The live stored value of every daemon-config knob, indexed 1:1 to
    /// [`DAEMON_CONFIG_REGISTRY`]. `None` means the knob has no stored row (the
    /// descriptor's coded default applies). Read from the daemon via
    /// `hangar/daemon_config_list` and written back per-knob via
    /// `hangar/daemon_config_set`.
    config_values: Vec<Option<String>>,
    /// The cursor over the Daemon-section config rows (0..registry len). Only
    /// meaningful while the Daemon section is active.
    config_sel: usize,
    /// The in-flight numeric-input overlay for an `Int` knob (opened with
    /// Enter/Space on an int row). `None` when no overlay is open.
    config_input: Option<ConfigInput>,
    /// The in-flight new-workspace name modal (opened with `n` on the Workspaces
    /// section). Holds the name typed so far; Enter derives the slug and emits a
    /// [`SettingsIntent::CreateWorkspace`], Esc cancels. `None` when closed
    /// (P-multica#4).
    workspace_name_input: Option<String>,
}

/// The in-flight numeric-input overlay for editing an `Int` daemon-config knob.
///
/// Holds the registry index being edited and the digits typed so far. Enter
/// commits (validated against the descriptor's range); Esc cancels in a single
/// press (clearing this — never a partial step-back).
///
/// The buffer opens EMPTY rather than seeded with the current value: digits
/// append, so a seeded `15` turned a user typing `3` into `153` — the opposite of
/// the intent. The current value is shown as a ghost hint instead (see
/// [`render_config_input_overlay`]).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConfigInput {
    /// The [`DAEMON_CONFIG_REGISTRY`] index being edited.
    reg_index: usize,
    /// The digits typed so far.
    buffer: String,
    /// The value in force while the overlay is open, shown as a ghost hint so
    /// the human can see what they are replacing.
    current: String,
    /// The rejection message from the last failed Enter, painted in the box. The
    /// overlay stays open on an invalid value so the human can correct it rather
    /// than silently losing the edit.
    error: Option<String>,
}

impl SettingsState {
    /// A fresh settings state from the four daemon snapshots, landing on the
    /// daemon section with connection status derived from `health.connected`.
    #[must_use]
    pub fn new(
        health: HealthSnapshot,
        providers: Vec<ProviderRow>,
        keys: Vec<KeyRow>,
        workspaces: Vec<WorkspaceRow>,
    ) -> Self {
        let connection = if health.connected {
            ConnectionStatus::Connected
        } else {
            ConnectionStatus::Disconnected
        };
        Self {
            health,
            providers,
            keys,
            workspaces,
            members: Vec::new(),
            notify_rules: Vec::new(),
            notify_kind_sel: 0,
            notify_channel_sel: 0,
            notify_scope: NotifyScope::Global,
            section: SettingsSection::Daemon,
            list_selected: 0,
            connection,
            key_entry: None,
            config_values: vec![None; DAEMON_CONFIG_REGISTRY.len()],
            config_sel: 0,
            config_input: None,
            workspace_name_input: None,
        }
    }

    /// The in-flight new-workspace name buffer, present while the modal is open
    /// (for render + tests).
    #[must_use]
    pub fn workspace_name_input(&self) -> Option<&str> {
        self.workspace_name_input.as_deref()
    }

    /// The effective value string for the config knob at registry `idx`: the
    /// stored value when set, else the descriptor's coded default. Panics only on
    /// an out-of-range index, which the reducer/render never produce (both clamp
    /// to the registry length).
    #[must_use]
    fn config_effective(&self, idx: usize) -> &str {
        self.config_values[idx]
            .as_deref()
            .unwrap_or(DAEMON_CONFIG_REGISTRY[idx].default)
    }

    /// Overwrite one knob's live value by key (from a `daemon_config_list`/`_set`
    /// reply). An unknown key is ignored so a stray/legacy key can't panic.
    pub fn set_config_value(&mut self, key: &str, value: Option<String>) {
        if let Some(idx) = DAEMON_CONFIG_REGISTRY.iter().position(|d| d.key == key) {
            self.config_values[idx] = value;
        }
    }

    /// The live config values, indexed to [`DAEMON_CONFIG_REGISTRY`] (for tests +
    /// the glue's `set_health` carry-over).
    #[must_use]
    pub fn config_values(&self) -> &[Option<String>] {
        &self.config_values
    }

    /// The Daemon-section config-row cursor (for tests).
    #[must_use]
    pub const fn config_sel(&self) -> usize {
        self.config_sel
    }

    /// The in-flight numeric overlay's buffer, or `None` when no overlay is open
    /// (for tests + render).
    #[must_use]
    pub fn config_input_buffer(&self) -> Option<&str> {
        self.config_input.as_ref().map(|c| c.buffer.as_str())
    }

    /// The rejection message shown in the open numeric overlay, if the last Enter
    /// was refused by the descriptor's validation (for tests + render).
    #[must_use]
    pub fn config_input_error(&self) -> Option<&str> {
        self.config_input.as_ref().and_then(|c| c.error.as_deref())
    }

    /// The in-section list cursor (workspaces / keys / providers / members).
    ///
    /// Exposed for the #450 reserved-key tests, which prove the rebound `]` / `[`
    /// bindings actually move it through the real key path.
    #[must_use]
    pub const fn list_selected(&self) -> usize {
        self.list_selected
    }

    /// The active section.
    #[must_use]
    pub const fn section(&self) -> SettingsSection {
        self.section
    }

    /// Replace the workspace rows (e.g. after a switch re-fetch), keeping the
    /// active section + selection clamped to the new list. Used by the plugin
    /// glue to refresh the pane after `host/workspace_set_active`.
    pub fn set_workspaces(&mut self, workspaces: Vec<WorkspaceRow>) {
        let max = workspaces.len().saturating_sub(1);
        if self.list_selected > max {
            self.list_selected = max;
        }
        self.workspaces = workspaces;
    }

    /// The workspace rows currently rendered (for the glue / tests).
    #[must_use]
    pub fn workspaces(&self) -> &[WorkspaceRow] {
        &self.workspaces
    }

    /// Replace the member rows (after a `hangar/members_list` fetch). The pane is
    /// render-only, so this only swaps the cached rows; no selection to clamp.
    pub fn set_members(&mut self, members: Vec<MemberWireRow>) {
        self.members = members;
    }

    /// The member rows currently rendered (for the glue / tests).
    #[must_use]
    pub fn members(&self) -> &[MemberWireRow] {
        &self.members
    }

    /// Replace the notification routing grid (after a `hangar/notify_rules_list`
    /// fetch), clamping the kind cursor to the new row count (tcp T5).
    pub fn set_notify_rules(&mut self, rules: Vec<NotifyRuleWireRow>) {
        let max = rules.len().saturating_sub(1);
        if self.notify_kind_sel > max {
            self.notify_kind_sel = max;
        }
        self.notify_rules = rules;
    }

    /// The notification routing rows currently rendered (for the glue / tests).
    #[must_use]
    pub fn notify_rules(&self) -> &[NotifyRuleWireRow] {
        &self.notify_rules
    }

    /// The Notifications grid cursor as `(kind_row, channel_col)` (for tests).
    #[must_use]
    pub const fn notify_cursor(&self) -> (usize, usize) {
        (self.notify_kind_sel, self.notify_channel_sel)
    }

    /// The scope the Notifications grid currently edits (global/workspace,
    /// agents-in-a-box-cqh) — the glue reads it to scope the `notify_*` RPCs.
    #[must_use]
    pub const fn notify_scope(&self) -> NotifyScope {
        self.notify_scope
    }

    /// The daemon-connection status.
    #[must_use]
    pub const fn connection_status(&self) -> ConnectionStatus {
        self.connection
    }

    /// Whether the key-entry modal is open.
    #[must_use]
    pub const fn key_entry_open(&self) -> bool {
        self.key_entry.is_some()
    }

    /// The live `autostandup.enabled` value, decoded from the generic config map
    /// (the single internal source) with the daemon's tolerant bool parse — a
    /// convenience view kept for the `a` shortcut + call sites that want the bool.
    #[must_use]
    pub fn autostandup_enabled(&self) -> bool {
        DAEMON_CONFIG_REGISTRY
            .iter()
            .position(|d| d.key == ainb_hangar_core::daemon_config::KEY_AUTOSTANDUP_ENABLED)
            .is_some_and(|idx| parse_bool_token(self.config_effective(idx)).unwrap_or(false))
    }

    /// Overwrite the `autostandup.enabled` value from a snapshot/re-read, writing
    /// through the generic config map so there is one internal source of truth.
    pub fn set_autostandup_enabled(&mut self, on: bool) {
        self.set_config_value(
            ainb_hangar_core::daemon_config::KEY_AUTOSTANDUP_ENABLED,
            Some(if on { "true" } else { "false" }.to_string()),
        );
    }
}

/// An input the settings reducer folds into [`SettingsState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEvent {
    /// A printable key. While the key-entry modal is open, printable chars
    /// extend the in-flight key value.
    Key(char),
    /// Move the in-section cursor up one row (the `↑` arrow).
    ///
    /// The cursor is bound to the arrows rather than a printable char because the
    /// routing layer claims every uppercase tab-switch char (`K` → Kanban, …)
    /// BEFORE the screen reducer sees it, so a `J`/`K` cursor could only ever move
    /// one way. Arrows are unclaimed, so both directions reach this reducer.
    CursorUp,
    /// Move the in-section cursor down one row (the `↓` arrow).
    CursorDown,
    /// Escape — aborts whichever modal is open.
    Esc,
    /// The daemon stream dropped (flips the connection status to red).
    DaemonDisconnected,
}

/// A side-effect the plugin glue performs after a settings reduction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsIntent {
    /// Write a key to the OS keychain (Enter in the key-entry modal). The
    /// `key` is [`KeyMaterial`] so the intent itself is log-safe.
    KeychainWrite {
        /// The provider the key authenticates.
        provider: String,
        /// The secret, redacted in any `Debug`/log.
        key: KeyMaterial,
    },
    /// Switch the active workspace (`s` on the Workspace pane). Carries the
    /// stable ULID workspace id (never the slug). The plugin glue maps this to
    /// the `host/workspace_set_active` cap.
    SwitchWorkspace(String),
    /// Toggle the default workspace (`d`). Carries the workspace id; maps to
    /// `host/workspace_set_default`.
    ToggleDefault(String),
    /// Create a new workspace (`Enter` in the name modal opened by `n`). Carries
    /// the derived `slug` + the typed `name`; the glue maps it to
    /// `host/workspace_create` (P-multica#4).
    CreateWorkspace {
        /// The slug derived from the typed name (`lower`, spaces → hyphens).
        slug: String,
        /// The human-readable name the operator typed.
        name: String,
    },
    /// Delete the selected workspace (`x` on the Workspace pane). Carries the
    /// stable ULID id; the glue maps it to `host/workspace_delete` (P-multica#4).
    DeleteWorkspace(String),
    /// Rename the selected workspace (`r`). Carries the workspace id.
    RenameWorkspace(String),
    /// Toggle one notification routing cell (`space` on the Notifications grid,
    /// tcp T5). Carries the attention kind wire token and the FULL new channel set
    /// for that kind; the glue maps it to `hangar/notify_rule_set` scoped to the
    /// state's current [`SettingsState::notify_scope`].
    SetNotifyRule {
        /// The attention kind wire token the rule governs.
        kind: String,
        /// The new push-channel set after the toggle.
        channels: ChannelSet,
    },
    /// Persist one daemon-config knob (edited on the Daemon section: a bool
    /// toggle, an enum cycle, or a committed int overlay). Carries the registry
    /// `key` + the NEW normalized `value`; the glue maps it to a
    /// `hangar/daemon_config_set` write, whose reply re-reads the whole config so
    /// the pane reflects the persisted state.
    SetDaemonConfig {
        /// The `daemon_config` registry key being written.
        key: String,
        /// The new value to persist (already normalized by the editor).
        value: String,
    },
    /// Re-fetch the Notifications grid for the current scope (`g` flipped the
    /// grid's global/workspace scope, agents-in-a-box-cqh). The glue maps it to a
    /// `hangar/notify_rules_list` scoped to the state's [`SettingsState::notify_scope`]
    /// so the rows the human now edits match the scope indicator.
    RefreshNotifyRules,
}

/// The result of folding one [`SettingsEvent`] into a [`SettingsState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsReduction {
    /// The next settings state.
    pub state: SettingsState,
    /// A side-effect for the plugin glue, if any.
    pub intent: Option<SettingsIntent>,
}

/// Fold one [`SettingsEvent`] into `state`. Pure: no IO, no input mutation.
#[must_use]
pub fn reduce_settings(state: &SettingsState, ev: SettingsEvent) -> SettingsReduction {
    match ev {
        SettingsEvent::Esc => reduce_esc(state),
        SettingsEvent::DaemonDisconnected => daemon_disconnected(state),
        SettingsEvent::Key(c) => reduce_key(state, c),
        SettingsEvent::CursorUp => reduce_cursor(state, -1),
        SettingsEvent::CursorDown => reduce_cursor(state, 1),
    }
}

/// Move the active section's row cursor by `delta`. On the Daemon section that is
/// the config-knob cursor; on Workspaces/Keys it is the in-section list
/// selection. A cursor key is inert while a modal owns the keyboard — the
/// key-entry modal and the numeric overlay both capture the arrows rather than
/// let them scroll the pane underneath. The Notifications grid keeps its own
/// 2D cursor keys (`]`/`[` × `h`/`l`) and is left alone here.
fn reduce_cursor(state: &SettingsState, delta: i32) -> SettingsReduction {
    if state.key_entry.is_some()
        || state.config_input.is_some()
        || state.workspace_name_input.is_some()
    {
        return unchanged(state);
    }
    match state.section {
        SettingsSection::Daemon => move_config_sel(state, delta),
        SettingsSection::Notifications => unchanged(state),
        _ => move_list(state, delta),
    }
}

/// Handle a printable key, branching on the active modal.
fn reduce_key(state: &SettingsState, c: char) -> SettingsReduction {
    if state.key_entry.is_some() {
        return reduce_key_entry_key(state, c);
    }
    // The new-workspace name modal owns the keyboard while open (P-multica#4).
    if state.workspace_name_input.is_some() {
        return reduce_workspace_name_key(state, c);
    }
    // The numeric-input overlay owns the keyboard while open (digits/commit/cancel).
    if state.config_input.is_some() {
        return reduce_config_input_key(state, c);
    }
    // The Notifications grid owns a 2D cursor (kind × channel) + a toggle, so it
    // handles its own keys; `j`/`k` still move between sections.
    if state.section == SettingsSection::Notifications {
        return reduce_notify_key(state, c);
    }
    // The Daemon section is a cursor over the config knobs (Enter/Space edit)
    // plus the `a` auto-standup shortcut; `j`/`k` still leave the section.
    if state.section == SettingsSection::Daemon {
        return reduce_daemon_key(state, c);
    }
    match c {
        'j' => move_section(state, SettingsSection::next),
        'k' => move_section(state, SettingsSection::prev),
        // `]`/`[` move the in-section list selection (workspaces / keys). They
        // were `J`/`K` until #450 — the hangar router claims bare `K` as the
        // Kanban tab, so `K` never reached this reducer.
        ']' => move_list(state, 1),
        '[' => move_list(state, -1),
        'n' if state.section == SettingsSection::Keys => open_key_entry(state),
        // Workspace pane controls (P5.5): s set-active, d toggle-default,
        // n new, r rename. All scoped to the Workspaces section.
        's' if state.section == SettingsSection::Workspaces => {
            workspace_intent(state, SettingsIntent::SwitchWorkspace)
        }
        'd' if state.section == SettingsSection::Workspaces => {
            workspace_intent(state, SettingsIntent::ToggleDefault)
        }
        'r' if state.section == SettingsSection::Workspaces => {
            workspace_intent(state, SettingsIntent::RenameWorkspace)
        }
        // `n` opens the new-workspace name modal; `x` deletes the selected
        // workspace (never the active row) (P-multica#4).
        'n' if state.section == SettingsSection::Workspaces => open_workspace_name_entry(state),
        'x' if state.section == SettingsSection::Workspaces => workspace_delete_intent(state),
        _ => unchanged(state),
    }
}

/// Emit a workspace intent for the currently-selected workspace row, mapping
/// its stable ULID id through `make`. A no-op (unchanged) when the list is
/// empty so a stray key on an empty pane can't emit a bogus intent.
fn workspace_intent(
    state: &SettingsState,
    make: fn(String) -> SettingsIntent,
) -> SettingsReduction {
    state.workspaces.get(state.list_selected).map_or_else(
        || unchanged(state),
        |w| SettingsReduction {
            state: state.clone(),
            intent: Some(make(w.id.clone())),
        },
    )
}

/// Emit a `DeleteWorkspace` intent for the selected row — but NEVER the active
/// one (you cannot delete the tenant you are standing in; the host would reject
/// it with `-32602` anyway, so guard here for a clean no-op) (P-multica#4).
fn workspace_delete_intent(state: &SettingsState) -> SettingsReduction {
    match state.workspaces.get(state.list_selected) {
        Some(w) if !w.current => SettingsReduction {
            state: state.clone(),
            intent: Some(SettingsIntent::DeleteWorkspace(w.id.clone())),
        },
        _ => unchanged(state),
    }
}

/// Open the new-workspace name modal with an empty buffer (P-multica#4).
fn open_workspace_name_entry(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    next.workspace_name_input = Some(String::new());
    no_intent(next)
}

/// Handle a key while the new-workspace name modal is open: Enter creates,
/// Backspace trims, any printable char extends the name (P-multica#4).
fn reduce_workspace_name_key(state: &SettingsState, c: char) -> SettingsReduction {
    match c {
        '\n' | '\r' => confirm_workspace_name(state),
        '\u{8}' | '\u{7f}' => {
            let mut next = state.clone();
            if let Some(buf) = next.workspace_name_input.as_mut() {
                buf.pop();
            }
            no_intent(next)
        }
        _ => {
            let mut next = state.clone();
            if let Some(buf) = next.workspace_name_input.as_mut() {
                buf.push(c);
            }
            no_intent(next)
        }
    }
}

/// Confirm the name modal: close it and, if the name yields a non-empty slug,
/// emit a `CreateWorkspace { slug, name }` intent. A blank/slug-less name closes
/// the modal with no intent (nothing to create) (P-multica#4).
fn confirm_workspace_name(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    let name = next.workspace_name_input.take().unwrap_or_default();
    let name = name.trim().to_string();
    let slug = slugify(&name);
    if slug.is_empty() {
        return no_intent(next);
    }
    SettingsReduction {
        state: next,
        intent: Some(SettingsIntent::CreateWorkspace { slug, name }),
    }
}

/// Derive a workspace slug from a display name: lower-case, map spaces/underscores
/// to hyphens, drop every other non-`[a-z0-9-]` char, and collapse/trim hyphens
/// so the result satisfies the host's `^[a-z0-9]+(-[a-z0-9]+)*$` guard (the host
/// re-validates; this just gives a sensible default) (P-multica#4).
fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut prev_hyphen = false;
    for ch in name.chars() {
        let mapped = if ch.is_ascii_alphanumeric() {
            Some(ch.to_ascii_lowercase())
        } else if ch == ' ' || ch == '_' || ch == '-' {
            Some('-')
        } else {
            None
        };
        match mapped {
            Some('-') => {
                if !out.is_empty() && !prev_hyphen {
                    out.push('-');
                    prev_hyphen = true;
                }
            }
            Some(c) => {
                out.push(c);
                prev_hyphen = false;
            }
            None => {}
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Handle a key on the Daemon section: `j`/`k` leave to the adjacent section,
/// Enter/Space edit the selected knob, and `a` is the auto-standup shortcut
/// (toggles `autostandup.enabled` from anywhere in the section, regardless of the
/// cursor).
///
/// The config-row cursor is NOT bound here: it is [`SettingsEvent::CursorUp`] /
/// [`SettingsEvent::CursorDown`] (the arrows). A `J`/`K` pair looks natural but
/// cannot work — the routing layer maps `K` to the Kanban tab before the key ever
/// reaches this reducer, so `K` would yank the user off Settings while `J` moved
/// the cursor: a cursor that only travels one way.
fn reduce_daemon_key(state: &SettingsState, c: char) -> SettingsReduction {
    match c {
        'j' => move_section(state, SettingsSection::next),
        'k' => move_section(state, SettingsSection::prev),
        '\n' | '\r' | ' ' => edit_config_row(state, state.config_sel),
        'a' => edit_autostandup_shortcut(state),
        _ => unchanged(state),
    }
}

/// Move the Daemon-section config-row cursor by `delta`, clamped to the registry.
fn move_config_sel(state: &SettingsState, delta: i32) -> SettingsReduction {
    let mut next = state.clone();
    let max = DAEMON_CONFIG_REGISTRY.len().saturating_sub(1);
    if delta < 0 {
        next.config_sel = next.config_sel.saturating_sub(1);
    } else {
        next.config_sel = (next.config_sel + 1).min(max);
    }
    no_intent(next)
}

/// Edit the config knob at registry `idx`: toggle a bool, cycle an enum, or open
/// the numeric overlay for an int. Bool/enum edits flip the pane optimistically
/// and emit a [`SettingsIntent::SetDaemonConfig`] the glue persists; the int case
/// opens the overlay and emits nothing until Enter commits.
fn edit_config_row(state: &SettingsState, idx: usize) -> SettingsReduction {
    let Some(desc) = DAEMON_CONFIG_REGISTRY.get(idx) else {
        return unchanged(state);
    };
    match desc.kind {
        ConfigKind::Bool => {
            let now = parse_bool_token(state.config_effective(idx)).unwrap_or(false);
            let value = if now { "false" } else { "true" };
            commit_config(state, idx, value.to_string())
        }
        ConfigKind::Enum { variants } => {
            let cur = state.config_effective(idx);
            // Cycle to the next variant after the current one (wrapping); default
            // to the first when the current value is not a known variant.
            let pos = variants.iter().position(|v| v.eq_ignore_ascii_case(cur)).unwrap_or(0);
            let next_variant = variants[(pos + 1) % variants.len()];
            commit_config(state, idx, next_variant.to_string())
        }
        ConfigKind::Int { .. } => {
            let mut next = state.clone();
            next.config_input = Some(ConfigInput {
                reg_index: idx,
                buffer: String::new(),
                current: state.config_effective(idx).to_string(),
                error: None,
            });
            no_intent(next)
        }
    }
}

/// The `a` shortcut: toggle `autostandup.enabled` from anywhere in the Daemon
/// section (independent of the cursor), matching the long-standing hotkey.
fn edit_autostandup_shortcut(state: &SettingsState) -> SettingsReduction {
    DAEMON_CONFIG_REGISTRY
        .iter()
        .position(|d| d.key == ainb_hangar_core::daemon_config::KEY_AUTOSTANDUP_ENABLED)
        .map_or_else(|| unchanged(state), |idx| edit_config_row(state, idx))
}

/// Optimistically set knob `idx` to `value` and emit the persist intent. The
/// glue writes it via `hangar/daemon_config_set`, then re-reads the whole config
/// so the pane reconciles to the daemon's stored value.
fn commit_config(state: &SettingsState, idx: usize, value: String) -> SettingsReduction {
    let mut next = state.clone();
    next.config_values[idx] = Some(value.clone());
    SettingsReduction {
        state: next,
        intent: Some(SettingsIntent::SetDaemonConfig {
            key: DAEMON_CONFIG_REGISTRY[idx].key.to_string(),
            value,
        }),
    }
}

/// Handle a key while the numeric-input overlay is open: Enter commits (only when
/// the typed value passes the descriptor's range validation — an invalid value
/// keeps the overlay OPEN and paints the descriptor's own rejection message, so a
/// mistyped number is correctable rather than silently discarded), Backspace
/// trims, a digit extends the buffer, and every other key is ignored. Esc is
/// handled by [`reduce_esc`] and cancels in a single press.
fn reduce_config_input_key(state: &SettingsState, c: char) -> SettingsReduction {
    let Some(input) = &state.config_input else {
        return unchanged(state);
    };
    match c {
        '\n' | '\r' => {
            let idx = input.reg_index;
            let desc = &DAEMON_CONFIG_REGISTRY[idx];
            // Validate against the descriptor; a valid value commits + closes. An
            // invalid one keeps the overlay open carrying `validate`'s human error
            // (e.g. "must be between 1 and 1440, got 99999") — never a silent
            // close, and never a write.
            match desc.validate(&input.buffer) {
                Ok(value) => {
                    let mut committed = commit_config(state, idx, value);
                    committed.state.config_input = None;
                    committed
                }
                Err(msg) => {
                    let mut next = state.clone();
                    if let Some(inp) = next.config_input.as_mut() {
                        inp.error = Some(msg);
                    }
                    no_intent(next)
                }
            }
        }
        '\u{8}' | '\u{7f}' => {
            let mut next = state.clone();
            if let Some(inp) = next.config_input.as_mut() {
                inp.buffer.pop();
                // Editing clears the stale rejection: the message described the
                // value the human is now changing.
                inp.error = None;
            }
            no_intent(next)
        }
        d if d.is_ascii_digit() => {
            let mut next = state.clone();
            if let Some(inp) = next.config_input.as_mut() {
                inp.buffer.push(d);
                inp.error = None;
            }
            no_intent(next)
        }
        _ => unchanged(state),
    }
}

/// Handle a key on the Notifications grid (tcp T5): `j`/`k` leave to the adjacent
/// section, `]`/`[` move the kind row, `h`/`l` move the channel column, and
/// `space`/`t` toggle the selected cell (emitting a `SetNotifyRule` intent).
///
/// The kind-row pair was `J`/`K` until #450 — bare `K` is the router's Kanban tab
/// key and never reached this reducer, so the advertised `J/K kind` hint lied.
fn reduce_notify_key(state: &SettingsState, c: char) -> SettingsReduction {
    match c {
        'j' => move_section(state, SettingsSection::next),
        'k' => move_section(state, SettingsSection::prev),
        ']' => move_notify_kind(state, 1),
        '[' => move_notify_kind(state, -1),
        'h' => move_notify_channel(state, -1),
        'l' => move_notify_channel(state, 1),
        ' ' | 't' => toggle_notify_cell(state),
        'g' => toggle_notify_scope(state),
        _ => unchanged(state),
    }
}

/// Flip the grid's global/workspace scope (`g`, agents-in-a-box-cqh) and clear the
/// loaded rows + reset the kind cursor, so a stale-scope cell can't be toggled
/// before the re-fetch lands (`toggle_notify_cell` no-ops on an empty grid).
/// Emits a [`SettingsIntent::RefreshNotifyRules`] so the glue re-lists the rules
/// for the NEW scope — what the human now edits matches the scope they see.
fn toggle_notify_scope(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    next.notify_scope = next.notify_scope.toggled();
    next.notify_rules.clear();
    next.notify_kind_sel = 0;
    SettingsReduction {
        state: next,
        intent: Some(SettingsIntent::RefreshNotifyRules),
    }
}

/// Move the Notifications kind-row cursor by `delta`, clamped to the grid.
fn move_notify_kind(state: &SettingsState, delta: i32) -> SettingsReduction {
    let mut next = state.clone();
    let max = next.notify_rules.len().saturating_sub(1);
    if delta < 0 {
        next.notify_kind_sel = next.notify_kind_sel.saturating_sub(1);
    } else if !next.notify_rules.is_empty() {
        next.notify_kind_sel = (next.notify_kind_sel + 1).min(max);
    }
    no_intent(next)
}

/// Move the Notifications channel-column cursor by `delta`, clamped to the four
/// channels.
fn move_notify_channel(state: &SettingsState, delta: i32) -> SettingsReduction {
    let mut next = state.clone();
    let max = Channel::all().len() - 1;
    if delta < 0 {
        next.notify_channel_sel = next.notify_channel_sel.saturating_sub(1);
    } else {
        next.notify_channel_sel = (next.notify_channel_sel + 1).min(max);
    }
    no_intent(next)
}

/// Toggle the selected `(kind, channel)` cell: flip the channel in the kind's
/// set, update the grid optimistically, and emit a [`SettingsIntent::SetNotifyRule`]
/// carrying the FULL new set (the RPC is an upsert of the whole set). A no-op when
/// the grid is empty (not yet fetched) so a stray toggle can't emit a bogus rule.
fn toggle_notify_cell(state: &SettingsState) -> SettingsReduction {
    let channel = Channel::all()[state.notify_channel_sel];
    let Some(rule) = state.notify_rules.get(state.notify_kind_sel) else {
        return unchanged(state);
    };
    let kind = rule.kind.clone();
    let new_channels = rule.channels.toggled(channel);
    let mut next = state.clone();
    // Optimistic local update so the cell flips immediately; the glue's
    // re-fetch after the RPC reconciles authoritatively.
    next.notify_rules[next.notify_kind_sel].channels = new_channels;
    SettingsReduction {
        state: next,
        intent: Some(SettingsIntent::SetNotifyRule {
            kind,
            channels: new_channels,
        }),
    }
}

/// Handle a key while the key-entry modal is open: Enter writes, Backspace
/// trims, any printable char extends the in-flight value.
fn reduce_key_entry_key(state: &SettingsState, c: char) -> SettingsReduction {
    match c {
        '\n' | '\r' => confirm_key_entry(state),
        '\u{8}' | '\u{7f}' => {
            let mut next = state.clone();
            if let Some(km) = &next.key_entry {
                let mut s = km.expose().to_string();
                s.pop();
                next.key_entry = Some(KeyMaterial::new(s));
            }
            no_intent(next)
        }
        _ => {
            let mut next = state.clone();
            if let Some(km) = &next.key_entry {
                let mut s = km.expose().to_string();
                s.push(c);
                next.key_entry = Some(KeyMaterial::new(s));
            }
            no_intent(next)
        }
    }
}

/// Esc: cancel whichever overlay is open in a SINGLE press — the numeric config
/// input or the key-entry modal — clearing it outright (never a partial
/// step-back that would leave the overlay half-open). A no-op when neither is up.
fn reduce_esc(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    next.config_input = None;
    next.key_entry = None;
    next.workspace_name_input = None;
    no_intent(next)
}

/// Flip the connection status to red on a daemon disconnect.
fn daemon_disconnected(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    next.connection = ConnectionStatus::Disconnected;
    no_intent(next)
}

/// Move the active section with `step` (next/prev), resetting the list cursor.
fn move_section(
    state: &SettingsState,
    step: fn(SettingsSection) -> SettingsSection,
) -> SettingsReduction {
    let mut next = state.clone();
    next.section = step(next.section);
    next.list_selected = 0;
    no_intent(next)
}

/// Move the in-section list selection by `delta`, clamped to the section's list.
fn move_list(state: &SettingsState, delta: i32) -> SettingsReduction {
    let mut next = state.clone();
    let len = match next.section {
        SettingsSection::Workspaces => next.workspaces.len(),
        SettingsSection::Keys => next.keys.len(),
        SettingsSection::Providers => next.providers.len(),
        SettingsSection::Members => next.members.len(),
        // Notifications owns its own 2D cursor (see `reduce_notify_key`); the
        // generic list mover is never reached for it.
        SettingsSection::Daemon | SettingsSection::Notifications => 0,
    };
    if delta < 0 {
        next.list_selected = next.list_selected.saturating_sub(1);
    } else if len > 0 {
        next.list_selected = (next.list_selected + 1).min(len - 1);
    }
    no_intent(next)
}

/// Open the key-entry modal with an empty in-flight value.
fn open_key_entry(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    next.key_entry = Some(KeyMaterial::new(String::new()));
    no_intent(next)
}

/// Confirm the key-entry modal: close it and emit the (log-safe) keychain write
/// intent for the currently-selected provider section.
fn confirm_key_entry(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    let key = next.key_entry.take().unwrap_or_else(|| KeyMaterial::new(String::new()));
    // The provider is the selected key row's provider, or the first provider as a
    // sensible default when the list is empty.
    let provider = next
        .keys
        .get(next.list_selected)
        .map(|k| k.provider.clone())
        .or_else(|| next.providers.first().map(|p| p.name.clone()))
        .unwrap_or_default();
    SettingsReduction {
        state: next,
        intent: Some(SettingsIntent::KeychainWrite { provider, key }),
    }
}

/// A reduction that changes state but emits no intent.
const fn no_intent(state: SettingsState) -> SettingsReduction {
    SettingsReduction {
        state,
        intent: None,
    }
}

/// A no-op reduction: state cloned unchanged, no intent.
fn unchanged(state: &SettingsState) -> SettingsReduction {
    no_intent(state.clone())
}

// ---------------------------------------------------------------------------
// Width-aware render
// ---------------------------------------------------------------------------

/// Paint the Members section body (render-only; e38.11): each member as
/// `email · role`, the `owner` role in `SELECTION_GREEN` so the administrator
/// stands out, others in `TEXT`. Returns the next free row. Split out of
/// [`render_settings`] to keep that function within the line cap; the mutation
/// surface for members is CLI-first (`ainb hangar member set-role|remove`).
fn render_member_rows(
    buf: &mut WireBuffer,
    area_w: u16,
    mut row: u16,
    bottom: u16,
    members: &[MemberWireRow],
) -> u16 {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const TEXT: Color = Color::rgb(220, 220, 230);
    const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

    for m in members {
        if row >= bottom {
            break;
        }
        let color = if m.role == "owner" {
            SELECTION_GREEN
        } else {
            TEXT
        };
        let line = format!("{} · {}", m.email, m.role);
        for (ch, cx) in line.chars().zip(4..area_w) {
            let mut cell = Cell::new(ch.to_string());
            cell.fg = Some(color);
            buf.push(Coord::new(cx, row), cell);
        }
        row += 1;
    }
    row
}

/// Paint the Daemon section body and return `(next free row, cursor row)`.
///
/// The body is PROGRESSIVE: only the focused section shows its editor.
///
/// - Inactive: the socket line plus a one-line auto-standup summary. The config
///   rows carry a cursor that only the focused section can move, so painting five
///   of them from an unfocused section is both unusable and (at seven rows) enough
///   to push whole sections below the fold.
/// - Active: the socket line, EVERY knob (one row per [`DAEMON_CONFIG_REGISTRY`]
///   descriptor) with its label + live value, and the key hints. The `▶` cursor
///   (in `SELECTION_GREEN`) marks the selected row; a bool ON row also paints
///   green with a filled dot.
///
/// The returned cursor row is `Some` only when the section is active, and lets
/// [`render_settings`] keep the selected knob in view when the pane scrolls.
fn render_daemon_body(
    buf: &mut WireBuffer,
    area_w: u16,
    mut row: u16,
    bottom: u16,
    state: &SettingsState,
) -> (u16, Option<u16>) {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const GREEN: Color = Color::rgb(100, 200, 100);
    const RED: Color = Color::rgb(220, 100, 100);
    const TEXT: Color = Color::rgb(220, 220, 230);
    const MUTED: Color = Color::rgb(120, 120, 140);
    const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

    let section_active = state.section == SettingsSection::Daemon;
    let mut cursor_row = None;
    let put = |buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color| {
        for (ch, cx) in s.chars().zip(x..area_w) {
            let mut cell = Cell::new(ch.to_string());
            cell.fg = Some(color);
            buf.push(Coord::new(cx, row), cell);
        }
    };

    if row < bottom {
        let (status, color) = match state.connection {
            ConnectionStatus::Connected => ("● connected", GREEN),
            ConnectionStatus::Disconnected => ("○ disconnected", RED),
        };
        put(
            buf,
            4,
            row,
            &format!("{} · {status}", state.health.socket_path),
            color,
        );
        row += 1;
    }

    // Unfocused: collapse the editor to the auto-standup summary. `a` still works
    // from anywhere in the section, so the summary names the key that drives it.
    if !section_active {
        if row < bottom {
            let on = state.autostandup_enabled();
            let (dot, color) = if on {
                ("● on", SELECTION_GREEN)
            } else {
                ("○ off", TEXT)
            };
            put(
                buf,
                6,
                row,
                &format!("Auto-standup: {dot}  ·  [a] toggle"),
                color,
            );
            row += 1;
        }
        return (row, None);
    }

    for (idx, desc) in DAEMON_CONFIG_REGISTRY.iter().enumerate() {
        if row >= bottom {
            break;
        }
        let selected = section_active && idx == state.config_sel;
        if selected {
            cursor_row = Some(row);
        }
        let cursor = if selected { "▶ " } else { "  " };
        let value = state.config_effective(idx);
        // A bool row shows a ●/○ dot; other kinds show the raw value. A selected
        // row (or a bool that is ON) accents green; the rest are plain text.
        let (shown, color) = match desc.kind {
            ConfigKind::Bool => {
                let on = parse_bool_token(value).unwrap_or(false);
                let dot = if on { "● on" } else { "○ off" };
                let color = if on { SELECTION_GREEN } else { TEXT };
                (
                    dot.to_string(),
                    if selected { SELECTION_GREEN } else { color },
                )
            }
            _ => (
                value.to_string(),
                if selected { SELECTION_GREEN } else { TEXT },
            ),
        };
        put(
            buf,
            4,
            row,
            &format!("{cursor}{}: {shown}", desc.label),
            color,
        );
        row += 1;
    }

    // Key hints right by the control (house rule).
    if row < bottom {
        put(
            buf,
            4,
            row,
            "↑/↓ move · enter/space edit · [a] auto-standup",
            MUTED,
        );
        row += 1;
    }
    (row, cursor_row)
}

/// A compact display label for an attention-kind wire token (the grid's row
/// header). An unknown token renders verbatim so a future kind is never blank.
fn notify_kind_label(wire_kind: &str) -> &str {
    match wire_kind {
        "ask_user_question" => "ask",
        "codex_request_user" => "codex-req",
        other => other,
    }
}

/// Render the Notifications routing grid (tcp T5): a header row of channel names,
/// then one row per attention kind with a ●/○ cell per channel. The active
/// cursor cell is bracketed + accented, and a hint line names the keys. Returns
/// the next free row. Split out of [`render_settings`] to keep it within the line
/// cap; the toggle mutation is the `space` key (see [`reduce_notify_key`]).
fn render_notify_grid(
    buf: &mut WireBuffer,
    area_w: u16,
    mut row: u16,
    bottom: u16,
    state: &SettingsState,
    section_active: bool,
) -> u16 {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const TEXT: Color = Color::rgb(220, 220, 230);
    const ON: Color = Color::rgb(100, 200, 100);
    const DIM: Color = Color::rgb(120, 120, 130);
    const CURSOR: Color = Color::rgb(255, 215, 0);
    // Cornflower-blue scope indicator — distinct from the gold cursor accent so
    // the two never read as the same control (agents-in-a-box-cqh).
    const SCOPE: Color = Color::rgb(100, 149, 237);

    let put = |buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color| {
        for (ch, cx) in s.chars().zip(x..area_w) {
            let mut cell = Cell::new(ch.to_string());
            cell.fg = Some(color);
            buf.push(Coord::new(cx, row), cell);
        }
    };

    // The four channel columns start after a fixed kind-label gutter; each cell is
    // a fixed width so the header and every row align.
    const KIND_COL: u16 = 4;
    const CELL_W: u16 = 8;
    let channels = Channel::all();

    // Scope indicator + toggle hint (agents-in-a-box-cqh): the grid edits either
    // the host-wide GLOBAL rule (what HOOK-raised attentions honour) or the active
    // workspace's override; `g` flips between them so what you edit is what
    // applies. Rendered above the grid with its key hint right beside it.
    if row < bottom {
        put(
            buf,
            KIND_COL,
            row,
            &format!("scope: {} · [g] toggle", state.notify_scope.label()),
            SCOPE,
        );
        row += 1;
    }

    // Header: channel names above their columns.
    if row < bottom {
        for (i, ch) in channels.iter().enumerate() {
            let cx = KIND_COL + 14 + (u16::try_from(i).unwrap_or(0)) * CELL_W;
            put(buf, cx, row, ch.as_str(), DIM);
        }
        row += 1;
    }

    // One row per kind: label + a ●/○ cell per channel.
    for (ki, rule) in state.notify_rules.iter().enumerate() {
        if row >= bottom {
            break;
        }
        let kind_selected = section_active && ki == state.notify_kind_sel;
        let label = notify_kind_label(&rule.kind);
        let cursor = if kind_selected { "›" } else { " " };
        put(
            buf,
            KIND_COL,
            row,
            &format!("{cursor}{label}"),
            if kind_selected { CURSOR } else { TEXT },
        );
        for (ci, ch) in channels.iter().enumerate() {
            let on = rule.channels.contains(*ch);
            let cell_selected = kind_selected && ci == state.notify_channel_sel;
            // The cursor cell is bracketed so the 2D position reads without colour.
            let glyph = match (cell_selected, on) {
                (true, true) => "[●]",
                (true, false) => "[○]",
                (false, true) => " ● ",
                (false, false) => " ○ ",
            };
            let color = if cell_selected {
                CURSOR
            } else if on {
                ON
            } else {
                DIM
            };
            let cx = KIND_COL + 14 + (u16::try_from(ci).unwrap_or(0)) * CELL_W;
            put(buf, cx, row, glyph, color);
        }
        row += 1;
    }

    // Keybinding hints, rendered right by the control (house rule).
    if row < bottom {
        put(
            buf,
            KIND_COL,
            row,
            "] [ kind · h/l channel · space toggle",
            DIM,
        );
        row += 1;
    }
    row
}

/// Where each section landed in the unscrolled (virtual) paint, so
/// [`render_settings`] can pick a scroll offset without re-deriving the layout.
struct SectionLayout {
    /// Total painted height of every section.
    total: u16,
    /// The active section's first row (its header).
    active_start: u16,
    /// One past the active section's last row.
    active_end: u16,
    /// The active section's selected row, when it has a cursor.
    cursor_row: Option<u16>,
}

/// The scroll offset that keeps the active section — and its cursor — in view.
///
/// The pane paints top-down from a fixed origin, so with no offset a section far
/// enough down the stack simply falls off the bottom and becomes unreachable
/// (invisible AND unusable, since the keys that drive it give no feedback).
///
/// Rules, in order: show everything when it fits; otherwise scroll just far
/// enough to reveal the active section's tail, never past its header, and finally
/// keep the cursor row on screen when the active section is itself taller than
/// the viewport.
fn scroll_offset(layout: &SectionLayout, viewport_h: u16) -> u16 {
    if viewport_h == 0 || layout.total <= viewport_h {
        return 0;
    }
    let mut offset = layout.active_end.saturating_sub(viewport_h);
    offset = offset.min(layout.active_start);
    if let Some(cursor) = layout.cursor_row {
        if cursor < offset {
            offset = cursor;
        } else if cursor >= offset + viewport_h {
            offset = cursor + 1 - viewport_h;
        }
    }
    offset.min(layout.total.saturating_sub(viewport_h))
}

/// Render the settings screen into `buf` between rows `top` and `bottom`.
///
/// The six sections paint stacked top-down; the active one is accent-highlighted
/// and is the only one that expands its editor. When the key-entry modal is open
/// it overlays a centred password-style input (masked, never the raw value).
/// Width-aware: each row's value column is clipped at `area_w`.
///
/// Sections are painted at VIRTUAL rows into a scratch buffer first, then the
/// visible window is blitted in at `top`. That keeps the scroll offset a single
/// decision made against the real layout ([`scroll_offset`]) instead of duplicating
/// each section's height here — the sections' own painters stay the one place that
/// knows how tall they are.
pub fn render_settings(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    top: u16,
    bottom: u16,
    state: &SettingsState,
) {
    use ainb_plugin_sdk::Coord;

    let viewport_h = bottom.saturating_sub(top);
    let mut scratch = WireBuffer::new(area_w, u16::MAX);
    let layout = paint_sections(&mut scratch, area_w, state);
    let offset = scroll_offset(&layout, viewport_h);

    for (coord, cell) in scratch.cells {
        if coord.y >= offset && coord.y - offset < viewport_h {
            buf.push(Coord::new(coord.x, top + (coord.y - offset)), cell);
        }
    }

    // Modal overlays sit above the scrolled pane, centred on the whole area.
    if let Some(km) = &state.key_entry {
        render_key_entry_modal(buf, area_w, area_h, km.expose().chars().count());
    }
    if let Some(input) = &state.config_input {
        render_config_input_overlay(buf, area_w, area_h, input);
    }
    if let Some(name) = &state.workspace_name_input {
        render_workspace_name_modal(buf, area_w, area_h, name);
    }
}

/// Paint every section stacked from virtual row 0, reporting the resulting
/// [`SectionLayout`]. Unbounded in height on purpose — the caller clips.
fn paint_sections(buf: &mut WireBuffer, area_w: u16, state: &SettingsState) -> SectionLayout {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const TITLE: Color = Color::rgb(255, 215, 0);
    const TEXT: Color = Color::rgb(220, 220, 230);
    // Active-workspace indicator colour (TUI palette SELECTION_GREEN).
    const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

    let top = 0;
    let bottom = u16::MAX;
    let mut active_start = 0;
    let mut active_end = 0;
    let mut cursor_row = None;
    let mut row = top;
    let put = |buf: &mut WireBuffer, x: u16, row: u16, s: &str, color: Color| {
        for (ch, cx) in s.chars().zip(x..area_w) {
            let mut cell = Cell::new(ch.to_string());
            cell.fg = Some(color);
            buf.push(Coord::new(cx, row), cell);
        }
    };

    for section in [
        SettingsSection::Daemon,
        SettingsSection::Providers,
        SettingsSection::Keys,
        SettingsSection::Workspaces,
        SettingsSection::Members,
        SettingsSection::Notifications,
    ] {
        if row >= bottom {
            break;
        }
        let is_active = section == state.section;
        if is_active {
            active_start = row;
        }
        let marker = if is_active { "▶ " } else { "  " };
        put(buf, 0, row, &format!("{marker}{}", section.title()), TITLE);
        row += 1;
        if row >= bottom {
            break;
        }
        match section {
            SettingsSection::Daemon => {
                let (next, cursor) = render_daemon_body(buf, area_w, row, bottom, state);
                row = next;
                cursor_row = cursor.or(cursor_row);
            }
            SettingsSection::Providers => {
                for p in &state.providers {
                    if row >= bottom {
                        break;
                    }
                    let dot = if p.online { "●" } else { "○" };
                    put(buf, 4, row, &format!("{dot} {}", p.name), TEXT);
                    row += 1;
                }
            }
            SettingsSection::Keys => {
                for k in &state.keys {
                    if row >= bottom {
                        break;
                    }
                    put(buf, 4, row, &format!("{}: {}", k.provider, k.masked), TEXT);
                    row += 1;
                }
            }
            SettingsSection::Workspaces => {
                for (i, w) in state.workspaces.iter().enumerate() {
                    if row >= bottom {
                        break;
                    }
                    // `▶ <slug>` in SELECTION_GREEN marks the active workspace;
                    // a leading two-space gutter keeps inactive rows aligned.
                    let active_mark = if w.current { "▶ " } else { "  " };
                    let default_mark = if w.default { " [default]" } else { "" };
                    let selected =
                        state.section == SettingsSection::Workspaces && i == state.list_selected;
                    let cursor = if selected { "›" } else { " " };
                    let line =
                        format!("{cursor}{active_mark}{} · {}{default_mark}", w.slug, w.name);
                    // The active row paints in SELECTION_GREEN; others in TEXT.
                    let color = if w.current { SELECTION_GREEN } else { TEXT };
                    put(buf, 4, row, &line, color);
                    row += 1;
                }
            }
            SettingsSection::Members => {
                row = render_member_rows(buf, area_w, row, bottom, &state.members);
            }
            SettingsSection::Notifications => {
                let selected = state.section == SettingsSection::Notifications;
                row = render_notify_grid(buf, area_w, row, bottom, state, selected);
            }
        }
        if is_active {
            active_end = row;
        }
    }

    SectionLayout {
        total: row,
        active_start,
        active_end,
        cursor_row,
    }
}

/// Render the numeric-input overlay for editing an int daemon-config knob: a
/// centred box showing the knob's label, its range, the current value as a ghost
/// hint, and the digits typed so far with a caret. A rejected Enter paints the
/// descriptor's error in place of the hint line. `enter` commits, `esc` cancels —
/// both named in the box. Never masks the value (unlike the key-entry modal, an
/// int is not a secret).
///
/// Clips on BOTH axes: a pane shorter than the box must drop the overflowing rows
/// rather than paint `Coord`s below the area ([`WireBuffer::push`] does not clip).
fn render_config_input_overlay(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    input: &ConfigInput,
) {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const CORNFLOWER: Color = Color::rgb(100, 149, 237);
    const GOLD: Color = Color::rgb(255, 215, 0);
    const TEXT: Color = Color::rgb(220, 220, 230);
    const MUTED: Color = Color::rgb(120, 120, 140);
    const RED: Color = Color::rgb(220, 100, 100);

    let desc = &DAEMON_CONFIG_REGISTRY[input.reg_index];
    let range = desc.type_hint();
    let title = format!(" {} ", desc.label);
    let value_line = format!("{}_", input.buffer);
    // The status line is the rejection when there is one, else the ghost hint
    // naming the value being replaced.
    let (status, status_color) = input.error.as_ref().map_or_else(
        || (format!("current: {}", input.current), MUTED),
        |msg| (msg.clone(), RED),
    );
    let hint = "enter commit · esc cancel";

    // A box sized to the widest line, centred in the area. Width is measured in
    // CHARS, not bytes — a non-ASCII label (`.len()`) would over-size the box and
    // push the border off the pane.
    let inner_w = title
        .chars()
        .count()
        .max(range.chars().count())
        .max(value_line.chars().count())
        .max(status.chars().count())
        .max(hint.chars().count());
    let box_w = u16::try_from(inner_w + 4).unwrap_or(24).min(area_w);
    // One row per line (title/range/value/status/hint) plus the two borders.
    let box_h: u16 = 7.min(area_h);
    let x0 = area_w.saturating_sub(box_w) / 2;
    let y0 = area_h.saturating_sub(box_h) / 2;

    let put = |buf: &mut WireBuffer, x: u16, y: u16, s: &str, color: Color| {
        if y >= area_h {
            return;
        }
        for (i, ch) in s.chars().enumerate() {
            let cx = x + u16::try_from(i).unwrap_or(0);
            if cx >= area_w {
                break;
            }
            let mut cell = Cell::new(ch.to_string());
            cell.fg = Some(color);
            buf.push(Coord::new(cx, y), cell);
        }
    };

    // Rounded border.
    let top = format!("╭{}╮", "─".repeat(box_w.saturating_sub(2) as usize));
    let bot = format!("╰{}╯", "─".repeat(box_w.saturating_sub(2) as usize));
    put(buf, x0, y0, &top, CORNFLOWER);
    for row in 1..box_h.saturating_sub(1) {
        put(buf, x0, y0 + row, "│", CORNFLOWER);
        put(buf, x0 + box_w.saturating_sub(1), y0 + row, "│", CORNFLOWER);
    }
    put(buf, x0, y0 + box_h.saturating_sub(1), &bot, CORNFLOWER);

    let tx = x0 + 2;
    put(buf, tx, y0 + 1, &title, GOLD);
    put(buf, tx, y0 + 2, &range, MUTED);
    put(buf, tx, y0 + 3, &value_line, TEXT);
    put(buf, tx, y0 + 4, &status, status_color);
    put(buf, tx, y0 + 5, hint, MUTED);
}
