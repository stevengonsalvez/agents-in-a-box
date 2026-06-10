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
use ainb_plugin_types_sessions::{
    RefreshRequest, ScanProgressEvent, UsageData, UsageDataEvent, WIRE_VERSION,
};
use async_trait::async_trait;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::scanner::{self, ProviderRoots};

/// Nanoseconds per day, for the incremental watermark arithmetic.
#[cfg(not(target_arch = "wasm32"))]
const NANOS_PER_DAY: u64 = 86_400_000_000_000;

/// Decode the `sessions.refresh_request` payload. The topic was
/// historically a bare ping, so an empty payload — what the host FS
/// watcher and burndown's `r` key still send — decodes to the default
/// (incremental). A malformed payload also degrades to incremental
/// with a warn: a bad publisher must never block refreshes, and
/// incremental is the safe (cheap) direction to fail in.
fn decode_refresh_request(payload: &[u8]) -> RefreshRequest {
    if payload.is_empty() {
        return RefreshRequest::default();
    }
    match rmp_serde::from_slice(payload) {
        Ok(req) => req,
        Err(err) => {
            tracing::warn!(
                "session-reader: refresh_request payload undecodable ({err}); \
                treating as incremental"
            );
            RefreshRequest::default()
        }
    }
}

/// Everything one refresh's blocking scan produced — the glue between
/// the async plugin and the scanner, factored out so the "cache
/// present → incremental, else legacy full scan" decision is directly
/// testable without a `HostClient`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct BlockingScanOutcome {
    /// The snapshot to publish — `None` when the scanner proved this
    /// refresh byte-identical to the previous one (unchanged-snapshot
    /// short-circuit, issue #255): the caller keeps the published
    /// snapshot and skips the publish entirely.
    pub(crate) data: Option<ainb_plugin_types_sessions::UsageData>,
    /// The (reused or rebuilt) stable rollup to keep in memory for the
    /// next refresh. `None` on the cache-less full-scan path.
    pub(crate) stable: Option<scanner::StableAggregate>,
    /// Cache handle returned to the plugin for reuse.
    pub(crate) cache: Option<crate::cache::UsageCache>,
    /// What the incremental scan saw on the recent side — feed back as
    /// `prev_memo` next refresh to arm the short-circuit. `None` on
    /// the cache-less full-scan path.
    pub(crate) memo: Option<scanner::RecentMemo>,
    /// Scan instrumentation — `Some` iff the incremental path ran.
    pub(crate) counters: Option<scanner::ScanCounters>,
    pub(crate) stable_rebuilt: bool,
    /// `true` when no stable rollup existed in memory or on disk —
    /// this refresh was the one-time seeding scan.
    pub(crate) cold_start: bool,
}

/// The blocking half of a refresh: decide incremental-vs-full, run the
/// scan, persist the rollup on rebuild.
///
/// - `stored` is the caller's in-memory rollup; when `None` (process
///   restart, first refresh) the persisted rollup is hydrated from the
///   cache before deciding cold start.
/// - With a cache: run [`scanner::scan_incremental`] against
///   `now - window_days` and persist the rollup whenever it was
///   rebuilt.
/// - Without a cache (path unresolved / open failed): nowhere to
///   persist a rollup, so run the legacy full scan.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn run_blocking_scan(
    roots: &ProviderRoots,
    cache: Option<crate::cache::UsageCache>,
    stored: Option<scanner::StableAggregate>,
    prev_memo: Option<&scanner::RecentMemo>,
    window_days: u32,
    reporter: &mut scanner::ProgressReporter,
) -> BlockingScanOutcome {
    let mut cache_opt = cache;

    // Hydrate from the persisted rollup when memory is empty — this is
    // what makes a plugin restart skip the seeding scan.
    let stored = match stored {
        Some(s) => Some(s),
        None => cache_opt.as_ref().and_then(|c| c.load_stable().unwrap_or_default()),
    };
    let cold_start = stored.is_none();

    if cache_opt.is_some() {
        let watermark =
            now_ns().saturating_sub(u64::from(window_days).saturating_mul(NANOS_PER_DAY));
        let outcome = scanner::scan_incremental(
            roots,
            &mut cache_opt,
            stored,
            watermark,
            prev_memo,
            reporter,
        );
        if outcome.stable_rebuilt {
            if let Some(cache) = cache_opt.as_mut() {
                if let Err(err) = cache.store_stable(&outcome.stable) {
                    tracing::warn!(
                        "session-reader: stable rollup persist failed ({err}); \
                        next refresh rebuilds again"
                    );
                }
            }
        }
        BlockingScanOutcome {
            data: outcome.data,
            stable: Some(outcome.stable),
            cache: cache_opt,
            memo: Some(outcome.memo),
            counters: Some(outcome.counters),
            stable_rebuilt: outcome.stable_rebuilt,
            cold_start,
        }
    } else {
        let data = scanner::scan_with_cache_and_progress(roots, &mut cache_opt, reporter);
        BlockingScanOutcome {
            data: Some(data),
            stable: None,
            cache: cache_opt,
            memo: None,
            counters: None,
            stable_rebuilt: false,
            cold_start,
        }
    }
}

