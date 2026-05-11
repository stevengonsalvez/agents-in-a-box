//! ABI v2 [`Plugin`] implementation for burndown.
//!
//! `BurndownPlugin` owns the in-memory analytics view + the latest
//! `UsageData` snapshot pushed by the host on the `sessions.usage_data`
//! topic. Render translates the existing ratatui `Buffer` painter into
//! a wire `WireBuffer` cell stream; `cli_dispatch` re-parses argv via
//! clap and falls back to `host.snapshot_get` when no event push has
//! arrived yet.

use ainb_plugin_sdk::{
    Cell, CliOutput, Color, Coord, HostClient, Plugin, RenderParams, Result, SdkError,
    WireBuffer,
};
use ainb_plugin_types_sessions::{
    UsageData as WireUsageData, UsageDataEvent, WIRE_VERSION,
};
use async_trait::async_trait;
use ratatui::buffer::Buffer as RBuffer;
use ratatui::layout::Rect as RRect;
use ratatui::style::{Color as RColor, Modifier as RModifier};

use crate::cli::{UsageCommands, execute_for_plugin};
use crate::data::usage::UsageData;
use crate::output_format::OutputFormat;
use crate::ui::{UsageTab, UsageViewState, render as render_ui};
use crate::wire::wire_to_local;

/// Static manifest TOML loaded at compile time. The Server uses this on
/// `plugin/init` to echo `name`/`version` back to the host.
const MANIFEST_TOML: &str = include_str!("../plugin.toml");

/// Default render viewport — the host sends an explicit one in
/// `RenderParams.viewport`, but we fall back to this if a degenerate
/// 0×0 ever arrives. Matches the historical 80×24 baseline.
const FALLBACK_VIEWPORT: (u16, u16) = (80, 24);

/// Disposition of one [`UsageDataEvent`] chunk after
/// [`BurndownPlugin::apply_chunk_pure`] processes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkOutcome {
    /// Chunk accepted; sequence not yet final.
    Buffered,
    /// `is_final = true` chunk completed the sequence; `self.data` is
    /// now live.
    Finalised,
    /// Follow-on chunk arrived without a preceding `chunk_index = 0`
    /// — dropped, accumulator state unchanged.
    DroppedFollowOn,
}

/// Burndown plugin state.
#[derive(Default)]
pub struct BurndownPlugin {
    /// In-memory UI state — populated from cached snapshots and
    /// CLI/event-driven tab switches.
    ui: UsageViewState,
    /// Most recent fully-assembled `UsageData` snapshot decoded from
    /// `sessions.usage_data`.
    data: Option<UsageData>,
    /// Accumulator for the in-flight chunked publish (if any). Reset
    /// on `chunk_index == 0`, appended-to on follow-on chunks,
    /// finalised + moved into `self.data` on `is_final = true`. See
    /// [`UsageDataEvent`] docs for the chunking protocol.
    pending: Option<WireUsageData>,
    /// Set when the most recent snapshot's wire `version` did not match
    /// the crate's compiled [`WIRE_VERSION`]. Latched — the render path
    /// surfaces an upgrade hint instead of the wait-spinner.
    schema_mismatch: bool,
}

