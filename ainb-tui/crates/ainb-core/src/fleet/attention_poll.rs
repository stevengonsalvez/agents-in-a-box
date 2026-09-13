// ABOUTME: The host's `attention/list` poller — the daemon half of the sessions
// screen's one attention surface.
//
// The local producer (notifyd's store, read straight off disk) always works.
// This adds what only the daemon knows: the attention id an answer targets, the
// structured options an ASK offers, and the request families no local hook
// classifies — `error`, `escalation`, `codex_request_user`.
//
// It runs on its own thread with its own current-thread runtime, publishing
// into a shared cell the render loop reads. The render loop never dials a
// socket: a daemon that has wedged must cost a frame nothing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_hangar_proto::{events::AttentionRow as WireRow, fleet::FleetSession};

use super::attention::{AttentionOption, DaemonAttention, SessionAttention, chip_for_daemon_kind};

/// How often the poller asks the daemon.
///
/// Matched to the sessions screen's own preview cadence: a chip that appears
/// five seconds after the agent raised it is indistinguishable from instant to
/// a human reading a list, and a tighter loop just wakes a socket for nothing.
/// The daemon also pushes `AttentionRaised`, so this poll is the floor, not the
/// only path.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The cell the render loop reads and the worker writes.
pub type Shared = Arc<Mutex<DaemonAttention>>;

/// How many times the worker has published into either cell.
///
/// The two cells above are `Arc<Mutex<..>>`, so the worker changes what the
/// sessions surface shows through a shared handle the render path only ever
/// reads by `&`. Nothing in that path takes `&mut`, so the section holding
/// them cannot notice. This counter is the one thing the render path CAN
/// compare cheaply, and folding it into a versioned field is what turns a
/// daemon-side change into a section bump.
pub type Generation = Arc<AtomicU64>;

/// Last Fleet snapshot observed off the render path.
///
/// This owns only metadata that attention/list cannot provide: the observed
/// model, reasoning effort, and direct-child count for the selected session.
pub type SnapshotShared = Arc<Mutex<Vec<FleetSession>>>;

/// Start the poller, or return `None` when one is already running.
///
/// Idempotent by an atomic flag rather than by a handle, because the caller is
/// a render loop that would otherwise have to remember whether it had started
/// one — and starting a second poller means two threads dialling one socket and
/// writing one cell in an order neither controls.
pub fn spawn(
    shared: &Shared,
    snapshot_shared: &SnapshotShared,
    running: &Arc<AtomicBool>,
    generation: &Generation,
) {
    if running.swap(true, Ordering::AcqRel) {
        return;
    }
    let shared = Arc::clone(shared);
    let snapshot_shared = Arc::clone(snapshot_shared);
    let generation = Arc::clone(generation);
    let worker_flag = Arc::clone(running);
    let spawn_err_flag = Arc::clone(running);
    let spawned = std::thread::Builder::new().name("ainb-attention-poll".into()).spawn(move || {
        // Release the flag on EVERY exit path, including an unwind, so a
        // worker that dies is replaced on the next frame rather than
        // leaving the surface permanently daemon-blind.
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = Guard(worker_flag);
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
            tracing::warn!("attention poller could not build a runtime");
            return;
        };
        runtime.block_on(async move {
            // The last rows the daemon actually reported. Carried across a
            // failed poll so a transient socket error greys the chips
            // rather than making them disappear — a request that vanishes
            // because one read timed out is a request nobody answers.
            let mut last_good = DaemonAttention::default();
            let mut last_snapshot: Vec<FleetSession> = Vec::new();
            loop {
                let next = poll_once(&last_good).await;
                if next.reachable {
                    last_good = next.clone();
                }
                if let Ok(mut cell) = shared.lock() {
                    *cell = next;
                }
                if let Some(snapshot) = poll_snapshot_once().await {
                    last_snapshot = snapshot;
                }
                if let Ok(mut cell) = snapshot_shared.lock() {
                    *cell = last_snapshot.clone();
                }
                // Published AFTER both cells, so a render that sees the new
                // generation sees the rows that go with it.
                generation.fetch_add(1, Ordering::Release);
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        });
    });
    if let Err(error) = spawned {
        tracing::warn!(%error, "attention poller thread spawn failed");
        spawn_err_flag.store(false, Ordering::Release);
    }
}

