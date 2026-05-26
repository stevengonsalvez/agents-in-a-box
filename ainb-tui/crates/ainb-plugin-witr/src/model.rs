//! Rust mirrors of witr's `pkg/model` types.
//!
//! The wire shape is whatever `witr --json <target>` prints, which
//! upstream encodes via Go's default `encoding/json` behaviour:
//! PascalCase field names with `omitempty` on optionals. We mirror
//! the contract field-for-field for the stable surface and degrade to
//! [`serde_json::Value`] for nested structures the renderer doesn't
//! need typed yet (cfx.5 can refine in place).
//!
//! ## Stability contract
//!
//! Per `research/2026-05-19_12-13-09_witr-spec-open-questions.md`
//! (Q3), the top-level surface has been *additive-only* across the
//! witr v0.2 → v0.3 series. The six fields explicitly covered by
//! upstream's `json_test.go` are required and load-bearing:
//!
//! - `Target`, `ResolvedTarget`, `Process`, `Ancestry`, `Source`,
//!   `Warnings`
//!
//! Everything beyond is decorated with `#[serde(default)]` so a
//! field added in witr v0.4 (e.g. `NetworkContext`) parses cleanly
//! into a sibling unknown-field on the snapshot, and a field
//! *removed* in v0.4 falls back to its `Default`.
//!
//! Inner types we don't yet need typed access to (`SocketInfo`,
//! `ResourceContext`, `FileContext`, per-process `Memory`/`IO`/etc.)
//! are held as `Option<serde_json::Value>`. The renderer in cfx.5
//! either reads them via `pointer()` lookups or tightens the type
//! when a real field becomes load-bearing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Top-level witr snapshot. Renamed from Go's `Result` to avoid
/// colliding with [`std::result::Result`] in Rust call sites; the
/// JSON shape is preserved by `#[serde(rename_all = "PascalCase")]`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct WitrSnapshot {
    /// What the user asked about (PID / port / name / file / container).
    pub target: Target,
    /// Resolved string form of the target (e.g. `"nginx"` for a
    /// `name`-typed target, the listening process name for a `port`).
    pub resolved_target: String,
    /// The primary process the target resolves to.
    pub process: Process,
    /// Service-manager restart count. Upstream emits `0` when unknown
    /// rather than omitting the field; we default to `0` for
    /// forward-compat.
    #[serde(default)]
    pub restart_count: i64,
    /// Chain of ancestors (parents up to PID 1).
    pub ancestry: Vec<Process>,
    /// Child processes (omitted by upstream when empty). Note the
    /// type difference vs [`Process::children`]: this field carries
    /// full resolved `Process` structs (one tree level down from the
    /// target), whereas `Process::children` is `Vec<i64>` of PIDs
    /// only. The naming collision is upstream's; the types diverge.
    #[serde(default)]
    pub children: Vec<Process>,
    /// Where this process came from (systemd / launchd / container /
    /// shell / cron / ...).
    pub source: Source,
    /// Soft warnings the detector wants surfaced — e.g. "running as
    /// root", "executable deleted".
    pub warnings: Vec<String>,
    /// Socket state for `port`-typed targets. Held opaque for v1 —
    /// refine in cfx.5 if the Ports tab needs typed access.
    #[serde(default)]
    pub socket_info: Option<serde_json::Value>,
    /// macOS resource-usage context. Opaque for v1.
    #[serde(default)]
    pub resource_context: Option<serde_json::Value>,
    /// File-descriptor and lock context for `file`-typed targets.
    /// Opaque for v1 — the Locks tab in cfx.5 will refine.
    #[serde(default)]
    pub file_context: Option<serde_json::Value>,
}

/// What the user asked about.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Target {
    /// Target kind. Upstream emits one of: `"name"`, `"pid"`, `"port"`,
    /// `"file"`, `"container"`. Held as `String` rather than an enum
    /// so a new upstream variant doesn't break parse.
    #[serde(rename = "Type")]
    pub kind: String,
    /// Literal target value as the user typed it.
    pub value: String,
}

