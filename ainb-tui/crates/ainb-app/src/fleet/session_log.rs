// ABOUTME: The `log` tab's notification history, read off the render thread on
// one long-lived read-only handle.
//
// It used to be read INSIDE `terminal.draw`: every repaint of the `log` tab
// called `Store::open` (read-write, `PRAGMA journal_mode=WAL`, the full
// `CREATE ... IF NOT EXISTS` batch), pulled up to 4000 rows, and closed the
// connection again. Three separate costs, all on the UI thread:
//
// 1. the open itself, which on a store the daemon has been writing to for
//    months is not free;
// 2. the schema migration, which is the TUI applying DDL to a database the
//    daemon owns;
// 3. the CLOSE, which on the last connection CHECKPOINTS the WAL. Measured
//    against a byte-copy of a real store (546 MB file, 9 927 rows, 73.5 MB
//    WAL): 948 ms for the frame that inherited the fat WAL, 37-631 ms per
//    frame after that.
//
// The render loop repaints on every keystroke and every 250 ms app tick, so
// that is a pane which answers a `Tab` press a second later. This module is
// where that read went: one worker thread, one read-only connection held open,
// a refresh cadence instead of a frame cadence, and a render path that only
// ever reads a `Vec` someone else filled in.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::components::session_tabs::LogRow;

/// How often the worker re-reads while the pane stays on one session.
///
/// A notification history is not a live stream — a row that appears half a
/// second late is indistinguishable from instant. A CHANGE of session does not
/// wait for this: an unanswered key outranks the interval in [`next_action`],
/// and [`Shared::read`] signals the condvar so a worker already asleep on it
/// wakes rather than serving the switch up to `REFRESH` late.
const REFRESH: Duration = Duration::from_millis(750);

/// How long the worker waits before retrying after a failed read.
///
/// Longer than [`REFRESH`] on purpose: the usual failure is "the store does not
/// exist on this machine", which will still be true 750 ms from now, and
/// retrying it at the refresh cadence is a syscall per second forever.
///
/// Enforced by `next_attempt`, NOT by how long the worker happens to sleep. It
/// used to be a `wait_timeout` argument, which the render path could defeat
/// without meaning to: every frame raises `asked` and signals, so a failing
/// store was re-opened on every keystroke and the backoff never happened.
const RETRY: Duration = Duration::from_secs(5);

/// Which session's history is wanted.
///
/// The worktree path and the agent's hook name, which is the same identity the
/// attention producer files notifyd rows under — a session is not identified in
/// that store by ainb's own UUID, which the hook never sees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogKey {
    /// The session's worktree path, trailing slash trimmed.
    pub cwd: String,
    /// The agent's hook name, or `None` to accept every agent in that cwd.
    pub agent: Option<String>,
}

impl LogKey {
    /// The key for a session at `cwd` running `agent`.
    #[must_use]
    pub fn new(cwd: &str, agent: Option<&str>) -> Self {
        Self {
            cwd: cwd.trim_end_matches('/').to_string(),
            agent: agent.map(ToString::to_string),
        }
    }
}

/// What the worker has published, and what it should read next.
#[derive(Debug, Default)]
struct Published {
    /// The key the render path last asked for.
    want: Option<LogKey>,
    /// The key `rows` actually belongs to, so a stale answer is never rendered
    /// under a session it is not about.
    have: Option<LogKey>,
    /// The rows for `have`.
    rows: Vec<LogRow>,
    /// The key a read FAILED for, and why.
    ///
    /// Keyed for the same reason `rows` is. An un-keyed reason is a failure
    /// about one session rendered under whichever session the cursor is on
    /// next: move off a row whose store read failed and the healthy row beside
    /// it paints the red "could not be read" line, on evidence that was never
    /// about it.
    error: Option<(LogKey, String)>,
    /// Whether the render path has asked for rows since the last attempt.
    ///
    /// This is what stops the worker outliving the pane. Set by every frame
    /// that wants the log, cleared by every attempt the worker makes: once the
    /// operator leaves the tab nothing sets it again, the worker parks on the
    /// condvar, and a session nobody is looking at stops costing a query every
    /// 750 ms for the rest of the process's life.
    asked: bool,
    /// The earliest the worker may attempt a read again.
    ///
    /// Where both cadences actually live: `REFRESH` after a success, `RETRY`
    /// after a failure. Held here rather than in the worker's sleep duration
    /// because the render path can end that sleep at any moment, and a backoff
    /// a keystroke can cancel is not a backoff.
    next_attempt: Option<Instant>,
}

