//! Integration: `fleet/usage_dashboard` over a real framed `UnixStream`.
//!
//! `crate::fleet_usage` (`crates/ainb-hangar-daemon/src/fleet_usage.rs`) has
//! solid unit coverage of its projection, but nothing previously exercised the
//! RPC layer around it: the dispatch arm, the capability guard, `params: {}`
//! parsing, and result serialization over the wire. This file closes that gap
//! by driving the WHOLE query path the way `boot()` wires it, mirroring the
//! harness style of `rpc_usage_rollup.rs` / `rpc_fleet.rs`.
//!
//! `crate::fleet_usage::ACTIVE_SERVICE` is a process-global `OnceLock`
//! (installed once by `fleet_usage::install(home)`, which spawns a REAL
//! filesystem scan of `$HOME`'s provider session logs). None of these tests
//! call `install`, so every request here hits the daemon in its natural
//! "usage service is not initialized" state — the same coherent
//! not-an-error response a freshly booted daemon gives before its first
//! scan completes. That keeps the suite hermetic and deterministic; it also
//! means `weekly` / `heatmap` / `forecast` / `shell_commands` stay empty in
//! every response here, so a couple of assertions below are noted as
//! type-level checks rather than "the daemon really shipped this value" —
//! see the doc comment on each for exactly which is which.

use std::time::{Duration, Instant};

use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_proto::fleet::{
    FleetHeatmapCell, FleetUsageBucket, FleetUsageDashboardResult, FleetUsageForecast,
    FleetUsageNamedBucket, FleetUsageSummaryState, FleetUsageWeeklyBucket,
};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

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
            id: RpcId::Number(7),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn read_frame_inner(&mut self) -> serde_json::Value {
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

    async fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.send(method, params).await;
        loop {
            let frame = tokio::time::timeout(Duration::from_secs(5), self.read_frame_inner())
                .await
                .unwrap_or_else(|_| panic!("no response to {method} within 5s"));
            if frame.get("id").is_some() {
                return frame;
            }
        }
    }

    async fn auth_from_file(&mut self, dir: &std::path::Path) {
        let token_path = ainb_hangar_proto::auth::token_file_in(dir);
        let token = std::fs::read_to_string(&token_path).expect("read daemon.token");
        let resp = self
            .call(
                methods::AUTH_HELLO,
                serde_json::json!({ "token": token.trim() }),
            )
            .await;
        assert!(resp["error"].is_null(), "auth/hello must ack: {resp}");
    }

    /// Call `fleet/usage_dashboard`, returning the full response envelope
    /// (caller inspects both `error` and `result`).
    async fn usage_dashboard(&mut self, params: serde_json::Value) -> serde_json::Value {
        self.call(methods::FLEET_USAGE_DASHBOARD, params).await
    }
}

/// Bind + serve the real listener over a freshly opened store, returning the
/// socket path (mirrors `boot()`'s wiring, minus `fleet_usage::install` — see
/// the module doc comment for why that is deliberate).
async fn start_server(dir: &std::path::Path) -> std::path::PathBuf {
    let store = Store::open_in(dir).await.unwrap();
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
    socket_path
}

/// `fleet/usage_dashboard` is reachable through dispatch, passes the
/// `fleet.dashboard.read` capability guard, and returns a result that
/// deserializes into the REAL proto type — not a hand-parsed subset of the
/// JSON.
#[tokio::test]
async fn fleet_usage_dashboard_is_reachable_and_deserializes_into_the_real_proto_type() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c.usage_dashboard(serde_json::json!({})).await;
    assert!(
        resp["error"].is_null(),
        "fleet/usage_dashboard must ack: {resp}"
    );
    let result: FleetUsageDashboardResult = serde_json::from_value(resp["result"].clone())
        .unwrap_or_else(|e| {
            panic!("result did not deserialize as FleetUsageDashboardResult: {e}\nraw: {resp}")
        });
    // A round-trip through the real type is itself the proof: an unexpected
    // shape (missing/renamed required field, wrong type) would have failed
    // the `unwrap_or_else` above rather than silently producing a default.
    assert!(matches!(
        result.state,
        FleetUsageSummaryState::Scanning | FleetUsageSummaryState::Unavailable
    ));
}

