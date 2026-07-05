//! tcp T5 — per-attention-kind notification routing, end to end.
//!
//! The unseeded acceptance for T5: flip a routing rule through the SAME RPC the
//! settings grid drives (`hangar/notify_rule_set`), then raise a matching
//! attention through the HOOK SEAM (the daemon's `events.jsonl` → attention
//! ingest, exactly as a live session's `Notification` hook does), and prove the
//! fan-out honours the flipped rule: the channel the flip ENABLED fires, the
//! channel it SUPPRESSED does not.
//!
//! ```text
//!  notify_rule_set(ask → phone,web)      (settings-grid write, via RPC)
//!         │
//!  events.jsonl Notification ─▶ attention ingest ─▶ classify ASK
//!         │                         └─ resolve rules AT EMIT ─▶ row.channels = {phone,web}
//!         ▼
//!  attention/list ─▶ stub consumers filter on row.channels:
//!         phone consumer delivers ✓   os consumer suppressed ✗ (flip dropped os)
//! ```
//!
//! This is a genuine regression guard: the flip to `phone,web` DROPS os from the
//! seeded default (`phone,web,os`), so BEFORE the emit-time resolution the row
//! would carry the default and the os assertion (suppressed) would fail. Nothing
//! is seeded for the interaction path — the rule flip and the raise both run live.
//! No tmux, no real Telegram/web-push: the "consumers" are pure
//! `ChannelSet::contains` filters (the exact gate the bridge + web push run),
//! recording deliveries into a Vec.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use ainb_hangar_core::channel::{Channel, ChannelSet};
use ainb_hangar_daemon::attention_ingest::AttentionIngest;
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_daemon::health_stats::HealthStats;
use ainb_hangar_daemon::rpc::{dispatch, DaemonHealth};
use ainb_hangar_proto::events::{AttentionRow, HangarEvent};
use ainb_hangar_proto::snapshots::AttentionListResult;
use ainb_hangar_proto::{methods, RpcId, RpcRequest};
use ainb_hangar_store::Store;

/// Plant an AskUserQuestion transcript under `~/.claude/projects/<slug>` for a
/// UNIQUE cwd so the ingest's classifier resolves it to ASK (mirrors the
/// attention-ingest unit fixture). Returns the fabricated cwd + a cleanup guard.
struct AskTranscript {
    cwd: String,
    dir: PathBuf,
}
impl Drop for AskTranscript {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn plant_ask(tag: &str) -> AskTranscript {
    let cwd = format!(
        "/ainb-test-notify-routing/{tag}/{}/{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let mut dir = dirs::home_dir().expect("home dir");
    dir.push(".claude");
    dir.push("projects");
    let slug = ainb_fleet_core::read::jsonl_tail::cwd_to_project_slug(&cwd);
    dir.push(&slug);
    std::fs::create_dir_all(&dir).expect("create project dir");
    let mut f = std::fs::File::create(dir.join("session.jsonl")).expect("create transcript");
    writeln!(
        f,
        r#"{{"type":"assistant","message":{{"content":[{{"type":"tool_use","name":"AskUserQuestion","input":{{"questions":[{{"question":"Ship it?","options":[{{"label":"yes"}},{{"label":"no"}}]}}]}}}}]}},"timestamp":"2026-01-01T00:00:00Z"}}"#
    )
    .unwrap();
    AskTranscript { cwd, dir }
}

fn health() -> DaemonHealth {
    DaemonHealth {
        socket_path: "/tmp/notify-routing.sock".into(),
        pid: 1,
        started_at: Instant::now(),
        version: "0.1.0".into(),
        stats: Arc::new(HealthStats::default()),
    }
}

fn hook_line(session: &str, cwd: &str) -> String {
    format!(
        r#"{{"ts":1700000000000,"session_id":"{session}","cwd":"{cwd}","transcript_path":"","agent":"claude","event_type":"Notification"}}"#
    )
}

/// The delivery gate every bus consumer runs: deliver a row iff the rules routed
/// it to `channel`. Returns the ids delivered — the recorded stub-transport log.
fn deliver_to(rows: &[AttentionRow], channel: Channel) -> Vec<String> {
    rows.iter()
        .filter(|r| r.channels.contains(channel))
        .map(|r| r.id.clone())
        .collect()
}

/// Fetch the open attention feed the way a real consumer does — through the
/// `attention/list` RPC (fleet scope), which carries each row's resolved channels.
async fn attention_feed(store: &Store, health: &DaemonHealth, sink: &ainb_hangar_daemon::events::EventSink) -> Vec<AttentionRow> {
    let req = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(1),
        method: methods::ATTENTION_LIST.into(),
        params: serde_json::json!({ "fleet": true }),
    };
    let resp = dispatch(store.pool(), &req, health, sink).await;
    let result = resp.result.expect("attention/list ok");
    serde_json::from_value::<AttentionListResult>(result).expect("decode").attention
}

#[tokio::test]
async fn flipping_ask_to_phone_routes_to_phone_and_suppresses_os() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let sink = broker.sink();
    let health = health();