/// A single process in the ancestry chain or as the primary target.
///
/// Upstream's `Process` carries 25+ fields. We type the ones the
/// render path needs (PID/PPID/Command/User/CPU/RSS/git/container/
/// service/health) and hold the long-tail nested types as
/// [`serde_json::Value`] until cfx.5 wires real surfaces for them.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Process {
    /// Process ID.
    #[serde(rename = "PID")]
    pub pid: i64,
    /// Parent process ID.
    #[serde(rename = "PPID")]
    pub ppid: i64,
    /// `argv[0]` (short form).
    pub command: String,
    /// Full command line.
    #[serde(default)]
    pub cmdline: String,
    /// Resolved executable path.
    #[serde(default)]
    pub exe: String,
    /// RFC3339 timestamp when the process started.
    #[serde(default)]
    pub started_at: Option<String>,
    /// Owning user.
    #[serde(default)]
    pub user: String,
    /// Percent CPU over a short window.
    #[serde(default, rename = "CPUPercent")]
    pub cpu_percent: f64,
    /// Resident set size in bytes.
    #[serde(default, rename = "MemoryRSS")]
    pub memory_rss: u64,
    /// Percent of total memory.
    #[serde(default)]
    pub memory_percent: f64,
    /// Process working directory.
    #[serde(default)]
    pub working_dir: String,
    /// Git repo path the working dir sits in (empty if none).
    #[serde(default)]
    pub git_repo: String,
    /// Git branch checked out in `git_repo`.
    #[serde(default)]
    pub git_branch: String,
    /// Container ID/name if this is a container process.
    #[serde(default)]
    pub container: String,
    /// Service-manager service name if applicable.
    #[serde(default)]
    pub service: String,
    /// All sockets owned by the process. Opaque for v1 — typed in
    /// cfx.5 when the Ports tab needs each socket's fields.
    #[serde(default)]
    pub sockets: Vec<serde_json::Value>,
    /// Health classification (`"healthy"` / `"zombie"` / `"stopped"` /
    /// `"high-cpu"` / `"high-mem"`).
    #[serde(default)]
    pub health: String,
    /// Forked status (`"forked"` / `"not-forked"` / `"unknown"`).
    #[serde(default)]
    pub forked: String,
    /// Environment variables as `KEY=value` strings.
    #[serde(default)]
    pub env: Vec<String>,
    /// True if the on-disk executable was deleted after the process
    /// started — common with rolling deploys.
    #[serde(default)]
    pub exe_deleted: bool,
    /// Linux capabilities the process holds.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Detailed memory breakdown. Opaque for v1.
    #[serde(default)]
    pub memory: Option<serde_json::Value>,
    /// Block-I/O counters. Opaque for v1.
    #[serde(default, rename = "IO")]
    pub io: Option<serde_json::Value>,
    /// File descriptors as strings.
    #[serde(default)]
    pub file_descs: Vec<String>,
    /// Count of open FDs.
    #[serde(default, rename = "FDCount")]
    pub fd_count: i64,
    /// FD limit (RLIMIT_NOFILE).
    #[serde(default, rename = "FDLimit")]
    pub fd_limit: u64,
    /// Child PIDs. Distinct from [`WitrSnapshot::children`] — that
    /// field carries `Vec<Process>` (resolved structs); this is just
    /// the PID numbers. Same name in upstream's JSON, different
    /// types depending on which struct owns the field.
    #[serde(default)]
    pub children: Vec<i64>,
    /// Thread count.
    #[serde(default)]
    pub thread_count: i64,
}

