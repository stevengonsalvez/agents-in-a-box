//! Integration: the `hangar/squads_list`, `hangar/squad_create`,
//! `hangar/squad_member_add`, and `hangar/squad_member_remove` RPCs over a real
//! framed `UnixStream`, plus a proof that leader-routing actually TAKES EFFECT
//! (e38.17).
//!
//! A connection authenticates, then drives each verb through the dispatcher and
//! asserts:
//! * `squad_create` + `squad_member_add` build a squad with a leader + members,
//!   and `squads_list` reads it back;
//! * a duplicate squad name is rejected (resolve-or-reject guard);
//! * squad mutations are workspace-scoped (a sibling tenant cannot see / mutate
//!   the squad);
//! * LEADER ROUTING takes effect THROUGH A PRODUCT SURFACE — the
//!   `hangar/squad_assign` RPC turns a squad assignment into a routed task: the
//!   daemon resolves the squad's leader, derives the leader's runtime, and
//!   enqueues the task, all server-side. The test asserts the RPC's response
//!   names the leader and then proves the leader's runtime (and only it) claims
//!   the enqueued task via the real `ClaimTaskService`. The test does NOT
//!   hand-build the task or hand-resolve the leader — the routing happens inside
//!   the daemon, not the test body.

use std::time::{Duration, Instant};

use ainb_hangar_core::clock::FixedClock;
use ainb_hangar_daemon::rpc::{self, DaemonHealth};
use ainb_hangar_daemon::seed::{self, WS_ID, WS_SLUG};
use ainb_hangar_proto::{RpcId, RpcRequest, methods};
use ainb_hangar_store::Store;
use ainb_hangar_store::service::claim::ClaimTaskService;
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
            id: RpcId::Number(17),
            method: method.into(),
            params,
        };
        let body = serde_json::to_vec(&req).unwrap();
        let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
        out.extend_from_slice(&body);
        self.writer.write_all(&out).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn read_frame(&mut self, timeout: Duration) -> Option<serde_json::Value> {
        tokio::time::timeout(timeout, self.read_frame_inner()).await.ok()
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
            let frame = self
                .read_frame(Duration::from_secs(5))
                .await
                .unwrap_or_else(|| panic!("no response to {method} within 5s"));
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
}

/// Bind + serve the real listener over the seeded store (mirrors `boot()`).
async fn start_server(dir: &std::path::Path) -> (std::path::PathBuf, Store) {
    let store = Store::open_in(dir).await.unwrap();
    seed::seed_p4_fixture(store.pool()).await.unwrap();
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

/// Seed a SECOND runtime + agent (`runtime-2` / `agent-2`) with no in-flight work,
/// so a squad whose LEADER is `agent-2` is distinguishable from the already-busy
/// default `agent-1` (the seed fixture leaves a running task on `agent-1`).
async fn seed_second_agent(store: &Store) {
    use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
    use ainb_hangar_store::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};

    AgentRuntimeRepo::insert(
        store.pool(),
        &AgentRuntime {
            id: "runtime-2".into(),
            workspace_id: WS_ID.into(),
            daemon_id: "daemon-1".into(),
            provider: "codex".into(),
            runtime_mode: "local".into(),
            last_seen_at: Some(1),
            status: "online".into(),
        },
    )
    .await
    .unwrap();
    AgentRepo::insert(
        store.pool(),
        &Agent {
            id: "agent-2".into(),
            workspace_id: WS_ID.into(),
            name: "lead-agent".into(),
            runtime_id: "runtime-2".into(),
            instructions: None,
            visibility: "workspace".into(),
            owner_id: "user-1".into(),
            archived: false,
            model: None,
            cli_args: Vec::new(),
            mcp_config: None,
            thinking: None,
            agent_env: Vec::new(),
        },
    )
    .await
    .unwrap();
}

/// Seed one online `agent_runtime` + `agent` pair (`agent_id` on `runtime_id`),
/// each runtime carrying a distinct provider so two runtimes in one workspace do
/// not collide on the `(workspace_id, daemon_id, provider)` index. Used by the
/// fan-out test to stand up a leader + member agents on their own runtimes.
async fn seed_agent_on(store: &Store, agent_id: &str, runtime_id: &str) {
    use ainb_hangar_store::repo::agent::{Agent, AgentRepo};
    use ainb_hangar_store::repo::agent_runtime::{AgentRuntime, AgentRuntimeRepo};

    AgentRuntimeRepo::insert(
        store.pool(),
        &AgentRuntime {
            id: runtime_id.into(),
            workspace_id: WS_ID.into(),
            daemon_id: "daemon-1".into(),
            provider: format!("provider-{runtime_id}"),
            runtime_mode: "local".into(),
            last_seen_at: Some(1),
            status: "online".into(),
        },
    )
    .await
    .unwrap();
    AgentRepo::insert(
        store.pool(),
        &Agent {
            id: agent_id.into(),
            workspace_id: WS_ID.into(),
            name: agent_id.into(),
            runtime_id: runtime_id.into(),
            instructions: None,
            visibility: "workspace".into(),
            owner_id: "user-1".into(),
            archived: false,
            model: None,
            cli_args: Vec::new(),
            mcp_config: None,
            thinking: None,
            agent_env: Vec::new(),
        },
    )
    .await
    .unwrap();
}

