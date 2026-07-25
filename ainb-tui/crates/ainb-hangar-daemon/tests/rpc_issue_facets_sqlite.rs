//! multica-gap #10 acceptance — faceted issue filtering agrees with SQLite.
//!
//! The plugin-side tripwire (`ainb-plugin-hangar/tests/tripwire_issue_facets.rs`)
//! proves the SCREEN half honestly, but every row it filters is a hand-built
//! `IssueRow` from a mock daemon — nothing there proves the facet numbers agree
//! with what the database actually holds
//! (`noteworthy_fake_script_e2e_hid_broken_headless`). This test closes that
//! half: it drives the REAL daemon over the REAL framed unix socket against a
//! REAL temp sqlite store, seeds the same discriminating 5-row matrix through
//! real RPCs (`issue_create` / `issue_update` / `issue_label_attach`), and then
//!
//!   1. asserts the wire rows carry the exact `(state, priority, labels)` triple
//!      — the leg that catches a daemon silently dropping `labels`, which the
//!      mock-daemon tripwire structurally cannot; and
//!   2. runs the REAL plugin reducer over those wire rows and cross-checks the
//!      visible-row count against a raw SQL count on the same connection, so the
//!      assertion is anchored in the DB and not in the reducer's own arithmetic.
//!
//! The matrix has exactly ONE `todo + P0 + bug` survivor:
//!
//! | title       | state         | priority | labels  |
//! |-------------|---------------|----------|---------|
//! | `target`    | `todo`        | 3 (P0)   | `bug`   |
//! | `d_nolbl`   | `todo`        | 3 (P0)   | —       |
//! | `d_p2bug`   | `todo`        | 1 (P2)   | `bug`   |
//! | `d_progbug` | `in_progress` | 3 (P0)   | `bug`   |
//! | `d_chore`   | `done`        | 2 (P1)   | `chore` |

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::events::IssueRow;
use ainb_hangar_proto::lifecycle::IssueLifecycle;
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_plugin_hangar::IssueListState;
use ainb_plugin_hangar::screen::issue_list::{
    FacetKind, FacetValue, IssueColumn, IssueListEvent, reduce_issue_list,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

/// The workspace this test owns end-to-end (its own tenant, so the facet counts
/// are exactly the seeded matrix and nothing else).
const WS_ID: &str = "01HANGARFACETWS0000000000";
/// The workspace slug the RPCs address.
const WS_SLUG: &str = "facets";

/// One test client connection: a persistent buffered reader + writer half.
struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(socket_path: &std::path::Path) -> Self {
        let deadline = Instant::now() + Duration::from_secs(5);
        let stream = loop {
            match UnixStream::connect(socket_path).await {
                Ok(c) => break c,
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => panic!("never connected: {e}"),
            }
        };
        let (read_half, writer) = stream.into_split();
        Self {
            reader: BufReader::new(read_half),
            writer,
        }
    }

    async fn send(&mut self, method: &str, params: serde_json::Value) {
        let req = RpcRequest {
            jsonrpc: ainb_hangar_proto::jsonrpc_version(),
            id: RpcId::Number(11),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn read_frame(&mut self) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;
        let mut len: Option<usize> = None;
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).await.unwrap();
            assert!(n > 0, "connection closed while awaiting a frame");
            let t = line.trim_end_matches("\r\n");
            if t.is_empty() {
                let mut body = vec![0u8; len.expect("Content-Length header")];
                self.reader.read_exact(&mut body).await.unwrap();
                return serde_json::from_slice(&body).unwrap();
            }
            if let Some((name, v)) = t.split_once(':') {
                if name.trim().eq_ignore_ascii_case("Content-Length") {
                    len = v.trim().parse().ok();
                }
            }
        }
    }

    /// Issue one call and return its RESULT, asserting the daemon did not error
    /// (a silent error would otherwise read as an empty facet scope).
    async fn call_ok(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(method, params).await;
        let frame = loop {
            let frame = tokio::time::timeout(Duration::from_secs(10), self.read_frame())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 10s"));
            // Skip interleaved event notifications (this connection never
            // subscribes, but be robust to broadcast changes).
            if frame.get("id").is_some() {
                break frame;
            }
        };
        assert!(frame["error"].is_null(), "{method} must ack: {frame}");
        frame["result"].clone()
    }

    async fn auth_from_file(&mut self, dir: &std::path::Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let token = std::fs::read_to_string(&token_path).expect("read daemon.token");
        self.call_ok(
            methods::AUTH_HELLO,
            serde_json::json!({ "token": token.trim() }),
        )
        .await;
    }
}

/// Bind + serve the real listener over a migrated store owning ONE empty
/// workspace (deliberately not the shared P4 fixture: its issues would pollute
/// the facet counts this test asserts exactly).
async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(WS_ID)
        .bind(WS_SLUG)
        .bind("Facets")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();
    rpc::auth::ensure_socket_token(store.pool(), dir)
        .await
        .expect("ensure socket token");
    let socket_path = rpc::socket_path_in(dir);
    let listener = rpc::bind(&socket_path).expect("bind socket");
    let health = DaemonHealth {
        socket_path: socket_path.to_string_lossy().into_owned(),
        pid: std::process::id(),
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: std::sync::Arc::new(ainb_hangar_daemon::health_stats::HealthStats::default()),
    };
    tokio::spawn(rpc::serve(
        listener,
        store.pool().clone(),
        health,
        ainb_hangar_daemon::events::EventBroker::new(),
    ));
    (socket_path, store)
}