#[async_trait]
impl Plugin for BurndownPlugin {
    fn manifest(&self) -> &'static str {
        MANIFEST_TOML
    }

    async fn on_init(&mut self, host: &HostClient, _granted: &[String]) -> Result<()> {
        // Subscribe up front so chunked publishes from session-reader
        // arrive via `handle_event`. The snapshot store only retains
        // the most recent publish, so for >1-chunk snapshots a passive
        // `snapshot_get` would miss every chunk except the last.
        let _ = host.snapshot_subscribe("sessions.usage_data").await;

        // Trigger a fresh publish. Eager-spawned session-reader runs
        // its first publish during its own `on_init`, which races
        // with us — burndown can subscribe mid-stream and end up
        // missing chunk 0 of the in-flight sequence. Publishing on
        // the `sessions.refresh_request` topic asks session-reader
        // to rescan and re-publish from scratch, this time with us
        // already subscribed. Best-effort: failure is non-fatal.
        let _ = host
            .snapshot_publish("sessions.refresh_request", bytes::Bytes::new())
            .await;
        Ok(())
    }

    async fn handle_event(
        &mut self,
        host: &HostClient,
        params: ainb_plugin_sdk::HandleEventParams,
    ) -> Result<()> {
        if params.topic == "sessions.usage_data" {
            self.ingest_usage_payload(host, &params.payload).await;
        }
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, params: RenderParams) -> Result<WireBuffer> {
        let (w, h) = match (params.viewport.width, params.viewport.height) {
            (0, _) | (_, 0) => FALLBACK_VIEWPORT,
            (w, h) => (w, h),
        };
        let area = RRect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        };
        let mut rbuf = RBuffer::empty(area);

        if self.schema_mismatch && self.data.is_none() {
            paint_schema_mismatch(&mut rbuf, area);
        } else {
            // Snapshot the UI state so we can paint without holding a
            // mutable borrow on `self` for the whole call.
            let mut ui = self.ui.clone();
            ui.data = self.data.clone();
            render_ui(&mut rbuf, area, &ui);
        }
        Ok(buffer_to_wire(&rbuf, area))
    }

    async fn cli_dispatch(
        &mut self,
        host: &HostClient,
        namespace: &str,
        argv: &[String],
    ) -> Result<CliOutput> {
        if namespace != "usage" {
            return Ok(CliOutput {
                stdout: Vec::new(),
                stderr: format!("burndown: unknown namespace `{namespace}`\n").into_bytes(),
                exit_code: 2,
            });
        }

        // Pull `--format=<text|json|csv>` out of argv (host-level
        // global flag) and strip it before clap parses the subcommand
        // surface — clap doesn't declare it.
        let format = extract_format(argv);
        let stripped = strip_format_flag(argv);

        // Make sure we have a snapshot. If the host hasn't pushed one
        // yet, ask synchronously; bail with an actionable error if the
        // session-reader plugin isn't installed.
        if self.data.is_none() {
            self.refresh_snapshot(host).await;
        }
        let Some(data) = self.data.clone() else {
            return Ok(CliOutput {
                stdout: Vec::new(),
                stderr: b"error: usage analytics requires the session-reader plugin (install via 'ainb plugin install session-reader')\n".to_vec(),
                exit_code: 1,
            });
        };

        // Argv reshape: clap's `try_parse_from` expects argv[0] to be
        // the program name. We feed it "usage" so subcommands like
        // `report --json` parse naturally.
        let mut clap_argv: Vec<String> = vec!["usage".to_string()];
        clap_argv.extend(stripped);

        use clap::Parser;
        #[derive(Parser)]
        #[command(name = "usage")]
        struct UsageRoot {
            #[command(subcommand)]
            cmd: UsageCommands,
        }

        let parsed = match UsageRoot::try_parse_from(clap_argv) {
            Ok(p) => p,
            Err(e) => {
                let msg = e.to_string();
                let exit = if e.use_stderr() { 2 } else { 0 };
                return Ok(CliOutput {
                    stdout: Vec::new(),
                    stderr: msg.into_bytes(),
                    exit_code: exit,
                });
            }
        };

        match capture_stdout(|| execute_for_plugin(&data, parsed.cmd, format)) {
            (Ok(()), out) => Ok(CliOutput {
                stdout: out,
                stderr: Vec::new(),
                exit_code: 0,
            }),
            (Err(e), out) => Ok(CliOutput {
                stdout: out,
                stderr: format!("{e}\n").into_bytes(),
                exit_code: 1,
            }),
        }
    }
}

impl BurndownPlugin {
    /// Decode a `sessions.usage_data` payload (msgpack-encoded
    /// `UsageDataEvent`) and update local state. Handles chunked
    /// publishes: chunk 0 resets the accumulator, follow-on chunks
    /// append their `calls` slice, and `is_final = true` moves the
    /// accumulator into `self.data`. Single-chunk publishes (the v1
    /// path, plus small `$HOME`s) take exactly the same code path
    /// because their defaults are `chunk_index = 0, is_final = true`.
    /// Notes wire-version drift on the latched `schema_mismatch` flag.
    async fn ingest_usage_payload(&mut self, host: &HostClient, payload: &[u8]) {
        let event: UsageDataEvent = match rmp_serde::from_slice(payload) {
            Ok(e) => e,
            Err(e) => {
                let _ = host
                    .log_info(format!(
                        "burndown: malformed sessions.usage_data payload: {e}"
                    ))
                    .await;
                return;
            }
        };
        if event.version != WIRE_VERSION {
            self.schema_mismatch = true;
            let _ = host
                .log_info(format!(
                    "burndown: sessions.usage_data wire version mismatch: got {}, expected {}",
                    event.version, WIRE_VERSION
                ))
                .await;
            return;
        }
        self.apply_chunk(event, host).await;
    }

