//! Request and response param structs for every JSON-RPC method.
//!
//! Naming convention:
//!
//! - `<Method>Params`  — payload sent on the request side.
//! - `<Method>Result`  — payload returned on success.
//!
//! Wire-level field naming is ``snake_case`` to match JSON-RPC method
//! names (`plugin/handle_event`) and TOML manifest keys. We do **not**
//! follow JavaScript camelCase — JSON-RPC's spec is silent and the
//! rest of our wire surface (manifest, method names, error codes) is
//! `snake_case`.
//!
//! Binary payloads use [`bytes::Bytes`] rather than `Vec<u8>` so the
//! runtime + SDK can share zero-copy buffers across handler hops. We
//! deliberately don't use `serde_bytes::ByteBuf` (would force opaque
//! base64 in JSON and lose interop with `serde_json::Value` in the
//! testkit).

use serde::{Deserialize, Serialize};

use crate::wire_buffer::WireBuffer;

// =====================================================================
// plugin/init
// =====================================================================

/// `plugin/init` params: host hands the plugin its manifest path and
/// the capability set actually granted (post install-time validation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInitParams {
    /// Absolute path to the resolved manifest file.
    pub manifest_path: String,
    /// Capabilities the host has granted, by canonical capability key.
    /// Subset of `[capabilities]` in the manifest.
    pub granted_capabilities: Vec<String>,
    /// Wire ABI version the host speaks. Plugin MUST refuse to init
    /// if it can't speak this revision.
    pub abi_version: u32,
    /// Resolved per-plugin config (the `[plugins.<name>]` table) the host
    /// injects at init. Defaults to JSON `null` so ABI-2 peers that omit it
    /// keep decoding. The plugin parses this into its typed config struct.
    #[serde(default)]
    pub config: serde_json::Value,
}

/// `plugin/init` result: plugin echoes its name + version so the host
/// can sanity-check spawn vs manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginInitResult {
    /// Plugin's self-reported name. Must match `[plugin].name` in the manifest.
    pub name: String,
    /// Plugin's self-reported version. Must match `[plugin].version`.
    pub version: String,
}

// =====================================================================
// plugin/shutdown
// =====================================================================

/// `plugin/shutdown` params: graceful shutdown signal. Empty payload
/// today; reserved for future flags (e.g., `restart_pending`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginShutdownParams {}

/// `plugin/shutdown` result: empty acknowledgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginShutdownResult {}

// =====================================================================
// plugin/render
// =====================================================================

/// Viewport dimensions a render targets, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewport {
    /// Viewport width in cells.
    pub width: u16,
    /// Viewport height in cells.
    pub height: u16,
}

impl Viewport {
    /// Construct a viewport with explicit dimensions.
    #[must_use]
    pub const fn new(width: u16, height: u16) -> Self {
        Self { width, height }
    }
}

/// `plugin/render` params: viewport + an opaque dirty flag for the
/// plugin to use as a hint (host always paints what's returned).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderParams {
    /// Viewport the host wants painted.
    pub viewport: Viewport,
    /// Plugin-defined render generation. Plugins free to use this as
    /// a redraw token; host doesn't interpret the value.
    #[serde(default)]
    pub generation: u64,
}

/// `plugin/render` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderResult {
    /// Cell grid to paint.
    pub buffer: WireBuffer,
    /// Self-animation hint: when `true`, the plugin is mid-transition and
    /// wants to be rendered again on the next host tick without any further
    /// input. The runtime re-marks the plugin's render-dirty flag so the
    /// host's render loop calls `plugin/render` again, frame-by-frame, until
    /// the plugin returns `false`. A requestAnimationFrame analogue.
    ///
    /// `#[serde(default)]` (defaults to `false`) so ABI-2 peers that omit it
    /// keep decoding — a plugin that never animates is byte-compatible with
    /// the pre-`redraw` wire shape.
    #[serde(default)]
    pub redraw: bool,
}

// =====================================================================
// plugin/handle_event
// =====================================================================

/// `plugin/handle_event` params (notification). Carries a snapshot
/// or action delivery. Body is left as raw bytes — plugin owns
/// decoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleEventParams {
    /// Snapshot/event topic name (e.g., `sessions.usage_data`).
    pub topic: String,
    /// Opaque payload. Codec is by convention per topic.
    #[serde(with = "bytes_serde")]
    pub payload: bytes::Bytes,
}

// =====================================================================
// plugin/handle_key
// =====================================================================

/// Normalized key event delivered from host → plugin.
///
/// Host translates the terminal-specific event (e.g. `crossterm::event::KeyEvent`)
/// into this portable representation exactly once, so non-Rust plugins can
/// participate without depending on crossterm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyEvent {
    /// Logical key pressed.
    pub code: KeyCode,
    /// Bitmask of active modifiers — see [`KEY_MOD_SHIFT`], [`KEY_MOD_CTRL`],
    /// [`KEY_MOD_ALT`], [`KEY_MOD_SUPER`].
    #[serde(default)]
    pub mods: u8,
    /// Whether this is the initial press, an auto-repeat, or a release.
    #[serde(default)]
    pub kind: KeyKind,
}

/// Logical key identity. Wire tag is `type`, variants `snake_case` —
/// `Char { ch }` is `{"type":"char","ch":"1"}`, `BackTab` is `{"type":"back_tab"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum KeyCode {
    /// Printable character key.
    Char {
        /// The character pressed.
        ch: char,
    },
    /// Enter / Return.
    Enter,
    /// Tab.
    Tab,
    /// Shift-Tab (reverse tab).
    BackTab,
    /// Escape.
    Esc,
    /// Backspace.
    Backspace,
    /// Delete (forward delete).
    Delete,
    /// Up arrow.
    Up,
    /// Down arrow.
    Down,
    /// Left arrow.
    Left,
    /// Right arrow.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page Up.
    PageUp,
    /// Page Down.
    PageDown,
    /// Function key F1..F24.
    F {
        /// Function number (1-based).
        n: u8,
    },
}

