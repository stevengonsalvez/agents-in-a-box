//! P8.3 — every store-side FSM transition (and the autopilot tick fire) emits a
//! structured tracing span with an exact name and the required fields.
//!
//! These spans are the daemon's observability surface: a single run shows up as
//! `task.claim -> task.start -> task.complete` (or `.fail` / `.cancel`) and an
//! autopilot firing shows up as `autopilot.tick`, each carrying the ids an
//! operator filters by. The field key names mirror Multica's structured `slog`
//! keys (`task_id`, `workspace_id`, `runtime_id`, `outcome`, ...).
//!
//! ## Stale-plan → real-method mapping
//!
//! The P8 plan names services that do not exist by those names. The real fns:
//!
//! | Plan row            | Real method                                            | Span           |
//! |---------------------|--------------------------------------------------------|----------------|
//! | `TaskService::claim`    | `service::claim::ClaimTaskService::claim_for_runtime` | `task.claim`    |
//! | `TaskService::start`    | `service::start::StartTaskService::start`             | `task.start`    |
//! | `TaskService::complete` | `service::complete::CompleteTaskService::complete`    | `task.complete` |
//! | `TaskService::fail`     | `service::fail::FailTaskService::fail`                | `task.fail`     |
//! | `TaskService::cancel`   | `service::cancel::CancelTaskService::cancel`          | `task.cancel`   |
//! | `AutopilotService::tick`| `repo::autopilot_run::fire_autopilot_tick`            | `autopilot.tick`|
//!
//! The two beads rows (`beads.push` / `beads.pull`) live in `ainb-hangar-daemon`
//! and are covered by that crate's `beads_sync_spans_emit.rs`.

use std::sync::{Arc, Mutex};

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_core::ids::{AgentId, WorkspaceId};
use ainb_hangar_store::repo::autopilot::{AutopilotRepo, NewAutopilot};
use ainb_hangar_store::repo::task::{NewTask, TaskRepo};
use ainb_hangar_store::service::cancel::CancelTaskService;
use ainb_hangar_store::service::claim::ClaimTaskService;
use ainb_hangar_store::service::complete::{CompleteParams, CompleteTaskService};
use ainb_hangar_store::service::fail::{FailTaskService, FailureReason};
use ainb_hangar_store::service::start::StartTaskService;
use ainb_hangar_store::Store;
use tracing::field::{Field, Visit};
use tracing::subscriber::set_default;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Fixed clock instant all tests fire at (epoch-ms, 2026-01-01T00:00:00Z).
const T0: i64 = 1_767_225_600_000;

/// One captured span: its metadata name and the field key/value pairs recorded
/// onto it (both at `new_span` and via later `record` calls).
#[derive(Debug, Clone, Default)]
struct CapturedSpan {
    name: String,
    fields: Vec<(String, String)>,
}

impl CapturedSpan {
    /// The recorded value of `key`, if the field was set.
    fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A handle to one captured span that both the shared log and the span's
/// extensions point at, so a `record` call mutates the same entry the log holds.
type SpanHandle = Arc<Mutex<CapturedSpan>>;

/// Shared buffer of every span this layer observed. Each entry is pushed at
/// `on_new_span` (so it exists the moment the span opens, independent of close
/// timing) and mutated in place by later `on_record` calls.
type SpanLog = Arc<Mutex<Vec<SpanHandle>>>;

/// A minimal `tracing_subscriber::Layer` that records each span's name and the
/// fields set on it (at creation and via `Span::record`). Field values are
/// captured via their `Debug`/`Display` rendering, which is enough to assert the
/// recorded id strings and the `outcome` / `failure_reason` tokens.
struct CollectLayer {
    log: SpanLog,
}

/// A field visitor that renders every value to a `String` keyed by field name.
struct FieldCollector<'a>(&'a mut Vec<(String, String)>);

impl Visit for FieldCollector<'_> {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push((field.name().to_string(), value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push((field.name().to_string(), format!("{value:?}")));
    }
}

