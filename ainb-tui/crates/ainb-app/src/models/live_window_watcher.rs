// ABOUTME: Background poller for `live_window::current()` so the render
// thread never does the work itself.
//
// `live_window::current()` is sync and can be expensive on the Tier 2 path
// (walks `~/.claude/projects/*.jsonl` and parses recent entries). Calling
// it on every render frame stalls input handling — even Tier 1's small
// JSON read is filesystem I/O on the hot path.
//
// The watcher owns an `Arc<RwLock<LiveWindow>>` and a tokio task that
// refreshes it on a fixed interval via `spawn_blocking`. Render code
// reads via `snapshot()` which is a cheap `read().clone()`.
//
// `Default` returns an unspawned watcher (empty snapshot, no task) so
// tests and AppState construction don't require a tokio runtime. Call
// `start()` from an async context to begin polling.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use tracing::{trace, warn};

use crate::models::live_window::{self, LiveWindow};

/// How often the watcher recomputes the live window. Cheap on the Tier 1
/// path (~1KB JSON read); on the Tier 2 fallback this is the upper bound
/// on how often we walk the JSONL transcripts. 5s feels live without
/// thrashing the disk.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Default, Debug)]
pub struct LiveWindowWatcher {
    inner: Arc<RwLock<LiveWindow>>,
}

impl LiveWindowWatcher {
    /// Cheap snapshot for the render path. Never blocks beyond a brief
    /// `RwLock::read` against a single writer.
    pub fn snapshot(&self) -> LiveWindow {
        self.inner.read().map(|w| w.clone()).unwrap_or_else(|err| {
            warn!(error = %err, "live_window_watcher: snapshot RwLock poisoned");
            LiveWindow::empty()
        })
    }

    /// Start a background poll loop. Safe to call only from inside a
    /// tokio runtime. The task holds a `Weak` to the inner state so it
    /// exits cleanly when the watcher is dropped.
    pub fn start(&self) {
        self.start_with_interval(REFRESH_INTERVAL);
    }

    fn start_with_interval(&self, interval: Duration) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                let Some(strong) = weak.upgrade() else {
                    break;
                };
                // Drop the strong ref before the blocking call so we
                // don't pin the Arc longer than necessary.
                drop(strong);

                // Keep the Codex usage cache warm while the TUI is open.
                // Codex (unlike Claude) has no external driver feeding it —
                // so we pull it here. The pull is throttled internally
                // (~60s) so firing it every REFRESH_INTERVAL (5s) is a
                // cheap cache-read no-op on most ticks. Fire-and-forget so
                // a slow network pull never delays the Claude refresh
                // below; the freshly-written `codex-live.json` is picked up
                // by the next `current()` tick (which overlays it).
                tokio::spawn(async {
                    let _ = crate::cli::codex_statusline::execute(false).await;
                });

                let fresh = tokio::task::spawn_blocking(live_window::current).await;
                let Some(strong) = weak.upgrade() else {
                    break;
                };
                match fresh {
                    Ok(value) => {
                        // Heartbeat — fires every REFRESH_INTERVAL (5s).
                        // At debug it'd be 17K events/day; trace keeps
                        // it gated behind explicit `RUST_LOG=ainb=trace`.
                        trace!(source = ?value.source, "live_window_watcher: refresh ok");
                        match strong.write() {
                            Ok(mut guard) => *guard = value,
                            Err(err) => warn!(
                                error = %err,
                                "live_window_watcher: write RwLock poisoned"
                            ),
                        }
                    }
                    Err(err) => warn!(
                        error = %err,
                        "live_window_watcher: spawn_blocking failed"
                    ),
                }
                drop(strong);
                tokio::time::sleep(interval).await;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::live_window::Source;

    #[test]
    fn default_snapshot_is_empty() {
        let w = LiveWindowWatcher::default();
        assert_eq!(w.snapshot().source, Source::None);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn snapshot_works_after_start() {
        let watcher = LiveWindowWatcher::default();
        watcher.start_with_interval(Duration::from_millis(10));
        // Yield to let the spawned task make progress. We can't assert a
        // specific source without controlling the FS, but `snapshot()`
        // must not panic and must return a valid `LiveWindow`.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _snap = watcher.snapshot();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn task_exits_when_watcher_dropped() {
        let watcher = LiveWindowWatcher::default();
        watcher.start_with_interval(Duration::from_millis(10));
        let weak = Arc::downgrade(&watcher.inner);
        drop(watcher);
        // Loop should observe the Weak failing to upgrade and exit.
        // Give it a couple of iterations' worth of time.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(weak.upgrade().is_none(), "inner Arc should be released");
    }
}
