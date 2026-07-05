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
//! This is a genuine regression guard: BEFORE the emit-time resolution the row
//! would carry the SEEDED default (web+os), so the phone assertion (allowed) and
//! the os assertion (suppressed) would both fail. Nothing is seeded for the
//! interaction path — the rule flip and the raise both run live. No tmux, no real
//! Telegram/web-push: the "consumers" are pure `ChannelSet::contains` filters (the
//! exact gate the bridge + web push run), recording deliveries into a Vec.

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

    // Baseline: the SEEDED default routes ASK to web+os (never phone).
    use ainb_hangar_store::repo::attention::AttentionKind;
    use ainb_hangar_store::repo::notify_rule::NotifyRuleRepo;
    let default = NotifyRuleRepo::resolve(store.pool(), AttentionKind::AskUserQuestion, None)
        .await
        .unwrap();
    assert_eq!(
        default,
        ChannelSet::from_channels([Channel::Web, Channel::Os]),
        "precondition: the seeded ASK default is web+os"
    );

    // FLIP the global ASK rule to phone+web through the settings-grid RPC. This
    // ENABLES phone and DROPS os for ASK.
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
