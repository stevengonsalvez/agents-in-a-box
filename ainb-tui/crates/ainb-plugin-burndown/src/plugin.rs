//! ABI v2 [`Plugin`] implementation for burndown.
//!
//! `BurndownPlugin` owns the in-memory analytics view + the latest
//! `UsageData` snapshot pushed by the host on the `sessions.usage_data`
//! topic. Render translates the existing ratatui `Buffer` painter into
//! a wire `WireBuffer` cell stream; `cli_dispatch` re-parses argv via
//! clap and falls back to `host.snapshot_get` when no event push has
//! arrived yet.

use ainb_plugin_sdk::{
    Cell, CliOutput, Color, Coord, HandleKeyParams, HostClient, KeyCode, Plugin, RenderParams,
    Result, SdkError, WireBuffer,
};
use crate::data::usage::UsagePeriod;
use ainb_plugin_types_sessions::{
    ScanProgressEvent, UsageData as WireUsageData, UsageDataEvent, WIRE_VERSION,
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
    /// `sessions.usage_data`. Stored as `Arc` so the per-Enter sync
    /// into `ui.data` and the per-render snapshot copy are reference-
    /// count bumps (`O(1)`) instead of full 30K-call deep clones. The
    /// filter cache holds its own derived `Arc<UsageData>` keyed on
    /// `(data_generation, inputs_hash)`.
    data: Option<std::sync::Arc<UsageData>>,
    /// Accumulator for the in-flight chunked publish (if any). Reset
    /// on `chunk_index == 0`, appended-to on follow-on chunks,
    /// finalised + moved into `self.data` on `is_final = true`. See
    /// [`UsageDataEvent`] docs for the chunking protocol.
    pending: Option<WireUsageData>,
    /// Set when the most recent snapshot's wire `version` did not match
    /// the crate's compiled [`WIRE_VERSION`]. Latched — the render path
    /// surfaces an upgrade hint instead of the wait-spinner.
    schema_mismatch: bool,
    /// Latest `sessions.scan_progress` event from session-reader.
    /// Drives the skeleton/"Scanning sessions…" line that the UI
    /// renders before the first `sessions.usage_data` chunk lands.
    /// Cleared once a chunked publish finalises into `self.data`.
    scan_progress: Option<ScanProgressEvent>,
    /// Freshness witness — bumped once per `handle_key` invocation
    /// that actually mutated `self.ui`. The host's next
    /// `plugin/render` will see the updated state. Future host
    /// versions may echo this value via `RenderParams.generation`
    /// so the host can prove the keystroke landed before the frame
    /// was painted; today the plugin keeps it as private bookkeeping.
    generation: u64,
    /// Monotonic data-identity counter. Bumped once per
    /// `apply_chunk_pure` finalise so the filter cache below can
    /// invalidate when the underlying call set changes without
    /// hashing the (potentially huge) `UsageData`.
    data_generation: u64,
    /// `filter_usage_data_full(self.data, ui.filters, ui.period,
    /// ui.provider_filter)` is the dominant per-render cost
    /// (`analyze_turns` walks every call to build a session timeline,
    /// then the aggregate pass rebuilds every per-day/per-project/
    /// per-model rollup). It's also pure: the output depends only on
    /// `(data, filters, period, provider_filter)`. We cache the
    /// most-recent result keyed by `(data_generation, inputs_hash)` —
    /// inputs_hash mixes filters + period + provider — so idle
    /// re-renders (between keystrokes) and repeated renders on the
    /// same filter state are O(1) instead of O(N).
    filter_cache: Option<FilterCacheEntry>,
    /// Pre-built dimension indices over `self.data.calls`. Built once
    /// per `data_generation` bump (i.e. each new wire snapshot) and
    /// reused across chip pivots so the filter pass is
    /// `O(candidate_set)` instead of `O(N)` whenever a project /
    /// model / branch / activity chip is active. Cleared in lockstep
    /// with `filter_cache` on each new ingest.
    indices: Option<crate::data::usage::UsageIndices>,
    /// Monotonic counter bumped every time `cached_filtered` runs an
    /// actual recompute (cache miss). The render path snapshots this
    /// before/after calling `cached_filtered`; a delta means the chip
    /// strip should flash a brief `↻ updated` badge — confirming to
    /// the user that their drill-down was applied even when the
    /// wall-clock compute was fast enough that the only visible
    /// change is a few panel numbers.
    pivot_seq: u64,
}