/// Find a squad row in a `squads_list`-shaped result by id.
fn find_squad<'a>(resp: &'a serde_json::Value, id: &str) -> &'a serde_json::Value {
    resp["result"]["squads"]
        .as_array()
        .unwrap_or_else(|| panic!("squads array: {resp}"))
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("squad {id} present: {resp}"))
}

/// The id of the (single) squad in a `squads_list`-shaped result.
fn only_squad_id(resp: &serde_json::Value) -> String {
    let squads = resp["result"]["squads"].as_array().unwrap();
    assert_eq!(squads.len(), 1, "exactly one squad: {resp}");
    squads[0]["id"].as_str().unwrap().to_string()
}

/// `squad_create` + `squad_member_add` build a squad with a leader + members; a
/// follow-up `squads_list` reads it back.
#[tokio::test]
async fn squad_create_and_add_members_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_agent(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Create a squad led by agent-2.
    let resp = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "alpha", "leader": "agent:agent-2" }),
        )
        .await;
    assert!(resp["error"].is_null(), "squad_create must ack: {resp}");
    let squad_id = only_squad_id(&resp);
    assert_eq!(find_squad(&resp, &squad_id)["leader"], "agent:agent-2");

    // Add an agent member and a human member.
    c.call(
        methods::HANGAR_SQUAD_MEMBER_ADD,
        serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id, "member": "agent:agent-1" }),
    )
    .await;
    let resp = c
        .call(
            methods::HANGAR_SQUAD_MEMBER_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id, "member": "member:user-1" }),
        )
        .await;
    assert!(resp["error"].is_null(), "member_add must ack: {resp}");

    // The list reads the squad back with its leader and both members.
    let list = c
        .call(
            methods::HANGAR_SQUADS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    let squad = find_squad(&list, &squad_id);
    assert_eq!(squad["name"], "alpha");
    assert_eq!(squad["leader"], "agent:agent-2");
    let members = squad["members"].as_array().unwrap();
    assert_eq!(members.len(), 2, "both members listed: {list}");
    assert!(members.iter().any(|m| m == "agent:agent-1"));
    assert!(members.iter().any(|m| m == "member:user-1"));

    // A duplicate name is rejected (resolve-or-reject).
    let dup = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "alpha", "leader": "agent:agent-1" }),
        )
        .await;
    assert!(
        !dup["error"].is_null(),
        "a duplicate squad name must be rejected: {dup}"
    );
}

/// `squad_member_remove` drops a member; the refreshed list no longer carries it.
#[tokio::test]
async fn squad_member_remove_drops_a_member() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_agent(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "alpha", "leader": "agent:agent-2" }),
        )
        .await;
    let squad_id = only_squad_id(&created);
    c.call(
        methods::HANGAR_SQUAD_MEMBER_ADD,
        serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id, "member": "agent:agent-1" }),
    )
    .await;

    let resp = c
        .call(
            methods::HANGAR_SQUAD_MEMBER_REMOVE,
            serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id, "member": "agent:agent-1" }),
        )
        .await;
    assert!(resp["error"].is_null(), "member_remove must ack: {resp}");
    assert!(
        find_squad(&resp, &squad_id)["members"].as_array().unwrap().is_empty(),
        "member dropped: {resp}"
    );
}