/// Per-chunk msgpack byte budget. Each chunk's encoded msgpack stays
/// below this size so the post-base64 + JSON-envelope frame fits
/// comfortably under the host framer's 16 MiB body cap (base64
/// inflates ~33%; JSON wrapping adds a few hundred bytes).
///
/// Real `$HOME` data is wildly non-uniform — some chunks of 4000 calls
/// land at ~600 KiB, others at ~12 MiB because of long bash-command
/// strings or user_message bodies. A fixed call-count budget therefore
/// can't guarantee staying under the wire cap. [`chunk_usage_data`]
/// sizes each item once as it's appended and cuts as soon as the
/// running encoded size would cross this threshold.
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
    /// Trailing recent-window size (days) from `[session_reader]`
    /// config; bounds the incremental watermark.
    #[cfg(not(target_arch = "wasm32"))]
    window_days: u32,
    /// In-memory copy of the persisted stable rollup so steady-state
    /// refreshes don't re-deserialize the blob; loaded from the cache
    /// on the first refresh, replaced whenever a rebuild runs.
    #[cfg(not(target_arch = "wasm32"))]
    stable: Option<scanner::StableAggregate>,
    #[cfg(not(target_arch = "wasm32"))]
    cache: Option<crate::cache::UsageCache>,
    /// What the previous *successfully published* refresh saw on the
    /// recent side — arms the scanner's unchanged-snapshot
    /// short-circuit. Deliberately `None` until a publish lands (and
    /// reset to `None` whenever one fails) so a skipped publish can
    /// never strand consumers on a snapshot that was never delivered.
    #[cfg(not(target_arch = "wasm32"))]
    last_memo: Option<scanner::RecentMemo>,
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
            window_days: crate::config::load().window_days(),
            #[cfg(not(target_arch = "wasm32"))]
            stable: None,
            #[cfg(not(target_arch = "wasm32"))]
            cache: None,
            #[cfg(not(target_arch = "wasm32"))]
            last_memo: None,
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
            window_days: crate::config::DEFAULT_INCREMENTAL_WINDOW_DAYS,
            #[cfg(not(target_arch = "wasm32"))]
            stable: None,
            #[cfg(not(target_arch = "wasm32"))]
            cache: None,
            #[cfg(not(target_arch = "wasm32"))]
            last_memo: None,
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
        // The persisted stable rollup dies with the cache (clear()
        // wipes both tables); the in-memory copy must die with it or
        // the next refresh would resurrect stale history. The refresh
        // memo dies too — a hard refresh exists precisely because the
        // user distrusts cached state, so the unchanged-snapshot
        // short-circuit must not suppress the rebuilt publish.
        self.stable = None;
        self.last_memo = None;
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
            // Take the WAL/SHM siblings with the db: SQLite can
            // recover a leftover -wal into a freshly created database,
            // silently resurrecting rows this flush just promised were
            // gone.
            for suffix in ["-wal", "-shm"] {
                let mut sibling = path.as_os_str().to_owned();
                sibling.push(suffix);
                let _ = std::fs::remove_file(std::path::Path::new(&sibling));
            }
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
    /// `host` as they arrive. Returns the assembled `UsageData` (or
    /// `None` when the unchanged-snapshot short-circuit fired) plus
    /// the refresh's memo, which the caller commits to
    /// [`Self::last_memo`] only after the publish lands.
    ///
    /// The blocking task owns the cache for the duration of the scan
    /// (moved out of `self.cache`), then hands it back so the plugin
    /// can reuse it on the next publish. The previous memo travels the
    /// same way — taken at scan start, so a panicked task or a failed
    /// publish leaves `last_memo` empty and the next refresh publishes
    /// unconditionally. Progress events flow over a
    /// `tokio::sync::mpsc::unbounded` channel: the rate-limit lives in
    /// the [`scanner::ProgressReporter`] inside the blocking task, so
    /// the async drain loop never sees more than ~10 events/s.
    #[cfg(not(target_arch = "wasm32"))]
    async fn scan_streaming(
        &mut self,
        host: &HostClient,
    ) -> (
        Option<ainb_plugin_types_sessions::UsageData>,
        Option<scanner::RecentMemo>,
    ) {
        self.ensure_cache();
        let roots = self.roots.clone();
        let cache = self.cache.take();
        let stored = self.stable.take();
        let prev_memo = self.last_memo.take();
        let window_days = self.window_days;

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ScanProgressEvent>();

        let scan_handle = tokio::task::spawn_blocking(move || {
            let mut reporter = scanner::ProgressReporter::new(move |evt| {
                // Send is unbounded + non-blocking; failures only happen
                // when the receiver has been dropped (impossible here —
                // we hold `rx` until `scan_handle` resolves).
                let _ = tx.send(evt);
            });
            run_blocking_scan(
                &roots,
                cache,
                stored,
                prev_memo.as_ref(),
                window_days,
                &mut reporter,
            )
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
            Ok(outcome) => {
                self.cache = outcome.cache;
                self.stable = outcome.stable;
                if outcome.cold_start {
                    let _ = host
                        .log_info(
                            "session-reader: no stable rollup found — built full usage \
                            history (one-time, heavier than a normal refresh)",
                        )
                        .await;
                }
                // Host-visible counter line — the soak/observability
                // contract. The plugin subprocess has no tracing
                // subscriber of its own, so host.log() is the only
                // sink that reliably lands in the host JSONL
                // (~/.agents-in-a-box/logs/). scripts/soak-session-reader.sh
                // greps for exactly this shape.
                if let Some(c) = outcome.counters {
                    let _ = host
                        .log_info(format!(
                            "session-reader: incremental refresh statted={} parsed={} \
                            cache_hits={} stable_skipped={} stable_reused={} rebuilt={}",
                            c.files_statted,
                            c.parsed,
                            c.cache_hits,
                            c.stable_skipped,
                            c.stable_reused,
                            outcome.stable_rebuilt,
                        ))
                        .await;
                }
                (outcome.data, outcome.memo)
            }
            Err(err) => {
                // The panicked closure took ownership of the cache and
                // stable rollup with it — both are gone. Re-arm
                // ensure_cache (cache_init is already true, so without
                // this reset it would no-op forever and every future
                // refresh would silently take the cache-less FULL-scan
                // path — the exact CPU regression this plugin exists
                // to prevent).
                self.cache_init = false;
                let _ = host
                    .log_info(format!(
                        "session-reader: scan task join error: {err}; \
                        cache handle lost with the panicked task — reopening, \
                        falling back to in-line scan"
                    ))
                    .await;
                (Some(self.scan_now()), None)
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
        let (data, memo) = self.scan_streaming(host).await;
        #[cfg(target_arch = "wasm32")]
        let (data, memo) = (Some(self.scan_now()), None);

        let Some(data) = data else {
            // Unchanged-snapshot short-circuit: the scanner proved the
            // snapshot byte-identical to the one already published —
            // consumers keep what they have, nothing goes on the wire.
            let _ = host.log_info("publish: snapshot unchanged — publish skipped").await;
            // ...except the terminal progress event: consumers clear
            // their "Scanning sessions…" banner on the final
            // usage_data chunk, and no such chunk is coming. `done`
            // tells them the scan ended without a republish.
            let done = ScanProgressEvent {
                done: true,
                ..ScanProgressEvent::default()
            };
            if let Ok(bytes) = rmp_serde::to_vec_named(&done) {
                let _ = host.snapshot_publish(TOPIC_SCAN_PROGRESS, bytes).await;
            }
            self.set_last_memo(memo);
            return Ok(());
        };

        // The memo commits only after every chunk lands: an error exit
        // below leaves it empty, so the next refresh re-publishes
        // rather than skipping consumers onto a half-delivered
        // snapshot. (`scan_streaming` already took the previous memo.)
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
        self.set_last_memo(memo);
        Ok(())
    }

    /// Store the refresh memo that arms the next scan's
    /// unchanged-snapshot short-circuit. No-op on wasm32, where the
    /// incremental path (and the memo) don't exist.
    #[cfg(not(target_arch = "wasm32"))]
    fn set_last_memo(&mut self, memo: Option<scanner::RecentMemo>) {
        self.last_memo = memo;
    }

    #[cfg(target_arch = "wasm32")]
    fn set_last_memo(&mut self, _memo: Option<()>) {}
}

/// Headroom added to the per-chunk running-size estimate to cover
/// msgpack array-header growth: each of the three tail arrays encodes
/// a 1-byte header while empty (fixarray) that grows to at most 5
/// bytes (`array 32`) as items append — ≤ 12 bytes of growth total,
/// rounded up generously.
const CHUNKER_HEADER_SLACK: usize = 64;

/// Identifier for which tail-chunkable vec a chunker push came from.
/// Lets the spill-back path return the just-popped item to the right
/// queue when the per-chunk verification encode finds an overshoot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TailQueue {
    Calls,
    Sessions,
    ShellCommands,
}

/// Encoded msgpack size of one value on its own. msgpack array
/// elements are plain concatenated values, so an item's standalone
/// encoding is byte-for-byte what it contributes inside a tail vec —
/// which is what makes the chunker's running-sum sizing exact (modulo
/// the array headers covered by [`CHUNKER_HEADER_SLACK`]).
///
/// An encode failure returns 0: the item still ships, and the same
/// failure then surfaces at publish time exactly as it would have
/// before sizing existed.
fn encoded_len<T: serde::Serialize>(item: &T) -> usize {
    rmp_serde::to_vec_named(item).map_or(0, |b| b.len())
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
/// **Sizing strategy (issue #255).** Each tail item is encoded exactly
/// once, when it's popped: the chunk cuts when `envelope base +
/// Σ item sizes + header slack` would cross `target_bytes`. One
/// verification encode runs per chunk as a safety net (spilling items
/// back if the estimate ever under-counted — it shouldn't, msgpack
/// array elements concatenate). The previous implementation re-encoded
/// the whole candidate chunk every 64 items, which on a ~126-chunk
/// real-data snapshot meant ~7.8 GB of redundant encoding and ~20 s of
/// the measured ~23 s refresh cost.
///
/// Pop priority is `calls` → `sessions` → `shell_commands`, draining
/// the biggest queue first so the chunk count for call-dominated
/// snapshots matches the v3 distribution.
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

    // Envelope base: encoded size of an empty tail chunk. msgpack
    // encodes `u32` with 1–5 bytes depending on magnitude, so probe
    // with `u32::MAX` — the base must never under-count a late chunk.
    let envelope_base = encoded_len(&UsageDataEvent {
        version: WIRE_VERSION,
        published_ns,
        partial,
        chunk_index: u32::MAX,
        is_final: true,
        data: UsageData::default(),
    });

    let mut chunk_index: u32 = 1;
    while !(calls_q.is_empty() && sessions_q.is_empty() && shell_q.is_empty()) {
        let mut chunk_data = UsageData::default();
        let mut running = envelope_base + CHUNKER_HEADER_SLACK;
        let mut items_in_chunk = 0usize;

        loop {
            // Pop priority: calls (largest) → sessions → shell_commands.
            // Peek-encode-decide: an item that would push the chunk over
            // target stays at the front of its queue for the next chunk
            // — no spill bookkeeping on the normal path.
            let item_size = if let Some(c) = calls_q.front() {
                encoded_len(c)
            } else if let Some(s) = sessions_q.front() {
                encoded_len(s)
            } else if let Some(s) = shell_q.front() {
                encoded_len(s)
            } else {
                break; // every queue drained
            };

            // Cut before the overshooting item — unless the chunk is
            // still empty: single-item chunks always ship so a giant
            // outlier (e.g. one 5 MiB user_message) can't stall the loop.
            if items_in_chunk > 0 && running + item_size >= target_bytes {
                break;
            }

            if let Some(c) = calls_q.pop_front() {
                chunk_data.calls.push(c);
            } else if let Some(s) = sessions_q.pop_front() {
                chunk_data.sessions.push(s);
            } else if let Some(s) = shell_q.pop_front() {
                chunk_data.shell_commands.push(s);
            }
            running += item_size;
            items_in_chunk += 1;
        }

        // Safety net: one verification encode per chunk. The running
        // sum is exact up to array headers (covered by the slack), so
        // this should never trigger — but an estimate bug here would
        // otherwise ship a frame the host framer rejects, so verify
        // and spill items back until the chunk really fits.
        let verify_event = UsageDataEvent {
            version: WIRE_VERSION,
            published_ns,
            partial,
            chunk_index,
            is_final: calls_q.is_empty() && sessions_q.is_empty() && shell_q.is_empty(),
            data: chunk_data.clone(),
        };
        let verified_size =
            rmp_serde::to_vec_named(&verify_event).map(|b| b.len()).unwrap_or(target_bytes);
        let chunk_item_count =
            chunk_data.calls.len() + chunk_data.sessions.len() + chunk_data.shell_commands.len();
        if verified_size >= target_bytes && chunk_item_count > 1 {
            tracing::warn!(
                verified_size,
                target_bytes,
                "session-reader chunker: size estimate under-counted; spilling"
            );
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
            let req = decode_refresh_request(&params.payload);
            if req.hard {
                // Hard refresh: distrust every cache layer. Wipe the
                // parse cache + stable rollup, then rescan — the cold
                // incremental path re-parses everything from source
                // and seeds a fresh rollup.
                tracing::info!(
                    "session-reader: HARD refresh requested — wiping caches, rebuilding from source"
                );
                #[cfg(not(target_arch = "wasm32"))]
                self.flush_cache(host).await;
            } else {
                tracing::info!("session-reader: refresh requested — incremental rescan");
            }
            return self.publish(host).await;
        }
        if params.topic == TOPIC_FLUSH_CACHE_REQUEST {
            tracing::info!("session-reader: flush_cache requested — wiping cache + rescanning");
            #[cfg(not(target_arch = "wasm32"))]
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

    // ── run_blocking_scan glue (plugin-level counter assertions) ────
    //
    // The scanner-level tests prove scan_incremental's behavior; these
    // prove the PLUGIN's decision layer: cache present → incremental
    // (counters populated, rollup persisted, restart rehydrates),
    // cache absent → legacy full scan. Same oracle: byte-identity with
    // a cache-less full scan of the same tree.

    fn glue_claude_line(ts: &str, session: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","sessionId":"{session}","cwd":"/tmp/proj","gitBranch":"main","message":{{"model":"claude-3-5-sonnet","content":[{{"type":"text","text":"hi"}}],"usage":{{"input_tokens":10,"output_tokens":20}}}}}}"#
        )
    }

    fn glue_fixture() -> (tempfile::TempDir, ProviderRoots, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tree tempdir");
        let claude_root = tmp.path().join("claude/projects/proj-a");
        std::fs::create_dir_all(&claude_root).expect("mkdir");
        std::fs::write(
            claude_root.join("one.jsonl"),
            glue_claude_line("2026-05-01T10:00:00Z", "s1"),
        )
        .expect("write");
        std::fs::write(
            claude_root.join("two.jsonl"),
            glue_claude_line("2026-06-01T10:00:00Z", "s2"),
        )
        .expect("write");
        let roots = ProviderRoots {
            claude_projects: Some(tmp.path().join("claude/projects")),
            ..ProviderRoots::default()
        };
        let cache_dir = tempfile::tempdir().expect("cache tempdir");
        // Let the filesystem clock tick past the writes so a
        // `window_days = 0` watermark (now) puts both files on the
        // stable side.
        std::thread::sleep(std::time::Duration::from_millis(50));
        (tmp, roots, cache_dir)
    }

    fn open_cache(dir: &tempfile::TempDir) -> Option<crate::cache::UsageCache> {
        Some(crate::cache::UsageCache::open(&dir.path().join("usage.sqlite")).expect("cache"))
    }

    fn encode(data: &UsageData) -> Vec<u8> {
        rmp_serde::to_vec_named(data).expect("encode")
    }

    #[test]
    fn glue_runs_incremental_persists_rollup_and_rehydrates_on_restart() {
        let (_tree, roots, cache_dir) = glue_fixture();
        let oracle = encode(&scanner::scan(&roots));
        let mut reporter = scanner::ProgressReporter::noop();

        // First refresh: cold start seeds + persists the rollup.
        let first = run_blocking_scan(&roots, open_cache(&cache_dir), None, None, 0, &mut reporter);
        assert!(first.cold_start, "no rollup anywhere yet");
        assert!(first.stable_rebuilt);
        let c = first.counters.expect("incremental path ran");
        assert_eq!(c.parsed, 2, "seed scan parses both files");
        assert_eq!(
            encode(first.data.as_ref().expect("changed scan publishes")),
            oracle,
            "matches full-scan oracle"
        );
        assert!(
            first.cache.expect("cache returned").load_stable().expect("load").is_some(),
            "rollup persisted for the next process"
        );

        // Simulated plugin restart: in-memory rollup gone (stored =
        // None), fresh cache handle on the same file. The glue must
        // rehydrate from disk and reuse — zero parses.
        let second =
            run_blocking_scan(&roots, open_cache(&cache_dir), None, None, 0, &mut reporter);
        assert!(!second.cold_start, "rollup rehydrated from the cache");
        assert!(!second.stable_rebuilt);
        let c = second.counters.expect("incremental path ran");
        assert_eq!(
            c.parsed, 0,
            "no-change refresh after restart parses ZERO files"
        );
        assert!(c.stable_reused);
        assert_eq!(c.stable_skipped, 2, "both stable files skipped outright");
        assert_eq!(
            encode(second.data.as_ref().expect("changed scan publishes")),
            oracle,
            "still byte-identical to the oracle"
        );
    }

    #[test]
    fn glue_unchanged_refresh_returns_no_data_and_memo_round_trips() {
        let (tree, roots, cache_dir) = glue_fixture();
        let oracle = encode(&scanner::scan(&roots));
        let mut reporter = scanner::ProgressReporter::noop();

        // Seed (window 36500 days → everything is recent, exercising
        // the recent fingerprint memo rather than the stable rollup).
        let first = run_blocking_scan(
            &roots,
            open_cache(&cache_dir),
            None,
            None,
            36_500,
            &mut reporter,
        );
        assert!(first.data.is_some(), "first refresh publishes");
        let memo = first.memo.clone().expect("incremental path produces a memo");

        // Unchanged refresh with the memo armed: no data → no publish.
        let second = run_blocking_scan(
            &roots,
            open_cache(&cache_dir),
            Some(first.stable.expect("rollup")),
            Some(&memo),
            36_500,
            &mut reporter,
        );
        assert!(
            second.data.is_none(),
            "unchanged refresh short-circuits the publish"
        );
        let memo = second.memo.expect("memo still round-trips on a skip");

        // Touch a file: the same memo must now disarm and re-publish.
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(
            tree.path().join("claude/projects/proj-a/two.jsonl"),
            format!(
                "{}\n{}",
                glue_claude_line("2026-06-01T10:00:00Z", "s2"),
                glue_claude_line("2026-06-02T10:00:00Z", "s2")
            ),
        )
        .expect("append");
        let third = run_blocking_scan(
            &roots,
            open_cache(&cache_dir),
            second.stable,
            Some(&memo),
            36_500,
            &mut reporter,
        );
        let data = third.data.as_ref().expect("changed file re-publishes");
        let fresh_oracle = encode(&scanner::scan(&roots));
        assert_ne!(encode(data), oracle, "snapshot moved past the seed state");
        assert_eq!(encode(data), fresh_oracle, "matches the post-change oracle");
    }

    #[test]
    fn glue_falls_back_to_full_scan_without_cache() {
        let (_tree, roots, _cache_dir) = glue_fixture();
        let oracle = encode(&scanner::scan(&roots));
        let mut reporter = scanner::ProgressReporter::noop();

        let out = run_blocking_scan(&roots, None, None, None, 0, &mut reporter);
        assert!(
            out.counters.is_none(),
            "cache-less refresh takes the legacy full path"
        );
        assert!(out.stable.is_none(), "nothing to persist a rollup into");
        assert!(!out.stable_rebuilt);
        assert_eq!(
            encode(out.data.as_ref().expect("changed scan publishes")),
            oracle,
            "full path matches the oracle trivially"
        );
    }

    #[test]
    fn refresh_request_empty_payload_is_incremental() {
        // The host FS watcher and burndown's `r` key publish bare
        // pings (empty payload) — back-compat demands incremental.
        let req = decode_refresh_request(&[]);
        assert!(!req.hard);
    }

    #[test]
    fn refresh_request_empty_map_is_incremental() {
        // msgpack fixmap-0 (a publisher encoding `{}`) — `hard`
        // defaults off via #[serde(default)].
        let req = decode_refresh_request(&[0x80]);
        assert!(!req.hard);
    }

    #[test]
    fn refresh_request_hard_roundtrips() {
        let bytes = rmp_serde::to_vec_named(&RefreshRequest { hard: true }).expect("encode");
        let req = decode_refresh_request(&bytes);
        assert!(req.hard);
    }

    #[test]
    fn refresh_request_garbage_degrades_to_incremental() {
        // A broken publisher must never block refreshes; failing in
        // the cheap (incremental) direction is the safe default.
        let req = decode_refresh_request(&[0xC1, 0xFF, 0x00]);
        assert!(!req.hard);
    }

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