/// The cell the render loop reads and the worker writes.
#[derive(Debug, Default)]
pub struct Shared {
    published: Mutex<Published>,
    /// Signalled when the wanted key changes or a frame starts asking again.
    wake: Condvar,
    /// How many times [`Shared::read`] has signalled the worker.
    ///
    /// Test-only: the alternative is asserting on a condvar, and a missed
    /// signal is invisible from the outside until it shows up as a pane that
    /// took 750 ms to notice the cursor moved.
    #[cfg(test)]
    signals: std::sync::atomic::AtomicUsize,
}

/// What the `log` pane has to paint right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Log {
    /// The history for the requested session.
    Rows(Vec<LogRow>),
    /// The worker has not answered for THIS session yet.
    Reading,
    /// The read failed, with the reason.
    Failed(String),
}

impl Shared {
    fn guard(&self) -> std::sync::MutexGuard<'_, Published> {
        self.published.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Ask for `key`, and return whatever is available for it right now.
    ///
    /// One call, not a request and a separate read: the render path wants the
    /// rows for the session under the cursor, and splitting that into two
    /// locked operations is how a caller ends up rendering the previous
    /// session's history under the current session's name.
    #[must_use]
    pub fn read(&self, key: &LogKey) -> Log {
        let mut published = self.guard();
        let switched = published.want.as_ref() != Some(key);
        if switched {
            published.want = Some(key.clone());
        }
        let woke = !published.asked;
        published.asked = true;
        // BOTH edges. `woke` alone is not enough: a switch that happens while a
        // frame is already asking leaves the worker asleep on the refresh
        // interval, so the new session's history arrives up to `REFRESH` late
        // on the one action where the delay is visible. Signalled while this
        // lock is still held; the worker simply blocks on the mutex until the
        // guard drops at the end of the function.
        if switched || woke {
            #[cfg(test)]
            self.signals.fetch_add(1, Ordering::Relaxed);
            self.wake.notify_one();
        }
        if published.have.as_ref() == Some(key) {
            return Log::Rows(published.rows.clone());
        }
        match &published.error {
            Some((failed, reason)) if failed == key => Log::Failed(reason.clone()),
            // A failure recorded against a DIFFERENT session says nothing about
            // this one, so this one is still simply unread.
            _ => Log::Reading,
        }
    }

    /// How many times a `read` has signalled the worker.
    #[cfg(test)]
    fn signals(&self) -> usize {
        self.signals.load(Ordering::Relaxed)
    }
}

/// What the worker should do next.
#[derive(Debug, PartialEq, Eq)]
enum Action {
    /// Read this key now.
    Read(LogKey),
    /// Nothing is due yet; sleep at most this long.
    Wait(Duration),
    /// Nobody is looking at the pane. Sleep until signalled.
    Park,
}

/// Decide the worker's next move.
///
/// Pure, and separate from the loop, because every one of the three cadences
/// this balances — park when unwatched, refresh while watched, back off after a
/// failure — is a rule about state rather than about threading, and each is
/// only checkable in isolation.
fn next_action(published: &Published, now: Instant) -> Action {
    if !published.asked {
        return Action::Park;
    }
    let Some(want) = published.want.clone() else {
        // Asked with nothing to ask FOR. Not reachable through `read`, which
        // always sets a key; treated as "wait" rather than as a spin.
        return Action::Wait(REFRESH);
    };
    // Neither an answer nor a failure for this key yet — this is a session the
    // worker has never served, so it outranks whatever interval is running. The
    // failure half matters: without it, a key that just failed would look
    // unanswered forever and retry in a tight loop.
    let answered = published.have.as_ref() == Some(&want);
    let failed = published.error.as_ref().is_some_and(|(key, _)| *key == want);
    if !answered && !failed {
        return Action::Read(want);
    }
    match published.next_attempt {
        Some(at) if at > now => Action::Wait(at.saturating_duration_since(now)),
        _ => Action::Read(want),
    }
}