/// Squad mutations are workspace-scoped: a sibling tenant cannot see the squad,
/// and cannot mutate it (a create aimed at a foreign workspace is rejected, and a
/// member-add through a tenant that does not own the squad is a not-found error).
#[tokio::test]
async fn squads_are_workspace_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_agent(&store).await;

    // A second, real tenant workspace.
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind("01HANGARFIXTUREWSB00000000")
        .bind("other")
        .bind("Other")
        .bind(1_700_000_000_000_i64)
        .execute(store.pool())
        .await
        .unwrap();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Create a squad in the seeded workspace.
    let created = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "alpha", "leader": "agent:agent-2" }),
        )
        .await;
    let squad_id = only_squad_id(&created);

    // The "other" tenant sees no squads.
    let other_list = c
        .call(
            methods::HANGAR_SQUADS_LIST,
            serde_json::json!({ "workspace_id": "other" }),
        )
        .await;
    assert!(
        other_list["result"]["squads"].as_array().unwrap().is_empty(),
        "a sibling tenant must not see the squad: {other_list}"
    );

    // The "other" tenant cannot add a member to the seeded squad (not-found).
    let cross = c
        .call(
            methods::HANGAR_SQUAD_MEMBER_ADD,
            serde_json::json!({ "workspace_id": "other", "squad_id": squad_id, "member": "agent:agent-1" }),
        )
        .await;
    assert!(
        !cross["error"].is_null(),
        "a cross-tenant squad must not be mutable: {cross}"
    );

    // The seeded squad is untouched (still zero members).
    let list = c
        .call(
            methods::HANGAR_SQUADS_LIST,
            serde_json::json!({ "workspace_id": WS_SLUG }),
        )
        .await;
    assert!(
        find_squad(&list, &squad_id)["members"].as_array().unwrap().is_empty(),
        "cross-tenant add blocked: {list}"
    );
}

/// LEADER ROUTING TAKES EFFECT THROUGH THE `hangar/squad_assign` RPC: the daemon
/// turns a squad assignment into a task routed to the squad's LEADER — the test
/// drives the product surface, it does not route by hand.
///
/// The squad's leader is `agent-2` on `runtime-2` (a fresh agent with no in-flight
/// work), distinct from the busy default `agent-1`. The test calls the
/// `hangar/squad_assign` RPC naming ONLY the squad — never the agent or runtime.
/// The daemon resolves the leader, derives the leader's runtime, and enqueues the
/// task server-side; the RPC response names the leader (`agent-2` on `runtime-2`).
/// Then the real `ClaimTaskService` claims the enqueued task for `runtime-2` and
/// the claimed `agent_id` is the leader's. Claiming for the non-leader runtime
/// returns nothing — proving the daemon routed the work to the leader, not anyone
/// else.
#[tokio::test]
async fn squad_assign_rpc_routes_a_task_to_the_leader_agent() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_agent(&store).await;
    let pool = store.pool();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // Create a squad whose LEADER is agent-2 (on runtime-2).
    let created = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "shippers", "leader": "agent:agent-2" }),
        )
        .await;
    let squad_id = only_squad_id(&created);

    // ASSIGN TO THE SQUAD through the product RPC — naming ONLY the squad. The
    // daemon resolves the leader, derives its runtime, and enqueues the task; the
    // response names the leader it routed to.
    let assigned = c
        .call(
            methods::HANGAR_SQUAD_ASSIGN,
            serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id }),
        )
        .await;
    assert!(
        assigned["error"].is_null(),
        "squad_assign must ack: {assigned}"
    );
    let task_id = assigned["result"]["task_id"].as_str().expect("task_id").to_string();
    assert_eq!(
        assigned["result"]["leader_agent_id"], "agent-2",
        "the daemon routed the task to the LEADER agent: {assigned}"
    );
    assert_eq!(
        assigned["result"]["runtime_id"], "runtime-2",
        "the daemon DERIVED the leader's runtime (not supplied by the caller): {assigned}"
    );

    // The leader's runtime CLAIMS the squad task — routing took effect.
    let clock = FixedClock(10_000);
    let claimed = ClaimTaskService::claim_for_runtime(pool, "runtime-2", &clock)
        .await
        .unwrap()
        .expect("the leader's runtime claims the squad task");
    assert_eq!(claimed.id, task_id, "the squad task was claimed");
    assert_eq!(
        claimed.agent_id, "agent-2",
        "the squad task is dispatched to the LEADER agent, not anyone else"
    );

    // The non-leader runtime claims nothing of the squad's: the task is routed to
    // the leader, not the busy default agent.
    let other = ClaimTaskService::claim_for_runtime(pool, "runtime-1", &clock).await.unwrap();
    assert!(
        other.is_none() || other.as_ref().map(|t| t.id.as_str()) != Some(task_id.as_str()),
        "the squad task must not be claimable by the non-leader runtime"
    );
}