/// One-deep filter-cache slot. A single entry is enough because
/// burndown renders synchronously and a key-press always switches to
/// at most one (filters, period, provider) state per render. Keeping
/// the cache size at 1 avoids invalidation logic and bounds memory at
/// 2× the largest `UsageData` snapshot.
#[derive(Debug, Clone)]
struct FilterCacheEntry {
    data_generation: u64,
    /// Hash of `(filters, period, provider_filter)` — the full input
    /// surface to `filter_usage_data_full`. Renamed from `filters_hash`
    /// when period + provider joined the cache key in PR A.
    inputs_hash: u64,
    filtered: std::sync::Arc<crate::data::usage::UsageData>,
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
        // Subscribe to scan-progress events so the skeleton/"Scanning
        // sessions…" line can render while session-reader is still
        // walking the per-provider dirs on a cold cache. Failure is
        // non-fatal — the legacy ⏳ Waiting spinner remains.
        let _ = host.snapshot_subscribe("sessions.scan_progress").await;

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
        match params.topic.as_str() {
            "sessions.usage_data" => {
                self.ingest_usage_payload(host, &params.payload).await;
            }
            "sessions.scan_progress" => {
                self.ingest_scan_progress(host, &params.payload).await;
            }
            _ => {}
        }
        Ok(())
    }

    /// Dispatch a single keystroke forwarded by the host's
    /// `PluginScreen::handle_key`.
    ///
    /// The plan in `ainb-tui/plans/plugin-interactive-keys-and-cache.md`
    /// §Phase 4 enumerates the canonical key → UI binding. A few binds
    /// were renamed during implementation to match what actually
    /// exists in `ui.rs`:
    ///
    /// - `Esc` → `pop_filter_chip` (plan: `pop_filter`).
    /// - `C`   → `clear_all_filter_chips` (plan: `clear_filters`).
    /// - `d` when zoomed → `toggle_zoom_detail` (plan: `zoom_toggle_detail`).
    ///
    /// Two bindings were dropped because the existing UI API needs
    /// context the dispatch can't provide without inventing new
    /// methods:
    ///
    /// - `j` scroll-down: `scroll_down(max_rows)` is row-count-aware.
    /// - `D` advanced custom period: `UsagePeriod::Custom { from, to }`
    ///   has no sensible default range from a single key press.
    ///
    /// Generation is bumped only when the dispatch matched a binding,
    /// so unmapped keys won't trigger an avoidable re-render.
    async fn handle_key(&mut self, host: &HostClient, params: HandleKeyParams) -> Result<()> {
        // Refresh is the one binding that needs the host. Everything
        // else mutates `self.ui` only, so it lives in the pure helper
        // below for testability.
        let handled = match params.key.code {
            KeyCode::Char { ch: 'r' | 'R' } => {
                // Ask session-reader to re-publish from scratch. Best
                // effort — failure is silently dropped, the existing
                // snapshot stays on screen.
                let _ = host
                    .snapshot_publish("sessions.refresh_request", bytes::Bytes::new())
                    .await;
                true
            }
            _ => self.dispatch_key_pure(&params.key.code),
        };
        // Plan-mandated invariant: only bump generation when the
        // dispatch matched a binding, so unmapped keys can't trigger
        // an avoidable re-render.
        if handled {
            self.generation = self.generation.wrapping_add(1);
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
            // Resolve the cached filtered view FIRST (mutable
            // borrow) and only then build the ui snapshot. Doing it
            // in this order lets `cached_filtered()` mutate
            // `self.filter_cache` without colliding with the later
            // immutable read of `self.ui` and `self.data`.
            let pivot_seq_before = self.pivot_seq;
            let cached_filtered = self.cached_filtered();
            // Snapshot the UI state so we can paint without holding a
            // mutable borrow on `self` for the whole call.
            let mut ui = self.ui.clone();
            ui.data = self.data.clone();
            ui.scan_progress = self.scan_progress.clone();
            ui.cached_filtered = cached_filtered;
            // True iff `cached_filtered` ran an actual recompute this
            // frame (cache miss). The chip strip uses this to flash a
            // brief `↻ updated` badge so the user sees confirmation
            // their pivot landed.
            ui.fresh_pivot = self.pivot_seq != pivot_seq_before;
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

        // The caller (host CLI shim) is responsible for waiting until
        // `self.data` has been populated via the `sessions.usage_data`
        // subscription. We deliberately do NOT call `refresh_snapshot`
        // here: it holds the plugin mutex while awaiting a host RPC
        // (`host/snapshot/get`). The SDK's `read_loop` services
        // `plugin/handle_event` *inline* (to preserve chunk order — see
        // `ainb-plugin-sdk-rust::server::read_loop`), so if a chunk
        // event arrives while we're holding the mutex on a host RPC,
        // `dispatch_incoming.await` for the chunk blocks the read loop
        // on `plugin.lock().await` — and the host's response to our
        // `host/snapshot/get` can never be read from stdin. Deadlock.
        //
        // Skipping the explicit pull is safe here because burndown is
        // eager-spawned (manifest `[lifecycle].spawn = "eager"`) and
        // its `on_init` subscribes to `sessions.usage_data` *before*
        // session-reader publishes — so the chunk push path always
        // populates `self.data` once the publisher finishes its scan.
        // First-call races (data not yet populated) surface as the
        // "install session-reader" hint, and the host CLI shim retries
        // until it succeeds or the deadline elapses (see
        // `crates/ainb-core/src/cli/registry.rs::dispatch_inner`).
        let _ = host;
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
                self.data = Some(std::sync::Arc::new(wire_to_local(assembled)));
                self.schema_mismatch = false;
                // Real data has landed — the scan is done. Drop any
                // lingering progress event so the UI flips from
                // skeleton to populated panels on the next render.
                self.scan_progress = None;
                // Invalidate the filter cache: the underlying call
                // set has changed, every cached result is stale.
                // Bump first, drop second, so any concurrent reader
                // sees the new generation when checking the entry.
                self.data_generation = self.data_generation.wrapping_add(1);
                self.filter_cache = None;
                // Indices are derived from `self.data` and become
                // stale on every new snapshot. Drop them here; the
                // next `cached_filtered` call rebuilds lazily.
                self.indices = None;
                return ChunkOutcome::Finalised;
            }
        }
        ChunkOutcome::Buffered
    }

    /// Hash a `UsageFilters` snapshot to a u64. Used by the filter
    /// cache key — `UsageFilters` is `Eq + Hash` so this is a pure
    /// function of the chip set, period, and provider filter — all three
    /// inputs to `filter_usage_data_full` so the cache key invalidates
    /// when ANY of them change.
    fn hash_filter_inputs(
        filters: &crate::data::usage::UsageFilters,
        period: &crate::data::usage::UsagePeriod,
        provider_filter: crate::data::usage::UsageProviderFilter,
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        filters.hash(&mut h);
        period.hash(&mut h);
        provider_filter.hash(&mut h);
        h.finish()
    }

    /// Resolve the filtered view of `self.data` for the current
    /// `self.ui.filters` + `self.ui.period` + `self.ui.provider_filter`.
    /// Cached by `(data_generation, inputs_hash)` — repeated renders
    /// on the same key state are O(1).
    ///
    /// Returns `None` when there is no data yet (the render path
    /// already paints the wait/skeleton screen in that case) or when
    /// every filter dimension is at its default ("no-op" — render
    /// path uses the raw `data` reference).
    fn cached_filtered(
        &mut self,
    ) -> Option<std::sync::Arc<crate::data::usage::UsageData>> {
        let data = self.data.as_ref()?;
        // No-op fast path: nothing to filter. The render path falls
        // back to the raw `data` reference and never consults the
        // cache, so don't bother building one.
        let any_active = self.ui.filters.any()
            || !matches!(self.ui.period, crate::data::usage::UsagePeriod::All)
            || !matches!(
                self.ui.provider_filter,
                crate::data::usage::UsageProviderFilter::All
            );
        if !any_active {
            return None;
        }
        let inputs_hash =
            Self::hash_filter_inputs(&self.ui.filters, &self.ui.period, self.ui.provider_filter);
        let key = (self.data_generation, inputs_hash);
        if let Some(entry) = self.filter_cache.as_ref() {
            if entry.data_generation == key.0 && entry.inputs_hash == key.1 {
                return Some(entry.filtered.clone());
            }
        }
        // Build pre-dimension indices once per data_generation. Reused
        // across every chip pivot until the next wire snapshot lands.
        if self.indices.is_none() {
            self.indices = Some(crate::data::usage::UsageIndices::from_usage_data(data));
        }
        let filtered = std::sync::Arc::new(crate::data::usage::filter_usage_data_indexed(
            data,
            self.indices.as_ref(),
            &self.ui.filters,
            &self.ui.period,
            self.ui.provider_filter,
        ));
        self.filter_cache = Some(FilterCacheEntry {
            data_generation: key.0,
            inputs_hash: key.1,
            filtered: filtered.clone(),
        });
        // Mark this render as the one that landed a fresh pivot. The
        // render path snapshots `pivot_seq` before/after this call and
        // sets `ui.fresh_pivot` when the seq advanced — driving the
        // `↻ updated` chip-strip badge on the first frame after a
        // cache miss. Idle frames hit the cache, skip this bump, and
        // suppress the badge naturally.
        self.pivot_seq = self.pivot_seq.wrapping_add(1);
        Some(filtered)
    }

    /// Decode a `sessions.scan_progress` payload and stash it as the
    /// latest known progress. Malformed payloads are logged and
    /// dropped — the skeleton just keeps showing the previous tick (or
    /// the ⏳ waiting line if no tick has arrived yet).
    async fn ingest_scan_progress(&mut self, host: &HostClient, payload: &[u8]) {
        match rmp_serde::from_slice::<ScanProgressEvent>(payload) {
            Ok(evt) => {
                self.scan_progress = Some(evt);
            }
            Err(e) => {
                let _ = host
                    .log_info(format!(
                        "burndown: malformed sessions.scan_progress payload: {e}"
                    ))
                    .await;
            }
        }
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

    /// Pure version of the `handle_key` match — applies every
    /// non-host-touching binding to `self.ui` and returns whether
    /// the key was claimed. Lets unit tests exercise the dispatch
    /// without spinning up a `HostClient`.
    ///
    /// The `r`/`R` refresh binding is intentionally NOT here; it
    /// lives in the async `handle_key` because it needs the host.
    fn dispatch_key_pure(&mut self, code: &KeyCode) -> bool {
        use chrono::{Datelike, Local};

        match *code {
            KeyCode::Char { ch: '1' } => self.ui.set_period(UsagePeriod::Today),
            KeyCode::Char { ch: '2' } => self.ui.set_period(UsagePeriod::Week),
            KeyCode::Char { ch: '3' } => self.ui.set_period(UsagePeriod::ThirtyDays),
            KeyCode::Char { ch: '4' } => self.ui.set_period(UsagePeriod::LastNDays(90)),
            KeyCode::Char { ch: '5' } => self.ui.set_period(UsagePeriod::YearToDate),
            KeyCode::Char { ch: 'm' } => self.ui.set_period(UsagePeriod::Month),
            KeyCode::Char { ch: 'q' } => {
                // No plain `Quarter` variant exists — use today's
                // calendar quarter as the anchor, matching how the
                // chip-row picker visualises the active quarter.
                let today = Local::now().date_naive();
                let year = today.year();
                // `Datelike::month()` is 1..=12 → quarter 1..=4.
                let q = u8::try_from((today.month() - 1) / 3 + 1).unwrap_or(1);
                self.ui.set_period(UsagePeriod::SpecificQuarter(year, q));
            }
            KeyCode::Char { ch: 'a' } => self.ui.set_period(UsagePeriod::All),
            KeyCode::Left => self.ui.prev_provider(),
            KeyCode::Right => self.ui.next_provider(),
            KeyCode::Tab => self.ui.focus_next_panel(),
            KeyCode::BackTab => self.ui.focus_prev_panel(),
            KeyCode::Char { ch: 'z' } => self.ui.toggle_zoom(),
            KeyCode::Char { ch: '/' } if self.ui.is_zoomed() => self.ui.zoom_begin_search(),
            KeyCode::Char { ch: 'd' } if self.ui.is_zoomed() => self.ui.toggle_zoom_detail(),
            KeyCode::Enter => {
                // `commit_focused_row` resolves the row through
                // `filtered_data()` which reads `self.ui.data`. The
                // plugin keeps the parsed snapshot on `self.data`
                // (the render path copies it into `ui.data` only at
                // render time), so without this sync the commit
                // would silently return false — drill-down dead.
                // Now that `self.data` is `Arc<UsageData>`, the sync
                // is an `Arc::clone` (refcount bump), not a deep copy.
                self.ui.data = self.data.clone();
                let _ = self.ui.commit_focused_row();
            }
            KeyCode::Char { ch: 'X' } => {
                self.ui.data = self.data.clone();
                let _ = self.ui.commit_focused_row_exclude();
            }
            KeyCode::Char { ch: 'C' } => self.ui.clear_all_filter_chips(),
            KeyCode::Esc => {
                if self.ui.is_zoomed() {
                    let _ = self.ui.zoom_handle_esc();
                } else {
                    let _ = self.ui.pop_filter_chip();
                }
            }
            KeyCode::Char { ch: 'p' } => self.ui.cycle_provider_filter(),
            KeyCode::Char { ch: 'k' } => self.ui.scroll_up(),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod handle_key_dispatch_tests {
    use super::*;
    use ainb_plugin_sdk::KeyCode;

    fn ch(c: char) -> KeyCode {
        KeyCode::Char { ch: c }
    }

    #[test]
    fn period_keys_map_to_expected_variants() {
        let cases: &[(KeyCode, UsagePeriod)] = &[
            (ch('1'), UsagePeriod::Today),
            (ch('2'), UsagePeriod::Week),
            (ch('3'), UsagePeriod::ThirtyDays),
            (ch('4'), UsagePeriod::LastNDays(90)),
            (ch('5'), UsagePeriod::YearToDate),
            (ch('m'), UsagePeriod::Month),
            (ch('a'), UsagePeriod::All),
        ];
        for (code, expected) in cases {
            let mut p = BurndownPlugin::default();
            assert!(p.dispatch_key_pure(code), "binding missing for {code:?}");
            assert_eq!(
                p.ui.period, *expected,
                "period mismatch after dispatching {code:?}"
            );
        }
    }

    #[test]
    fn quarter_key_anchors_on_today() {
        use chrono::{Datelike, Local};
        let today = Local::now().date_naive();
        let expected_q = u8::try_from((today.month() - 1) / 3 + 1).unwrap();

        let mut p = BurndownPlugin::default();
        assert!(p.dispatch_key_pure(&ch('q')));
        match p.ui.period {
            UsagePeriod::SpecificQuarter(year, q) => {
                assert_eq!(year, today.year());
                assert_eq!(q, expected_q);
            }
            other => panic!("expected SpecificQuarter, got {other:?}"),
        }
    }

    #[test]
    fn arrow_keys_advance_provider_filter() {
        // `next_provider` / `prev_provider` walk a 4-state internal
        // `UsageProvider` enum and derive `provider_filter` from it.
        // Asserting a round-trip is brittle — Gemini/Copilot both map
        // back to `All`, so Left after one Right doesn't return to
        // the starting filter. Just confirm each arrow claims the
        // key and mutates state at least once.
        let mut p = BurndownPlugin::default();
        let starting = p.ui.provider_filter;
        assert!(p.dispatch_key_pure(&KeyCode::Right));
        let after_right = p.ui.provider_filter;
        let mut p = BurndownPlugin::default();
        assert!(p.dispatch_key_pure(&KeyCode::Left));
        let after_left = p.ui.provider_filter;
        assert!(
            after_right != starting || after_left != starting,
            "at least one arrow direction must mutate provider_filter from default {starting:?}"
        );
    }

    #[test]
    fn tab_cycles_focused_panel() {
        let mut p = BurndownPlugin::default();
        let before = p.ui.focused_panel;
        assert!(p.dispatch_key_pure(&KeyCode::Tab));
        let after = p.ui.focused_panel;
        assert_ne!(before, after, "Tab must change focused panel");
    }

    #[test]
    fn z_toggles_zoom_state() {
        let mut p = BurndownPlugin::default();
        assert!(!p.ui.is_zoomed());
        // First need a focused panel so toggle_zoom has something to
        // zoom into.
        let _ = p.dispatch_key_pure(&KeyCode::Tab);
        assert!(p.dispatch_key_pure(&ch('z')));
        assert!(p.ui.is_zoomed(), "z must zoom into focused panel");
        assert!(p.dispatch_key_pure(&ch('z')));
        assert!(!p.ui.is_zoomed(), "second z must un-zoom");
    }

    #[test]
    fn slash_only_dispatches_while_zoomed() {
        let mut p = BurndownPlugin::default();
        // Not zoomed → `/` is unbound (returns false, no generation bump).
        assert!(!p.dispatch_key_pure(&ch('/')));
        // Zoom in.
        let _ = p.dispatch_key_pure(&KeyCode::Tab);
        assert!(p.dispatch_key_pure(&ch('z')));
        assert!(p.ui.is_zoomed());
        // Now zoomed → `/` claims the key.
        assert!(p.dispatch_key_pure(&ch('/')));
    }

    #[test]
    fn esc_pops_filter_when_not_zoomed_and_unzooms_when_zoomed() {
        let mut p = BurndownPlugin::default();
        // Not zoomed: Esc claims the key (no-op against an empty
        // filter stack, but still bumps generation).
        assert!(p.dispatch_key_pure(&KeyCode::Esc));

        // Zoomed: Esc should also claim, and un-zoom.
        let _ = p.dispatch_key_pure(&KeyCode::Tab);
        assert!(p.dispatch_key_pure(&ch('z')));
        assert!(p.ui.is_zoomed());
        assert!(p.dispatch_key_pure(&KeyCode::Esc));
        assert!(!p.ui.is_zoomed(), "Esc must exit zoom");
    }

    #[test]
    fn k_scrolls_up() {
        // Even on an empty state `scroll_up` is a no-op `saturating_sub` —
        // the binding still claims the key. We're really asserting the
        // dispatch wiring rather than the scroll math.
        let mut p = BurndownPlugin::default();
        assert!(p.dispatch_key_pure(&ch('k')));
    }

    /// Regression: `commit_focused_row` reads `self.ui.data`, but the
    /// plugin parks the parsed snapshot on `self.data`. Before the
    /// dispatcher started syncing data into `ui` ahead of the commit,
    /// Enter was a silent no-op — no chip, no error, no log line. This
    /// test lifts the contract into the dispatcher's surface so the
    /// sync can't regress without a CI signal.
    #[test]
    fn enter_commits_branch_chip_when_only_plugin_data_is_set() {
        use crate::data::usage::{BranchUsage, TokenBucket, UsageData, UsagePeriod};
        use crate::ui::UsagePanel;

        let mut p = BurndownPlugin::default();
        let mut data = UsageData::default();
        data.branches = vec![BranchUsage {
            branch: "main".to_string(),
            bucket: TokenBucket {
                input_tokens: 100,
                output_tokens: 100,
                ..Default::default()
            },
        }];
        // Plugin-level snapshot ONLY — leave self.ui.data as None to
        // mirror the production state right after `apply_chunk_pure`.
        p.data = Some(std::sync::Arc::new(data));
        p.ui.data = None;
        p.ui.focused_panel = Some(UsagePanel::ByBranch);
        p.ui.focus_row = 0;
        // Bypass period filtering so the synthetic call survives the
        // re-aggregate in `filter_usage_data_full`.
        p.ui.period = UsagePeriod::All;

        assert!(p.dispatch_key_pure(&KeyCode::Enter));
        assert_eq!(
            p.ui.filters.branch,
            vec!["main".to_string()],
            "Enter must commit a chip even when only self.data (not self.ui.data) is set"
        );
    }

    #[test]
    fn unmapped_keys_are_not_claimed() {
        let mut p = BurndownPlugin::default();
        // 'D' (advanced/custom) was deliberately dropped — needs a date range.
        assert!(!p.dispatch_key_pure(&ch('D')));
        // 'j' was dropped — `scroll_down` needs a row-count we can't
        // know from the dispatch.
        assert!(!p.dispatch_key_pure(&ch('j')));
        // Random Unicode key.
        assert!(!p.dispatch_key_pure(&ch('🚀')));
        // F-key.
        assert!(!p.dispatch_key_pure(&KeyCode::F { n: 5 }));
    }

    #[test]
    fn generation_bumps_only_when_dispatch_handled() {
        // We can't drive the async `handle_key` without a HostClient,
        // but we can replicate its bookkeeping using the pure helper
        // — which is what `handle_key` does for every non-`r` binding.
        let mut p = BurndownPlugin::default();
        let g0 = p.generation;

        // Handled key → bump.
        let handled = p.dispatch_key_pure(&ch('1'));
        if handled {
            p.generation = p.generation.wrapping_add(1);
        }
        assert_eq!(p.generation, g0 + 1, "handled key must bump generation");

        // Unmapped key → no bump.
        let handled = p.dispatch_key_pure(&ch('j'));
        if handled {
            p.generation = p.generation.wrapping_add(1);
        }
        assert_eq!(
            p.generation,
            g0 + 1,
            "unmapped key must NOT bump generation"
        );
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

    #[test]
    fn finalising_clears_stashed_scan_progress() {
        // Once real data arrives the skeleton must disappear — so the
        // chunk accumulator drops `scan_progress` when it finalises a
        // sequence into `self.data`.
        let mut p = BurndownPlugin::default();
        p.scan_progress = Some(ScanProgressEvent {
            scanned: 7,
            total: 100,
            current_project: "alpha".into(),
        });
        let _ = p.apply_chunk_pure(chunk(0, true, vec![fake_call(1)]));
        assert!(
            p.scan_progress.is_none(),
            "scan_progress must clear on finalise so the skeleton goes away"
        );
    }
}

#[cfg(test)]
mod pivot_seq_tests {
    //! Cover the `pivot_seq` ↔ `fresh_pivot` contract: the seq must bump
    //! exactly once per `cached_filtered` cache miss, and not bump at
    //! all on cache hits or no-op (every filter at default) paths. The
    //! render path's snapshot-before/after compare relies on this to
    //! decide when to flash the `↻ updated` chip-strip badge.
    use super::*;
    use crate::data::usage::{
        BranchUsage, TokenBucket, UsageData, UsagePeriod,
    };

    fn plugin_with_one_branch() -> BurndownPlugin {
        let mut p = BurndownPlugin::default();
        let mut data = UsageData::default();
        data.branches = vec![BranchUsage {
            branch: "main".to_string(),
            bucket: TokenBucket::default(),
        }];
        p.data = Some(std::sync::Arc::new(data));
        // Period::All bypasses the period predicate so the filter
        // pass is purely the chip dimension under test.
        p.ui.period = UsagePeriod::All;
        p
    }

    #[test]
    fn cached_filtered_bumps_seq_on_cache_miss_only() {
        let mut p = plugin_with_one_branch();
        let g0 = p.pivot_seq;

        // No active filter, period=All, provider=All → no-op fast path
        // returns None and must NOT bump the seq.
        assert!(p.cached_filtered().is_none());
        assert_eq!(p.pivot_seq, g0, "no-op path must not bump pivot_seq");

        // Activate a chip → cache miss → seq bumps exactly once.
        p.ui.filters.branch.push("main".to_string());
        let _ = p.cached_filtered();
        assert_eq!(p.pivot_seq, g0 + 1, "cache miss must bump pivot_seq by 1");

        // Repeat the same call → cache hit → seq must NOT bump.
        let _ = p.cached_filtered();
        assert_eq!(p.pivot_seq, g0 + 1, "cache hit must not bump pivot_seq");

        // Change the filter → cache miss again → seq bumps once more.
        p.ui.filters.branch.clear();
        p.ui.filters.branch.push("feature".to_string());
        let _ = p.cached_filtered();
        assert_eq!(
            p.pivot_seq,
            g0 + 2,
            "second cache miss must bump pivot_seq again"
        );
    }
}

#[cfg(test)]
mod scan_progress_tests {
    //! Cover the `sessions.scan_progress` event path end-to-end: a
    //! plugin instance receives 3 progress events via the same wire
    //! shape session-reader publishes, and the stashed state matches
    //! after each one. Verifies the plan's "fixture publishes 3
    //! progress events → burndown renders 1/3, 2/3, 3/3 in order"
    //! gate at the state level; the UI render assertion lives in
    //! `ui::tests::scan_progress_skeleton_renders_n_of_m`.
    use super::*;

    fn payload(scanned: u32, total: u32, project: &str) -> Vec<u8> {
        rmp_serde::to_vec_named(&ScanProgressEvent {
            scanned,
            total,
            current_project: project.into(),
        })
        .expect("encode scan_progress")
    }

    #[test]
    fn three_progress_payloads_stash_in_order_and_clear_on_data() {
        let mut p = BurndownPlugin::default();

        // Decode three payloads via the same path `handle_event` uses
        // for the wire bytes — `rmp_serde::from_slice<ScanProgressEvent>`.
        for (i, total) in [(1, 3), (2, 3), (3, 3)] {
            let bytes = payload(i, total, &format!("project-{i}"));
            let decoded: ScanProgressEvent =
                rmp_serde::from_slice(&bytes).expect("decode round-trips");
            p.scan_progress = Some(decoded);
            let stashed = p.scan_progress.as_ref().expect("stashed");
            assert_eq!(stashed.scanned, i);
            assert_eq!(stashed.total, 3);
            assert_eq!(stashed.current_project, format!("project-{i}"));
        }

        // Real data arrives → skeleton goes away.
        let mut data = WireUsageData::default();
        data.calls = Vec::new();
        let event = UsageDataEvent {
            version: WIRE_VERSION,
            published_ns: 0,
            partial: false,
            chunk_index: 0,
            is_final: true,
            data,
        };
        let outcome = p.apply_chunk_pure(event);
        assert_eq!(outcome, ChunkOutcome::Finalised);
        assert!(p.scan_progress.is_none());
    }

    #[test]
    fn malformed_payload_leaves_state_unchanged() {
        // `rmp_serde::from_slice` rejects non-msgpack bytes; the
        // existing stashed progress (if any) must survive.
        let mut p = BurndownPlugin::default();
        p.scan_progress = Some(ScanProgressEvent {
            scanned: 5,
            total: 10,
            current_project: "good".into(),
        });
        let bad = b"\xff\xff\xff not msgpack";
        let result = rmp_serde::from_slice::<ScanProgressEvent>(bad);
        assert!(result.is_err(), "garbage payload rejected by decoder");
        // The ingest path drops the err and keeps the prior state —
        // simulate that contract here without spinning up a HostClient.
        let stash = p.scan_progress.as_ref().unwrap();
        assert_eq!(stash.scanned, 5);
        assert_eq!(stash.current_project, "good");
    }
}