    // Baseline: the SEEDED default routes ASK to phone+web+os (0038 restored phone).
    use ainb_hangar_store::repo::attention::AttentionKind;
    use ainb_hangar_store::repo::notify_rule::NotifyRuleRepo;
    let default = NotifyRuleRepo::resolve(store.pool(), AttentionKind::AskUserQuestion, None)
        .await
        .unwrap();
    assert_eq!(
        default,
        ChannelSet::from_channels([Channel::Phone, Channel::Web, Channel::Os]),
        "precondition: the seeded ASK default is phone+web+os"
    );

    // FLIP the global ASK rule to phone+web through the settings-grid RPC. This
    // DROPS os for ASK (and keeps phone+web).
    let flip = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(2),
        method: methods::HANGAR_NOTIFY_RULE_SET.into(),
        params: serde_json::json!({ "kind": "ask_user_question", "channels": ["phone", "web"] }),
    };
    let flip_resp = dispatch(store.pool(), &flip, &health, &sink).await;
    assert!(flip_resp.error.is_none(), "the rule flip RPC succeeds: {flip_resp:?}");

    // Subscribe to the attention stream BEFORE the raise so we capture the emitted
    // event's channels (the live-consumer copy of the routing decision).
    let mut events = broker.subscribe_attention();

    // Raise an ASK through the HOOK SEAM: plant the transcript, append the
    // Notification hook line, run one ingest pass.
    let fx = plant_ask("flip");
    let sid = "sid-ask-flip";
    let events_jsonl = dir.path().join("events.jsonl");
    let cursor = dir.path().join("attention_ingest.offset");
    std::fs::write(&events_jsonl, format!("{}\n", hook_line(sid, &fx.cwd))).unwrap();

    let ingest = AttentionIngest::new(store.pool().clone(), sink.clone(), events_jsonl, cursor);
    let raised = ingest.ingest_once(5000).await;
    assert_eq!(raised, 1, "the Notification hook raises exactly one ASK row");

    // The stamped row carries the FLIPPED channels (phone+web), NOT the default.
    let feed = attention_feed(&store, &health, &sink).await;
    assert_eq!(feed.len(), 1);
    let ask = &feed[0];
    assert_eq!(ask.kind, "ask_user_question");
    assert_eq!(
        ask.channels,
        ChannelSet::from_channels([Channel::Phone, Channel::Web]),
        "the raised row carries the flipped rule, resolved at emit time"
    );

    // The consumers filter on the row's channels. The flip ENABLED phone and
    // DROPPED os, so: the phone consumer delivers, the os consumer is suppressed,
    // and web (kept) still delivers.
    let phone = deliver_to(&feed, Channel::Phone);
    let os = deliver_to(&feed, Channel::Os);
    let web = deliver_to(&feed, Channel::Web);
    assert_eq!(phone, vec![ask.id.clone()], "the ALLOWED phone channel fired");
    assert!(os.is_empty(), "the SUPPRESSED os channel did NOT fire after the flip");
    assert_eq!(web, vec![ask.id.clone()], "the still-routed web channel fired");

    // The AttentionRaised event carried the same resolved channels (the live
    // consumers reacting to the nudge see the identical decision, no re-resolve).
    let evt = events.try_recv().expect("an AttentionRaised event was emitted");
    match evt {
        HangarEvent::AttentionRaised { channels, kind, .. } => {
            assert_eq!(kind, "ask_user_question");
            assert_eq!(
                channels,
                ChannelSet::from_channels([Channel::Phone, Channel::Web]),
                "the event carries the emit-time channels"
            );
        }
        other => panic!("expected AttentionRaised, got {other:?}"),
    }
}

/// Seed a workspace row so a per-workspace notify-rule override can be set + FK'd
/// by an attention raised WITH that workspace (mirrors the store repo fixture).
async fn seed_workspace(pool: &sqlx::SqlitePool, ws: &str) {
    sqlx::query("INSERT INTO workspace (id, slug, name, created_at) VALUES (?, ?, ?, ?)")
        .bind(ws)
        .bind(ws)
        .bind(ws)
        .bind(1_000_i64)
        .execute(pool)
        .await
        .expect("seed workspace");
}

/// Seed a minimal ATC instance so `raise_escalation`'s retry-ledger write
/// (`atc_retry.instance_name` FKs `atc_instance(name)`) succeeds — the real
/// escalation raise path needs a registered instance.
async fn seed_atc_instance(pool: &sqlx::SqlitePool, name: &str) {
    sqlx::query("INSERT INTO atc_instance (name, heartbeat_cron, created_at) VALUES (?, ?, ?)")
        .bind(name)
        .bind("*/5 * * * *")
        .bind(1_000_i64)
        .execute(pool)
        .await
        .expect("seed atc instance");
}