    /// Drive the chunk accumulator. Logs to `host` on protocol misuse
    /// (follow-on chunk with no in-flight publish); calls into the
    /// pure [`Self::apply_chunk_pure`] for the actual state transition
    /// so unit tests can hit every branch without a `HostClient`.
    async fn apply_chunk(&mut self, event: UsageDataEvent, host: &HostClient) {
        let chunk_index = event.chunk_index;
        let outcome = self.apply_chunk_pure(event);
        if matches!(outcome, ChunkOutcome::DroppedFollowOn) {
            let _ = host
                .log_info(format!(
                    "burndown: dropped follow-on chunk {chunk_index} (no in-flight publish)"
                ))
                .await;
        }
    }

    /// Pure version of [`Self::apply_chunk`] — no host I/O. Returns
    /// the disposition so the async wrapper can log the misuse path.
    fn apply_chunk_pure(&mut self, event: UsageDataEvent) -> ChunkOutcome {
        if event.chunk_index == 0 {
            // Fresh publish sequence — seed the accumulator with the
            // full aggregates + first call slice. Any prior in-flight
            // accumulator is discarded (the publisher abandoned it).
            self.pending = Some(event.data);
        } else {
            // Append `calls` onto the in-flight accumulator. If we
            // missed chunk 0 (subscriber joined late or sequence got
            // reordered), drop the chunk — partial data without
            // aggregates is worse than no data.
            match self.pending.as_mut() {
                Some(acc) => acc.calls.extend(event.data.calls),
                None => return ChunkOutcome::DroppedFollowOn,
            }
        }

        if event.is_final {
            if let Some(assembled) = self.pending.take() {
                self.data = Some(wire_to_local(assembled));
                self.schema_mismatch = false;
                return ChunkOutcome::Finalised;
            }
        }
        ChunkOutcome::Buffered
    }

    /// Pull the latest snapshot synchronously. Used by `cli_dispatch`
    /// when no event push has arrived yet; failure is non-fatal — the
    /// caller surfaces an actionable error.
    async fn refresh_snapshot(&mut self, host: &HostClient) {
        match host.snapshot_get("sessions.usage_data").await {
            Ok(snap) => {
                if let Some(payload) = snap.payload {
                    self.ingest_usage_payload(host, &payload).await;
                }
            }
            Err(SdkError::Rpc(_)) | Err(_) => {
                // Treat any failure as "no data" — caller emits the
                // install-session-reader hint.
            }
        }
    }
}

fn paint_schema_mismatch(buf: &mut RBuffer, area: RRect) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let msg =
        "  ⚠ Schema mismatch — upgrade session-reader/burndown plugins to matching versions.";
    let max = area.width as usize;
    let truncated: String = msg.chars().take(max).collect();
    buf.set_string(
        area.x,
        area.y,
        truncated,
        ratatui::style::Style::default(),
    );
}

/// Convert a ratatui `Buffer` to the SDK's `WireBuffer` cell stream.
///
/// Iterates row-major so the resulting `Vec<(Coord, Cell)>` is
/// deterministic between renders. Empty cells (default attrs and a
/// blank symbol) are dropped to keep the wire payload sparse.
fn buffer_to_wire(rbuf: &RBuffer, area: RRect) -> WireBuffer {
    let mut wire = WireBuffer::new(area.width, area.height);
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = rbuf.get(area.x + x, area.y + y);
            let symbol = cell.symbol();
            let fg = ratatui_color(cell.fg);
            let bg = ratatui_color(cell.bg);
            let modifier = ratatui_modifiers(cell.modifier);
            // Skip cells that wouldn't render anything visible — saves
            // bytes on the wire and keeps the JSON sparse like the host
            // expects.
            if symbol == " " && fg.is_none() && bg.is_none() && modifier == 0 {
                continue;
            }
            wire.push(
                Coord::new(x, y),
                Cell {
                    symbol: symbol.to_string(),
                    fg,
                    bg,
                    modifier,
                },
            );
        }
    }
    wire
}