/// Start the worker, or return immediately when one is already running.
///
/// Idempotent by an atomic flag, and released on every exit path including an
/// unwind, so a worker that dies is replaced on the next frame rather than
/// leaving the pane permanently stuck on "reading". Same shape, and for the
/// same reason, as [`crate::fleet::attention_poll::spawn`].
pub fn spawn(shared: &Arc<Shared>, running: &Arc<AtomicBool>) {
    if running.swap(true, Ordering::AcqRel) {
        return;
    }
    let shared = Arc::clone(shared);
    let worker_flag = Arc::clone(running);
    let spawn_err_flag = Arc::clone(running);
    let spawned = std::thread::Builder::new().name("ainb-session-log".into()).spawn(move || {
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = Guard(worker_flag);
        worker(&shared);
    });
    if let Err(error) = spawned {
        tracing::warn!(%error, "session log worker thread spawn failed");
        spawn_err_flag.store(false, Ordering::Release);
    }
}

/// How many rows the pane shows.
///
/// A history an operator scrolls, not a stream: 200 lines is more than anyone
/// reads in one sitting and bounds what the query has to carry.
const LIMIT: u32 = 200;

/// The worker loop: read what is wanted, publish it, wait for a change or the
/// refresh cadence.
fn worker(shared: &Shared) {
    // Opened once and held. The per-call open is the cost this module exists to
    // remove, and a read-only handle additionally cannot migrate the daemon's
    // schema or checkpoint its WAL.
    let mut store: Option<ainb_plugin_notifyd::Store> = None;
    loop {
        let action = {
            let published = shared.guard();
            let action = next_action(&published, Instant::now());
            // Decided AND waited on under one guard, so a `read` landing in
            // between cannot have its signal lost: `wait` releases the mutex
            // atomically, and that `read` is still blocked on it.
            match action {
                Action::Park => {
                    let _unused = shared.wake.wait(published);
                    continue;
                }
                Action::Wait(interval) => {
                    let _unused = shared.wake.wait_timeout(published, interval);
                    continue;
                }
                Action::Read(key) => key,
            }
        };
        match read_once(&mut store, &action) {
            Ok(rows) => {
                let mut published = shared.guard();
                published.have = Some(action);
                published.rows = rows;
                published.error = None;
                published.asked = false;
                published.next_attempt = Some(Instant::now() + REFRESH);
            }
            Err(reason) => {
                // Drop the handle: the usual causes (the file was replaced, the
                // daemon rebuilt it) are not fixed by reusing it.
                store = None;
                let mut published = shared.guard();
                published.error = Some((action, reason));
                published.asked = false;
                published.next_attempt = Some(Instant::now() + RETRY);
            }
        }
    }
}

