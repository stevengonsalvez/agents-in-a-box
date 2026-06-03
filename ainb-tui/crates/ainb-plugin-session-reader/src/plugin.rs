//! Session-reader [`Plugin`] implementation.
//!
//! Pure data-plane plugin: no UI, no CLI namespace. On startup
//! ([`Plugin::on_init`]) it scans the four provider directories,
//! aggregates a [`UsageData`] snapshot, and publishes it on the
//! `sessions.usage_data` topic. While running it answers
//! [`SnapshotGet`](`HostClient::snapshot_get`)-style requests by simply
//! republishing the cached snapshot — but since the host already
//! tracks the latest publish per topic, plugins typically just rely on
//! the host's snapshot store. We re-scan when the host pushes a
//! `sessions.refresh_request` event.
//!
//! [`Plugin`]: ainb_plugin_sdk::Plugin
//! [`UsageData`]: ainb_plugin_types_sessions::UsageData

use ainb_plugin_sdk::{
    HandleEventParams, HostClient, InitContext, Plugin, RenderParams, Result, SdkError, WireBuffer,
};
use ainb_plugin_types_sessions::{ScanProgressEvent, UsageData, UsageDataEvent, WIRE_VERSION};
use async_trait::async_trait;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::scanner::{self, ProviderRoots};

/// Per-chunk msgpack byte budget. Each chunk's encoded msgpack stays
/// below this size so the post-base64 + JSON-envelope frame fits
/// comfortably under the host framer's 16 MiB body cap (base64
/// inflates ~33%; JSON wrapping adds a few hundred bytes).
///
/// Real `$HOME` data is wildly non-uniform — some chunks of 4000 calls
/// land at ~600 KiB, others at ~12 MiB because of long bash-command
/// strings or user_message bodies. A fixed call-count budget therefore
/// can't guarantee staying under the wire cap. [`chunk_usage_data`]
/// re-encodes the candidate chunk after each call appended and cuts
/// as soon as the actual msgpack size crosses this threshold.
///
/// Set to 2 MiB → ~2.7 MiB after base64 → ~2.7 MiB JSON frame, well
/// under the 16 MiB framer cap.
const CHUNK_TARGET_BYTES: usize = 2 * 1024 * 1024;

/// Topic the plugin publishes the aggregated snapshot on. Burndown
/// (the consumer in Phase 7c-burndown) subscribes to this topic.
pub const TOPIC_USAGE_DATA: &str = "sessions.usage_data";

/// Topic any consumer can publish to to force a re-scan + re-publish.
pub const TOPIC_REFRESH_REQUEST: &str = "sessions.refresh_request";

/// Topic any consumer can publish to to wipe the per-file parse cache
/// and re-publish from scratch. Distinct from `sessions.refresh_request`
/// because the latter is cache-aware (most files short-circuit on
/// `(mtime, size)` match) — flush-cache is the escape hatch for
/// suspected cache corruption or behavioural drift where the user
/// wants to know the snapshot was built from scratch.
pub const TOPIC_FLUSH_CACHE_REQUEST: &str = "sessions.flush_cache_request";

/// Topic the plugin publishes per-file scan progress on. Burndown
/// subscribes here to render the skeleton/`Scanning sessions…` line
/// while the cold scan is in flight.
pub const TOPIC_SCAN_PROGRESS: &str = "sessions.scan_progress";

mod manifest_text {
    pub const TOML: &str = include_str!("../manifest.toml");
}

/// Plugin state.
///
/// Holds the most recently published [`UsageDataEvent`] in memory so a
/// follow-up render or refresh can republish without re-running the
/// scan if no source files changed (rescan is still cheap on the
/// freshness path — we always rescan on `sessions.refresh_request`).
///
/// `cache` is opened lazily on the first publish so a host that grants
/// the plugin without `write_plugin_data` (or where the DB path can't
/// be resolved) still functions — every scan just runs uncached.
pub struct SessionReader {
    last_event: Option<UsageDataEvent>,
    roots: ProviderRoots,
    #[cfg(not(target_arch = "wasm32"))]
    cache: Option<crate::cache::UsageCache>,
    /// Set to `true` once we've attempted (and possibly failed) to open
    /// the cache so we don't retry on every publish.
    #[cfg(not(target_arch = "wasm32"))]
    cache_init: bool,
}