/// Read Fleet metadata without changing attention reachability.
///
/// Metadata is additive. A failed snapshot must preserve the last good values
/// instead of blanking model and effort while the daemon is briefly busy.
async fn poll_snapshot_once() -> Option<Vec<FleetSession>> {
    let client = crate::fleet::bridge::daemon::DaemonClient::from_env().ok()?;
    match client.fleet_snapshot().await {
        Ok(snapshot) => Some(snapshot.sessions),
        Err(error) => {
            tracing::debug!(%error, "fleet snapshot poll failed");
            None
        }
    }
}

/// One poll. Never panics; every failure becomes a named, reportable reason.
///
/// `last_good` is what the daemon reported when it last answered, handed back
/// on a failure so the surface greys its chips instead of losing them.
async fn poll_once(last_good: &DaemonAttention) -> DaemonAttention {
    let client = match crate::fleet::bridge::daemon::DaemonClient::from_env() {
        Ok(client) => client,
        // Not an error worth a banner: no hangar home configured is the normal
        // state of a host that never ran the daemon.
        //
        // The classification travels WITH the reason, from the typed error
        // rather than its wording: this cell is the host's only off-thread
        // answer to "is there a daemon at all", and the Pal pane's offer to
        // start one is only honest if that answer is.
        Err(error) => {
            return DaemonAttention::down_cached(
                last_good.clone(),
                error.to_string(),
                error.means_not_running(),
            );
        }
    };
    let socket = client.socket().display().to_string();
    match client.attention_list_fleet().await {
        Ok(rows) => {
            let grouped = group_rows(&rows);
            DaemonAttention::up_indexed(
                grouped.by_session_id,
                grouped.by_cwd,
                grouped.by_cwd_without_session_id,
                grouped.all,
            )
        }
        // Name the socket. "attention/list failed" without it leaves the
        // operator guessing which daemon, which home, which socket.
        Err(error) => {
            let not_running = error.means_not_running();
            DaemonAttention::down_cached(
                last_good.clone(),
                format!("attention/list via {socket}: {error}"),
                not_running,
            )
        }
    }
}

/// The three indexes one daemon attention list needs.
///
/// Provider session id is primary. Cwd remains only a unique fallback for
/// legacy local rows that have not yet learned their provider id.
#[derive(Default)]
struct GroupedRows {
    by_session_id: HashMap<String, Vec<SessionAttention>>,
    by_cwd: HashMap<String, Vec<SessionAttention>>,
    by_cwd_without_session_id: HashMap<String, Vec<SessionAttention>>,
    all: HashMap<String, SessionAttention>,
}

/// Fold wire rows into exact-session and guarded-cwd indexes.
fn group_rows(rows: &[WireRow]) -> GroupedRows {
    let mut grouped = GroupedRows::default();
    for row in rows {
        // A kind this build does not know renders as NO chip rather than a
        // wrong one; the header's elsewhere count is what reports it honestly.
        let Some(kind) = chip_for_daemon_kind(&row.kind) else {
            continue;
        };
        let payload: serde_json::Value =
            serde_json::from_str(&row.payload).unwrap_or(serde_json::Value::Null);
        let chip = SessionAttention::daemon(kind, row.created_at, row.id.clone())
            .with_detail(question_of(&payload).unwrap_or_default())
            .with_options(options_of(&payload));
        if !row.session_id.is_empty() {
            grouped
                .by_session_id
                .entry(row.session_id.clone())
                .or_default()
                .push(chip.clone());
        }
        let cwd = row.cwd.trim_end_matches('/');
        if !cwd.is_empty() {
            grouped.by_cwd.entry(cwd.to_string()).or_default().push(chip.clone());
            if row.session_id.is_empty() {
                grouped
                    .by_cwd_without_session_id
                    .entry(cwd.to_string())
                    .or_default()
                    .push(chip.clone());
            }
        }
        grouped.all.insert(row.id.clone(), chip);
    }
    grouped
}

