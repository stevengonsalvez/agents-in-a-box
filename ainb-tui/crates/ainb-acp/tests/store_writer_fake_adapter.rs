//! Reducer + store writer end to end, driven by the SCRIPTED FIXTURE adapter.
//!
//! DISCLOSURE: fixture adapter, never a real one. Proves the I10 writer half
//! (`raw_blake3` on every row, duplicate `event_id` is a no-op) and the commit
//! cadence (a 500-chunk turn is not 500 transactions).

mod support;

use std::path::Path;

use ainb_acp::client::AdapterProcess;
use ainb_acp::reducer::TranscriptReducer;
use ainb_acp::store_writer::{
    ACP_SOURCE, Lifecycle, MAX_BUFFERED_BYTES, StoreWriter, WriterConfig,
};
use ainb_hangar_core::idgen::SystemIdGen;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet_provider_event::{
    FleetProviderEventError, FleetProviderEventRepo, FleetProviderEventRow, NewFleetProviderEvent,
};
use tokio::sync::mpsc;

use support::{env, fake_config};

const SESSION_KEY: &str = "acp:01j0test";
const PROVIDER: &str = "claude-agent-acp";

fn writer(store: &Store, config: WriterConfig) -> StoreWriter {
    StoreWriter::new(
        store.clone(),
        PROVIDER,
        SESSION_KEY,
        Box::new(SystemIdGen),
        config,
    )
}

async fn rows(store: &Store) -> Vec<FleetProviderEventRow> {
    FleetProviderEventRepo::list_by_session_after(store.pool(), SESSION_KEY, 0, 10_000)
        .await
        .expect("read transcript")
}

/// The three Observability counters this phase owes the daemon health pane.
struct Counters {
    commits: u64,
    rows: u64,
    bytes: u64,
}

/// Run one scripted turn through client -> reducer -> writer.
async fn run_turn(
    store: &Store,
    script_env: Vec<(String, String)>,
    config: WriterConfig,
) -> Counters {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let adapter = AdapterProcess::spawn(&fake_config("default", script_env), tx, permission_sink())
        .await
        .expect("spawn fixture adapter");
    let session = adapter.new_session(Path::new("/tmp")).await.expect("session/new");

    let mut writer = writer(store, config);
    writer.set_acp_session_id(Some(session.clone()));
    writer
        .lifecycle(Lifecycle::TurnStarted, serde_json::json!({"turn": 1}))
        .await
        .expect("turn_started");

    adapter.prompt(&session, "go").await.expect("session/prompt");

    let mut reducer = TranscriptReducer::new(session);
    for notification in support::drain_notifications(&mut rx).await {
        for chunk in reducer.push(&notification.update) {
            writer.push(&chunk).await.expect("push chunk");
        }
    }
    if let Some(chunk) = reducer.flush() {
        writer.push(&chunk).await.expect("push tail chunk");
    }
    writer
        .lifecycle(Lifecycle::TurnCompleted, serde_json::json!({"turn": 1}))
        .await
        .expect("turn_completed");
    Counters {
        commits: writer.commits(),
        rows: writer.rows_written(),
        bytes: writer.bytes_written(),
    }
}

#[tokio::test]
async fn a_scripted_turn_lands_as_acp_rows_with_lifecycle_markers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let script = support::fixture("scripted_turn.ndjson");
    run_turn(
        &store,
        env(&[("FAKE_ACP_SCRIPT", script.to_str().expect("utf8"))]),
        WriterConfig::default(),
    )
    .await;

    let rows = rows(&store).await;
    let types: Vec<&str> = rows.iter().map(|row| row.event_type.as_str()).collect();
    assert_eq!(
        types,
        vec![
            "acp.turn_started",
            "acp.thought",
            "acp.tool_call",
            "acp.tool_call",
            "acp.plan",
            "acp.usage",
            "acp.message",
            "acp.turn_completed",
        ]
    );
    assert!(rows.iter().all(|row| row.source == ACP_SOURCE));
    assert!(rows.iter().all(|row| row.session_key.as_deref() == Some(SESSION_KEY)));
    assert!(
        rows.iter()
            .all(|row| row.provider_session_id.is_some() || row.event_type == "acp.turn_started")
    );
}