impl SessionReader {
    /// Build a fresh reader with the canonical default provider roots.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_event: None,
            roots: ProviderRoots::defaults(),
            #[cfg(not(target_arch = "wasm32"))]
            cache: None,
            #[cfg(not(target_arch = "wasm32"))]
            cache_init: false,
        }
    }

    /// Construct with explicit roots — used by unit tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_roots(roots: ProviderRoots) -> Self {
        Self {
            last_event: None,
            roots,
            #[cfg(not(target_arch = "wasm32"))]
            cache: None,
            #[cfg(not(target_arch = "wasm32"))]
            cache_init: false,
        }
    }

    /// Wipe every cached parse row and force the next scan to re-parse
    /// every file from disk.
    ///
    /// Two-tier recovery: first try the fast in-place wipe
    /// (`UsageCache::clear()` runs `DELETE FROM file_cache` + `VACUUM`,
    /// preserving the schema row so the next open doesn't migrate). If
    /// that fails — the user is likely pressing `F` precisely because
    /// they suspect corruption, so SQL clear may fail too — drop the
    /// cache handle, `rm -f` the file, and let the next
    /// [`Self::ensure_cache`] re-create it from scratch. This way the
    /// rescan that runs immediately afterwards starts cold-cache
    /// regardless of how broken the previous cache was.
    #[cfg(not(target_arch = "wasm32"))]
    async fn flush_cache(&mut self, host: &HostClient) {
        self.ensure_cache();
        if let Some(cache) = self.cache.as_mut() {
            match cache.clear() {
                Ok(()) => {
                    let _ = host.log_info("flush_cache: persistent cache wiped via clear()").await;
                    return;
                }
                Err(err) => {
                    let _ = host
                        .log_info(format!(
                            "flush_cache: clear() failed: {err}; falling back to file delete"
                        ))
                        .await;
                }
            }
        }
        // Either no cache was open, or clear() failed. Drop the handle
        // (closes the sqlite connection if any) and remove the file —
        // the next ensure_cache rebuilds.
        self.cache = None;
        self.cache_init = false;
        if let Some(path) = crate::cache::default_db_path() {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    let _ = host
                        .log_info(format!(
                            "flush_cache: removed cache file at {}",
                            path.display()
                        ))
                        .await;
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    let _ = host.log_info("flush_cache: cache file already absent").await;
                }
                Err(err) => {
                    let _ = host
                        .log_info(format!(
                            "flush_cache: remove cache file failed: {err}; \
                            next rescan still runs (but cache may stay stale)"
                        ))
                        .await;
                }
            }
        }
    }

    /// Open the cache on first use, swallowing any error so the scan
    /// still runs cache-less. Subsequent calls are a no-op.
    #[cfg(not(target_arch = "wasm32"))]
    fn ensure_cache(&mut self) {
        if self.cache_init {
            return;
        }
        self.cache_init = true;
        let Some(path) = crate::cache::default_db_path() else {
            tracing::warn!(
                "session-reader: cache path unresolved (AINB_HOME / XDG_DATA_HOME / HOME all unset); scanning uncached"
            );
            return;
        };
        match crate::cache::UsageCache::open(&path) {
            Ok(cache) => {
                tracing::debug!(path = %path.display(), "session-reader cache opened");
                self.cache = Some(cache);
            }
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "session-reader: cache open failed; scanning uncached"
                );
            }
        }
    }

    /// Build a single-chunk `UsageDataEvent` from a fresh scan. Used by
    /// unit tests only — the real publish path goes through
    /// [`Self::publish`] which chunks for the wire budget.
    // TODO(fix/runtime-eager-spawn): keep until chunked-publish test
    // matrix lands; not exercised by the real publish path.
    #[cfg(test)]
    fn build_event(&self) -> UsageDataEvent {
        let data = scanner::scan(&self.roots);
        UsageDataEvent {
            version: WIRE_VERSION,
            published_ns: now_ns(),
            partial: false,
            chunk_index: 0,
            is_final: true,
            data,
        }
    }

    /// Cache-aware scan helper used by [`Self::publish`]. Lazily opens
    /// the on-disk cache (best-effort) and threads it through the
    /// per-file parse path; on wasm32 the cache module is absent so we
    /// fall through to the uncached `scanner::scan`.
    ///
    /// Cache-less + no-progress fallback used by tests and the wasm32
    /// build. Production publishes go through [`Self::scan_streaming`]
    /// which threads progress events to the host.
    fn scan_now(&mut self) -> ainb_plugin_types_sessions::UsageData {
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.ensure_cache();
            scanner::scan_with_cache(&self.roots, &mut self.cache)
        }
        #[cfg(target_arch = "wasm32")]
        {
            scanner::scan(&self.roots)
        }
    }

    /// Run the scan on a blocking task and publish progress events to
    /// `host` as they arrive. Returns the assembled `UsageData` once
    /// the scan task completes.
    ///
    /// The blocking task owns the cache for the duration of the scan
    /// (moved out of `self.cache`), then hands it back so the plugin
    /// can reuse it on the next publish. Progress events flow over a
    /// `tokio::sync::mpsc::unbounded` channel: the rate-limit lives in
    /// the [`scanner::ProgressReporter`] inside the blocking task, so
    /// the async drain loop never sees more than ~10 events/s.
    #[cfg(not(target_arch = "wasm32"))]
    async fn scan_streaming(&mut self, host: &HostClient) -> ainb_plugin_types_sessions::UsageData {
        self.ensure_cache();
        let roots = self.roots.clone();
        let cache = self.cache.take();

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ScanProgressEvent>();

        let scan_handle = tokio::task::spawn_blocking(move || {
            let mut cache_opt = cache;
            let mut reporter = scanner::ProgressReporter::new(move |evt| {
                // Send is unbounded + non-blocking; failures only happen
                // when the receiver has been dropped (impossible here —
                // we hold `rx` until `scan_handle` resolves).
                let _ = tx.send(evt);
            });
            let data = scanner::scan_with_cache_and_progress(&roots, &mut cache_opt, &mut reporter);
            (data, cache_opt)
        });

        // Drain progress events. `rx.recv()` returns `None` once the
        // scan task drops its `Sender` (i.e. when the closure +
        // reporter are dropped at the end of the blocking work).
        while let Some(evt) = rx.recv().await {
            match rmp_serde::to_vec_named(&evt) {
                Ok(bytes) => {
                    if let Err(err) = host.snapshot_publish(TOPIC_SCAN_PROGRESS, bytes).await {
                        let _ = host
                            .log_info(format!(
                                "session-reader: scan_progress publish failed: {err}"
                            ))
                            .await;
                    }
                }
                Err(err) => {
                    let _ = host
                        .log_info(format!(
                            "session-reader: scan_progress encode failed: {err}"
                        ))
                        .await;
                }
            }
        }

        match scan_handle.await {
            Ok((data, cache_opt)) => {
                self.cache = cache_opt;
                data
            }
            Err(err) => {
                let _ = host
                    .log_info(format!(
                        "session-reader: scan task join error: {err}; \
                        falling back to in-line scan"
                    ))
                    .await;
                self.scan_now()
            }
        }
    }

    /// Encode + publish a single chunk. Returns the encoded byte size
    /// so callers can log/audit. Encoding failures bubble up; the
    /// caller stops the publish sequence on first error.
    async fn publish_chunk(host: &HostClient, event: &UsageDataEvent) -> Result<usize> {
        let bytes = rmp_serde::to_vec_named(event)
            .map_err(|e| SdkError::plugin(format!("encode UsageDataEvent: {e}")))?;
        let len = bytes.len();
        host.snapshot_publish(TOPIC_USAGE_DATA, bytes).await?;
        Ok(len)
    }

    /// Scan once, then publish the snapshot — chunking large `calls`
    /// vecs into N envelopes whose encoded msgpack stays under
    /// [`CHUNK_TARGET_BYTES`] so each JSON-RPC body fits the host
    /// framer's 16 MiB cap. The first chunk carries the full
    /// aggregates plus the first slice of calls; subsequent chunks
    /// carry only the remaining calls. The last chunk is flagged
    /// `is_final = true`.
    async fn publish(&mut self, host: &HostClient) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        let data = self.scan_streaming(host).await;
        #[cfg(target_arch = "wasm32")]
        let data = self.scan_now();
        let published_ns = now_ns();
        let chunks = chunk_usage_data(data, published_ns, false, CHUNK_TARGET_BYTES);
        let total_chunks = chunks.len();
        let total_calls: usize = chunks.iter().map(|c| c.data.calls.len()).sum();

        let _ = host
            .log_info(format!(
                "publish: {} calls in {} chunk(s), target {} bytes/chunk",
                total_calls, total_chunks, CHUNK_TARGET_BYTES
            ))
            .await;

        for event in chunks {
            let chunk_index = event.chunk_index;
            let is_final = event.is_final;
            let len = Self::publish_chunk(host, &event).await?;
            let _ = host
                .log_info(format!(
                    "publish: chunk {}/{} ({} bytes, is_final={})",
                    chunk_index as usize + 1,
                    total_chunks,
                    len,
                    is_final
                ))
                .await;
            if is_final {
                self.last_event = Some(event);
            }
        }
        Ok(())
    }
}