/// Empty params parse, and so do unexpected/extra params — matching every
/// other Fleet params struct's contract. Checked, not assumed: none of the
/// param structs in `ainb_hangar_proto::fleet` carry
/// `#[serde(deny_unknown_fields)]`, so `parse_params` tolerates unknown keys
/// uniformly across the whole Fleet surface, and `fleet/usage_dashboard`
/// must behave the same way rather than growing a stricter one-off contract.
#[tokio::test]
async fn empty_params_and_unexpected_extra_params_both_ack_without_error() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let empty = c.usage_dashboard(serde_json::json!({})).await;
    assert!(empty["error"].is_null(), "empty params must ack: {empty}");

    let with_extras = c
        .usage_dashboard(serde_json::json!({
            "period": "week",
            "bogus_extra_field": true,
            "another_one": [1, 2, 3]
        }))
        .await;
    assert!(
        with_extras["error"].is_null(),
        "unexpected/extra params must still ack, matching every other fleet/* params struct: {with_extras}"
    );
    // Both calls must have reached the SAME handler and produced the same
    // shape of result — extras are ignored, not routed differently.
    assert_eq!(empty["result"]["state"], with_extras["result"]["state"]);
}

/// On a daemon with no scan data (nothing has ever called
/// `fleet_usage::install`), the response is a coherent state — `scanning` or
/// `unavailable`, each carrying a `detail` string — never an RPC error and
/// never a half-populated body (totals/weekly/heatmap/forecast stay empty
/// together, not some populated and others not).
#[tokio::test]
async fn a_dashboard_request_on_a_daemon_with_no_scan_data_returns_a_coherent_state_not_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c.usage_dashboard(serde_json::json!({})).await;
    assert!(
        resp["error"].is_null(),
        "an uninitialized usage service must be a coherent read, not an error: {resp}"
    );
    let result = &resp["result"];
    let state = result["state"].as_str().expect("state string");
    assert!(
        state == "scanning" || state == "unavailable",
        "expected scanning or unavailable, got {state}: {resp}"
    );
    assert!(
        result["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "a non-ready state must explain itself: {resp}"
    );
    // Half-populated would mean SOME of these carry data while the state
    // still claims scanning/unavailable. Pin them all empty/absent together.
    assert!(result["totals"].is_null(), "totals must be absent: {resp}");
    assert_eq!(result["weekly"].as_array(), Some(&vec![]));
    assert_eq!(result["heatmap"].as_array(), Some(&vec![]));
    assert!(
        result["forecast"].is_null(),
        "forecast must be absent: {resp}"
    );
}

/// SECURITY invariant (regression guard for commit 7485cff3, "stop leaking
/// shell commands and report honest cost"): the raw shell command line —
/// full argv, which routinely carries absolute paths and can carry
/// credentials — leaked onto the wire and into `fleet-usage.json` (mode
/// 0644) once already. Only the program name may ship. This is the point of
/// the whole exercise: assert, over the real socket response, that no
/// shipped `shell_commands[].name` contains a space or a `/` — the two
/// telltale signs of a full command line rather than a bare program name.
///
/// This hermetic daemon (no `fleet_usage::install` call, see module doc)
/// always reports an empty `shell_commands` list, so this assertion holds
/// vacuously today rather than against real sanitized data — it cannot be
/// driven non-vacuously at the RPC layer without a live scan of real
/// provider session logs under `$HOME` (`ProviderRoots::defaults()`), which
/// is not something this suite can do hermetically. The non-vacuous proof
/// that the sanitizer itself strips arguments lives in
/// `fleet_usage::tests::shell_commands_ship_the_program_only_never_the_arguments`
/// (`crates/ainb-hangar-daemon/src/fleet_usage.rs`). What THIS test pins is
/// the wire contract: whatever ships in `shell_commands[].name`, in any
/// future state this endpoint can reach, must never look like a command
/// line — so a regression that reintroduces the leak by, say, bypassing
/// `program_name()` in a new code path is still caught here once real data
/// exists.
#[tokio::test]
async fn shipped_shell_commands_never_carry_a_raw_command_line() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let resp = c.usage_dashboard(serde_json::json!({})).await;
    assert!(resp["error"].is_null(), "must ack: {resp}");
    let shell_commands = resp["result"]["shell_commands"]
        .as_array()
        .expect("shell_commands must be an array, even when empty");
    for entry in shell_commands {
        let name = entry["name"].as_str().expect("shell_commands[].name is a string");
        assert!(
            !name.contains(' ') && !name.contains('/'),
            "shell_commands[].name must ship the program only, never a full \
             command line (see commit 7485cff3): got {name:?}"
        );
    }
}