impl<S> Layer<S> for CollectLayer
where
    S: tracing::Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::span::Id,
        ctx: Context<'_, S>,
    ) {
        let mut fields = Vec::new();
        attrs.record(&mut FieldCollector(&mut fields));
        let handle: SpanHandle = Arc::new(Mutex::new(CapturedSpan {
            name: attrs.metadata().name().to_string(),
            fields,
        }));
        // Record the span in the log immediately (it exists from the moment it
        // opens, so the assertion never depends on close ordering), and stash the
        // same handle on the span's extensions so later `record` calls mutate the
        // very entry the log holds.
        self.log.lock().expect("span log lock").push(handle.clone());
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(handle);
        }
    }

    fn on_record(
        &self,
        id: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        ctx: Context<'_, S>,
    ) {
        if let Some(span) = ctx.span(id) {
            let ext = span.extensions();
            if let Some(handle) = ext.get::<SpanHandle>() {
                let mut captured = handle.lock().expect("span handle lock");
                values.record(&mut FieldCollector(&mut captured.fields));
            }
        }
    }
}

/// Snapshot the captured spans into owned `CapturedSpan` values for assertion.
fn snapshot(log: &SpanLog) -> Vec<CapturedSpan> {
    log.lock()
        .expect("span log")
        .iter()
        .map(|h| h.lock().expect("span handle").clone())
        .collect()
}

/// Find the (single) captured span with `name`, asserting exactly one exists.
fn span_named<'a>(spans: &'a [CapturedSpan], name: &str) -> &'a CapturedSpan {
    let matches: Vec<&CapturedSpan> = spans.iter().filter(|s| s.name == name).collect();
    assert_eq!(
        matches.len(),
        1,
        "expected exactly one `{name}` span, got {}: {spans:#?}",
        matches.len()
    );
    matches[0]
}

/// Seed the workspace + user + runtime + agent FK chain. Returns
/// `(workspace_id, runtime_id, agent_id)`.
async fn seed_graph(store: &Store) -> (String, String, String) {
    let pool = store.pool();
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert workspace");
    sqlx::query("INSERT INTO user (id, email, created_at) VALUES (?, ?, ?)")
        .bind("user-1")
        .bind("a@example.com")
        .bind(0_i64)
        .execute(pool)
        .await
        .expect("insert user");
    sqlx::query(
        "INSERT INTO agent_runtime (id, workspace_id, daemon_id, provider, runtime_mode) \
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind("rt-1")
    .bind("ws-1")
    .bind("daemon-1")
    .bind("claude")
    .bind("local")
    .execute(pool)
    .await
    .expect("insert runtime");
    sqlx::query(
        "INSERT INTO agent (id, workspace_id, name, runtime_id, visibility, owner_id) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind("agent-1")
    .bind("ws-1")
    .bind("Agent")
    .bind("rt-1")
    .bind("workspace")
    .bind("user-1")
    .execute(pool)
    .await
    .expect("insert agent");
    (
        "ws-1".to_string(),
        "rt-1".to_string(),
        "agent-1".to_string(),
    )
}

/// Enqueue one `queued` task and return its id. `created_at` is explicit so the
/// claim's `ORDER BY created_at` picks a deterministic row regardless of id
/// string ordering.
async fn enqueue_task(store: &Store, id: &str, created_at: i64) -> String {
    TaskRepo::insert(
        store.pool(),
        &NewTask {
            id: id.to_string(),
            workspace_id: "ws-1".to_string(),
            runtime_id: "rt-1".to_string(),
            agent_id: "agent-1".to_string(),
            issue_id: None,
            work_dir: None,
            created_at,
            autopilot_run_id: None,
        },
    )
    .await
    .expect("enqueue task");
    id.to_string()
}

/// Drive the full FSM (claim -> start -> complete, plus a fail and a cancel on
/// separate tasks) and the autopilot tick under a span-collecting subscriber,
/// then assert every span name + required field from the P8.3 table.
#[tokio::test]
async fn store_fsm_and_autopilot_emit_named_spans_with_required_fields() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open_in(dir.path()).await.expect("open store");
    seed_graph(&store).await;
    let clock = FixedClock(T0);

    // Seed an autopilot for the tick span.
    let ap_id = AutopilotRepo::create(
        store.pool(),
        &clock,
        &NewAutopilot {
            workspace_id: WorkspaceId::from_str("ws-1").unwrap(),
            agent_id: AgentId::from_str("agent-1").unwrap(),
            name: "daily".to_string(),
            instructions: Some("do the thing".to_string()),
            cron_expr: "0 9 * * *".to_string(),
            max_concurrent_runs: 1,
        },
    )
    .await
    .expect("create autopilot");
    let autopilot = AutopilotRepo::get(
        store.pool(),
        &WorkspaceId::from_str("ws-1").unwrap(),
        &ap_id,
    )
    .await
    .expect("get autopilot")
    .expect("autopilot present");

    // Tasks: one for the happy path (claim/start/complete), one to fail, one to
    // cancel. claim picks the oldest queued row by `created_at`, so give
    // task-complete the earliest stamp to guarantee it is the one claimed.
    enqueue_task(&store, "task-complete", T0).await;
    let fail_id = enqueue_task(&store, "task-fail", T0 + 1).await;
    let cancel_id = enqueue_task(&store, "task-cancel", T0 + 2).await;

    let log: SpanLog = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CollectLayer { log: log.clone() });

    let pool = store.pool().clone();
    let ap = autopilot.clone();

    // `#[tokio::test]` runs on a single current-thread runtime, so a thread-local
    // default subscriber held across `.await` stays installed for every span the
    // FSM opens. The guard drops (restoring the prior default) at scope end.
    {
        let _guard = set_default(subscriber);

        // task.claim — claims the oldest queued (task-complete).
        let claimed = ClaimTaskService::claim_for_runtime(&pool, "rt-1", &clock)
            .await
            .expect("claim")
            .expect("a task was claimable");
        let claim_id = claimed.id.clone();

        // task.start — dispatched -> running.
        StartTaskService::start(&pool, &claim_id, &clock)
            .await
            .expect("start");

        // task.complete — running -> done, with an outcome.
        CompleteTaskService::complete(
            &pool,
            &claim_id,
            CompleteParams {
                result: serde_json::json!({"ok": true}),
                session_id: Some("sess-1".to_string()),
                work_dir: Some("/tmp/wd".to_string()),
            },
            &clock,
        )
        .await
        .expect("complete");

        // task.fail — queued -> failed with a reason.
        FailTaskService::fail(&pool, &fail_id, FailureReason::AgentError, &clock)
            .await
            .expect("fail");

        // task.cancel — queued -> cancelled.
        CancelTaskService::cancel(&pool, &cancel_id, &clock)
            .await
            .expect("cancel");

        // autopilot.tick — fire one tick.
        ainb_hangar_store::repo::autopilot_run::fire_autopilot_tick(&pool, &clock, &ap)
            .await
            .expect("fire tick");
    }

    assert_spans(&snapshot(&log), autopilot.id.as_str());
}