/// I10, writer half.
#[tokio::test]
async fn every_row_carries_a_64_char_blake3_digest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let script = support::fixture("scripted_turn.ndjson");
    run_turn(
        &store,
        env(&[("FAKE_ACP_SCRIPT", script.to_str().expect("utf8"))]),
        WriterConfig::default(),
    )
    .await;

    let rows = rows(&store).await;
    assert!(!rows.is_empty());
    for row in &rows {
        assert_eq!(row.raw_blake3.len(), 64, "{row:?}");
        assert!(row.raw_blake3.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(
            row.projection_revision.is_none(),
            "ACP rows carry no projection contract; NULL is their steady state"
        );
    }
}

/// I10, writer half: a committed adapter id is never overwritten, and the
/// second row never lands. The ledger enforces one contract on both doors, so
/// a reused id carrying a DIFFERENT envelope (here, a later receipt stamp) is
/// a typed collision rather than a payload the store silently drops. The exact
/// -replay no-op is pinned at the store
/// (`append_batch_replay_is_a_no_op_that_still_reports_the_original_order`).
#[tokio::test]
async fn a_reused_adapter_event_id_never_writes_a_second_row() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let mut writer = writer(&store, WriterConfig::default());

    let mut chunk = TranscriptReducer::new("fake-session-1")
        .permission_chunk(serde_json::json!({"options": ["allow"]}));
    chunk.event_id = Some("adapter-supplied-1".to_string());

    writer.push(&chunk).await.expect("first push");
    let first = writer.flush().await.expect("flush").expect("high water");
    // A later receipt stamp is what makes the second row a DIFFERENT envelope
    // under the same id, which is the case that must never overwrite.
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    writer.push(&chunk).await.expect("replayed push is buffered");
    let error = writer.flush().await.expect_err("a reused id must not overwrite");

    assert!(
        matches!(error, FleetProviderEventError::EventIdCollision { .. }),
        "a reused id is a typed collision, not a silent overwrite: {error:?}"
    );
    assert_eq!(rows(&store).await.len(), 1, "still exactly one row");
    let stored = &rows(&store).await[0];
    assert_eq!(stored.event_id, "adapter-supplied-1");
    assert_eq!(stored.ingest_order, first.ingest_order);
}

/// A batch that can NEVER commit is dropped rather than restored, and the hole
/// it leaves is recorded. Restoring it would pin it at the head of the buffer:
/// every later flush retries the same doomed rows, nothing behind them ever
/// commits, and the buffer grows until the cap starts eating live transcript.
///
/// The failure is a real permanent one (a reused `event_id` carrying a
/// different envelope), which no retry can clear while the committed row
/// stands.
#[tokio::test]
async fn a_permanently_doomed_batch_is_dropped_with_a_truncation_marker() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");

    // A row already in the ledger under the id the writer is about to reuse,
    // with a different payload.
    FleetProviderEventRepo::append(
        store.pool(),
        &NewFleetProviderEvent {
            event_id: "adapter-supplied-1".to_string(),
            provider: PROVIDER.to_string(),
            source: ACP_SOURCE.to_string(),
            session_key: Some(SESSION_KEY.to_string()),
            provider_session_id: None,
            observed_at: 1,
            received_at: 1,
            event_type: "acp.permission".to_string(),
            raw_payload: "{\"someone\":\"else\"}".to_string(),
        },
    )
    .await
    .expect("seed the colliding row");

    let mut writer = writer(&store, WriterConfig::default());
    let reducer = TranscriptReducer::new("fake-session-1");
    let mut collides = reducer.permission_chunk(serde_json::json!({"options": ["allow"]}));
    collides.event_id = Some("adapter-supplied-1".to_string());
    let survivor = reducer.permission_chunk(serde_json::json!({"options": ["deny"]}));

    writer.push(&collides).await.expect("buffer the colliding chunk");
    writer.push(&survivor).await.expect("buffer the second chunk");
    let error = writer.flush().await.expect_err("the collision must fail the commit");
    assert!(
        matches!(error, FleetProviderEventError::EventIdCollision { .. }),
        "the caller still sees the real error: {error:?}"
    );
    assert_eq!(writer.commits(), 0);
    assert_eq!(writer.rows_dropped(), 2, "both doomed rows were let go");
    assert_eq!(
        writer.buffered_rows(),
        1,
        "the doomed batch is gone, replaced by exactly one marker"
    );
    assert_eq!(
        rows(&store).await.len(),
        1,
        "the whole batch rolled back; only the seeded row is there"
    );

    // The buffer DRAINS: the very next flush commits, and what it commits is
    // the marker, so a transcript reader can see that rows went missing here
    // instead of reading a short stream as a complete one.
    writer.flush().await.expect("the next flush commits").expect("high water");
    let rows = rows(&store).await;
    assert_eq!(
        rows.iter().map(|row| row.event_type.as_str()).collect::<Vec<_>>(),
        vec!["acp.permission", "acp.transcript_truncated"],
    );
    let marker: serde_json::Value =
        serde_json::from_str(&rows[1].raw_payload).expect("marker payload");
    assert_eq!(marker["droppedRows"], 2);
}

