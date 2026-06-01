// ABOUTME: Host-side filesystem watcher that keeps the burndown usage
// snapshot live without the user pressing `r`.
//
// The `session-reader` plugin has no timer and no watcher of its own — it
// scans once when it receives `sessions.refresh_request` and is otherwise
// idle (see `ainb-plugin-session-reader/src/plugin.rs` `on_init`). Burndown
// fires that request once at startup and again on the `r`/`R` key. So a
// long-running TUI shows a snapshot frozen at the last manual refresh, and
// "today" never appears until the user refreshes by hand.
//
// This watcher closes that gap. It watches the provider session dirs and,
// on debounced + throttled filesystem changes, publishes
// `sessions.refresh_request` via the host-side `RuntimeHandle`. session-
// reader picks it up, does its (cache-aware) rescan — only changed files
// re-parse — and republishes `sessions.usage_data`; burndown re-renders.
//
// Two timers shape the cadence:
//   * DEBOUNCE — quiet period after a burst, so the dozens of writes a
//     single agent turn produces coalesce into one refresh.
//   * MIN_INTERVAL — floor between two refreshes, so continuous session
//     activity can't trigger a full rescan+republish every few seconds.
//     The republish re-sends the whole snapshot (chunked); doing that
//     too often previously starved keystrokes on the burndown screen.
//
// Owning the returned `UsageDirWatcher` keeps the watch alive; dropping it
// (e.g. when `App` drops) closes the channel, so the debounce task exits.
//
// macOS uses FSEvents (kernel-level, handles large trees cheaply). On
// Linux's inotify a recursive watch over a directory with thousands of
// project subdirs can hit the per-watch limit; failure to watch a dir is
// non-fatal here — the watcher simply covers the dirs it could register.

use std::path::PathBuf;
use std::time::Duration;

use ainb_plugin_runtime::RuntimeHandle;
use bytes::Bytes;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Topic session-reader subscribes to; a host-side publish triggers its
/// rescan + republish. Must match
/// `ainb-plugin-session-reader`'s `TOPIC_REFRESH_REQUEST`.
const TOPIC_REFRESH_REQUEST: &str = "sessions.refresh_request";

/// Quiet period after the first change before publishing — coalesces a
/// burst of writes (one agent turn = many JSONL appends) into one refresh.
const DEBOUNCE: Duration = Duration::from_secs(3);

/// Floor on the gap between two refreshes. The session-reader republish
/// re-sends the full (chunked) snapshot, so continuous activity must not
/// be able to fire it back-to-back. 5 minutes — burndown is a usage
/// dashboard, not a live ticker; minute-scale freshness is ample and
/// keeps the full rescan+republish cost negligible.
const MIN_INTERVAL: Duration = Duration::from_secs(300);

/// Owns the OS watcher. Keeping this value alive keeps the watch (and the
/// debounce task) running; dropping it stops both.
pub struct UsageDirWatcher {
    // Field is never read — its `Drop` is the contract. Dropping the
    // watcher unregisters the OS-level watches and closes the event
    // channel, which ends the spawned debounce task.
    _watcher: RecommendedWatcher,
}

impl UsageDirWatcher {
    /// Watch the provider session dirs and nudge session-reader to rescan
    /// on change. Best-effort: returns `None` (and the burndown keeps its
    /// press-`r` behaviour) when no dir is watchable or the OS watcher
    /// can't be created. Must be called from inside a tokio runtime.
    #[must_use]
    pub fn start(handle: RuntimeHandle) -> Option<Self> {
        Self::start_with_trigger(provider_session_dirs(), DEBOUNCE, MIN_INTERVAL, move || {
            // Empty subscriber set (plugins disabled / session-reader not
            // yet subscribed) is a silent no-op in `publish_snapshot`, so
            // an early or plugin-free fire costs nothing.
            handle.publish_snapshot(TOPIC_REFRESH_REQUEST, Bytes::new());
        })
    }

    /// Core wiring, parameterised over the dirs, timers, and the action
    /// fired on a debounced change so the debounce loop is unit-testable
    /// without a live `RuntimeHandle`.
    fn start_with_trigger<F>(
        dirs: Vec<PathBuf>,
        debounce: Duration,
        min_interval: Duration,
        trigger: F,
    ) -> Option<Self>
    where
        F: FnMut() + Send + 'static,
    {
        if dirs.is_empty() {
            return None;
        }

        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(event) => {
                    // Only content-changing events matter; ignore pure
                    // access/metadata noise so we don't refresh on reads.
                    if matches!(
                        event.kind,
                        notify::EventKind::Create(_)
                            | notify::EventKind::Modify(_)
                            | notify::EventKind::Remove(_)
                    ) {
                        // Receiver gone only after the watcher is dropped;
                        // a failed send at that point is expected.
                        let _ = tx.send(());
                    }
                }
                Err(err) => warn!(error = %err, "usage_dir_watcher: notify event error"),
            }
        })
        .map_err(|err| warn!(error = %err, "usage_dir_watcher: failed to create watcher"))
        .ok()?;

        let mut watched_any = false;
        for dir in &dirs {
            if !dir.is_dir() {
                continue;
            }
            match watcher.watch(dir, RecursiveMode::Recursive) {
                Ok(()) => {
                    debug!(dir = %dir.display(), "usage_dir_watcher: watching");
                    watched_any = true;
                }
                Err(err) => {
                    warn!(dir = %dir.display(), error = %err, "usage_dir_watcher: watch failed");
                }
            }
        }
        if !watched_any {
            return None;
        }

        tokio::spawn(debounce_loop(rx, debounce, min_interval, trigger));
        Some(Self { _watcher: watcher })
    }
}

