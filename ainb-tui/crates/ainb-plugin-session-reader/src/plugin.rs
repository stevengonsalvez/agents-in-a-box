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
    HandleEventParams, HostClient, Plugin, RenderParams, Result, SdkError, WireBuffer,
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

/// Split a `UsageData` snapshot into one or more `UsageDataEvent`
/// chunks. Pure; unit-tested directly.
///
/// Re-encodes the candidate chunk after every [`CHUNKER_PROBE_STEP`]
/// calls; cuts when the encoded msgpack size crosses `target_bytes`.
/// This avoids the unbounded variance seen with a static call-count
/// budget (some real-world calls are >10x larger than others).
///
/// - Chunk 0 carries every aggregate field + the first slice of
///   `data.calls` (sized so the encoded msgpack stays under
///   `target_bytes`).
/// - Chunks 1..N carry only the remaining `calls` (every other field
///   is `UsageData::default()`).
/// - The last chunk has `is_final = true`. A single-chunk publish has
///   `chunk_index = 0, is_final = true`.
/// - When `data.calls` is empty, returns a single chunk.
/// - A single huge call that on its own exceeds `target_bytes` still
///   ships (otherwise the loop would stall); the host framer's
///   16 MiB cap still leaves an order-of-magnitude headroom.
pub(crate) fn chunk_usage_data(
    mut data: UsageData,
    published_ns: u64,
    partial: bool,
    target_bytes: usize,
) -> Vec<UsageDataEvent> {
    assert!(target_bytes >= 1024, "target_bytes too small");
    let all_calls = std::mem::take(&mut data.calls);

    let mut out: Vec<UsageDataEvent> = Vec::new();
    let mut remaining: std::collections::VecDeque<_> = all_calls.into_iter().collect();
    let mut chunk_index: u32 = 0;

    loop {
        // Build the candidate chunk's data shell once per chunk —
        // chunk 0 includes aggregates, others are calls-only.
        let mut chunk_data = if chunk_index == 0 {
            let mut head = data.clone();
            head.calls = Vec::new();
            head
        } else {
            UsageData::default()
        };

        // Push calls one at a time, probing the encoded size every
        // CHUNKER_PROBE_STEP appends OR when the queue drains. On
        // overshoot, roll the trailing call back so the next chunk
        // gets it. Single-call chunks always ship (giant-outlier
        // protection).
        let mut since_probe = 0usize;
        loop {
            let Some(call) = remaining.pop_front() else {
                break;
            };
            chunk_data.calls.push(call);
            since_probe += 1;
            // Probe when we've accumulated STEP calls, or when the
            // queue just drained (so we don't miss tiny final
            // overshoots).
            if since_probe < CHUNKER_PROBE_STEP && !remaining.is_empty() {
                continue;
            }
            since_probe = 0;

            let probe_event = UsageDataEvent {
                version: WIRE_VERSION,
                published_ns,
                partial,
                chunk_index,
                is_final: remaining.is_empty(),
                data: chunk_data.clone(),
            };
            let probe_size =
                rmp_serde::to_vec_named(&probe_event).map(|b| b.len()).unwrap_or(target_bytes);
            if probe_size >= target_bytes && chunk_data.calls.len() > 1 {
                // Shed the trailing call back to `remaining`.
                if let Some(spill) = chunk_data.calls.pop() {
                    remaining.push_front(spill);
                }
                break;
            }
        }

        let is_final = remaining.is_empty();
        out.push(UsageDataEvent {
            version: WIRE_VERSION,
            published_ns,
            partial,
            chunk_index,
            is_final,
            data: chunk_data,
        });
        if is_final {
            break;
        }
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

    async fn on_init(&mut self, host: &HostClient, _granted: &[String]) -> Result<()> {
        // Subscribe to refresh-requests so any consumer (burndown,
        // tests) can force a rescan. We deliberately *do not* publish
        // on startup — consumers ask via the refresh topic so they're
        // guaranteed to be subscribed before chunk 0 hits the wire.
        // A startup publish races the consumer's own `on_init` and
        // can arrive while burndown is still subscribing, losing
        // chunks of the sequence (Bug A in fix/runtime-eager-spawn).
        let _ = host.log_info("on_init: subscribing to sessions.refresh_request").await;
        host.snapshot_subscribe(TOPIC_REFRESH_REQUEST).await?;
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
        ActivityCategory, ActivityUsage, ProjectUsage, Provider, ProviderCall, TokenBucket,
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

    #[test]
    fn chunk_single_when_calls_fit_in_one_chunk() {
        let data = fake_data(10);
        // 4 MiB budget — small fake calls all fit.
        let chunks = chunk_usage_data(data, 42, false, 4 * 1024 * 1024);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(chunks[0].is_final);
        assert_eq!(chunks[0].data.calls.len(), 10);
        assert_eq!(chunks[0].data.projects.len(), 1, "aggregates ride chunk 0");
        assert_eq!(chunks[0].published_ns, 42);
    }

    #[test]
    fn chunk_single_when_calls_empty() {
        let data = fake_data(0);
        let chunks = chunk_usage_data(data, 0, false, 4 * 1024 * 1024);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].is_final);
        assert!(chunks[0].data.calls.is_empty());
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
        let total_calls: usize = chunks.iter().map(|c| c.data.calls.len()).sum();
        assert_eq!(total_calls, 40, "no calls lost across chunks");

        // Chunk 0 carries aggregates.
        assert_eq!(chunks[0].chunk_index, 0);
        assert!(!chunks[0].is_final);
        assert_eq!(chunks[0].data.projects.len(), 1);
        assert_eq!(chunks[0].data.activities.len(), 1);

        // Chunks 1+ carry only calls (no aggregates).
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
        let mut data = fake_data(0);
        let mut huge = fake_call(1);
        huge.user_message = "x".repeat(2048);
        data.calls = vec![huge];
        let chunks = chunk_usage_data(data, 0, false, 1024);
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].is_final);
        assert_eq!(chunks[0].data.calls.len(), 1);
    }
}