/// Where the process came from — i.e. what put it there. Maps to
/// upstream's `Source` struct.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Source {
    /// Source kind. Upstream uses one of: `"container"`, `"systemd"`,
    /// `"launchd"`, `"bsdrc"`, `"supervisor"`, `"cron"`, `"ssh"`,
    /// `"shell"`, `"windows_service"`, `"init"`, `"unknown"`. Held as
    /// `String` for forward-compat with new sources.
    #[serde(rename = "Type")]
    pub kind: String,
    /// Friendly name of the source (e.g. `"nginx.service"`).
    pub name: String,
    /// Free-form description.
    #[serde(default)]
    pub description: String,
    /// Path to the unit file (systemd) or plist (launchd) if known.
    #[serde(default)]
    pub unit_file: String,
    /// Source-specific extra metadata as opaque key/value pairs.
    #[serde(default)]
    pub details: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The canonical sample shape from
    /// `research/2026-05-19_10-01-49_witr-as-ainb-plugin.md`.
    ///
    /// We pin the basics (target shape, PID extraction, source kind)
    /// so a contract drift on the stable-6 surface fails this test
    /// before any rendering tab gets a chance to read missing fields.
    const SAMPLE_JSON: &str = r#"{
        "Target": {"Type": "name", "Value": "nginx"},
        "ResolvedTarget": "nginx",
        "Process": {
            "PID": 1234,
            "PPID": 800,
            "Command": "nginx",
            "Cmdline": "nginx: master process /usr/sbin/nginx",
            "User": "root",
            "CPUPercent": 0.5,
            "MemoryRSS": 12345678,
            "Health": "healthy"
        },
        "RestartCount": 0,
        "Ancestry": [
            {"PID": 800, "PPID": 1, "Command": "systemd"},
            {"PID": 1, "PPID": 0, "Command": "systemd"}
        ],
        "Source": {
            "Type": "systemd",
            "Name": "nginx.service",
            "UnitFile": "/etc/systemd/system/nginx.service"
        },
        "Warnings": [],
        "SocketInfo": null,
        "ResourceContext": null,
        "FileContext": null
    }"#;

    #[test]
    fn parses_canonical_sample() {
        let snap: WitrSnapshot =
            serde_json::from_str(SAMPLE_JSON).expect("canonical sample must parse");
        assert_eq!(snap.target.kind, "name");
        assert_eq!(snap.target.value, "nginx");
        assert_eq!(snap.resolved_target, "nginx");
        assert_eq!(snap.process.pid, 1234);
        assert_eq!(snap.process.ppid, 800);
        assert_eq!(snap.process.command, "nginx");
        assert_eq!(snap.process.cpu_percent, 0.5);
        assert_eq!(snap.process.memory_rss, 12_345_678);
        assert_eq!(snap.process.health, "healthy");
        assert_eq!(snap.restart_count, 0);
        assert_eq!(snap.ancestry.len(), 2);
        assert_eq!(snap.ancestry[0].pid, 800);
        assert_eq!(snap.source.kind, "systemd");
        assert_eq!(snap.source.name, "nginx.service");
        assert_eq!(snap.source.unit_file, "/etc/systemd/system/nginx.service");
        assert!(snap.warnings.is_empty());
        assert!(snap.socket_info.is_none());
    }

    #[test]
    fn parses_minimal_snapshot_with_only_stable_fields() {
        // Everything beyond the stable-6 is omitted. The
        // `#[serde(default)]` decorations must fill the gaps without
        // erroring. This pins the forward-compat / backward-compat
        // contract — a hypothetical witr v0.2 that never emitted
        // SocketInfo/RestartCount/etc. would still parse.
        let raw = r#"{
            "Target": {"Type": "pid", "Value": "1234"},
            "ResolvedTarget": "1234",
            "Process": {"PID": 1234, "PPID": 1, "Command": "init"},
            "Ancestry": [],
            "Source": {"Type": "init", "Name": "init"},
            "Warnings": []
        }"#;
        let snap: WitrSnapshot =
            serde_json::from_str(raw).expect("minimal stable-6 must parse");
        assert_eq!(snap.process.pid, 1234);
        assert_eq!(snap.restart_count, 0);
        assert!(snap.children.is_empty());
        assert!(snap.socket_info.is_none());
        assert!(snap.resource_context.is_none());
        assert!(snap.file_context.is_none());
    }

    #[test]
    fn tolerates_unknown_top_level_field() {
        // Forward-compat: a future witr v0.4 adds a `NetworkContext`
        // field. The default serde behaviour is "ignore unknown
        // fields", which is what we want — we explicitly do NOT
        // `#[serde(deny_unknown_fields)]` anywhere on this surface.
        let raw = r#"{
            "Target": {"Type": "name", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": 1, "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": [],
            "NetworkContext": {"FutureField": 42}
        }"#;
        let snap: WitrSnapshot =
            serde_json::from_str(raw).expect("unknown field must not break parse");
        assert_eq!(snap.target.value, "x");
    }

    #[test]
    fn rejects_wrong_pid_type() {
        // PID is i64; a string-shaped PID is a contract break. We
        // want a hard parse error rather than silent zero-coercion.
        let raw = r#"{
            "Target": {"Type": "pid", "Value": "x"},
            "ResolvedTarget": "x",
            "Process": {"PID": "not-a-number", "PPID": 0, "Command": "x"},
            "Ancestry": [],
            "Source": {"Type": "unknown", "Name": ""},
            "Warnings": []
        }"#;
        assert!(
            serde_json::from_str::<WitrSnapshot>(raw).is_err(),
            "PID-as-string must error",
        );
    }

    #[test]
    fn round_trips_through_serialize_then_deserialize() {
        // Pins serde Serialize symmetry — important because cfx.8's
        // event-bus publisher re-emits the snapshot as msgpack via
        // serde_json::Value, which requires the Serialize impl to
        // round-trip cleanly.
        let original: WitrSnapshot =
            serde_json::from_str(SAMPLE_JSON).expect("sample parses");
        let bytes = serde_json::to_vec(&original).expect("serialize");
        let back: WitrSnapshot = serde_json::from_slice(&bytes).expect("deserialize");
        assert_eq!(original, back);
    }
}
