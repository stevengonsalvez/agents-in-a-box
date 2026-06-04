# ainb-plugin-witr

A native ainb **v2 plugin** (subprocess + JSON-RPC 2.0 over
Content-Length stdio) that wraps the
[`pranshuparmar/witr`](https://github.com/pranshuparmar/witr) CLI to
surface **process causality tracing** — *"why is this process
running?"* — inside ainb's TUI.

The plugin owns the full data plane: it exec's `witr --json` under the
`spawn_subprocess` capability, parses stdout, renders a `WireBuffer`,
and publishes results on the event bus. `ainb-core` has zero knowledge
of witr.

## Requirements

`witr >= 0.3.2` on the user's `PATH`. The plugin detects it at startup
and renders a copy-paste install hint when missing (it never bundles
or auto-installs witr). Officially supported: **macOS arm64** and
**Linux x86_64** (incl. WSL).

## Surfaces

| Surface | What it does |
|---|---|
| Screen `witr` | 4 tabs — Processes / Ports / Containers / Locks — with a `/`-key detail overlay. |
| Slash `/witr <target>` | Opens the detail overlay for a target. (Host slash dispatch is pending — see `agents-in-a-box-6qc`.) |
| CLI `ainb witr <target>` | Text / JSON / tree / warnings / short output. |
| Event topic `witr.snapshot` | Published on every successful TUI scan. |

### Target addressing

witr addresses targets by **kind**, not a bare positional. A bare arg
is a process *name*; PIDs/ports/files/containers use explicit flags:

```
ainb witr nginx                # by name (fuzzy)
ainb witr --pid 1234           # by PID
ainb witr --port 5432          # by listening port
ainb witr --file /var/x.lock   # by file holder
ainb witr --container redis    # by container
```

In the TUI target box and `/witr`, typed targets use a prefix:
`pid:1234`, `port:5432`, `file:/x`, `container:redis`; anything else is
a name.

CLI flags: `--format text|json` · `--tree` (ancestry only) ·
`--warnings` · `--short` (raw witr passthrough). `--short` is mutually
exclusive with `--tree`/`--warnings`/`--format json`.

## `witr.snapshot` event payload

On every successful TUI scan the plugin publishes on the
`witr.snapshot` topic (capability `event_bus`). The payload is the
parsed snapshot serialised to a `serde_json::Value`, then
**msgpack-encoded** (Path 2 of the spec — no shared-types crate for
v1; consumers decode msgpack → `Value` and read fields by name).

Decode (Rust consumer):

```rust
// payload: bytes::Bytes from HandleEventParams.payload
let value: serde_json::Value = rmp_serde::from_slice(&payload)?;
let pid = value["Process"]["PID"].as_i64();
```

The JSON shape mirrors witr's `--json` output (Go PascalCase). Stable
top-level keys (always present): `Target`, `ResolvedTarget`,
`Process`, `Ancestry`, `Source`, `Warnings`. Additive keys
(`RestartCount`, `Children`, `SocketInfo`, `ResourceContext`,
`FileContext`) appear when witr emits them.

```jsonc
{
  "Target":        { "Type": "pid", "Value": "1234" },
  "ResolvedTarget": "nginx",
  "Process":       { "PID": 1234, "PPID": 800, "Command": "nginx", "User": "root", /* … */ },
  "RestartCount":  0,
  "Ancestry":      [ { "PID": 800, "PPID": 1, "Command": "systemd" } ],
  "Source":        { "Type": "systemd", "Name": "nginx.service", "Details": { /* sorted map */ } },
  "Warnings":      [ "running as root" ],
  "SocketInfo":    null,
  "ResourceContext": null,
  "FileContext":   null
}
```

**Byte-determinism:** the same snapshot always encodes to identical
msgpack bytes — the crate doesn't enable serde_json's `preserve_order`
(so `Value::Object` is a sorted `BTreeMap`) and the only typed map
(`Source.Details`) is a `BTreeMap`. No `HashMap` on the wire path.

## Module map

| Module | Responsibility |
|---|---|
| `detect` | `which witr` + `--version` parse + `>= 0.3.2` gate. |
| `exec` | `WitrTarget` addressing + `witr --json` subprocess (5s timeout, argv-array, no shell). |
| `model` | Rust mirrors of witr's `pkg/model` JSON (PascalCase). |
| `render/` | Tab strip + per-tab painters + detail overlay + missing/outdated empty states. |
| `state` | Lifecycle gate, tab/UI mode, LRU snapshot cache (8 / 60s), scan-coalescing gate. |
| `cli` / `slash` | `ainb witr` clap surface + `/witr` parser. |
| `publish` | `witr.snapshot` msgpack payload encoder. |
| `plugin` | The SDK `Plugin` trait impl wiring it all together. |