/// How many calls to push into a chunk between size probes. Tuned
/// to balance encoder cost (O(n^2) worst case if STEP=1) against
/// chunk granularity (larger STEP = fatter chunks because we only
/// detect overshoot at probe time). With STEP=64 and a 2 MiB target,
/// the chunker pays at most `target/avg_call_bytes` encodes per
/// chunk — a few dozen for real data.
const CHUNKER_PROBE_STEP: usize = 64;

/// Identifier for which tail-chunkable vec a chunker push came from.
/// Lets the spill-back path return the just-popped item to the right
/// queue when an over-target probe forces a cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailQueue {
    Calls,
    Sessions,
    ShellCommands,
}

/// Split a `UsageData` snapshot into one or more `UsageDataEvent`
/// chunks. Pure; unit-tested directly.
///
/// **Wire model (WIRE_VERSION 4):**
/// - Chunk 0 carries the bounded aggregates only (`daily`, `weekly`,
///   `projects`, `grand_total`, `models`, `activities`, `tools`,
///   `mcp_servers`, `branches`, `model_project_counts`). `calls`,
///   `sessions`, and `shell_commands` are emptied out and tail-chunked.
/// - Chunks 1..N each carry slices of `calls` + `sessions` +
///   `shell_commands`, sized so the encoded msgpack stays under
///   `target_bytes`. Other fields are `UsageData::default()`. The
///   consumer accumulator `extend`s the three tail vecs as chunks
///   arrive.
/// - The last chunk has `is_final = true`. A snapshot whose tail vecs
///   are all empty ships as a single chunk-0 with `is_final = true`.
/// - A single huge tail item that on its own exceeds `target_bytes`
///   still ships (otherwise the loop would stall); the host framer's
///   16 MiB cap still leaves an order-of-magnitude headroom.
///
/// **Why tail-chunk these three.** They're the only fields whose size
/// grows with raw call volume rather than with bounded entity counts
/// (distinct projects, days, models, branches). At ~128k calls /
/// ~4k sessions / many distinct bash commands a v3 chunk 0 encoded
/// to ~17 MiB and tripped the 16 MiB framer cap — see WIRE_VERSION
/// docs for the incident report.
///
/// Re-encodes the candidate chunk after every [`CHUNKER_PROBE_STEP`]
/// items appended; cuts when the encoded msgpack size crosses
/// `target_bytes`. Pop priority is `calls` → `sessions` →
/// `shell_commands`, draining the biggest queue first so the chunk
/// count for call-dominated snapshots matches the v3 distribution.
pub(crate) fn chunk_usage_data(
    mut data: UsageData,
    published_ns: u64,
    partial: bool,
    target_bytes: usize,
) -> Vec<UsageDataEvent> {
    assert!(target_bytes >= 1024, "target_bytes too small");

    // Detach the tail-chunkable vecs from `data` so chunk 0 carries
    // only the bounded aggregates. Chunks 1..N drain from these three
    // queues until every one is empty.
    let mut calls_q: std::collections::VecDeque<_> =
        std::mem::take(&mut data.calls).into_iter().collect();
    let mut sessions_q: std::collections::VecDeque<_> =
        std::mem::take(&mut data.sessions).into_iter().collect();
    let mut shell_q: std::collections::VecDeque<_> =
        std::mem::take(&mut data.shell_commands).into_iter().collect();

    let mut out: Vec<UsageDataEvent> = Vec::with_capacity(1);

    // Chunk 0: bounded aggregates only, empty tail vecs.
    out.push(UsageDataEvent {
        version: WIRE_VERSION,
        published_ns,
        partial,
        chunk_index: 0,
        is_final: calls_q.is_empty() && sessions_q.is_empty() && shell_q.is_empty(),
        data,
    });

    let mut chunk_index: u32 = 1;
    while !(calls_q.is_empty() && sessions_q.is_empty() && shell_q.is_empty()) {
        let mut chunk_data = UsageData::default();
        let mut since_probe = 0usize;

        loop {
            // Pop priority: calls (largest) → sessions → shell_commands.
            if let Some(c) = calls_q.pop_front() {
                chunk_data.calls.push(c);
            } else if let Some(s) = sessions_q.pop_front() {
                chunk_data.sessions.push(s);
            } else if let Some(s) = shell_q.pop_front() {
                chunk_data.shell_commands.push(s);
            } else {
                break; // every queue drained
            }
            since_probe += 1;

            let all_empty = calls_q.is_empty() && sessions_q.is_empty() && shell_q.is_empty();
            // Probe when we've accumulated STEP items, or when every
            // queue just drained (so we don't miss tiny final overshoots).
            if since_probe < CHUNKER_PROBE_STEP && !all_empty {
                continue;
            }
            since_probe = 0;

            let probe_event = UsageDataEvent {
                version: WIRE_VERSION,
                published_ns,
                partial,
                chunk_index,
                is_final: all_empty,
                data: chunk_data.clone(),
            };
            let probe_size =
                rmp_serde::to_vec_named(&probe_event).map(|b| b.len()).unwrap_or(target_bytes);

            // Cut on probe overshoot, but only if the chunk has >1 item
            // total — single-item chunks always ship so a giant outlier
            // (e.g. one 5 MiB user_message) can't stall the loop.
            //
            // Spill in a loop, not just once: STEP=64 means the chunk
            // can land many MiB over budget when one of those 64 items
            // is a 100+ KiB outlier. Single-spill would still ship a
            // chunk 50× over target. Each iteration picks the largest
            // tail vec as the spill source (best chance of cheaply
            // shrinking the chunk) and re-probes; keep spilling until
            // we're back under target or only 1 item remains.
            let chunk_item_count = chunk_data.calls.len()
                + chunk_data.sessions.len()
                + chunk_data.shell_commands.len();
            if probe_size >= target_bytes && chunk_item_count > 1 {
                loop {
                    // Choose the spill source: whichever tail vec
                    // currently has the most items. Re-evaluate every
                    // iteration because vecs shrink.
                    let from = if chunk_data.calls.len() >= chunk_data.sessions.len()
                        && chunk_data.calls.len() >= chunk_data.shell_commands.len()
                        && !chunk_data.calls.is_empty()
                    {
                        TailQueue::Calls
                    } else if chunk_data.sessions.len() >= chunk_data.shell_commands.len()
                        && !chunk_data.sessions.is_empty()
                    {
                        TailQueue::Sessions
                    } else if !chunk_data.shell_commands.is_empty() {
                        TailQueue::ShellCommands
                    } else {
                        break; // nothing left to spill
                    };

                    match from {
                        TailQueue::Calls => {
                            if let Some(spill) = chunk_data.calls.pop() {
                                calls_q.push_front(spill);
                            }
                        }
                        TailQueue::Sessions => {
                            if let Some(spill) = chunk_data.sessions.pop() {
                                sessions_q.push_front(spill);
                            }
                        }
                        TailQueue::ShellCommands => {
                            if let Some(spill) = chunk_data.shell_commands.pop() {
                                shell_q.push_front(spill);
                            }
                        }
                    }

                    let remaining_items = chunk_data.calls.len()
                        + chunk_data.sessions.len()
                        + chunk_data.shell_commands.len();
                    if remaining_items <= 1 {
                        // Single-item chunks always ship (giant-outlier
                        // protection). Even if still oversize, exit
                        // here rather than spill the lone survivor.
                        break;
                    }

                    // Re-probe. Cost is O(chunk_data_encoded_size) per
                    // probe — at ~1 MiB target and 100 KiB outliers,
                    // worst case ~10 probes per cut, totally affordable
                    // relative to the I/O cost of the actual publish.
                    let probe = UsageDataEvent {
                        version: WIRE_VERSION,
                        published_ns,
                        partial,
                        chunk_index,
                        is_final: calls_q.is_empty() && sessions_q.is_empty() && shell_q.is_empty(),
                        data: chunk_data.clone(),
                    };
                    let probe_after_spill =
                        rmp_serde::to_vec_named(&probe).map(|b| b.len()).unwrap_or(target_bytes);
                    if probe_after_spill < target_bytes {
                        break;
                    }
                }
                break;
            }
        }

        let is_final = calls_q.is_empty() && sessions_q.is_empty() && shell_q.is_empty();
        out.push(UsageDataEvent {
            version: WIRE_VERSION,
            published_ns,
            partial,
            chunk_index,
            is_final,
            data: chunk_data,
        });
        chunk_index = chunk_index.saturating_add(1);
    }

    out
}

