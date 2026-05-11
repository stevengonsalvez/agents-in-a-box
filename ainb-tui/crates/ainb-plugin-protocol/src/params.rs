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
// (de)serialise bytes::Bytes as a JSON array of u8 (no base64 layer).
// =====================================================================

/// Binary payloads ride the JSON-RPC envelope as **base64 strings**.
///
/// serde_json's default for `&[u8]` (and `Vec<u8>`) is a JSON array of
/// numbers — `[12, 34, 56, ...]` — which costs 3-4 ASCII chars per
/// byte. For the session-reader → burndown handoff that ballooned a
/// ~25 MB msgpack snapshot to ~80 MB of JSON, exceeding the host
/// framer's `MAX_BODY_BYTES = 16 MiB` and OOMing the plugin process
/// mid-write. Base64 costs ~1.33 chars per byte — fits the budget and
/// matches how JSON-RPC servers in the wild ship binary blobs.
///
/// The deserializer also accepts the legacy byte-array shape so older
/// peers (host paired with an older plugin, or vice versa) keep
/// working across the version bump.
mod bytes_serde {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use bytes::Bytes;
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

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
            Repr::B64(s) => B64
                .decode(s.as_bytes())
                .map(Bytes::from)
                .map_err(D::Error::custom),
            Repr::Bytes(v) => Ok(Bytes::from(v)),
        }
    }
}

mod opt_bytes_serde {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    use bytes::Bytes;
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

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
            Some(Repr::B64(s)) => B64
                .decode(s.as_bytes())
                .map(|v| Some(Bytes::from(v)))
                .map_err(D::Error::custom),
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
    fn round_trip_each() {
        rt(&PluginInitParams {
            manifest_path: "/x/manifest.toml".into(),
            granted_capabilities: vec!["read_sessions".into()],
            abi_version: 2,
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
        rt(&RenderResult { buffer: buf });

        rt(&HandleEventParams {
            topic: "sessions.usage_data".into(),
            payload: bytes::Bytes::from_static(b"hello"),
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

        rt(&SnapshotGetParams {
            topic: "t".into(),
        });
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
        rt(&SnapshotSubscribeParams {
            topic: "t".into(),
        });
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
    }

    #[test]
    fn log_level_lowercased_on_wire() {
        let s = serde_json::to_string(&LogLevel::Warn).unwrap();
        assert_eq!(s, "\"warn\"");
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