/// Press / repeat / release. `Press` is the default — older peers that
/// omit the field decode as `Press`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// Initial key-down.
    #[default]
    Press,
    /// Auto-repeated key-down while held.
    Repeat,
    /// Key-up.
    Release,
}

/// Shift modifier bit for [`KeyEvent::mods`].
pub const KEY_MOD_SHIFT: u8 = 0b0001;
/// Control modifier bit for [`KeyEvent::mods`].
pub const KEY_MOD_CTRL: u8 = 0b0010;
/// Alt / Option modifier bit for [`KeyEvent::mods`].
pub const KEY_MOD_ALT: u8 = 0b0100;
/// Super / Command / Windows modifier bit for [`KeyEvent::mods`].
pub const KEY_MOD_SUPER: u8 = 0b1000;

/// `plugin/handle_key` params (notification). Host forwards keys the
/// host hasn't reserved (e.g. global navigation) to the plugin owning
/// the currently focused screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleKeyParams {
    /// Plugin-defined screen identifier (e.g. `ainb_analytics`). Lets a
    /// plugin host multiple screens and dispatch by id without extra
    /// per-key bookkeeping on the host.
    pub screen_id: String,
    /// The key event itself.
    pub key: KeyEvent,
    /// Monotonic counter the host increments per forwarded key. The plugin
    /// is expected to echo the value back via [`RenderParams::generation`]
    /// on the next render, giving the host a freshness witness that the
    /// key has been observed.
    pub generation: u64,
}

// =====================================================================
// plugin/handle_mouse
// =====================================================================

/// Mouse button identity for the press/release/drag variants of
/// [`MouseKind`]. Wire tag is `type`, variants `snake_case` —
/// `Left` is `{"type":"left"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MouseButton {
    /// Left / primary button.
    Left,
    /// Right / secondary button.
    Right,
    /// Middle / wheel button.
    Middle,
}

/// Logical mouse action. Wire tag is `type`, variants `snake_case` —
/// `Down { button }` is `{"type":"down","button":{"type":"left"}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MouseKind {
    /// Button pressed.
    Down {
        /// Which button went down.
        button: MouseButton,
    },
    /// Button released.
    Up {
        /// Which button came up.
        button: MouseButton,
    },
    /// Pointer moved with a button held.
    Drag {
        /// Which button is held during the drag.
        button: MouseButton,
    },
    /// Pointer moved with no button held.
    Moved,
    /// Wheel scrolled up.
    ScrollUp,
    /// Wheel scrolled down.
    ScrollDown,
    /// Wheel scrolled left.
    ScrollLeft,
    /// Wheel scrolled right.
    ScrollRight,
}

/// Normalized mouse event delivered from host → plugin.
///
/// Coordinates are **plugin-viewport-relative**: the host subtracts the
/// plugin screen's origin before forwarding, so `(0, 0)` is the top-left
/// cell of the plugin's own buffer. This lets a plugin hit-test against
/// the same coordinate space it painted into, without knowing where on
/// the terminal its screen was placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MouseEvent {
    /// What the pointer did.
    pub kind: MouseKind,
    /// Column within the plugin viewport (0-based).
    pub col: u16,
    /// Row within the plugin viewport (0-based).
    pub row: u16,
    /// Bitmask of active modifiers — same bits as [`KeyEvent::mods`]
    /// ([`KEY_MOD_SHIFT`], [`KEY_MOD_CTRL`], [`KEY_MOD_ALT`], [`KEY_MOD_SUPER`]).
    #[serde(default)]
    pub mods: u8,
}

/// `plugin/handle_mouse` params (notification). Host forwards mouse
/// events to the plugin owning the currently focused screen, mirroring
/// [`HandleKeyParams`]. The [`MouseEvent`] carries coordinates already
/// translated into the plugin's viewport space.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleMouseParams {
    /// Plugin-defined screen identifier (mirrors [`HandleKeyParams::screen_id`]).
    pub screen_id: String,
    /// The mouse event, in plugin-viewport coordinates.
    pub mouse: MouseEvent,
    /// Monotonic counter the host increments per forwarded mouse event,
    /// mirroring [`HandleKeyParams::generation`].
    pub generation: u64,
}

// =====================================================================
// plugin/cli_dispatch
// =====================================================================

/// `plugin/cli_dispatch` params: CLI namespace + argv.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliDispatchParams {
    /// CLI subcommand namespace (e.g., `usage`).
    pub namespace: String,
    /// Full argv as the user typed it, namespace already stripped.
    pub argv: Vec<String>,
}

/// `plugin/cli_dispatch` result: captured stdout/stderr + exit code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliDispatchResult {
    /// Captured stdout bytes. UTF-8 by convention but not enforced.
    #[serde(with = "bytes_serde")]
    pub stdout: bytes::Bytes,
    /// Captured stderr bytes.
    #[serde(with = "bytes_serde")]
    pub stderr: bytes::Bytes,
    /// Process-style exit code (0 = success).
    pub exit_code: i32,
}

// =====================================================================
// host/snapshot/get
// =====================================================================

/// `host/snapshot/get` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotGetParams {
    /// Topic to fetch.
    pub topic: String,
}