impl Default for SessionReader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SessionReader {
    fn manifest(&self) -> &'static str {
        manifest_text::TOML
    }

    async fn on_init(&mut self, host: &HostClient, _ctx: InitContext<'_>) -> Result<()> {
        // Subscribe to refresh-requests so any consumer (burndown,
        // tests) can force a rescan. We deliberately *do not* publish
        // on startup — consumers ask via the refresh topic so they're
        // guaranteed to be subscribed before chunk 0 hits the wire.
        // A startup publish races the consumer's own `on_init` and
        // can arrive while burndown is still subscribing, losing
        // chunks of the sequence (Bug A in fix/runtime-eager-spawn).
        let _ = host.log_info("on_init: subscribing to sessions.refresh_request").await;
        host.snapshot_subscribe(TOPIC_REFRESH_REQUEST).await?;
        // Also subscribe to flush-cache so burndown's `F` key can wipe
        // the persistent cache and republish from scratch.
        host.snapshot_subscribe(TOPIC_FLUSH_CACHE_REQUEST).await?;
        let _ = host.log_info("on_init: subscribed, idle until refresh_request").await;
        Ok(())
    }

    async fn render(&mut self, _host: &HostClient, _params: RenderParams) -> Result<WireBuffer> {
        // No UI surface — return an empty 0×0 buffer. The host's render
        // path tolerates empty buffers from headless plugins.
        Ok(WireBuffer::new(0, 0))
    }

    async fn handle_event(&mut self, host: &HostClient, params: HandleEventParams) -> Result<()> {
        if params.topic == TOPIC_REFRESH_REQUEST {
            tracing::info!("session-reader: refresh requested — rescanning");
            return self.publish(host).await;
        }
        if params.topic == TOPIC_FLUSH_CACHE_REQUEST {
            tracing::info!("session-reader: flush_cache requested — wiping cache + rescanning");
            self.flush_cache(host).await;
            return self.publish(host).await;
        }
        tracing::debug!(topic = %params.topic, "session-reader: ignoring unrelated event");
        Ok(())
    }
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_toml_parses_as_abi_v2() {
        let m: ainb_plugin_sdk::Manifest =
            toml::from_str(manifest_text::TOML).expect("manifest TOML parses");
        assert_eq!(m.plugin.name, "session-reader");
        assert_eq!(m.plugin.abi_version, 2);
        assert!(
            m.provides.snapshots.iter().any(|t| t == TOPIC_USAGE_DATA),
            "expected manifest to declare publishing topic"
        );
        assert!(
            m.subscribes.snapshots.iter().any(|t| t == TOPIC_REFRESH_REQUEST),
            "expected manifest to subscribe to refresh topic"
        );
    }

    #[test]
    fn build_event_has_correct_wire_version_and_partial_flag() {
        let plugin = SessionReader::with_roots(ProviderRoots::default());
        let event = plugin.build_event();
        assert_eq!(event.version, WIRE_VERSION);
        assert!(!event.partial);
        assert_eq!(event.chunk_index, 0);
        assert!(event.is_final);
    }

    use ainb_plugin_types_sessions::{
        ActivityCategory, ActivityUsage, NamedUsage, ProjectUsage, Provider, ProviderCall,
        SessionUsage, TokenBucket,
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

    fn fake_data(n_calls: usize) -> UsageData {
        let mut d = UsageData::default();
        d.calls = (0..n_calls).map(|i| fake_call(i as u64)).collect();
        // Plant a couple of aggregate fields to verify chunk 0 keeps
        // them and follow-on chunks drop them.
        d.grand_total = TokenBucket {
            input_tokens: n_calls as u64,
            output_tokens: n_calls as u64,
            call_count: n_calls,
            ..TokenBucket::default()
        };
        d.projects = vec![ProjectUsage {
            name: "p".into(),
            path: "/tmp".into(),
            bucket: d.grand_total,
            repo: None,
        }];
        d.activities = vec![ActivityUsage {
            category: ActivityCategory::Code,
            bucket: d.grand_total,
            turns: n_calls,
            retries: 0,
            edit_turns: 0,
            one_shot_turns: n_calls,
        }];
        d
    }

    /// Sum the lengths of every tail vec across every chunk —
    /// callers compare to the pre-chunk totals to assert no-loss.
    fn tail_totals(chunks: &[UsageDataEvent]) -> (usize, usize, usize) {
        let calls: usize = chunks.iter().map(|c| c.data.calls.len()).sum();
        let sessions: usize = chunks.iter().map(|c| c.data.sessions.len()).sum();
        let shells: usize = chunks.iter().map(|c| c.data.shell_commands.len()).sum();
        (calls, sessions, shells)
    }

    #[test]
    fn chunk_zero_carries_aggregates_calls_ride_follow_on() {
        // 10 small calls + bounded aggregates. v4 model: chunk 0
        // carries the aggregates (and is_final=false because there's
        // tail data to ship), chunk 1 carries the calls.
        let data = fake_data(10);
        let chunks = chunk_usage_data(data, 42, false, 4 * 1024 * 1024);
        assert_eq!(chunks.len(), 2, "v4: chunk0 + 1 tail chunk for 10 calls");

        // Chunk 0: aggregates, no tail items.
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(!chunks[0].is_final);
        assert!(
            chunks[0].data.calls.is_empty(),
            "chunk 0 has no calls in v4"
        );
        assert!(chunks[0].data.sessions.is_empty());
        assert!(chunks[0].data.shell_commands.is_empty());
        assert_eq!(chunks[0].data.projects.len(), 1, "aggregates ride chunk 0");
        assert_eq!(chunks[0].data.activities.len(), 1);
        assert_eq!(chunks[0].published_ns, 42);

        // Chunk 1: the calls.
        assert_eq!(chunks[1].chunk_index, 1);
        assert!(chunks[1].is_final);
        assert_eq!(chunks[1].data.calls.len(), 10);
        assert!(
            chunks[1].data.projects.is_empty(),
            "no aggregates in tail chunks"
        );
    }

    #[test]
    fn chunk_single_when_every_tail_vec_empty() {
        // Aggregates only — no calls, no sessions, no shell_commands.
        // Single chunk-0 with is_final=true.
        let data = fake_data(0);
        let chunks = chunk_usage_data(data, 0, false, 4 * 1024 * 1024);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].is_final);
        assert!(chunks[0].data.calls.is_empty());
        assert_eq!(chunks[0].chunk_index, 0);
    }

    #[test]
    fn chunk_splits_when_calls_exceed_byte_budget() {
        // 40 fat calls with ~5 KiB user_message each. With a
        // 128 KiB budget that forces multiple chunks.
        let mut data = fake_data(0);
        data.calls = (0..40)
            .map(|i| {
                let mut c = fake_call(i);
                c.user_message = "x".repeat(5 * 1024);
                c
            })
            .collect();
        data.projects = vec![ProjectUsage {
            name: "p".into(),
            path: "/tmp".into(),
            bucket: TokenBucket::default(),
            repo: None,
        }];
        data.activities = vec![ActivityUsage {
            category: ActivityCategory::Code,
            bucket: TokenBucket::default(),
            turns: 40,
            retries: 0,
            edit_turns: 0,
            one_shot_turns: 40,
        }];

        let chunks = chunk_usage_data(data, 0, false, 128 * 1024);
        assert!(chunks.len() > 1, "expected >1 chunks, got {}", chunks.len());
        let (total_calls, _, _) = tail_totals(&chunks);
        assert_eq!(total_calls, 40, "no calls lost across chunks");

        // Chunk 0 carries aggregates and no tail items.
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(!chunks[0].is_final);
        assert_eq!(chunks[0].data.projects.len(), 1);
        assert_eq!(chunks[0].data.activities.len(), 1);
        assert!(
            chunks[0].data.calls.is_empty(),
            "v4: tails live in follow-on chunks"
        );

        // Chunks 1+ carry only tail vecs (no aggregates).
        assert!(chunks[1].data.projects.is_empty());
        assert!(chunks[1].data.activities.is_empty());
        assert_eq!(chunks[1].data.grand_total, TokenBucket::default());

        // Last chunk is is_final=true; chunk_index increments contiguously.
        let last = chunks.last().unwrap();
        assert!(last.is_final);
        assert_eq!(last.chunk_index as usize, chunks.len() - 1);
    }

    #[test]
    fn chunk_preserves_call_order_and_no_loss() {
        // Force multi-chunk via fat user_message + tight budget.
        let mut data = fake_data(0);
        data.calls = (0..9)
            .map(|i| {
                let mut c = fake_call(i);
                c.user_message = "x".repeat(20 * 1024);
                c
            })
            .collect();
        let chunks = chunk_usage_data(data, 0, false, 64 * 1024);
        assert!(chunks.len() > 1);
        let mut ids = Vec::new();
        for c in &chunks {
            for call in &c.data.calls {
                ids.push(call.id);
            }
        }
        assert_eq!(ids, (0..9).collect::<Vec<_>>());
    }

    #[test]
    fn chunk_with_single_giant_call_still_progresses() {
        // Even a single estimated-over-budget call must ship —
        // otherwise the chunker would loop forever waiting for room.
        // v4: chunk 0 (empty aggregates) + chunk 1 (the giant call).
        let mut data = fake_data(0);
        let mut huge = fake_call(1);
        huge.user_message = "x".repeat(2048);
        data.calls = vec![huge];
        let chunks = chunk_usage_data(data, 0, false, 1024);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(!chunks[0].is_final);
        assert_eq!(chunks[1].chunk_index, 1);
        assert!(chunks[1].is_final);
        assert_eq!(chunks[1].data.calls.len(), 1);
    }

    #[test]
    fn chunk_distributes_sessions_alongside_calls() {
        // Mixed-tail snapshot: 5 calls + 5 sessions. Single chunk
        // should fit both at 1 MiB budget.
        let mut data = fake_data(0);
        data.calls = (0..5).map(|i| fake_call(i)).collect();
        data.sessions = (0..5)
            .map(|i| SessionUsage {
                provider: Provider::Claude,
                project: format!("proj-{i}"),
                session_id: format!("sess-{i}"),
                first_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
                last_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
                bucket: TokenBucket::default(),
            })
            .collect();

        let chunks = chunk_usage_data(data, 0, false, 1024 * 1024);
        let (calls, sessions, shells) = tail_totals(&chunks);
        assert_eq!(calls, 5, "all calls present");
        assert_eq!(sessions, 5, "all sessions present");
        assert_eq!(shells, 0);
    }

    #[test]
    fn chunk_distributes_huge_sessions_vec_across_multiple_chunks() {
        // Regression: at 128k calls / ~4k sessions / many shell commands,
        // v3 chunk 0 encoded to 17 MiB and tripped the 16 MiB framer
        // cap. v4 must keep every chunk under the configured target.
        //
        // Simulate "sessions-heavy" by inflating the sessions vec with
        // long string fields so the encoded msgpack exceeds the budget.
        let mut data = fake_data(0);
        data.sessions = (0..200)
            .map(|i| SessionUsage {
                provider: Provider::Claude,
                project: format!("project-with-a-fairly-long-name-{i:04}"),
                session_id: format!("session-id-uuid-style-{i:020}"),
                first_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
                last_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
                bucket: TokenBucket::default(),
            })
            .collect();

        // 4 KiB budget — much smaller than the sessions vec total to
        // force multi-chunk distribution.
        let chunks = chunk_usage_data(data, 0, false, 4 * 1024);
        assert!(
            chunks.len() > 1,
            "expected sessions to span multiple chunks, got {}",
            chunks.len()
        );
        let (_, total_sessions, _) = tail_totals(&chunks);
        assert_eq!(total_sessions, 200, "no sessions lost");

        // Last chunk is is_final.
        assert!(chunks.last().unwrap().is_final);
    }

    #[test]
    fn chunk_distributes_huge_shell_commands_vec_across_multiple_chunks() {
        // Same shape as sessions — shell_commands can carry many
        // long bash strings, easily 5-10 MiB in real $HOME data.
        let mut data = fake_data(0);
        data.shell_commands = (0..500)
            .map(|i| NamedUsage {
                name: format!(
                    "git log --oneline --all --decorate --since=\"30 days ago\" --grep=fix_{i:05}"
                ),
                calls: 1,
            })
            .collect();

        let chunks = chunk_usage_data(data, 0, false, 4 * 1024);
        assert!(chunks.len() > 1);
        let (_, _, total_shells) = tail_totals(&chunks);
        assert_eq!(total_shells, 500, "no shell_commands lost");
        assert!(chunks.last().unwrap().is_final);
    }

    #[test]
    fn chunk_pop_priority_drains_calls_before_sessions() {
        // With a budget that fits the first few of each kind, calls
        // should appear in chunk 1 first because the pop order is
        // calls → sessions → shell_commands.
        let mut data = fake_data(0);
        data.calls = (0..3).map(|i| fake_call(i)).collect();
        data.sessions = (0..3)
            .map(|i| SessionUsage {
                provider: Provider::Claude,
                project: "p".into(),
                session_id: format!("s-{i}"),
                first_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
                last_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
                bucket: TokenBucket::default(),
            })
            .collect();

        let chunks = chunk_usage_data(data, 0, false, 1024 * 1024);
        // Big budget — everything fits in one tail chunk. Order check:
        // calls must be drained into chunk 1 before sessions.
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[1].data.calls.len(), 3);
        assert_eq!(chunks[1].data.sessions.len(), 3);
    }

    #[test]
    fn chunk_multispill_keeps_each_chunk_under_target_with_outlier_calls() {
        // Regression for the multi-spill fix: when the probe step
        // (STEP=64) accumulates a chunk that's many MiB over budget
        // because one or more items are outliers, a single-spill cut
        // would still ship the chunk way over. Multi-spill must drain
        // back until under target (or 1 item remains).
        //
        // Build 128 fat calls (~50 KiB each). With STEP=64 and a
        // 1 MiB target, the first probe lands at ~3.2 MiB. Without
        // multi-spill, the chunk would ship at ~3.15 MiB. With
        // multi-spill, each chunk must encode under 1 MiB.
        let mut data = fake_data(0);
        data.calls = (0..128)
            .map(|i| {
                let mut c = fake_call(i);
                c.user_message = "x".repeat(50 * 1024);
                c
            })
            .collect();

        let target = 1024 * 1024; // 1 MiB
        let chunks = chunk_usage_data(data, 0, false, target);

        // Verify each chunk (except possibly chunk 0 which carries
        // only bounded aggregates, well under target, and chunks
        // containing a single mandatory item) encodes under target.
        for (i, chunk) in chunks.iter().enumerate() {
            let size = rmp_serde::to_vec_named(chunk).unwrap().len();
            let item_count = chunk.data.calls.len()
                + chunk.data.sessions.len()
                + chunk.data.shell_commands.len();
            // Single-item chunks always ship even if oversize (giant-
            // outlier protection). Otherwise must fit.
            if item_count > 1 {
                assert!(
                    size < target,
                    "chunk {i} has {item_count} items at {size} bytes \
                    (target {target}) — multi-spill should have kept \
                    this under target"
                );
            }
        }

        // No call loss.
        let (calls, _, _) = tail_totals(&chunks);
        assert_eq!(calls, 128);
        assert!(chunks.last().unwrap().is_final);
    }

    #[test]
    fn chunk_zero_stays_under_target_even_with_oversized_tails() {
        // Smoking-gun regression: v3 chunk 0 carrying massive sessions
        // + shell_commands ballooned past the framer cap. v4 chunk 0
        // carries only bounded aggregates; tail vecs are stripped.
        // Even if the input has 10k sessions + 5k shell_commands,
        // chunk 0 must encode small.
        let mut data = fake_data(0);
        data.sessions = (0..10_000)
            .map(|i| SessionUsage {
                provider: Provider::Claude,
                project: format!("proj-{i}"),
                session_id: format!("session-{i}"),
                first_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap(),
                last_timestamp: DateTime::<Utc>::from_timestamp(1_700_000_001, 0).unwrap(),
                bucket: TokenBucket::default(),
            })
            .collect();
        data.shell_commands = (0..5_000)
            .map(|i| NamedUsage {
                name: format!("cmd-{i}"),
                calls: 1,
            })
            .collect();

        let chunks = chunk_usage_data(data, 0, false, 2 * 1024 * 1024);

        // Encode chunk 0 and check its size — it must be far under
        // the framer cap because tail vecs were stripped before
        // shipping chunk 0.
        let chunk_0_bytes = rmp_serde::to_vec_named(&chunks[0]).unwrap().len();
        assert!(
            chunk_0_bytes < 100 * 1024,
            "chunk 0 should be tiny (no tail vecs), got {chunk_0_bytes} bytes"
        );
        assert!(chunks[0].data.sessions.is_empty());
        assert!(chunks[0].data.shell_commands.is_empty());

        // All 15_000 tail items ship across follow-on chunks with no loss.
        let (_, total_sessions, total_shells) = tail_totals(&chunks);
        assert_eq!(total_sessions, 10_000);
        assert_eq!(total_shells, 5_000);
        assert!(chunks.last().unwrap().is_final);
    }
}