#[cfg(test)]
fn group_by_cwd(rows: &[WireRow]) -> HashMap<String, Vec<SessionAttention>> {
    group_rows(rows).by_cwd
}

/// The one-line question an attention payload is asking, if it says.
///
/// Every producer nests it differently and none of them is guaranteed, so this
/// walks the known shapes and gives up rather than inventing a line: a chip
/// with no detail renders as the chip alone, which is honest. A fabricated
/// "waiting for input" would read as something the agent actually said.
fn question_of(payload: &serde_json::Value) -> Option<String> {
    const PATHS: &[&str] = &[
        // Claude AskUserQuestion, first question.
        "/tool_input/questions/0/question",
        "/payload/tool_input/questions/0/question",
        // Approval / notification prose.
        "/message",
        "/payload/message",
        // ATC escalation.
        "/reason",
    ];
    PATHS
        .iter()
        .find_map(|path| payload.pointer(path).and_then(serde_json::Value::as_str))
        .map(|found| found.trim().to_string())
        .filter(|found| !found.is_empty())
}

/// The structured options an ASK offers, or empty for free text.
fn options_of(payload: &serde_json::Value) -> Vec<AttentionOption> {
    const PATHS: &[&str] = &[
        "/tool_input/questions/0/options",
        "/payload/tool_input/questions/0/options",
    ];
    PATHS
        .iter()
        .find_map(|path| payload.pointer(path).and_then(serde_json::Value::as_array))
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    let label = option.get("label").and_then(serde_json::Value::as_str)?;
                    Some(AttentionOption {
                        label: label.to_string(),
                        description: option
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;
    use crate::fleet::attention::{Answerable, AttentionKind, AttentionSource};

    fn wire(id: &str, kind: &str, cwd: &str, payload: serde_json::Value) -> WireRow {
        WireRow {
            id: id.to_string(),
            session_id: format!("provider-{id}"),
            cwd: cwd.to_string(),
            workspace_id: None,
            kind: kind.to_string(),
            payload: payload.to_string(),
            degraded: false,
            created_at: 1_000,
            channels: ainb_hangar_proto::ChannelSet::default(),
            version: 1,
        }
    }

    #[test]
    fn every_answerable_daemon_kind_maps_to_a_chip() {
        for (kind, expected) in [
            ("ask_user_question", AttentionKind::Ask),
            ("waiting", AttentionKind::Wait),
            ("codex_request_user", AttentionKind::Ask),
            ("approval", AttentionKind::Approve),
            ("error", AttentionKind::Err),
            ("escalation", AttentionKind::Err),
        ] {
            assert_eq!(chip_for_daemon_kind(kind), Some(expected), "kind {kind}");
        }
    }

    #[test]
    fn an_unknown_kind_renders_no_chip_rather_than_a_wrong_one() {
        assert_eq!(
            chip_for_daemon_kind("something_a_newer_daemon_raises"),
            None
        );
        let grouped = group_by_cwd(&[wire("a", "something_new", "/w", serde_json::json!({}))]);
        assert!(grouped.is_empty());
    }

    #[test]
    fn a_row_with_no_cwd_is_never_guessed_onto_a_session() {
        let grouped = group_by_cwd(&[wire("a", "approval", "", serde_json::json!({}))]);
        assert!(
            grouped.is_empty(),
            "a cwd-less row has no session to land on; guessing one delivers an \
             answer into the wrong agent"
        );
    }

    #[test]
    fn a_cwdless_row_is_retained_under_its_exact_provider_session_id() {
        let grouped = group_rows(&[wire("a", "approval", "", serde_json::json!({}))]);
        assert_eq!(grouped.by_session_id["provider-a"].len(), 1);
        assert_eq!(grouped.all.len(), 1, "an elsewhere count can still see it");
    }

    #[test]
    fn a_daemon_row_keeps_its_attention_id_options_and_question() {
        let grouped = group_by_cwd(&[wire(
            "att-1",
            "ask_user_question",
            "/work/proj/",
            serde_json::json!({
                "payload": {
                    "tool_input": {
                        "questions": [{
                            "question": "Decide the sqlite path",
                            "options": [
                                {"label": "data/box.db", "description": "repo-root data dir"},
                                {"label": "api/src/db.sqlite"}
                            ]
                        }]
                    }
                }
            }),
        )]);
        // Trailing slash normalised, so it matches a session's own worktree path.
        let rows = grouped.get("/work/proj").expect("grouped under the trimmed cwd");
        let row = &rows[0];
        assert_eq!(row.kind, AttentionKind::Ask);
        assert_eq!(row.source, AttentionSource::Daemon);
        assert_eq!(row.detail.as_deref(), Some("Decide the sqlite path"));
        assert_eq!(row.options.len(), 2);
        assert_eq!(row.options[0].label, "data/box.db");
        assert_eq!(row.options[0].description, "repo-root data dir");
        // The option with no description carries an empty one, not the previous
        // option's — a description shifted by one is worse than none.
        assert_eq!(row.options[1].description, "");
        assert_eq!(
            row.answerable,
            Answerable::Daemon {
                attention_id: "att-1".to_string()
            }
        );
    }

    #[test]
    fn a_payload_with_no_question_carries_no_invented_one() {
        let grouped = group_by_cwd(&[wire(
            "att-2",
            "approval",
            "/work/proj",
            serde_json::json!({ "kind": "approval" }),
        )]);
        assert_eq!(grouped["/work/proj"][0].detail, None);
        assert!(grouped["/work/proj"][0].options.is_empty());
    }

    #[test]
    fn an_escalation_reason_becomes_the_detail_line() {
        let grouped = group_by_cwd(&[wire(
            "att-3",
            "escalation",
            "/work/proj",
            serde_json::json!({ "reason": "retry budget exhausted" }),
        )]);
        assert_eq!(
            grouped["/work/proj"][0].detail.as_deref(),
            Some("retry budget exhausted")
        );
    }

    #[test]
    fn a_malformed_payload_still_yields_its_chip() {
        // The chip is what tells the operator something needs them. A payload
        // that will not parse must cost the DETAIL, never the chip.
        let mut row = wire("att-4", "approval", "/work/proj", serde_json::json!({}));
        row.payload = "{not json".to_string();
        let grouped = group_by_cwd(&[row]);
        assert_eq!(grouped["/work/proj"][0].kind, AttentionKind::Approve);
        assert_eq!(grouped["/work/proj"][0].detail, None);
    }

    #[test]
    fn a_down_daemon_keeps_its_last_rows_and_says_why() {
        // The chip is what tells the operator something needs them. Losing it
        // because one socket read timed out is the silent no-op the spec
        // forbids; greying it and naming the failed call is not.
        let previous = group_by_cwd(&[wire(
            "att-1",
            "approval",
            "/work/proj",
            serde_json::json!({}),
        )]);
        let down = DaemonAttention::down(
            previous,
            "attention/list via /x/hangar.sock: refused".into(),
            true,
        );
        assert!(!down.reachable);
        assert_eq!(
            down.rows_for("/work/proj").len(),
            1,
            "a transient failure must not make a live request disappear"
        );
        assert!(
            down.error.as_deref().is_some_and(|e| e.contains("/x/hangar.sock")),
            "the reason must name the socket"
        );
    }

    #[test]
    fn a_daemon_that_never_answered_has_nothing_to_carry_forward() {
        let down = DaemonAttention::down(HashMap::new(), "no hangar home".into(), false);
        assert!(down.rows_for("/work/proj").is_empty());
    }

    #[test]
    fn unmatched_rows_are_counted_elsewhere_not_dropped_silently() {
        let grouped = group_by_cwd(&[
            wire("a", "approval", "/on/screen", serde_json::json!({})),
            wire(
                "b",
                "ask_user_question",
                "/off/screen",
                serde_json::json!({}),
            ),
            // An ERR blocks nobody, so it is not in the needs-you count even
            // when it is elsewhere.
            wire("c", "error", "/off/screen", serde_json::json!({})),
        ]);
        let up = DaemonAttention::up(grouped);
        let claimed: HashSet<String> = ["a".to_string()].into_iter().collect();
        assert_eq!(up.elsewhere(&claimed), 1);
    }
}