/// `host/snapshot/get` result. `payload` is `None` if the topic has
/// no current snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotGetResult {
    /// Latest snapshot bytes, or `None` if topic is empty.
    #[serde(default, with = "opt_bytes_serde")]
    pub payload: Option<bytes::Bytes>,
    /// Monotonic version. Increments per publish; `0` if `payload` is `None`.
    #[serde(default)]
    pub version: u64,
}

// =====================================================================
// host/snapshot/publish
// =====================================================================

/// `host/snapshot/publish` params (notification).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotPublishParams {
    /// Topic to publish under.
    pub topic: String,
    /// Snapshot payload bytes.
    #[serde(with = "bytes_serde")]
    pub payload: bytes::Bytes,
}

// =====================================================================
// host/snapshot/subscribe
// =====================================================================

/// `host/snapshot/subscribe` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSubscribeParams {
    /// Topic to subscribe to.
    pub topic: String,
}

/// `host/snapshot/subscribe` result: empty acknowledgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotSubscribeResult {}

// =====================================================================
// host/action/invoke
// =====================================================================

/// `host/action/invoke` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionInvokeParams {
    /// Fully-qualified action name (e.g., `sessions.rescan`).
    pub action: String,
    /// Opaque request payload.
    #[serde(with = "bytes_serde")]
    pub payload: bytes::Bytes,
    /// Caller-supplied timeout in milliseconds. `0` = no timeout.
    #[serde(default)]
    pub timeout_ms: u64,
}

/// `host/action/invoke` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionInvokeResult {
    /// Opaque response payload.
    #[serde(with = "bytes_serde")]
    pub payload: bytes::Bytes,
}

// =====================================================================
// host/log
// =====================================================================

/// Log severity levels. Wire form is lowercase string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Verbose tracing — disabled by default.
    Trace,
    /// Diagnostic detail useful while debugging.
    Debug,
    /// Routine progress information.
    Info,
    /// Recoverable degradation.
    Warn,
    /// Operation failed.
    Error,
}

/// `host/log` params (notification).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogParams {
    /// Log severity.
    pub level: LogLevel,
    /// Log message body. UTF-8.
    pub message: String,
    /// Optional structured fields, carried as JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<serde_json::Value>,
}

// =====================================================================
// host/fs/read_dir
// =====================================================================

/// `host/fs/read_dir` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadDirParams {
    /// Absolute filesystem path to enumerate. Capability-gated.
    pub path: String,
}

/// One entry in a `host/fs/read_dir` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsDirEntry {
    /// Entry name relative to the queried directory (no path separators).
    pub name: String,
    /// `true` if the entry is itself a directory.
    pub is_dir: bool,
    /// Size in bytes for files; `0` for directories.
    #[serde(default)]
    pub size: u64,
}

/// `host/fs/read_dir` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadDirResult {
    /// Entries, in `read_dir` order (filesystem-defined).
    pub entries: Vec<FsDirEntry>,
}

// =====================================================================
// host/fs/read_file
// =====================================================================

/// `host/fs/read_file` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadFileParams {
    /// Absolute filesystem path to read. Capability-gated.
    pub path: String,
}

/// `host/fs/read_file` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsReadFileResult {
    /// File contents.
    #[serde(with = "bytes_serde")]
    pub bytes: bytes::Bytes,
}

// =====================================================================
// host/network/fetch
// =====================================================================

/// `host/network/fetch` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkFetchParams {
    /// Absolute URL to fetch. Hostname must be on the
    /// capability allow-list.
    pub url: String,
    /// HTTP method (`GET`, `POST`, ...). Defaults to `GET` if empty.
    #[serde(default)]
    pub method: String,
    /// Optional request body.
    #[serde(default, with = "opt_bytes_serde")]
    pub body: Option<bytes::Bytes>,
    /// Request headers as `(name, value)` pairs.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
}

/// `host/network/fetch` result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkFetchResult {
    /// HTTP status code.
    pub status: u16,
    /// Response headers.
    pub headers: Vec<(String, String)>,
    /// Response body.
    #[serde(with = "bytes_serde")]
    pub body: bytes::Bytes,
}

// =====================================================================
// host/event_stream_subscribe
// =====================================================================

/// `host/event_stream_subscribe` params: open a cancellable streaming
/// subscription on `topic`.
///
/// The host validates `topic` against the plugin's
/// `event_stream_subscribe` capability allow-list (list form = topic-prefix
/// whitelist; bool-true = wildcard) before allocating a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStreamSubscribeParams {
    /// Topic to subscribe to (e.g. `workspace:default`).
    pub topic: String,
    /// Optional resume point. When present, the host replays from this
    /// version; when omitted, the stream starts from the current version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_version: Option<u64>,
}

/// `host/event_stream_subscribe` result: the host-allocated stream handle.
///
/// `stream_id` is a host-minted opaque token (ULID) the plugin cannot forge;
/// subsequent events arrive via `plugin/handle_event` under topic
/// `stream:<stream_id>` and the plugin cancels using this same id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStreamSubscribeResult {
    /// Host-allocated stream identifier.
    pub stream_id: String,
    /// Topic version the stream is positioned at (first event the plugin
    /// will observe is `> version` unless `since_version` requested a replay).
    #[serde(default)]
    pub version: u64,
}

// =====================================================================
// host/event_stream_cancel
// =====================================================================

/// `host/event_stream_cancel` params (notification): tear down a stream.
///
/// Cancels a stream the plugin previously opened; the host stops emitting
/// further `stream:<stream_id>` events. Cancellation is also implicit on
/// plugin restart / shutdown / quarantine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventStreamCancelParams {
    /// Stream to cancel — the `stream_id` returned by `event_stream_subscribe`.
    pub stream_id: String,
}

// =====================================================================
// host/spawn_managed_subprocess
// =====================================================================