/// A failure that CAN clear must not eat the transcript: the rows stay
/// buffered for a later flush. A closed pool is the deterministic stand-in for
/// the contended-writer case the store's own `busy_timeout` comment warns
/// about; both are environmental, so both restore.
#[tokio::test]
async fn a_transient_store_fault_keeps_its_rows_for_a_later_flush() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let mut writer = writer(&store, WriterConfig::default());
    let reducer = TranscriptReducer::new("fake-session-1");

    writer
        .push(&reducer.permission_chunk(serde_json::json!({"options": ["allow"]})))
        .await
        .expect("buffer the first chunk");
    writer
        .push(&reducer.permission_chunk(serde_json::json!({"options": ["deny"]})))
        .await
        .expect("buffer the second chunk");

    store.pool().close().await;
    let error = writer.flush().await.expect_err("a closed pool fails the commit");
    assert!(
        matches!(error, FleetProviderEventError::Sql(_)),
        "an environmental fault, not a row-shaped one: {error:?}"
    );
    assert_eq!(writer.commits(), 0);
    assert_eq!(
        writer.rows_dropped(),
        0,
        "nothing may be thrown away for a fault the operator can clear"
    );
    assert_eq!(
        writer.buffered_rows(),
        2,
        "both rows are still there for the retry"
    );
}

/// A store that keeps refusing writes must not be able to grow this buffer
/// without bound: an unbounded buffer turns one wedged session into an
/// OOM-killed daemon, which loses EVERY session's transcript instead of the
/// oldest slice of one.
#[tokio::test]
async fn a_wedged_store_cannot_grow_the_buffer_without_bound() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let mut writer = writer(&store, WriterConfig::default());
    let reducer = TranscriptReducer::new("fake-session-1");
    store.pool().close().await; // every flush from here on fails, retryably

    let blob = "x".repeat(8 * 1024);
    for index in 0..400 {
        // Errors are expected and irrelevant: what matters is that the pump
        // keeps feeding a writer that cannot commit.
        let _ = writer
            .push(&reducer.permission_chunk(serde_json::json!({"index": index, "blob": blob})))
            .await;
    }

    assert!(
        writer.buffered_bytes() <= MAX_BUFFERED_BYTES + blob.len(),
        "3 MiB of chunks left {} bytes buffered against a {MAX_BUFFERED_BYTES} cap",
        writer.buffered_bytes()
    );
    assert!(
        writer.rows_dropped() > 0,
        "the cap must actually have shed rows"
    );
    assert!(
        writer.buffered_rows() > 0,
        "the live tail of the conversation survives"
    );
}