/// tcp T5 (agents-in-a-box-cqh) — the WORKSPACE-scoped routing path, end to end.
///
/// The global test above proves a hook-raised (workspace-less) attention honours
/// the GLOBAL rule. This one closes the workspace half the settings grid's new
/// `g`-toggle "Workspace" scope drives: set a per-workspace OVERRIDE through the
/// SAME `notify_rule_set` RPC (now carrying a `workspace_id`), then raise an
/// attention WITH that workspace attached (an ATC escalation — the real
/// workspace-scoped raise path) and prove the row resolves the OVERRIDE, while a
/// simultaneous workspace-less raise still resolves the untouched GLOBAL default.
///
/// ```text
///  notify_rule_set(escalation → os, workspace_id=ws-a)   (grid Workspace scope)
///         │
///  atc::raise_escalation(ws-a)  ─▶ resolve at emit ─▶ row.channels = {os}   (override wins)
///  atc::raise_escalation(None)  ─▶ resolve at emit ─▶ row.channels = {phone,web,os} (global)
/// ```
///
/// A genuine regression guard: BEFORE the workspace override is written, ws-a
/// resolves the global escalation default (phone+web+os), so the `{os}`-only
/// assertion would fail; and if the override leaked to the global scope, the
/// workspace-less raise's phone/web assertions would fail.
#[tokio::test]
async fn workspace_override_routes_a_workspace_scoped_raise_and_leaves_global_intact() {
    use ainb_hangar_daemon::atc;
    use ainb_hangar_store::repo::attention::AttentionKind;
    use ainb_hangar_store::repo::notify_rule::NotifyRuleRepo;

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_in(dir.path()).await.unwrap();
    let broker = EventBroker::new();
    let sink = broker.sink();
    let health = health();
    seed_workspace(store.pool(), "ws-a").await;
    seed_atc_instance(store.pool(), "inst-ws").await;
    seed_atc_instance(store.pool(), "inst-global").await;

    // Baseline: with no override, ws-a resolves the GLOBAL escalation default.
    let default = NotifyRuleRepo::resolve(store.pool(), AttentionKind::Escalation, Some("ws-a"))
        .await
        .unwrap();
    assert_eq!(
        default,
        ChannelSet::from_channels([Channel::Phone, Channel::Web, Channel::Os]),
        "precondition: ws-a inherits the global escalation default (phone+web+os)"
    );

    // Write a WORKSPACE override through the grid's RPC WITH a workspace_id: ws-a
    // escalation → os only (drops phone + web). This is exactly what the settings
    // grid now sends in "Workspace" scope.
    let flip = RpcRequest {
        jsonrpc: ainb_hangar_proto::jsonrpc_version(),
        id: RpcId::Number(3),
        method: methods::HANGAR_NOTIFY_RULE_SET.into(),
        params: serde_json::json!({
            "workspace_id": "ws-a",
            "kind": "escalation",
            "channels": ["os"],
        }),
    };
    let flip_resp = dispatch(store.pool(), &flip, &health, &sink).await;
    assert!(flip_resp.error.is_none(), "the workspace rule flip succeeds: {flip_resp:?}");

    // Raise an escalation FOR ws-a (a real workspace-scoped raise path) and one
    // with NO workspace (host-wide), both resolved once at emit time.
    atc::raise_escalation(
        store.pool(),
        &sink,
        "inst-ws",
        "sid-esc-ws",
        "/tmp/ws",
        Some("ws-a"),
        "stuck",
        7_000,
    )
    .await
    .expect("workspace-scoped escalation raises");
    atc::raise_escalation(
        store.pool(),
        &sink,
        "inst-global",
        "sid-esc-global",
        "/tmp/global",
        None,
        "stuck",
        8_000,
    )
    .await
    .expect("host-wide escalation raises");

    let feed = attention_feed(&store, &health, &sink).await;
    let ws_row = feed
        .iter()
        .find(|r| r.workspace_id.as_deref() == Some("ws-a"))
        .expect("the workspace escalation is in the feed");
    let global_row = feed
        .iter()
        .find(|r| r.session_id == "sid-esc-global")
        .expect("the host-wide escalation is in the feed");

    // POSITIVE: the workspace-scoped raise honours the OVERRIDE (os only).
    assert_eq!(ws_row.kind, "escalation");
    assert_eq!(
        ws_row.channels,
        ChannelSet::from_channels([Channel::Os]),
        "the workspace-scoped raise resolves the workspace override (os only)"
    );
    // The flipped channel decision is what the consumers filter on.
    assert!(deliver_to(&feed, Channel::Os).contains(&ws_row.id), "os fired for ws-a");
    assert!(
        !deliver_to(&feed, Channel::Phone).contains(&ws_row.id),
        "phone was DROPPED by the ws-a override"
    );

    // NEGATIVE: the workspace override did NOT leak to the global scope — a
    // workspace-less raise still resolves the untouched global default.
    assert_eq!(
        global_row.channels,
        ChannelSet::from_channels([Channel::Phone, Channel::Web, Channel::Os]),
        "the host-wide raise resolves the untouched global escalation default"
    );
}