/// `host/spawn_managed_subprocess` params: ask the host to spawn a
/// host-supervised child process.
///
/// `bin` must appear on the plugin's `spawn_managed_subprocess`
/// capability allow-list (list form is mandatory; a bool-true grant is
/// rejected at manifest validation). `env_allowlist` names the parent
/// environment variables the child is permitted to inherit — every other
/// variable is stripped before exec, so a plugin can't exfiltrate the
/// host's secrets into an arbitrary child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnManagedSubprocessParams {
    /// Binary name or absolute path to spawn. Must be on the cap allow-list.
    pub bin: String,
    /// Arguments passed to the child, `argv[1..]` style.
    #[serde(default)]
    pub argv: Vec<String>,
    /// Names of parent env vars the child may inherit. Everything not
    /// listed is removed from the child's environment.
    #[serde(default)]
    pub env_allowlist: Vec<String>,
    /// Optional working directory for the child. Inherits the host's
    /// cwd when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// `host/spawn_managed_subprocess` result: the host-allocated handle +
/// the child's OS process id.
///
/// `handle` is an opaque, host-minted token the plugin uses to compose
/// with `host/event_stream_subscribe` on topic `managed:<handle>:stdout`.
/// `pid` is informational (e.g. for the plugin to surface in its UI).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpawnManagedSubprocessResult {
    /// Host-allocated, opaque managed-subprocess handle.
    pub handle: String,
    /// OS process id of the spawned child.
    pub pid: u32,
}

// =====================================================================
// host/unix_socket_dial
// =====================================================================

/// `host/unix_socket_dial` params: ask the host to dial an `AF_UNIX` socket.
///
/// `path` must resolve (after symlink canonicalization) to an entry on
/// the plugin's `unix_socket_dial` capability allow-list (list form is
/// mandatory; a bool-true grant is rejected at manifest validation). The
/// host expands env vars (`${XDG_RUNTIME_DIR}`) and `~` itself — the
/// plugin always sends the literal path string from its manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnixSocketDialParams {
    /// Filesystem path of the socket to dial. Must be on the cap allow-list.
    pub path: String,
}

/// `host/unix_socket_dial` result: the host-allocated stream handle.
///
/// `stream_id` is a host-minted opaque token (ULID) the plugin cannot
/// forge; subsequent socket frames arrive via `plugin/handle_event` under
/// topic `socket:<stream_id>` and the plugin writes / closes using this
/// same id.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnixSocketDialResult {
    /// Host-allocated socket stream identifier.
    pub stream_id: String,
}

// =====================================================================
// host/unix_socket_send
// =====================================================================

/// `host/unix_socket_send` params (notification): write bytes to a dialled
/// socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnixSocketSendParams {
    /// Stream to write to — the `stream_id` returned by `unix_socket_dial`.
    pub stream_id: String,
    /// Bytes to write to the socket.
    #[serde(with = "bytes_serde")]
    pub bytes: bytes::Bytes,
}

// =====================================================================
// host/unix_socket_close
// =====================================================================

/// `host/unix_socket_close` params (notification): tear down a dialled
/// socket.
///
/// The host shuts the connection and stops emitting `socket:<stream_id>`
/// events. Closure is also implicit on plugin shutdown / crash /
/// quarantine.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnixSocketCloseParams {
    /// Stream to close — the `stream_id` returned by `unix_socket_dial`.
    pub stream_id: String,
}

/// Kind tag for a `socket:<stream_id>` delivery frame's payload.
///
/// The host wraps each inbound socket event in a small JSON envelope so
/// the plugin can distinguish ordinary data from end-of-stream and from a
/// transport error without an out-of-band channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnixSocketEventKind {
    /// Bytes read from the socket; `bytes` is populated.
    Data,
    /// The peer closed the socket; no further events follow.
    Eof,
    /// A transport error occurred; `error` describes it, no further events.
    Error,
}

/// Payload of a `socket:<stream_id>` `plugin/handle_event` notification.
///
/// Carried as the event `payload` bytes (JSON-encoded). `bytes` is present
/// only for [`UnixSocketEventKind::Data`]; `error` only for
/// [`UnixSocketEventKind::Error`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnixSocketEvent {
    /// What this frame represents.
    pub kind: UnixSocketEventKind,
    /// Bytes read from the socket (present iff `kind == Data`).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "opt_bytes_serde"
    )]
    pub bytes: Option<bytes::Bytes>,
    /// Human-readable transport error (present iff `kind == Error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// =====================================================================
// host/secret_store_get
// =====================================================================

/// `host/secret_store_get` params: read a secret from the platform secret
/// store, addressed by `(scope, key)`.
///
/// `scope` selects the addressing namespace and is one of `"workspace"` or
/// `"global"`:
///
/// - `"workspace"` requires `workspace_id` and isolates the secret to a
///   single workspace. The host folds `workspace_id` into the backend's
///   service string so per-workspace secrets never collide with each other
///   or with `"global"` secrets that share a `key`.
/// - `"global"` is a host-wide secret shared across all workspaces;
///   `workspace_id` is ignored.
///
/// The capability `secrets:read` must be granted (list form = an allow-list
/// of permitted `key`s; bool-true = any key). On macOS the host reads the
/// login Keychain; on linux the backend is deferred and the host returns
/// `-32005`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretStoreGetParams {
    /// Addressing scope: `"workspace"` or `"global"`.
    pub scope: String,
    /// Workspace id — required when `scope == "workspace"`, ignored for
    /// `"global"`. Omitted from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The secret key within the scope (e.g. `anthropic_api_key`). Cap-gated.
    pub key: String,
}