fn ratatui_color(c: RColor) -> Option<Color> {
    match c {
        RColor::Reset => None,
        RColor::Black => Some(Color::rgb(0, 0, 0)),
        RColor::Red => Some(Color::rgb(170, 0, 0)),
        RColor::Green => Some(Color::rgb(0, 170, 0)),
        RColor::Yellow => Some(Color::rgb(170, 170, 0)),
        RColor::Blue => Some(Color::rgb(0, 0, 170)),
        RColor::Magenta => Some(Color::rgb(170, 0, 170)),
        RColor::Cyan => Some(Color::rgb(0, 170, 170)),
        RColor::Gray => Some(Color::rgb(170, 170, 170)),
        RColor::DarkGray => Some(Color::rgb(85, 85, 85)),
        RColor::LightRed => Some(Color::rgb(255, 85, 85)),
        RColor::LightGreen => Some(Color::rgb(85, 255, 85)),
        RColor::LightYellow => Some(Color::rgb(255, 255, 85)),
        RColor::LightBlue => Some(Color::rgb(85, 85, 255)),
        RColor::LightMagenta => Some(Color::rgb(255, 85, 255)),
        RColor::LightCyan => Some(Color::rgb(85, 255, 255)),
        RColor::White => Some(Color::rgb(255, 255, 255)),
        RColor::Indexed(_) => None,
        RColor::Rgb(r, g, b) => Some(Color::rgb(r, g, b)),
    }
}

fn ratatui_modifiers(m: RModifier) -> u16 {
    let mut out = 0_u16;
    if m.contains(RModifier::BOLD) {
        out |= 1;
    }
    if m.contains(RModifier::DIM) {
        out |= 2;
    }
    if m.contains(RModifier::ITALIC) {
        out |= 4;
    }
    if m.contains(RModifier::UNDERLINED) {
        out |= 8;
    }
    if m.contains(RModifier::REVERSED) {
        out |= 16;
    }
    out
}

fn extract_format(argv: &[String]) -> OutputFormat {
    let mut iter = argv.iter().peekable();
    while let Some(a) = iter.next() {
        if let Some(rest) = a.strip_prefix("--format=") {
            return parse_format(rest);
        }
        if a == "--format" {
            if let Some(next) = iter.peek() {
                return parse_format(next);
            }
        }
    }
    OutputFormat::default()
}

fn strip_format_flag(argv: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(argv.len());
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--format" {
            i += 1;
            if i < argv.len() {
                i += 1;
            }
            continue;
        }
        if a.starts_with("--format=") {
            i += 1;
            continue;
        }
        out.push(a.clone());
        i += 1;
    }
    out
}

fn parse_format(s: &str) -> OutputFormat {
    match s {
        "json" => OutputFormat::Json,
        "csv" => OutputFormat::Csv,
        _ => OutputFormat::Text,
    }
}

/// Run `f`, collecting anything it `println!`'d into a `Vec<u8>` in
/// place of stdout. Stderr is unaffected — it still goes to the
/// process's real stderr, which the runtime drains and forwards.
///
/// Implementation: redirect fd 1 to a tempfile via `dup2`, run `f`,
/// restore the original stdout, then read the tempfile back. The
/// tempfile drains synchronously (no pipe-buffer deadlock concern).
/// Unix-only — Phase 7c plugins ship on the same targets the runtime
/// already supports.
///
/// The legacy `cli` module emits its 9-subcommand report output via
/// `println!` / `print!` (~80 sites). Refactoring each helper to take a
/// `&mut impl Write` would touch a 1700-line file; this brief
/// fd-redirect keeps the diff tight while still producing a captured
/// stdout for the JSON-RPC `cli_dispatch` reply.
#[cfg(unix)]
fn capture_stdout<F: FnOnce() -> anyhow::Result<()>>(f: F) -> (anyhow::Result<()>, Vec<u8>) {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    use std::os::fd::AsRawFd;

    let mut buf = Vec::new();
    let mut tmp = match tempfile::tempfile() {
        Ok(f) => f,
        Err(_) => return (f(), buf),
    };

    // SAFETY: dup/dup2/close are POSIX fd ops. We own fd 1 for the
    // duration of this call (single-threaded inside the SDK's plugin
    // mutex), so swapping it in and out is race-free.
    unsafe {
        let stdout_fd: libc::c_int = 1;
        let saved = libc::dup(stdout_fd);
        if saved < 0 {
            return (f(), buf);
        }
        if libc::dup2(tmp.as_raw_fd(), stdout_fd) < 0 {
            libc::close(saved);
            return (f(), buf);
        }
        let _ = std::io::stdout().flush();
        let result = f();
        let _ = std::io::stdout().flush();
        libc::dup2(saved, stdout_fd);
        libc::close(saved);

        let _ = tmp.seek(SeekFrom::Start(0));
        let _ = tmp.read_to_end(&mut buf);
        (result, buf)
    }
}

#[cfg(not(unix))]
fn capture_stdout<F: FnOnce() -> anyhow::Result<()>>(f: F) -> (anyhow::Result<()>, Vec<u8>) {
    // Non-Unix targets fall back to no-capture. The plugin ships on
    // macOS + Linux only in Phase 7c.
    (f(), Vec::new())
}

