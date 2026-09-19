//! The inbox reader: one task per host that reads `hangar/inbox_list` for the
//! local human on a cadence and reports into the inbox section (D3-prime).
//!
//! The daemon has no inbox subscription verb, so this polls. The shape is the
//! agent status reader's (#1215): a task the host spawns, a channel the host
//! drains on its tick, a `Dialer` that resolves a fresh client per connection,
//! and a backoff that only a connection which stayed up restarts. A host
//! starts it when something reads the section and drops it when nothing does,
//! so no daemon RPC is issued per tick for a screen nobody has open.
//!
//! ```text
//!  host tick ──drain_into──▶ section 16 ◀── updates ◀── task ──inbox_list──▶ daemon
//! ```

use std::time::Duration;

use ainb_hangar_client::{DaemonClient, DaemonError};
use ainb_hangar_proto::methods;
use ainb_hangar_proto::mutation::MutationEnvelope;
use ainb_hangar_proto::snapshots::{InboxListResult, InboxScopedParams};
use futures_util::future::BoxFuture;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::AppState;
use crate::app::sections::{INBOX_RECIPIENT, INBOX_WORKSPACE_ID};

const METHOD_NOT_FOUND: i32 = -32601;

/// The unreachable reason the section renders once the reader task has died.
pub const READER_STOPPED: &str = "inbox reader stopped";

/// Cadence and backoff bounds. Shortened only by the tests.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// Wait between two successful reads.
    pub poll: Duration,
    /// First retry wait after a failure.
    pub backoff_initial: Duration,
    /// Longest retry wait, and the wait after a daemon without the method.
    pub backoff_max: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(5),
            backoff_initial: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
        }
    }
}

/// One thing the task learned, for the host to fold into the inbox section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxUpdate {
    /// A read landed, received at this local epoch-ms clock.
    Read(InboxListResult, i64),
    /// A read failed for this reason.
    Failed(String),
    /// The daemon cannot serve `hangar/inbox_list`.
    Absent(String),
}

/// Resolves a fresh daemon client for every connection attempt.
pub type Dialer = Box<dyn Fn() -> Result<DaemonClient, DaemonError> + Send + Sync>;

/// One read of the inbox, however it is made. Production wraps a [`Dialer`];
/// tests hand in a closure so no socket is needed to prove the fold.
pub type Read =
    Box<dyn Fn() -> BoxFuture<'static, Result<InboxListResult, DaemonError>> + Send + Sync>;

/// The running reader task and the channel its updates arrive on. Dropping it
/// stops the task.
pub struct InboxReader {
    updates: mpsc::UnboundedReceiver<InboxUpdate>,
    task: JoinHandle<()>,
    stop_reported: bool,
}

impl InboxReader {
    /// Start the task over `dialer`. Must be called inside a tokio runtime.
    #[must_use]
    pub fn spawn(dialer: Dialer) -> Self {
        Self::spawn_with(read_through(dialer), Timing::default())
    }

    /// Start the task over any read, with its timing given. For tests, and
    /// for a host that already holds a client.
    #[must_use]
    pub fn spawn_with(read: Read, timing: Timing) -> Self {
        let (tx, updates) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(read, tx, timing));
        Self {
            updates,
            task,
            stop_reported: false,
        }
    }

    /// Fold every update that has arrived into the inbox section. Returns
    /// whether the section's version moved. A finished task means it died,
    /// and the section says so rather than freezing its last read as live.
    pub fn drain_into(&mut self, state: &mut AppState) -> bool {
        let mut changed = false;
        while let Ok(update) = self.updates.try_recv() {
            changed |= apply(state, update);
        }
        if !self.stop_reported && self.task.is_finished() {
            self.stop_reported = true;
            tracing::warn!("inbox: the reader task stopped");
            changed |= state.inbox_read_failed(READER_STOPPED);
        }
        changed
    }
}

impl Drop for InboxReader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Fold one update into the inbox section through its reducer entry points.
pub fn apply(state: &mut AppState, update: InboxUpdate) -> bool {
    match update {
        InboxUpdate::Read(read, received_at_ms) => state.apply_inbox_read(read, received_at_ms),
        InboxUpdate::Failed(reason) => state.inbox_read_failed(reason),
        InboxUpdate::Absent(reason) => state.inbox_absent(reason),
    }
}

/// The params every inbox read sends: the daemon's default workspace and the
/// local human, as the hangar plugin sends them. A read, so its envelope is
/// empty; the write's envelope is minted in [`crate::fleet::inbox_write`].
#[must_use]
pub fn list_params() -> InboxScopedParams {
    InboxScopedParams {
        workspace_id: INBOX_WORKSPACE_ID.to_string(),
        recipient: Some(INBOX_RECIPIENT.to_string()),
        mutation: MutationEnvelope {
            op_id: None,
            fence: None,
        },
    }
}

/// A [`Read`] that dials a fresh client per read and calls `hangar/inbox_list`.
fn read_through(dialer: Dialer) -> Read {
    let dialer = std::sync::Arc::new(dialer);
    Box::new(move || {
        let dialer = std::sync::Arc::clone(&dialer);
        Box::pin(async move {
            let client = dialer()?;
            client
                .call_typed::<InboxScopedParams, InboxListResult>(
                    methods::HANGAR_INBOX_LIST,
                    &list_params(),
                )
                .await
        })
    })
}