/// FAN-OUT THROUGH THE `hangar/squad_fanout` RPC (P7): assigning an issue to a
/// squad briefs the LEADER *and* enqueues one task per distinct `agent` member,
/// all on the SAME issue, each claimable in parallel on its own runtime.
///
/// The squad's leader is `agent-2` (runtime-2); its members are `agent-3`
/// (runtime-3), `agent-4` (runtime-4), plus the human `member:user-1` (which must
/// NOT fan out). The test drives the product RPC naming only the squad + issue;
/// the daemon resolves the leader + members server-side. It then proves each of
/// the three agent runtimes claims its own task on `issue-2` via the real
/// `ClaimTaskService` — three pending tasks coexisting on one issue, the
/// per-(issue, agent) guard (migration 0012) at work.
#[tokio::test]
async fn squad_fanout_rpc_briefs_leader_and_fans_members_claimable_in_parallel() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    // Leader + two member agents, each on its own runtime.
    seed_agent_on(&store, "agent-2", "runtime-2").await;
    seed_agent_on(&store, "agent-3", "runtime-3").await;
    seed_agent_on(&store, "agent-4", "runtime-4").await;
    let pool = store.pool();

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // A squad led by agent-2 with two agent members + one human member.
    let created = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "shippers", "leader": "agent:agent-2" }),
        )
        .await;
    let squad_id = only_squad_id(&created);
    for m in ["agent:agent-3", "agent:agent-4", "member:user-1"] {
        c.call(
            methods::HANGAR_SQUAD_MEMBER_ADD,
            serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id, "member": m }),
        )
        .await;
    }

    // FAN OUT issue-2 across the squad — naming ONLY the squad + issue.
    let fanned = c
        .call(
            methods::HANGAR_SQUAD_FANOUT,
            serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id, "issue_id": "issue-2" }),
        )
        .await;
    assert!(fanned["error"].is_null(), "squad_fanout must ack: {fanned}");
    assert_eq!(
        fanned["result"]["leader"]["leader_agent_id"], "agent-2",
        "the leader gets the brief: {fanned}"
    );
    let members = fanned["result"]["members"].as_array().unwrap();
    assert_eq!(
        members.len(),
        2,
        "two agent members fanned out (human skipped): {fanned}"
    );
    let member_agents: Vec<&str> =
        members.iter().map(|m| m["agent_id"].as_str().unwrap()).collect();
    assert!(
        member_agents.contains(&"agent-3"),
        "agent-3 fanned: {fanned}"
    );
    assert!(
        member_agents.contains(&"agent-4"),
        "agent-4 fanned: {fanned}"
    );

    // PARALLEL CLAIM: every runtime claims its own task on issue-2 at once.
    let clock = FixedClock(10_000);
    for (runtime, agent) in [
        ("runtime-2", "agent-2"),
        ("runtime-3", "agent-3"),
        ("runtime-4", "agent-4"),
    ] {
        let claimed = ClaimTaskService::claim_for_runtime(pool, runtime, &clock)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("{runtime} claims its squad task"));
        assert_eq!(
            claimed.agent_id, agent,
            "{runtime} claimed the wrong agent's task"
        );
        assert_eq!(
            claimed.issue_id.as_deref(),
            Some("issue-2"),
            "the fanned-out task is on issue-2"
        );
    }
}

/// `hangar/squad_fanout` REJECTS a squad with a human-member leader: like
/// `squad_assign`, there is no agent to brief, so nothing is enqueued.
#[tokio::test]
async fn squad_fanout_rpc_rejects_a_human_leader() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_agent(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    let created = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "humans", "leader": "member:user-1" }),
        )
        .await;
    let squad_id = only_squad_id(&created);

    let fanned = c
        .call(
            methods::HANGAR_SQUAD_FANOUT,
            serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id, "issue_id": "issue-2" }),
        )
        .await;
    assert!(
        !fanned["error"].is_null(),
        "fanning out a human-led squad must be rejected: {fanned}"
    );
}

/// `hangar/squad_assign` REJECTS a squad with a human-member leader: there is no
/// agent to dispatch to, so nothing is enqueued (a client error, not a silent
/// no-op). Proves the routing path guards the human-leader case end-to-end.
#[tokio::test]
async fn squad_assign_rpc_rejects_a_human_leader() {
    let dir = tempfile::tempdir().unwrap();
    let (socket_path, store) = start_server(dir.path()).await;
    seed_second_agent(&store).await;

    let mut c = Client::connect(&socket_path).await;
    c.auth_from_file(dir.path()).await;

    // A squad led by a HUMAN member carries no agent to route work to.
    let created = c
        .call(
            methods::HANGAR_SQUAD_CREATE,
            serde_json::json!({ "workspace_id": WS_SLUG, "name": "humans", "leader": "member:user-1" }),
        )
        .await;
    let squad_id = only_squad_id(&created);

    let assigned = c
        .call(
            methods::HANGAR_SQUAD_ASSIGN,
            serde_json::json!({ "workspace_id": WS_SLUG, "squad_id": squad_id }),
        )
        .await;
    assert!(
        !assigned["error"].is_null(),
        "assigning to a human-led squad must be rejected: {assigned}"
    );
}
