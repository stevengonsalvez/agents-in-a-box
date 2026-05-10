# ainb-plugin-protocol

Wire protocol for the ainb plugin runtime. JSON-RPC 2.0 over
Content-Length framed stdio. Source-of-truth wire types shared between
the plugin SDK, the host runtime, and the test harness.

This crate is pure data — no tokio, no ratatui, no I/O policy. It
defines:

- **`manifest`** — Manifest v2 schema (TOML &harr; Rust)
- **`methods`** — JSON-RPC method-name constants (`plugin/*` and `host/*`)
- **`params`** — request/response param structs (serde-derived)
- **`errors`** — JSON-RPC error codes + the `RpcError` envelope
- **`framing`** — Content-Length stdio frame encode/decode
- **`wire_buffer`** — `WireBuffer` cell-based render output

Phase 7a.1 of the plugin runtime redesign — see
`plans/plugin-phase-7-runtime-redesign.md`.