/// Coalesce + throttle change signals, firing `trigger` at most once per
/// `debounce + min_interval`. Exits when the channel closes (watcher
/// dropped). Pure in everything but `trigger`, so tests drive it with
/// `tokio::time` paused and a counting closure.
async fn debounce_loop<F>(
    mut rx: mpsc::UnboundedReceiver<()>,
    debounce: Duration,
    min_interval: Duration,
    mut trigger: F,
) where
    F: FnMut() + Send + 'static,
{
    loop {
        // Block until something changes (or the watcher is dropped).
        if rx.recv().await.is_none() {
            break;
        }
        // Let the burst settle, then swallow everything it produced so a
        // single turn's many writes become one refresh.
        tokio::time::sleep(debounce).await;
        while rx.try_recv().is_ok() {}

        trigger();

        // Cooldown floor. Signals that arrive during cooldown stay queued
        // (unbounded channel), so the next `recv()` returns immediately and
        // we refresh once more — picking up post-trigger activity — rather
        // than firing mid-cooldown.
        tokio::time::sleep(min_interval).await;
    }
}

/// Provider session directories to watch. Mirrors session-reader's
/// `ProviderRoots::defaults()` exactly (HOME-based, five providers) so the
/// watcher covers precisely what a rescan reparses. Note: session-reader
/// does **not** honour `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, so neither does
/// this — keep the two in lockstep.
fn provider_session_dirs() -> Vec<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(provider_dirs_for_home)
        .unwrap_or_default()
}

/// The five provider roots under `home`. Split from `provider_session_dirs`
/// so it's testable without mutating the process `HOME` (which would
/// pollute parallel tests).
fn provider_dirs_for_home(home: PathBuf) -> Vec<PathBuf> {
    let cursor = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Cursor/User/workspaceStorage")
    } else {
        home.join(".config/Cursor/User/workspaceStorage")
    };
    vec![
        home.join(".claude/projects"),
        home.join(".codex/sessions"),
        home.join(".gemini/sessions"),
        home.join(".config/github-copilot/sessions"),
        cursor,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn provider_dirs_cover_five_providers_with_expected_suffixes() {
        let dirs = provider_dirs_for_home(PathBuf::from("/tmp/ainb-watcher-home"));
        assert_eq!(dirs.len(), 5, "five provider roots");
        let joined: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
        assert!(joined.iter().any(|p| p.ends_with(".claude/projects")));
        assert!(joined.iter().any(|p| p.ends_with(".codex/sessions")));
        assert!(joined.iter().any(|p| p.ends_with(".gemini/sessions")));
        assert!(joined.iter().any(|p| p.ends_with(".config/github-copilot/sessions")));
        assert!(joined.iter().any(|p| p.to_lowercase().contains("cursor")));
    }

    #[test]
    fn start_with_no_dirs_returns_none() {
        let out = UsageDirWatcher::start_with_trigger(
            Vec::new(),
            DEBOUNCE,
            MIN_INTERVAL,
            || unreachable!("no dirs => never triggers"),
        );
        assert!(out.is_none());
    }

    // Timing tests use real (short) sleeps with generous margins, matching
    // the `live_window_watcher` convention in this crate — that avoids
    // pulling in tokio's `test-util` feature just for `start_paused`.

    #[tokio::test(flavor = "current_thread")]
    async fn burst_coalesces_into_one_trigger() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let debounce = Duration::from_millis(80);
        let min_interval = Duration::from_secs(10); // large: cooldown irrelevant here
        tokio::spawn(debounce_loop(rx, debounce, min_interval, move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));

        // A burst of 10 change signals.
        for _ in 0..10 {
            tx.send(()).unwrap();
        }
        // Before debounce elapses, nothing has fired.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(count.load(Ordering::SeqCst), 0, "no fire before debounce");

        // After debounce, exactly one fire for the whole burst.
        tokio::time::sleep(Duration::from_millis(160)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "burst coalesced to one fire");

        drop(tx); // close channel so the task can exit
    }

    #[tokio::test(flavor = "current_thread")]
    async fn min_interval_throttles_back_to_back_activity() {
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let debounce = Duration::from_millis(40);
        let min_interval = Duration::from_millis(400);
        tokio::spawn(debounce_loop(rx, debounce, min_interval, move || {
            c.fetch_add(1, Ordering::SeqCst);
        }));

        // First change -> fires after debounce.
        tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);

        // Activity during the cooldown window must NOT fire again until
        // min_interval has elapsed.
        tx.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1, "throttled during cooldown");

        // After the cooldown + a fresh debounce, the queued activity is
        // picked up and fires once more.
        tokio::time::sleep(Duration::from_millis(450)).await;
        assert_eq!(count.load(Ordering::SeqCst), 2, "refresh after cooldown");

        drop(tx);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn loop_exits_when_channel_closes() {
        let (tx, rx) = mpsc::unbounded_channel::<()>();
        let handle = tokio::spawn(debounce_loop(rx, DEBOUNCE, MIN_INTERVAL, || {}));
        drop(tx);
        // The loop's `recv()` should observe the closed channel and return.
        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("debounce loop exits promptly after channel close")
            .expect("task did not panic");
    }
}