/// The reducer's replay suppression reaches all the way to the store: a
/// `session/load` history replay writes NO transcript rows, so a resumed
/// session does not duplicate the turns that produced it.
#[tokio::test]
async fn a_replayed_history_writes_no_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let mut writer = writer(&store, WriterConfig::default());

    let mut reducer = TranscriptReducer::new("fake-session-1");
    reducer.set_replaying(true);
    for text in ["replayed ", "history"] {
        let update: agent_client_protocol::schema::v1::SessionUpdate =
            serde_json::from_value(serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": text},
            }))
            .expect("agent_message_chunk");
        for chunk in reducer.push(&update) {
            writer.push(&chunk).await.expect("push");
        }
    }
    assert!(reducer.flush().is_none(), "nothing accumulated to flush");
    writer.flush().await.expect("flush");

    assert!(
        rows(&store).await.is_empty(),
        "a replayed history must not re-persist"
    );
    assert_eq!(reducer.final_message(), "");
}

/// The cadence's timer leg is caller-driven: `tick` commits a chunk that no
/// second push would ever flush (a slow turn's first thought, then a long
/// tool call). Without it, `flush_interval` never fires at all.
#[tokio::test]
async fn a_buffered_chunk_commits_on_the_interval_without_a_second_push() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let mut writer = writer(
        &store,
        WriterConfig {
            flush_bytes: 4 * 1024,
            flush_interval: std::time::Duration::from_millis(50),
        },
    );

    let chunk = TranscriptReducer::new("fake-session-1")
        .permission_chunk(serde_json::json!({"options": ["allow"]}));
    writer.push(&chunk).await.expect("push");
    assert!(
        writer.tick().await.expect("early tick").is_none(),
        "the interval has not elapsed yet"
    );
    assert_eq!(rows(&store).await.len(), 0);

    // Poll rather than sleeping a fixed 80ms: on a loaded CI runner the
    // commit interval had not elapsed by the time the assertion ran, and the
    // test failed claiming the chunk never committed.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let high_water = loop {
        if let Some(hw) = writer.tick().await.expect("tick") {
            break hw;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the buffered chunk never committed within 10s"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };

    assert_eq!(high_water.session_key, SESSION_KEY);
    assert_eq!(rows(&store).await.len(), 1);
    assert_eq!(writer.commits(), 1);
}

/// Commit cadence: coalescing bounds rows, the cadence bounds transactions.
#[tokio::test]
async fn a_five_hundred_chunk_turn_is_not_five_hundred_transactions() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("store");
    let counters = run_turn(
        &store,
        env(&[("FAKE_ACP_CHUNKS", "500")]),
        WriterConfig {
            flush_bytes: 4 * 1024,
            // Long enough that only the byte threshold and the two lifecycle
            // markers can commit, so the assertion measures the cadence and
            // not the wall clock of a loaded CI box.
            flush_interval: std::time::Duration::from_secs(60 * 60),
        },
    )
    .await;

    assert!(
        counters.commits <= 8,
        "500 chunks issued {} transactions; the cadence is not bounding commits",
        counters.commits
    );
    let rows = rows(&store).await;
    // The Observability counters must be populated, not just declared.
    assert_eq!(counters.rows, rows.len() as u64);
    assert!(counters.bytes >= 500 * "chunk-000 ".len() as u64);
    assert!(
        rows.len() <= 8,
        "coalescing did not bound the row count: {}",
        rows.len()
    );
    let message_text: String = rows
        .iter()
        .filter(|row| row.event_type == "acp.message")
        .filter_map(|row| {
            serde_json::from_str::<serde_json::Value>(&row.raw_payload)
                .ok()
                .and_then(|value| value["text"].as_str().map(ToString::to_string))
        })
        .collect();
    assert_eq!(
        message_text.split_whitespace().count(),
        500,
        "no delta was lost to coalescing"
    );
}

/// A permission sink nothing reads: these suites drive the protocol legs, not
/// R8's answer path (that lives in the daemon's pool tests). Dropping the
/// receiver would answer every ask `Cancelled`, so the sender is leaked
/// deliberately to keep the fixture's behaviour unchanged.
fn permission_sink() -> tokio::sync::mpsc::UnboundedSender<ainb_acp::client::PermissionRequest> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(rx);
    tx
}