/// One read, against a handle opened on demand.
fn read_once(
    store: &mut Option<ainb_plugin_notifyd::Store>,
    key: &LogKey,
) -> Result<Vec<LogRow>, String> {
    if store.is_none() {
        let paths = ainb_plugin_notifyd::Paths::from_home().map_err(|error| error.to_string())?;
        if !paths.db.exists() {
            return Err(format!(
                "no notification store at {} yet",
                paths.db.display()
            ));
        }
        // READ-ONLY. The TUI is a reader of a database the daemon owns: it must
        // not create it, must not apply DDL to it, and must not checkpoint its
        // WAL — the last of which is what made a single frame cost 948 ms.
        *store = Some(
            ainb_plugin_notifyd::Store::open_readonly(&paths.db)
                .map_err(|error| error.to_string())?,
        );
    }
    let store = store.as_ref().expect("opened above");
    // Filtered in SQL, not here. Reading the newest 4000 rows fleet-wide and
    // keeping the handful for one cwd is what made this expensive enough to
    // matter — see `Store::recent_for_cwd`.
    let records = store
        .recent_for_cwd(&key.cwd, key.agent.as_deref(), 0, LIMIT)
        .map_err(|error| error.to_string())?;
    Ok(crate::components::session_tabs::log_rows(
        &records,
        &key.cwd,
        key.agent.as_deref(),
        LIMIT as usize,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: i64) -> LogRow {
        LogRow {
            ts,
            event: "Notification".to_string(),
            detail: "hello".to_string(),
        }
    }

    #[test]
    fn a_key_normalises_its_trailing_slash() {
        assert_eq!(
            LogKey::new("/tmp/work/", Some("claude")),
            LogKey::new("/tmp/work", Some("claude"))
        );
    }

    #[test]
    fn an_unanswered_key_reads_as_reading_not_as_empty() {
        let shared = Shared::default();
        // The distinction the pane needs: "no rows yet" is not "this session
        // has no history", and rendering the empty state for the first is how
        // an operator concludes their log is gone.
        assert_eq!(shared.read(&LogKey::new("/tmp/a", None)), Log::Reading);
    }

    #[test]
    fn rows_are_only_returned_for_the_key_they_were_read_for() {
        let shared = Shared::default();
        let a = LogKey::new("/tmp/a", None);
        let b = LogKey::new("/tmp/b", None);
        {
            let mut published = shared.guard();
            published.have = Some(a.clone());
            published.rows = vec![row(1)];
        }
        assert_eq!(shared.read(&a), Log::Rows(vec![row(1)]));
        // `b` must NOT inherit `a`'s history just because it is what the worker
        // last published.
        assert_eq!(shared.read(&b), Log::Reading);
    }

    #[test]
    fn reading_records_what_the_worker_should_fetch_next() {
        let shared = Shared::default();
        let key = LogKey::new("/tmp/a", Some("claude"));
        let _unused = shared.read(&key);
        assert_eq!(shared.guard().want, Some(key));
    }

    /// The worker must not outlive the pane. Every frame that wants rows raises
    /// `asked`; the worker lowers it after each read and parks when it is down,
    /// so leaving the tab stops the query rather than leaving it running for
    /// the rest of the process's life.
    #[test]
    fn a_pane_nobody_is_looking_at_stops_asking() {
        let shared = Shared::default();
        let key = LogKey::new("/tmp/a", None);
        let _unused = shared.read(&key);
        assert!(
            shared.guard().asked,
            "a frame that wants rows asks for them"
        );

        // The worker completing a read.
        {
            let mut published = shared.guard();
            published.have = Some(key.clone());
            published.rows = vec![row(1)];
            published.asked = false;
        }
        // No further frames: nothing raises it again, so the worker parks.
        assert!(
            !shared.guard().asked,
            "a closed pane must leave nothing for the worker to refresh"
        );

        // Re-opening the pane raises it again.
        assert_eq!(shared.read(&key), Log::Rows(vec![row(1)]));
        assert!(
            shared.guard().asked,
            "and a frame that comes back asks again"
        );
    }

    #[test]
    fn a_failed_read_is_reported_rather_than_rendered_as_no_history() {
        let shared = Shared::default();
        let key = LogKey::new("/tmp/a", None);
        {
            let mut published = shared.guard();
            published.error = Some((key.clone(), "no notification store yet".to_string()));
        }
        assert_eq!(
            shared.read(&key),
            Log::Failed("no notification store yet".to_string())
        );
    }

    /// A failure is evidence about the session it was observed on, and about no
    /// other. Un-keyed, the red "could not be read" line followed the cursor
    /// onto healthy rows — an outcome painted under the wrong subject, which is
    /// the class of bug this whole surface exists to remove.
    #[test]
    fn a_failure_on_one_session_is_not_reported_against_another() {
        let shared = Shared::default();
        let broken = LogKey::new("/tmp/broken", None);
        let healthy = LogKey::new("/tmp/healthy", None);
        {
            let mut published = shared.guard();
            published.error = Some((broken.clone(), "no notification store yet".to_string()));
        }
        assert_eq!(
            shared.read(&broken),
            Log::Failed("no notification store yet".to_string()),
            "the session it actually happened to still reports it"
        );
        assert_eq!(
            shared.read(&healthy),
            Log::Reading,
            "and the session beside it is unread, not broken"
        );
    }

    /// A switch has to wake a worker that is already asleep on the interval.
    /// Signalling only on the asked edge leaves it sleeping, so the new
    /// session's history lands up to `REFRESH` late on the one action where the
    /// operator is watching for it.
    #[test]
    fn a_session_switch_signals_the_worker_even_while_a_frame_is_already_asking() {
        let shared = Shared::default();
        let a = LogKey::new("/tmp/a", None);
        let b = LogKey::new("/tmp/b", None);

        let _unused = shared.read(&a);
        assert_eq!(shared.signals(), 1, "the first frame wakes a parked worker");
        let _unused = shared.read(&a);
        assert_eq!(
            shared.signals(),
            1,
            "and every frame after it says nothing new"
        );

        let _unused = shared.read(&b);
        assert_eq!(
            shared.signals(),
            2,
            "but moving the cursor does, or the switch waits out the interval"
        );
    }

    /// The backoff has to be a fact about STATE, not about how long the worker
    /// happens to sleep. Held in the sleep duration, the render path cancelled
    /// it without meaning to: every frame raises `asked` and signals, so a
    /// missing store was re-opened on every keystroke.
    #[test]
    fn a_failed_read_backs_off_even_while_frames_keep_asking() {
        let now = Instant::now();
        let key = LogKey::new("/tmp/a", None);
        let published = Published {
            want: Some(key.clone()),
            have: None,
            rows: Vec::new(),
            error: Some((key, "no notification store yet".to_string())),
            // A frame asked again immediately, which is what the render loop
            // does four times a second.
            asked: true,
            next_attempt: Some(now + RETRY),
        };
        match next_action(&published, now) {
            Action::Wait(left) => assert!(
                left > REFRESH,
                "a failed read must wait out RETRY, not the refresh interval: {left:?}"
            ),
            other => panic!("a failing store must not be re-opened per frame, got {other:?}"),
        }
    }

    /// The other side of the same rule: once the window has passed, the retry
    /// actually happens.
    #[test]
    fn the_retry_fires_once_the_backoff_has_elapsed() {
        let now = Instant::now();
        let key = LogKey::new("/tmp/a", None);
        let published = Published {
            want: Some(key.clone()),
            have: None,
            rows: Vec::new(),
            error: Some((key.clone(), "no notification store yet".to_string())),
            asked: true,
            next_attempt: Some(now - Duration::from_millis(1)),
        };
        assert_eq!(next_action(&published, now), Action::Read(key));
    }

    /// A session the worker has never served outranks whatever interval is
    /// running: the switch is the one moment the delay is visible.
    #[test]
    fn an_unanswered_session_is_read_before_the_interval_elapses() {
        let now = Instant::now();
        let switched_to = LogKey::new("/tmp/b", None);
        let published = Published {
            want: Some(switched_to.clone()),
            have: Some(LogKey::new("/tmp/a", None)),
            rows: vec![row(1)],
            error: None,
            asked: true,
            // Mid-refresh for the PREVIOUS session.
            next_attempt: Some(now + REFRESH),
        };
        assert_eq!(next_action(&published, now), Action::Read(switched_to));
    }

    /// And an unwatched pane parks, whatever the intervals say.
    #[test]
    fn an_unwatched_pane_parks() {
        let now = Instant::now();
        let published = Published {
            want: Some(LogKey::new("/tmp/a", None)),
            have: None,
            rows: Vec::new(),
            error: None,
            asked: false,
            next_attempt: None,
        };
        assert_eq!(next_action(&published, now), Action::Park);
    }
}
