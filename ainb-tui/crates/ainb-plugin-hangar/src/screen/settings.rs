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

use ainb_hangar_proto::settings::{HealthSnapshot, KeyRow, ProviderRow, WorkspaceRow};
use ainb_hangar_proto::snapshots::{MemberWireRow, NotifyRuleWireRow};
use ainb_hangar_proto::{Channel, ChannelSet};
use ainb_plugin_sdk::WireBuffer;

use crate::widgets::key_entry::render_key_entry_modal;

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
    /// Per-attention-kind notification routing rules (tcp T5): a workspace-scoped
    /// grid of kind × channel toggles. `J`/`K` move the kind row, `h`/`l` move the
    /// channel column, `space` toggles the selected cell (emitting a
    /// `SetNotifyRule` intent → `hangar/notify_rule_set`).
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
    section: SettingsSection,
    /// Selection within the active section's list (workspaces / keys).
    list_selected: usize,
    connection: ConnectionStatus,
    /// The key-entry modal's in-flight value, present while the modal is open.
    key_entry: Option<KeyMaterial>,
}

impl SettingsState {
    /// A fresh settings state from the four daemon snapshots, landing on the
    /// daemon section with connection status derived from `health.connected`.
    #[must_use]
    pub const fn new(
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
            section: SettingsSection::Daemon,
            list_selected: 0,
            connection,
            key_entry: None,
        }
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
}

/// An input the settings reducer folds into [`SettingsState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsEvent {
    /// A printable key. While the key-entry modal is open, printable chars
    /// extend the in-flight key value.
    Key(char),
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
    /// Create a new workspace (`n` on the Workspace pane). The plugin glue
    /// opens the host's new-workspace flow.
    NewWorkspace,
    /// Rename the selected workspace (`r`). Carries the workspace id.
    RenameWorkspace(String),
    /// Toggle one notification routing cell (`space` on the Notifications grid,
    /// tcp T5). Carries the attention kind wire token and the FULL new channel set
    /// for that kind; the glue maps it to `hangar/notify_rule_set`.
    SetNotifyRule {
        /// The attention kind wire token the rule governs.
        kind: String,
        /// The new push-channel set after the toggle.
        channels: ChannelSet,
    },
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
    }
}

/// Handle a printable key, branching on the active modal.
fn reduce_key(state: &SettingsState, c: char) -> SettingsReduction {
    if state.key_entry.is_some() {
        return reduce_key_entry_key(state, c);
    }
    // The Notifications grid owns a 2D cursor (kind × channel) + a toggle, so it
    // handles its own keys; `j`/`k` still move between sections.
    if state.section == SettingsSection::Notifications {
        return reduce_notify_key(state, c);
    }
    match c {
        'j' => move_section(state, SettingsSection::next),
        'k' => move_section(state, SettingsSection::prev),
        // J/K move the in-section list selection (workspaces / keys).
        'J' => move_list(state, 1),
        'K' => move_list(state, -1),
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
        'n' if state.section == SettingsSection::Workspaces => SettingsReduction {
            state: state.clone(),
            intent: Some(SettingsIntent::NewWorkspace),
        },
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

/// Handle a key on the Notifications grid (tcp T5): `j`/`k` leave to the adjacent
/// section, `J`/`K` move the kind row, `h`/`l` move the channel column, and
/// `space`/`t` toggle the selected cell (emitting a `SetNotifyRule` intent).
fn reduce_notify_key(state: &SettingsState, c: char) -> SettingsReduction {
    match c {
        'j' => move_section(state, SettingsSection::next),
        'k' => move_section(state, SettingsSection::prev),
        'J' => move_notify_kind(state, 1),
        'K' => move_notify_kind(state, -1),
        'h' => move_notify_channel(state, -1),
        'l' => move_notify_channel(state, 1),
        ' ' | 't' => toggle_notify_cell(state),
        _ => unchanged(state),
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

/// Esc: abort the key-entry modal if open; otherwise a no-op.
fn reduce_esc(state: &SettingsState) -> SettingsReduction {
    let mut next = state.clone();
    next.key_entry = None;
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
    let key = next
        .key_entry
        .take()
        .unwrap_or_else(|| KeyMaterial::new(String::new()));
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
            "J/K kind · h/l channel · space toggle",
            DIM,
        );
        row += 1;
    }
    row
}

/// Render the settings screen into `buf` between rows `top` and `bottom`.
///
/// The five sections paint stacked top-down; the active one is accent-highlighted.
/// When the key-entry modal is open it overlays a centred password-style input
/// (masked, never the raw value). Width-aware: each row's value column is clipped
/// at `area_w`.
pub fn render_settings(
    buf: &mut WireBuffer,
    area_w: u16,
    area_h: u16,
    top: u16,
    bottom: u16,
    state: &SettingsState,
) {
    use ainb_plugin_sdk::{Cell, Color, Coord};
    const TITLE: Color = Color::rgb(255, 215, 0);
    const GREEN: Color = Color::rgb(100, 200, 100);
    const RED: Color = Color::rgb(220, 100, 100);
    const TEXT: Color = Color::rgb(220, 220, 230);
    // Active-workspace indicator colour (TUI palette SELECTION_GREEN).
    const SELECTION_GREEN: Color = Color::rgb(100, 200, 100);

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
        let marker = if section == state.section {
            "▶ "
        } else {
            "  "
        };
        put(buf, 0, row, &format!("{marker}{}", section.title()), TITLE);
        row += 1;
        if row >= bottom {
            break;
        }
        match section {
            SettingsSection::Daemon => {
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
    }

    // Modal overlays.
    if let Some(km) = &state.key_entry {
        render_key_entry_modal(buf, area_w, area_h, km.expose().chars().count());
    }
}