async fn run(read: Read, tx: mpsc::UnboundedSender<InboxUpdate>, timing: Timing) {
    let mut backoff = timing.backoff_initial;
    loop {
        let wait = match read().await {
            Ok(result) => {
                if tx.send(InboxUpdate::Read(result, crate::fleet::daemons::heartbeat::now_ms())).is_err() {
                    return;
                }
                backoff = timing.backoff_initial;
                timing.poll
            }
            Err(DaemonError::Rpc { code, message }) if code == METHOD_NOT_FOUND => {
                let reason = format!("daemon has no {}: {message}", methods::HANGAR_INBOX_LIST);
                if tx.send(InboxUpdate::Absent(reason)).is_err() {
                    return;
                }
                timing.backoff_max
            }
            Err(error) => {
                if tx.send(InboxUpdate::Failed(error.to_string())).is_err() {
                    return;
                }
                let wait = backoff;
                backoff = (backoff * 2).min(timing.backoff_max);
                wait
            }
        };
        tokio::time::sleep(wait).await;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::app::SectionId;
    use ainb_hangar_proto::events::InboxEntryRow;

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_millis(10),
            backoff_initial: Duration::from_millis(5),
            backoff_max: Duration::from_millis(20),
        }
    }

    fn row(n: usize) -> InboxEntryRow {
        InboxEntryRow {
            id: format!("01J0{n}"),
            kind: "issue".into(),
            event: "issue_created".into(),
            subject_id: format!("issue-{n}"),
            summary: format!("New issue: {n}"),
            recipient: INBOX_RECIPIENT.into(),
            created_at: n as i64,
            read_at: None,
        }
    }

    /// A read answering from a script, one answer per call, the last repeated.
    fn scripted(
        answers: Vec<Result<InboxListResult, DaemonError>>,
    ) -> (Read, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let answers = Arc::new(answers);
        let read: Read = Box::new(move || {
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let answers = Arc::clone(&answers);
            Box::pin(async move {
                let index = n.min(answers.len() - 1);
                match &answers[index] {
                    Ok(result) => Ok(result.clone()),
                    Err(DaemonError::Rpc { code, message }) => Err(DaemonError::Rpc {
                        code: *code,
                        message: message.clone(),
                    }),
                    Err(other) => Err(DaemonError::Io(other.to_string())),
                }
            })
        });
        (read, calls)
    }

    async fn drain_until(
        reader: &mut InboxReader,
        state: &mut AppState,
        ok: impl Fn(&AppState) -> bool,
    ) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !ok(state) {
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting on the reader");
            tokio::time::sleep(Duration::from_millis(2)).await;
            reader.drain_into(state);
        }
    }

    #[tokio::test]
    async fn a_read_folds_into_the_section_and_repeats_on_the_cadence() {
        let (read, calls) = scripted(vec![Ok(InboxListResult {
            entries: vec![row(1), row(2)],
            unread: 2,
        })]);
        let mut reader = InboxReader::spawn_with(read, fast());
        let mut state = AppState::new();
        let before = state.versions()[SectionId::Inbox.index()];
        drain_until(&mut reader, &mut state, |s| s.inbox.get().unread == 2).await;
        assert_eq!(state.inbox.get().entries.len(), 2);
        assert!(state.versions()[SectionId::Inbox.index()] > before);
        // It keeps reading, and the same rows again bump nothing.
        let after = state.versions()[SectionId::Inbox.index()];
        tokio::time::sleep(Duration::from_millis(60)).await;
        reader.drain_into(&mut state);
        assert!(calls.load(Ordering::SeqCst) >= 3, "the reader polls on its cadence");
        assert_eq!(state.versions()[SectionId::Inbox.index()], after);
    }

    #[tokio::test]
    async fn a_failure_keeps_the_rows_and_a_later_read_clears_it() {
        let (read, _) = scripted(vec![
            Ok(InboxListResult {
                entries: vec![row(1)],
                unread: 1,
            }),
            Err(DaemonError::Io("broken pipe".into())),
            Ok(InboxListResult {
                entries: vec![row(1)],
                unread: 1,
            }),
        ]);
        let mut reader = InboxReader::spawn_with(read, fast());
        let mut state = AppState::new();
        drain_until(&mut reader, &mut state, |s| s.inbox.get().unreachable.is_some()).await;
        assert_eq!(state.inbox.get().entries.len(), 1, "the rows stay through a failure");
        drain_until(&mut reader, &mut state, |s| s.inbox.get().unreachable.is_none()).await;
    }

    #[tokio::test]
    async fn a_daemon_without_the_method_leaves_the_section_absent() {
        let (read, calls) = scripted(vec![Err(DaemonError::Rpc {
            code: METHOD_NOT_FOUND,
            message: "method not found".into(),
        })]);
        let mut reader = InboxReader::spawn_with(read, fast());
        let mut state = AppState::new();
        drain_until(&mut reader, &mut state, |s| s.inbox.get().absent.is_some()).await;
        assert!(state.inbox.get().absent.as_deref().unwrap().contains("hangar/inbox_list"));
        let seen = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(15)).await;
        assert_eq!(calls.load(Ordering::SeqCst), seen, "an absent method waits the long backoff");
    }

    #[tokio::test]
    async fn dropping_the_reader_stops_the_task() {
        let (read, calls) = scripted(vec![Ok(InboxListResult {
            entries: Vec::new(),
            unread: 0,
        })]);
        let reader = InboxReader::spawn_with(read, fast());
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(reader);
        let seen = calls.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(calls.load(Ordering::SeqCst), seen, "no read after the drop");
    }

    #[test]
    fn the_read_names_the_default_workspace_and_the_local_human_with_no_op_id() {
        let params = list_params();
        assert_eq!(params.workspace_id, INBOX_WORKSPACE_ID);
        assert_eq!(params.recipient.as_deref(), Some(INBOX_RECIPIENT));
        assert!(params.mutation.op_id.is_none(), "a read carries no op id");
    }
}
