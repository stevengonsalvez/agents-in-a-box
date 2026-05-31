//! P8.3 — the two beads-sync directions emit structured tracing spans.
//!
//! The P8 plan names `BeadsSync::push_to_bd` / `pull_from_bd`; the real methods
//! are the per-issue mirror/reconcile fns that actually carry a single
//! `(hangar_id, bd_id)` pair:
//!
//! | Plan row              | Real method                                   | Span         |
//! |-----------------------|-----------------------------------------------|--------------|
//! | `BeadsSync::push_to_bd`  | `beads_sync::outbound::OutboundSync::mirror_create` | `beads.push` |
//! | `BeadsSync::pull_from_bd`| `beads_sync::inbound::InboundSync::reconcile_issue` | `beads.pull` |
//!
//! `mirror_create` is the outbound push (Hangar issue -> `bd create`, records the
//! correlation) and is the only outbound fn that learns the `bd_id`. The inbound
//! pull lands per `bd` issue in `reconcile_issue` — `reconcile_once` is a batch
//! over many issues with no single id pair, so the span is on the per-issue fn.
//!
//! The fake-bd fixtures are hermetic — no live `bd` binary is required.

use std::sync::{Arc, Mutex};

use ainb_hangar_core::actor::{ActorKind, ActorRef};
use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::beads_adapter::{fake_bd, BdClient};
use ainb_hangar_daemon::beads_sync::inbound::InboundSync;
use ainb_hangar_daemon::beads_sync::outbound::OutboundSync;
use ainb_hangar_store::repo::beads_mapping::{
    BeadsMappingRepo, BeadsMappingRow, MappingKind, MappingSource,
};
use ainb_hangar_store::repo::issue::{Issue, IssueRepo, NewIssue};
use ainb_hangar_store::Store;
use chrono::{TimeZone, Utc};
use tracing::field::{Field, Visit};
use tracing::subscriber::set_default;
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Frozen clock value used for every test's `last_synced` stamp.
const T0_MS: i64 = 1_700_000_000_000;

/// One captured span: metadata name + the (key, value) fields set on it.
#[derive(Debug, Clone, Default)]
struct CapturedSpan {
    name: String,
    fields: Vec<(String, String)>,
}

impl CapturedSpan {
    fn field(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// A handle both the log and the span's extensions point at, so a `record` call
/// mutates the same entry the log holds.
type SpanHandle = Arc<Mutex<CapturedSpan>>;

/// Shared buffer of observed spans, pushed at `on_new_span` (independent of
/// close timing) and mutated in place by later `on_record` calls.
type SpanLog = Arc<Mutex<Vec<SpanHandle>>>;

/// A minimal `Layer` capturing every span's name + recorded fields.
struct CollectLayer {
    log: SpanLog,
}

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

/// Snapshot the captured spans into owned values for assertion.
fn snapshot(log: &SpanLog) -> Vec<CapturedSpan> {
    log.lock()
        .expect("span log")
        .iter()
        .map(|h| h.lock().expect("span handle").clone())
        .collect()
}

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

async fn seed_workspace(store: &Store) -> String {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("ws-1")
        .bind("alpha")
        .bind("Alpha")
        .bind(0_i64)
        .execute(store.pool())
        .await
        .expect("insert workspace");
    "ws-1".to_string()
}

async fn seed_issue(store: &Store, ws: &str, id: &str, title: &str) -> Issue {
    let new = NewIssue {
        id: id.to_string(),
        workspace_id: ws.to_string(),
        title: title.to_string(),
        description: Some("d".to_string()),
        state: "open".to_string(),
        assignee: None,
        creator: ActorRef::new(ActorKind::Member, "stevie").expect("actor"),
        created_at: T0_MS,
    };
    IssueRepo::insert(store.pool(), &new)
        .await
        .expect("insert issue");
    IssueRepo::get_by_id(store.pool(), id)
        .await
        .expect("get issue")
        .expect("issue present")
}

/// `beads.push` — the outbound mirror of a Hangar issue create carries both the
/// Hangar id and the `bd` id it learned from the `bd create`.
#[tokio::test]
async fn outbound_mirror_create_emits_beads_push_span() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_1", "ship it").await;

    let bin = fake_bd::happy(bin_dir.path(), "bd-abc-123", "ship it", "open");
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let mapping = BeadsMappingRepo::new(store.pool());
    let clock = FixedClock(T0_MS);

    let log: SpanLog = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CollectLayer { log: log.clone() });

    {
        let _guard = set_default(subscriber);
        let sync = OutboundSync::new(&bd, &mapping, &clock);
        sync.mirror_create(&issue, MappingSource::Hangar, &["foo".into()])
            .await
            .expect("mirror create");
    }

    let spans = snapshot(&log);
    let push = span_named(&spans, "beads.push");
    assert_eq!(push.field("hangar_id"), Some("iss_1"), "push.hangar_id");
    assert_eq!(push.field("bd_id"), Some("bd-abc-123"), "push.bd_id");
}

/// `beads.pull` — the inbound per-issue reconcile that lands a `bd`-side status
/// change on the mapped Hangar issue carries both ids.
#[tokio::test]
async fn inbound_reconcile_emits_beads_pull_span() {
    let store_dir = tempfile::tempdir().expect("store dir");
    let bin_dir = tempfile::tempdir().expect("bin dir");
    let beads_dir = tempfile::tempdir().expect("beads dir");

    let store = Store::open_in(store_dir.path()).await.expect("store");
    let ws = seed_workspace(&store).await;
    let issue = seed_issue(&store, &ws, "iss_1", "ship it").await;
    let mapping = BeadsMappingRepo::new(store.pool());
    mapping
        .insert(&BeadsMappingRow {
            hangar_id: issue.id.clone(),
            bd_id: "bd-1".to_string(),
            hangar_kind: MappingKind::Issue,
            bd_kind: MappingKind::Issue,
            source: MappingSource::Hangar,
            last_synced: Utc.timestamp_millis_opt(T0_MS).single().expect("ts"),
        })
        .await
        .expect("seed mapping");

    // fake-bd `list` returns the correlated issue, now closed -> a real pull.
    let bin = fake_bd::listing(bin_dir.path(), &[("bd-1", "ship it", "closed")]);
    let bd = BdClient::new(bin, beads_dir.path().to_path_buf()).expect("client");
    let clock = FixedClock(T0_MS);

    let log: SpanLog = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CollectLayer { log: log.clone() });

    {
        let _guard = set_default(subscriber);
        let sync = InboundSync::new(&bd, &mapping, store.pool(), &clock);
        let stats = sync.reconcile_once().await.expect("reconcile");
        assert_eq!(stats.updated, 1, "the closed bd issue should land on Hangar");
    }

    let spans = snapshot(&log);
    let pull = span_named(&spans, "beads.pull");
    assert_eq!(pull.field("bd_id"), Some("bd-1"), "pull.bd_id");
    assert_eq!(pull.field("hangar_id"), Some("iss_1"), "pull.hangar_id");
}