/// Seed one matrix row through the real mutating RPCs and return its minted id.
async fn seed_row(c: &mut Client, title: &str, state: &str, priority: i64, labels: &[&str]) {
    let created = c
        .call_ok(
            methods::HANGAR_ISSUE_CREATE,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "title": title,
                "creator": "member:user-1",
            }),
        )
        .await;
    let id = created["id"].as_str().expect("minted issue id").to_string();

    c.call_ok(
        methods::HANGAR_ISSUE_UPDATE,
        serde_json::json!({
            "workspace_id": WS_SLUG,
            "issue_id": id,
            "state": state,
            "priority": priority,
        }),
    )
    .await;

    for name in labels {
        c.call_ok(
            methods::HANGAR_ISSUE_LABEL_ATTACH,
            serde_json::json!({
                "workspace_id": WS_SLUG,
                "issue_id": id,
                "name": name,
                "color": "#ff0000",
            }),
        )
        .await;
    }
}

/// Look up one seeded row by title (ids are daemon-minted ULIDs).
fn by_title<'a>(rows: &'a [IssueRow], title: &str) -> &'a IssueRow {
    rows.iter()
        .find(|r| r.title == title)
        .unwrap_or_else(|| panic!("no `{title}` row in the wire snapshot"))
}

/// The count for one facet value out of a `facet_counts` list.
fn count_of(counts: &[(FacetValue, usize)], want: &FacetValue) -> usize {
    counts
        .iter()
        .find(|(v, _)| v == want)
        .map_or(0, |(_, n)| *n)
}

/// The status + priority + label facet intersection narrows the real daemon's
/// wire rows to exactly the one survivor SQLite agrees on.
#[tokio::test]
async fn facet_intersection_over_real_sqlite_matches_reducer_and_sql() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    seed_row(&mut c, "target", "todo", 3, &["bug"]).await;
    seed_row(&mut c, "d_nolbl", "todo", 3, &[]).await;
    seed_row(&mut c, "d_p2bug", "todo", 1, &["bug"]).await;
    seed_row(&mut c, "d_progbug", "in_progress", 3, &["bug"]).await;
    seed_row(&mut c, "d_chore", "done", 2, &["chore"]).await;

    // ── 1. The wire snapshot carries the exact (state, priority, labels) triple.
    let listed = c
        .call_ok(
            methods::HANGAR_ISSUES_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let rows: Vec<IssueRow> =
        serde_json::from_value(listed["issues"].clone()).expect("issues decode as IssueRow");
    assert_eq!(rows.len(), 5, "the snapshot carries the whole matrix");

    for (title, state, priority, labels) in [
        ("target", "todo", 3_i64, vec!["bug"]),
        ("d_nolbl", "todo", 3, vec![]),
        ("d_p2bug", "todo", 1, vec!["bug"]),
        ("d_progbug", "in_progress", 3, vec!["bug"]),
        ("d_chore", "done", 2, vec!["chore"]),
    ] {
        let row = by_title(&rows, title);
        assert_eq!(row.state, state, "{title} state");
        assert_eq!(row.priority, priority, "{title} priority");
        assert_eq!(
            row.labels,
            labels.iter().map(|s| (*s).to_string()).collect::<Vec<_>>(),
            "{title} labels — the daemon must hydrate the issue_label join onto \
             the wire row, not drop it"
        );
    }

    // ── 2. The REAL reducer over those wire rows, with the three facets applied
    //       through the REAL ToggleFacet path.
    let unfiltered = IssueListState::with_rows(rows);

    // Drill-down counts on the unfiltered scope (multica `ListIssueTableFacets`).
    let label_counts = unfiltered.facet_counts(FacetKind::Label);
    assert_eq!(
        count_of(&label_counts, &FacetValue::Label("bug".into())),
        3,
        "bug drill-down count: {label_counts:?}"
    );
    assert_eq!(
        count_of(&label_counts, &FacetValue::Label("chore".into())),
        1,
        "chore drill-down count: {label_counts:?}"
    );
    let priority_counts = unfiltered.facet_counts(FacetKind::Priority);
    assert_eq!(
        count_of(&priority_counts, &FacetValue::Priority(3)),
        3,
        "P0 drill-down count: {priority_counts:?}"
    );

    let mut state = unfiltered;
    for value in [
        FacetValue::Status(IssueLifecycle::Todo),
        FacetValue::Priority(3),
        FacetValue::Label("bug".to_string()),
    ] {
        state = reduce_issue_list(&state, IssueListEvent::ToggleFacet(value.kind(), value)).state;
    }

    let visible: Vec<&str> = state.visible_rows().map(|r| r.title.as_str()).collect();
    assert_eq!(
        visible,
        vec!["target"],
        "todo ∧ P0 ∧ bug must leave exactly the one survivor"
    );
    assert_eq!(
        state.column_count(IssueColumn::Todo),
        1,
        "Todo column header count follows the facets"
    );
    assert_eq!(
        state.column_count(IssueColumn::InProgress),
        0,
        "the in_progress decoy is filtered out of its own column"
    );

    // ── 3. Cross-check against raw SQL on the same connection: the reducer's
    //       arithmetic must agree with the database, not just with itself.
    let sql_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM issue i \
         JOIN issue_label il ON il.issue_id = i.id \
         JOIN label l ON l.id = il.label_id \
         WHERE i.workspace_id = ? AND i.state = 'todo' AND i.priority = 3 AND l.name = 'bug'",
    )
    .bind(WS_ID)
    .fetch_one(store.pool())
    .await
    .unwrap();
    assert_eq!(sql_count, 1, "the SQL ground truth is the one survivor");
    assert_eq!(
        usize::try_from(sql_count).unwrap(),
        visible.len(),
        "the facet reducer's visible-row count must equal the SQL ground truth"
    );
}