/// The Swift client decodes this endpoint with hand-written snake_case
/// `CodingKeys`; a silent Rust field rename is a cross-language break no
/// compiler catches. Two legs:
///
/// 1. LIVE socket leg: the top-level keys that are guaranteed present in
///    this daemon's uninitialized-state response (`cost_complete`,
///    `shell_commands`, `mcp_servers`) are checked against the real wire
///    bytes that came back over the socket.
/// 2. TYPE-CONTRACT leg (not a socket exercise — see module doc comment on
///    why real nested data can't be produced hermetically here): the
///    remaining named keys (`week_start`, `call_count`,
///    `projected_30d_cost_usd`) only ever appear nested inside populated
///    `weekly[]` / `heatmap[]` / `forecast` entries, which this hermetic
///    daemon never populates. Those are pinned by constructing the real
///    proto type directly and inspecting its serialized keys — still a
///    genuine regression guard against a silent rename, just not one that
///    travels over this test's socket.
#[tokio::test]
async fn wire_key_names_match_the_swift_clients_snake_case_contract() {
    let dir = tempfile::tempdir().unwrap();
    let socket_path = start_server(dir.path()).await;
    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Leg 1: live socket bytes.
    let resp = c.usage_dashboard(serde_json::json!({})).await;
    assert!(resp["error"].is_null(), "must ack: {resp}");
    let result = &resp["result"];
    assert!(
        result.get("cost_complete").is_some(),
        "cost_complete: {resp}"
    );
    assert!(
        result.get("shell_commands").is_some(),
        "shell_commands: {resp}"
    );
    assert!(result.get("mcp_servers").is_some(), "mcp_servers: {resp}");

    // Leg 2: type-contract check for the nested-only keys.
    let populated = FleetUsageDashboardResult {
        state: FleetUsageSummaryState::Ready,
        generated_at: Some(1_700_000_000_000),
        start_at: Some(1_668_000_000_000),
        end_at: Some(1_700_000_000_000),
        cost_complete: true,
        totals: Some(FleetUsageBucket::default()),
        weekly: vec![FleetUsageWeeklyBucket {
            week_start: "2026-07-28".to_string(),
            bucket: FleetUsageBucket::default(),
        }],
        heatmap: vec![FleetHeatmapCell {
            date: "2026-08-06".to_string(),
            call_count: 15,
            cost_usd: Some(2.30),
        }],
        forecast: Some(FleetUsageForecast {
            projected_30d_cost_usd: Some(125.0),
            projected_30d_tokens: 5_000_000,
            avg_daily_cost_usd: Some(4.17),
            avg_daily_tokens: 166_667,
            sample_days: 7,
        }),
        providers: vec![],
        models: vec![],
        projects: vec![],
        sessions: vec![],
        branches: vec![],
        tools: vec![],
        mcp_servers: vec![FleetUsageNamedBucket {
            name: "playwright".to_string(),
            call_count: 3,
        }],
        shell_commands: vec![FleetUsageNamedBucket {
            name: "cargo".to_string(),
            call_count: 12,
        }],
        detail: None,
    };
    let encoded = serde_json::to_value(&populated).unwrap();
    assert_eq!(encoded["weekly"][0]["week_start"], "2026-07-28");
    assert_eq!(encoded["heatmap"][0]["call_count"], 15);
    assert_eq!(encoded["forecast"]["projected_30d_cost_usd"], 125.0);
    assert_eq!(encoded["mcp_servers"][0]["name"], "playwright");
    assert_eq!(encoded["shell_commands"][0]["call_count"], 12);
}