/// `host/secret_store_get` result: the secret, base64-encoded.
///
/// The secret bytes are carried base64-encoded (not as a raw JSON byte
/// array) so an arbitrary binary secret survives the JSON envelope and the
/// payload stays compact. The plugin decodes `value` itself.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretStoreGetResult {
    /// Base64 (standard alphabet) encoding of the secret's raw bytes.
    pub value: String,
}

// =====================================================================
// host/workspace_list
// =====================================================================

/// One workspace row in a `host/workspace_list` result.
///
/// The `id` is the workspace's stable ULID (what `state.toml` is keyed by and
/// what `set_active`/`set_default` switch on); `slug` is the short display
/// handle (e.g. `default`); `name` is the human-readable label. `active` and
/// `default` are resolved against `~/.agents-in-a-box/hangar/state.toml` at list time —
/// `active` reflects the effective active workspace (explicit active, else
/// default, else first), so exactly one row is `active` in a non-empty list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    /// Stable ULID workspace id. State + switching key on this, never the slug.
    pub id: String,
    /// Short display handle (e.g. `default`).
    pub slug: String,
    /// Human-readable display name.
    pub name: String,
    /// Whether this workspace is the effective active one.
    pub active: bool,
    /// Whether this workspace is the configured default.
    pub default: bool,
}

/// `host/workspace_list` params: empty — the list is host-wide.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceListParams {}

/// `host/workspace_list` result: every known workspace with its
/// active/default flags resolved from `state.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceListResult {
    /// The workspace rows, in the host's list order.
    pub workspaces: Vec<WorkspaceEntry>,
}

// =====================================================================
// host/workspace_get_active
// =====================================================================

/// `host/workspace_get_active` params: empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGetActiveParams {}

/// `host/workspace_get_active` result: the effective active workspace id, or
/// `None` when no workspaces exist.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGetActiveResult {
    /// The effective active workspace ULID, or `None` for an empty host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

// =====================================================================
// host/workspace_set_active / host/workspace_set_default
// =====================================================================

/// `host/workspace_set_active` params: the workspace ULID to make active.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSetActiveParams {
    /// The workspace ULID to switch to. Must name a known workspace.
    pub workspace_id: String,
}

/// `host/workspace_set_active` result: empty acknowledgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSetActiveResult {}

/// `host/workspace_set_default` params: the workspace ULID to make default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSetDefaultParams {
    /// The workspace ULID to set as default. Must name a known workspace.
    pub workspace_id: String,
}

/// `host/workspace_set_default` result: empty acknowledgement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSetDefaultResult {}

// =====================================================================
// (de)serialise bytes::Bytes as a base64 string (with a legacy
// byte-array fallback on decode).
// =====================================================================

/// Binary payloads ride the JSON-RPC envelope as **base64 strings**.
///
/// `serde_json`'s default for `&[u8]` (and `Vec<u8>`) is a JSON array of
/// numbers — `[12, 34, 56, ...]` — which costs 3-4 ASCII chars per
/// byte. For the session-reader → burndown handoff that ballooned a
/// ~25 MB msgpack snapshot to ~80 MB of JSON, exceeding the host
/// framer's `MAX_BODY_BYTES = 16 MiB` and `OOM`-ing the plugin process
/// mid-write. Base64 costs ~1.33 chars per byte — fits the budget and
/// matches how JSON-RPC servers in the wild ship binary blobs.
///
/// The deserializer also accepts the legacy byte-array shape so older
/// peers (host paired with an older plugin, or vice versa) keep
/// working across the version bump.
mod bytes_serde {
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    pub(super) fn serialize<S: Serializer>(b: &Bytes, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&B64.encode(b.as_ref()))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Bytes, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            B64(String),
            Bytes(Vec<u8>),
        }
        match Repr::deserialize(d)? {
            Repr::B64(s) => B64.decode(s.as_bytes()).map(Bytes::from).map_err(D::Error::custom),
            Repr::Bytes(v) => Ok(Bytes::from(v)),
        }
    }
}