/// Allow `set_tab` style commands once we wire them through the snapshot
/// bus. Today this lets `tests/stdio_smoke.rs` exercise the lifecycle
/// without depending on a session-reader installation.
impl BurndownPlugin {
    #[allow(dead_code)]
    pub fn set_active_tab(&mut self, tab: UsageTab) {
        self.ui.active_tab = tab;
    }
}

#[cfg(test)]
mod chunk_accumulator_tests {
    use super::*;
    use ainb_plugin_types_sessions::{
        ProjectUsage, Provider, ProviderCall, TokenBucket, WIRE_VERSION,
    };
    use chrono::{DateTime, Utc};

    fn fake_call(id: u64) -> ProviderCall {
        ProviderCall {
            id,
            provider: Provider::Claude,
            model: "claude-sonnet".into(),
            session_id: "s".into(),
            project: "p".into(),
            project_path: "/tmp".into(),
            timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
            input_tokens: 1,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            output_tokens: 1,
            reasoning_tokens: 0,
            cost_usd: None,
            tools: vec![],
            bash_commands: vec![],
            user_message: String::new(),
            branch: None,
        }
    }

    fn chunk(idx: u32, is_final: bool, calls: Vec<ProviderCall>) -> UsageDataEvent {
        let mut data = WireUsageData::default();
        data.calls = calls;
        if idx == 0 {
            // Plant an aggregate so we can assert chunk 0 seeds the
            // accumulator with the full payload.
            data.projects = vec![ProjectUsage {
                name: "p".into(),
                path: "/tmp".into(),
                bucket: TokenBucket::default(),
                repo: None,
            }];
        }
        UsageDataEvent {
            version: WIRE_VERSION,
            published_ns: 0,
            partial: false,
            chunk_index: idx,
            is_final,
            data,
        }
    }

    #[test]
    fn single_chunk_finalises_immediately() {
        let mut p = BurndownPlugin::default();
        let outcome =
            p.apply_chunk_pure(chunk(0, true, vec![fake_call(1), fake_call(2)]));
        assert_eq!(outcome, ChunkOutcome::Finalised);
        assert!(p.pending.is_none());
        let d = p.data.as_ref().expect("snapshot finalised");
        assert_eq!(d.calls.len(), 2);
    }

    #[test]
    fn three_chunk_sequence_accumulates_then_finalises() {
        let mut p = BurndownPlugin::default();
        assert_eq!(
            p.apply_chunk_pure(chunk(0, false, vec![fake_call(1)])),
            ChunkOutcome::Buffered
        );
        assert_eq!(
            p.apply_chunk_pure(chunk(1, false, vec![fake_call(2)])),
            ChunkOutcome::Buffered
        );
        // Mid-flight: no live data yet.
        assert!(p.data.is_none());
        assert_eq!(
            p.apply_chunk_pure(chunk(2, true, vec![fake_call(3)])),
            ChunkOutcome::Finalised
        );
        let d = p.data.as_ref().expect("snapshot finalised");
        assert_eq!(d.calls.len(), 3);
        // Calls arrive in chunk order.
        assert_eq!(d.calls[0].id, 1);
        assert_eq!(d.calls[2].id, 3);
    }

    #[test]
    fn follow_on_chunk_without_chunk_zero_is_dropped() {
        let mut p = BurndownPlugin::default();
        assert_eq!(
            p.apply_chunk_pure(chunk(1, true, vec![fake_call(1)])),
            ChunkOutcome::DroppedFollowOn
        );
        assert!(p.data.is_none(), "must not finalise from a stray chunk");
        assert!(p.pending.is_none());
    }

    #[test]
    fn new_chunk_zero_discards_in_flight_accumulator() {
        let mut p = BurndownPlugin::default();
        let _ = p.apply_chunk_pure(chunk(0, false, vec![fake_call(1)]));
        let _ = p.apply_chunk_pure(chunk(1, false, vec![fake_call(2)]));
        // Publisher abandoned and started over.
        assert_eq!(
            p.apply_chunk_pure(chunk(0, true, vec![fake_call(99)])),
            ChunkOutcome::Finalised
        );
        let d = p.data.as_ref().unwrap();
        assert_eq!(d.calls.len(), 1, "old accumulator was discarded");
        assert_eq!(d.calls[0].id, 99);
    }

    #[test]
    fn finalising_clears_schema_mismatch_flag() {
        let mut p = BurndownPlugin::default();
        p.schema_mismatch = true;
        let _ = p.apply_chunk_pure(chunk(0, true, vec![fake_call(1)]));
        assert!(!p.schema_mismatch);
    }
}