/// Assert every P8.3 span name + its required fields against the captured set.
/// `autopilot_id` is the seeded autopilot's id (a runtime ULID).
fn assert_spans(spans: &[CapturedSpan], autopilot_id: &str) {
    // task.claim — task_id, workspace_id, runtime_id.
    let claim = span_named(spans, "task.claim");
    assert_eq!(claim.field("runtime_id"), Some("rt-1"), "claim.runtime_id");
    assert_eq!(
        claim.field("task_id"),
        Some("task-complete"),
        "claim.task_id (the claimed row's id)"
    );
    assert_eq!(claim.field("workspace_id"), Some("ws-1"), "claim.workspace_id");

    // task.start — task_id, workspace_id.
    let start = span_named(spans, "task.start");
    assert_eq!(start.field("task_id"), Some("task-complete"), "start.task_id");
    assert_eq!(start.field("workspace_id"), Some("ws-1"), "start.workspace_id");

    // task.complete — task_id, outcome.
    let complete = span_named(spans, "task.complete");
    assert_eq!(
        complete.field("task_id"),
        Some("task-complete"),
        "complete.task_id"
    );
    assert_eq!(
        complete.field("outcome"),
        Some("done"),
        "complete.outcome recorded mid-method"
    );

    // task.fail — task_id, failure_reason.
    let fail = span_named(spans, "task.fail");
    assert_eq!(fail.field("task_id"), Some("task-fail"), "fail.task_id");
    assert_eq!(
        fail.field("failure_reason"),
        Some("agent_error"),
        "fail.failure_reason"
    );

    // task.cancel — task_id, cancel_source.
    let cancel = span_named(spans, "task.cancel");
    assert_eq!(cancel.field("task_id"), Some("task-cancel"), "cancel.task_id");
    assert!(
        cancel.field("cancel_source").is_some(),
        "cancel.cancel_source present: {cancel:#?}"
    );

    // autopilot.tick — autopilot_id, cron_expr.
    let tick = span_named(spans, "autopilot.tick");
    assert_eq!(
        tick.field("autopilot_id"),
        Some(autopilot_id),
        "tick.autopilot_id"
    );
    assert_eq!(tick.field("cron_expr"), Some("0 9 * * *"), "tick.cron_expr");
}