mod opt_bytes_serde {
    use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer, de::Error as _};

    // serde's serialize_with signature requires `&Option<T>` here —
    // can't reshape to the otherwise-idiomatic `Option<&T>`.
    #[allow(clippy::ref_option)]
    pub(super) fn serialize<S: Serializer>(b: &Option<Bytes>, s: S) -> Result<S::Ok, S::Error> {
        match b.as_ref() {
            Some(bytes) => s.serialize_str(&B64.encode(bytes.as_ref())),
            None => s.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Bytes>, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            B64(String),
            Bytes(Vec<u8>),
        }
        let opt = Option::<Repr>::deserialize(d)?;
        match opt {
            None => Ok(None),
            Some(Repr::B64(s)) => {
                B64.decode(s.as_bytes()).map(|v| Some(Bytes::from(v))).map_err(D::Error::custom)
            }
            Some(Repr::Bytes(v)) => Ok(Some(Bytes::from(v))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire_buffer::{Cell, Coord};

    fn rt<T>(v: &T) -> T
    where
        T: serde::Serialize + for<'de> serde::Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let s = serde_json::to_string(v).unwrap();
        let back: T = serde_json::from_str(&s).unwrap();
        assert_eq!(v, &back);
        back
    }

    #[test]
    #[allow(clippy::too_many_lines)] // exhaustive per-method round-trip list
    fn round_trip_each() {
        rt(&PluginInitParams {
            manifest_path: "/x/manifest.toml".into(),
            granted_capabilities: vec!["read_sessions".into()],
            abi_version: 2,
            config: serde_json::Value::Null,
        });
        rt(&PluginInitResult {
            name: "burndown".into(),
            version: "2.0.0".into(),
        });
        rt(&PluginShutdownParams::default());
        rt(&PluginShutdownResult::default());

        let mut buf = WireBuffer::new(4, 1);
        buf.push(Coord::new(0, 0), Cell::new("h"));
        rt(&RenderParams {
            viewport: Viewport::new(80, 24),
            generation: 7,
        });
        rt(&RenderResult {
            buffer: buf.clone(),
            redraw: false,
        });
        rt(&RenderResult {
            buffer: buf,
            redraw: true,
        });

        rt(&HandleEventParams {
            topic: "sessions.usage_data".into(),
            payload: bytes::Bytes::from_static(b"hello"),
        });

        rt(&HandleMouseParams {
            screen_id: "learnings".into(),
            mouse: MouseEvent {
                kind: MouseKind::Down {
                    button: MouseButton::Left,
                },
                col: 12,
                row: 3,
                mods: KEY_MOD_CTRL,
            },
            generation: 9,
        });

        rt(&CliDispatchParams {
            namespace: "usage".into(),
            argv: vec!["report".into(), "--json".into()],
        });
        rt(&CliDispatchResult {
            stdout: bytes::Bytes::from_static(b"{}\n"),
            stderr: bytes::Bytes::new(),
            exit_code: 0,
        });

        rt(&SnapshotGetParams { topic: "t".into() });
        rt(&SnapshotGetResult {
            payload: Some(bytes::Bytes::from_static(b"x")),
            version: 1,
        });
        rt(&SnapshotGetResult {
            payload: None,
            version: 0,
        });
        rt(&SnapshotPublishParams {
            topic: "t".into(),
            payload: bytes::Bytes::from_static(b"x"),
        });
        rt(&SnapshotSubscribeParams { topic: "t".into() });
        rt(&SnapshotSubscribeResult::default());

        rt(&ActionInvokeParams {
            action: "sessions.rescan".into(),
            payload: bytes::Bytes::from_static(b"{}"),
            timeout_ms: 5_000,
        });
        rt(&ActionInvokeResult {
            payload: bytes::Bytes::from_static(b"ok"),
        });

        rt(&LogParams {
            level: LogLevel::Info,
            message: "ready".into(),
            fields: Some(serde_json::json!({"k": "v"})),
        });

        rt(&FsReadDirParams {
            path: "/tmp/x".into(),
        });
        rt(&FsReadDirResult {
            entries: vec![FsDirEntry {
                name: "a".into(),
                is_dir: false,
                size: 42,
            }],
        });
        rt(&FsReadFileParams {
            path: "/tmp/x".into(),
        });
        rt(&FsReadFileResult {
            bytes: bytes::Bytes::from_static(b"abc"),
        });

        rt(&NetworkFetchParams {
            url: "https://api.example.com/x".into(),
            method: "GET".into(),
            body: None,
            headers: vec![("accept".into(), "application/json".into())],
        });
        rt(&NetworkFetchResult {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: bytes::Bytes::from_static(b"{}"),
        });

        rt(&EventStreamSubscribeParams {
            topic: "workspace:default".into(),
            since_version: Some(7),
        });
        rt(&EventStreamSubscribeParams {
            topic: "workspace:default".into(),
            since_version: None,
        });
        rt(&EventStreamSubscribeResult {
            stream_id: "01J9ZX8QK7".into(),
            version: 42,
        });
        rt(&EventStreamCancelParams {
            stream_id: "01J9ZX8QK7".into(),
        });

        rt(&SpawnManagedSubprocessParams {
            bin: "ainb-hangar-daemon".into(),
            argv: vec!["--socket".into(), "/tmp/x.sock".into()],
            env_allowlist: vec!["HOME".into(), "PATH".into()],
            cwd: Some("/tmp".into()),
        });
        rt(&SpawnManagedSubprocessParams {
            bin: "ainb-hangar-daemon".into(),
            argv: vec![],
            env_allowlist: vec![],
            cwd: None,
        });
        rt(&SpawnManagedSubprocessResult {
            handle: "01J9ZX8QK7-0000002a".into(),
            pid: 4242,
        });

        rt(&UnixSocketDialParams {
            path: "/run/user/1000/ainb-hangar.sock".into(),
        });
        rt(&UnixSocketDialResult {
            stream_id: "01J9ZX8QK7-0000002a".into(),
        });
        rt(&UnixSocketSendParams {
            stream_id: "01J9ZX8QK7-0000002a".into(),
            bytes: bytes::Bytes::from_static(b"ping\n"),
        });
        rt(&UnixSocketCloseParams {
            stream_id: "01J9ZX8QK7-0000002a".into(),
        });
        rt(&UnixSocketEvent {
            kind: UnixSocketEventKind::Data,
            bytes: Some(bytes::Bytes::from_static(b"pong\n")),
            error: None,
        });
        rt(&UnixSocketEvent {
            kind: UnixSocketEventKind::Eof,
            bytes: None,
            error: None,
        });
        rt(&UnixSocketEvent {
            kind: UnixSocketEventKind::Error,
            bytes: None,
            error: Some("connection reset".into()),
        });

        rt(&SecretStoreGetParams {
            scope: "workspace".into(),
            workspace_id: Some("ws-123".into()),
            key: "anthropic_api_key".into(),
        });
        rt(&SecretStoreGetParams {
            scope: "global".into(),
            workspace_id: None,
            key: "openai_api_key".into(),
        });
        rt(&SecretStoreGetResult {
            value: "c2VjcmV0".into(),
        });

        rt(&WorkspaceListParams::default());
        rt(&WorkspaceListResult {
            workspaces: vec![
                WorkspaceEntry {
                    id: "01J9ZX8QK7".into(),
                    slug: "default".into(),
                    name: "Default".into(),
                    active: true,
                    default: true,
                },
                WorkspaceEntry {
                    id: "01J9ZX8QK8".into(),
                    slug: "acme".into(),
                    name: "Acme".into(),
                    active: false,
                    default: false,
                },
            ],
        });
        rt(&WorkspaceGetActiveParams::default());
        rt(&WorkspaceGetActiveResult {
            workspace_id: Some("01J9ZX8QK7".into()),
        });
        rt(&WorkspaceGetActiveResult { workspace_id: None });
        rt(&WorkspaceSetActiveParams {
            workspace_id: "01J9ZX8QK8".into(),
        });
        rt(&WorkspaceSetActiveResult::default());
        rt(&WorkspaceSetDefaultParams {
            workspace_id: "01J9ZX8QK8".into(),
        });
        rt(&WorkspaceSetDefaultResult::default());
    }

    #[test]
    fn workspace_get_active_omits_none_id() {
        // A host with no workspaces returns `{}` (no `workspace_id` key).
        let r = WorkspaceGetActiveResult { workspace_id: None };
        assert_eq!(serde_json::to_string(&r).unwrap(), "{}");
    }

    #[test]
    fn workspace_entry_wire_shape() {
        let e = WorkspaceEntry {
            id: "01J9ZX8QK7".into(),
            slug: "default".into(),
            name: "Default".into(),
            active: true,
            default: false,
        };
        assert_eq!(
            serde_json::to_string(&e).unwrap(),
            r#"{"id":"01J9ZX8QK7","slug":"default","name":"Default","active":true,"default":false}"#
        );
    }

    #[test]
    fn secret_store_get_params_workspace_wire_shape() {
        let p = SecretStoreGetParams {
            scope: "workspace".into(),
            workspace_id: Some("ws-123".into()),
            key: "anthropic_api_key".into(),
        };
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(
            j,
            r#"{"scope":"workspace","workspace_id":"ws-123","key":"anthropic_api_key"}"#
        );
    }

    #[test]
    fn secret_store_get_params_global_omits_workspace_id() {
        // `workspace_id: None` must not ride the wire for a global read.
        let p = SecretStoreGetParams {
            scope: "global".into(),
            workspace_id: None,
            key: "openai_api_key".into(),
        };
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(j, r#"{"scope":"global","key":"openai_api_key"}"#);
    }

    #[test]
    fn secret_store_get_result_wire_shape() {
        // `value` carries the base64-encoded secret bytes (the secret is
        // never sent as a raw byte array).
        let r = SecretStoreGetResult {
            value: "c2VjcmV0".into(),
        };
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(j, r#"{"value":"c2VjcmV0"}"#);
    }

    #[test]
    fn unix_socket_event_data_kind_lowercased_on_wire() {
        let s = serde_json::to_string(&UnixSocketEventKind::Eof).unwrap();
        assert_eq!(s, "\"eof\"");
    }

    #[test]
    fn unix_socket_event_omits_absent_optional_fields() {
        // An EOF frame carries neither `bytes` nor `error`.
        let ev = UnixSocketEvent {
            kind: UnixSocketEventKind::Eof,
            bytes: None,
            error: None,
        };
        let j = serde_json::to_string(&ev).unwrap();
        assert_eq!(j, r#"{"kind":"eof"}"#);
    }

    #[test]
    fn spawn_managed_subprocess_skips_serializing_none_cwd() {
        // None must not emit a `cwd: null` key — keeps the wire payload
        // minimal and matches the optional contract.
        let p = SpawnManagedSubprocessParams {
            bin: "ainb-hangar-daemon".into(),
            argv: vec![],
            env_allowlist: vec![],
            cwd: None,
        };
        let j = serde_json::to_string(&p).unwrap();
        assert!(!j.contains("cwd"), "None cwd should be omitted: {j}");
    }

    #[test]
    fn spawn_managed_subprocess_defaults_empty_argv_and_env() {
        // Older/minimal peers may omit argv + env_allowlist entirely.
        let j = r#"{"bin":"ainb-hangar-daemon"}"#;
        let p: SpawnManagedSubprocessParams = serde_json::from_str(j).unwrap();
        assert_eq!(p.bin, "ainb-hangar-daemon");
        assert!(p.argv.is_empty());
        assert!(p.env_allowlist.is_empty());
        assert_eq!(p.cwd, None);
    }

    #[test]
    fn event_stream_subscribe_since_version_omitted_decodes_none() {
        // `since_version` is optional on the wire; older/absent peers omit it
        // entirely and the host starts the stream from the current version.
        let j = r#"{"topic":"workspace:default"}"#;
        let p: EventStreamSubscribeParams = serde_json::from_str(j).unwrap();
        assert_eq!(p.topic, "workspace:default");
        assert_eq!(p.since_version, None);
    }

    #[test]
    fn event_stream_subscribe_skips_serializing_none_since_version() {
        // None must not emit a `since_version: null` key — keeps the wire
        // payload minimal and matches the optional contract.
        let p = EventStreamSubscribeParams {
            topic: "workspace:default".into(),
            since_version: None,
        };
        let j = serde_json::to_string(&p).unwrap();
        assert_eq!(j, r#"{"topic":"workspace:default"}"#);
    }

    #[test]
    fn log_level_lowercased_on_wire() {
        let s = serde_json::to_string(&LogLevel::Warn).unwrap();
        assert_eq!(s, "\"warn\"");
    }

    #[test]
    fn mouse_event_wire_shape_and_viewport_coords() {
        // Coordinates are plugin-viewport-relative; kind carries the button.
        let ev = MouseEvent {
            kind: MouseKind::Down {
                button: MouseButton::Left,
            },
            col: 7,
            row: 2,
            mods: 0,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"col\":7"), "col missing: {json}");
        assert!(json.contains("\"row\":2"), "row missing: {json}");
        assert!(json.contains("\"kind\""), "kind missing: {json}");
        // Snake-case tagged union for the action + button.
        assert!(
            json.contains("\"type\":\"down\""),
            "down tag missing: {json}"
        );
        assert!(
            json.contains("\"type\":\"left\""),
            "left tag missing: {json}"
        );
        let back: MouseEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev, back);
    }

    #[test]
    fn mouse_event_mods_default_to_zero_when_absent() {
        let json = r#"{"kind":{"type":"moved"},"col":1,"row":1}"#;
        let ev: MouseEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.mods, 0);
        assert_eq!(ev.kind, MouseKind::Moved);
    }

    #[test]
    fn render_result_redraw_defaults_to_false_when_absent() {
        // Pre-`redraw` wire shape (buffer only) must still decode, with
        // redraw defaulting to false — ABI back-compat for non-animating
        // plugins.
        let mut buf = WireBuffer::new(1, 1);
        buf.push(Coord::new(0, 0), Cell::new("x"));
        let legacy = serde_json::json!({ "buffer": buf });
        let back: RenderResult = serde_json::from_value(legacy).unwrap();
        assert!(!back.redraw, "absent redraw must default to false");
    }

    #[test]
    fn handle_key_params_round_trip_ctrl_shift_press() {
        // Plan §Phase 1 gate: HandleKeyParams { Char('1'), SHIFT|CTRL, Press }
        // round-trips byte-stable.
        let params = HandleKeyParams {
            screen_id: "ainb_analytics".into(),
            key: KeyEvent {
                code: KeyCode::Char { ch: '1' },
                mods: KEY_MOD_SHIFT | KEY_MOD_CTRL,
                kind: KeyKind::Press,
            },
            generation: 42,
        };
        let json = serde_json::to_string(&params).unwrap();
        let back: HandleKeyParams = serde_json::from_str(&json).unwrap();
        assert_eq!(params, back);
        // Second encode of the decoded value must produce the same bytes.
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2, "encode is not byte-stable across decode");
    }

    #[test]
    fn key_code_wire_tag_and_payload() {
        // Wire tag is `type`, variants snake_case.
        let s = serde_json::to_string(&KeyCode::Char { ch: 'z' }).unwrap();
        assert_eq!(s, r#"{"type":"char","ch":"z"}"#);
        let s = serde_json::to_string(&KeyCode::BackTab).unwrap();
        assert_eq!(s, r#"{"type":"back_tab"}"#);
        let s = serde_json::to_string(&KeyCode::PageDown).unwrap();
        assert_eq!(s, r#"{"type":"page_down"}"#);
        let s = serde_json::to_string(&KeyCode::F { n: 7 }).unwrap();
        assert_eq!(s, r#"{"type":"f","n":7}"#);
    }

    #[test]
    fn key_kind_defaults_to_press_when_absent() {
        // Older peers that omit `kind` decode as Press.
        let j = r#"{"code":{"type":"enter"}}"#;
        let ev: KeyEvent = serde_json::from_str(j).unwrap();
        assert_eq!(ev.kind, KeyKind::Press);
        assert_eq!(ev.mods, 0);
        assert_eq!(ev.code, KeyCode::Enter);
    }

    #[test]
    fn key_mod_bits_are_independent() {
        // Each modifier occupies its own bit; OR-ing all four is 0b1111.
        let all = KEY_MOD_SHIFT | KEY_MOD_CTRL | KEY_MOD_ALT | KEY_MOD_SUPER;
        assert_eq!(all, 0b1111);
        assert_eq!(KEY_MOD_SHIFT & KEY_MOD_CTRL, 0);
        assert_eq!(KEY_MOD_ALT & KEY_MOD_SUPER, 0);
    }

    #[test]
    fn test_init_params_config_defaults_empty() {
        // ABI-2 back-compat: an init payload WITHOUT `config` still decodes,
        // and `config` defaults to JSON null.
        let legacy = r#"{
            "manifest_path": "/x/manifest.toml",
            "granted_capabilities": ["read_sessions"],
            "abi_version": 2
        }"#;
        let p: PluginInitParams = serde_json::from_str(legacy).unwrap();
        assert_eq!(p.config, serde_json::Value::Null);

        // With `config` present it round-trips byte-stably.
        let with_config = PluginInitParams {
            manifest_path: "/x/manifest.toml".into(),
            granted_capabilities: vec!["read_paths".into()],
            abi_version: 2,
            config: serde_json::json!({ "learnings_dir": "~/.learnings" }),
        };
        let json = serde_json::to_string(&with_config).unwrap();
        let back: PluginInitParams = serde_json::from_str(&json).unwrap();
        assert_eq!(with_config, back);
        assert_eq!(back.config["learnings_dir"], "~/.learnings");
    }

    #[test]
    fn handle_key_params_round_trip_each_code() {
        // Exercise the full KeyCode variant matrix through round-trip.
        let codes = [
            KeyCode::Char { ch: 'a' },
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Esc,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::F { n: 12 },
        ];
        for code in codes {
            let params = HandleKeyParams {
                screen_id: "s".into(),
                key: KeyEvent {
                    code: code.clone(),
                    mods: 0,
                    kind: KeyKind::Press,
                },
                generation: 1,
            };
            let j = serde_json::to_string(&params).unwrap();
            let back: HandleKeyParams = serde_json::from_str(&j).unwrap();
            assert_eq!(params, back, "round-trip failed for {code:?}");
        }
    }
}
