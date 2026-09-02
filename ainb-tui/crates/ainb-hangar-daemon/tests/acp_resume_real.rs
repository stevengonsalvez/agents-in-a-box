//! REAL ADAPTER resume probes, at the POOL level. `#[ignore]` + env gate.
//!
//! DISCLOSURE: unlike `acp_pool.rs` and `rpc_acp.rs`, which drive the scripted
//! `fake_acp_adapter` fixture, these drive the ACTUAL `claude-agent-acp` /
//! `codex-acp` binaries and consume real credentials. They are never
//! CI-required: adapter versions drift on npm and the runners have neither the
//! binaries nor the credentials.
//!
//! ```sh
//! AINB_ACP_REAL_ADAPTERS=1 cargo nextest run -p ainb-hangar-daemon \
//!   --test acp_resume_real --run-ignored all
//! ```
//!
//! `ainb-acp/tests/real_adapter.rs` proves the PROTOCOL leg (a secret word
//! survives `session/load` on a fresh `AdapterProcess`). These prove the DAEMON
//! leg, which is the one the plan's Phase 6 manual steps describe and the one
//! the protocol probe cannot reach:
//!
//! * the whole conversation survives a SIGKILL of the adapter AND a restart of
//!   the daemon that owned it, recovered through the pool's own resume routine
//!   with the boot scan in between;
//! * a load that CANNOT succeed falls back to re-prime, and the transcript says
//!   so with an `acp.context_rebuilt {mode: reprimed}` marker, with the rebuilt
//!   context good enough to answer a question about the earlier turn.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ainb_acp::config::{AdapterConfig, CLAUDE_ADAPTER, CODEX_ADAPTER};
use ainb_hangar_daemon::acp_pool::{AcpPool, PoolConfig, SubmitOutcome};
use ainb_hangar_daemon::events::EventBroker;
use ainb_hangar_store::Store;
use ainb_hangar_store::repo::fleet::{FleetSessionPatch, NewFleetEvent, ObservationAuthority};
use ainb_hangar_store::repo::fleet_acp_session::{FleetAcpSessionRepo, NewFleetAcpSession};
use ainb_hangar_store::repo::fleet_message::{FleetMessageRepo, NewFleetMessage};
use ainb_hangar_store::repo::fleet_provider_event::FleetProviderEventRepo;

const SECRET: &str = "kumquat";

/// `None` when the gate is closed, so a bare `--run-ignored all` on a
/// credential-less box SKIPS loudly instead of failing.
fn gated(adapter: &str) -> Option<AdapterConfig> {
    if std::env::var("AINB_ACP_REAL_ADAPTERS").ok().as_deref() != Some("1") {
        eprintln!("skipped: AINB_ACP_REAL_ADAPTERS is not 1");
        return None;
    }
    if which(adapter).is_none() {
        eprintln!("skipped: {adapter} is not on PATH");
        return None;
    }
    let mode = std::env::var("AINB_ACP_REAL_MODE").unwrap_or_else(|_| "default".to_string());
    Some(AdapterConfig::new(adapter, mode).env_passthrough(vec![
        // The adapters' own credential paths. NAMED, never inherited (I13).
        "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
        "ANTHROPIC_API_KEY".to_string(),
        "OPENAI_API_KEY".to_string(),
        "XDG_CONFIG_HOME".to_string(),
    ]))
}

fn which(binary: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(binary))
            .find(|candidate| candidate.is_file())
    })
}

fn pool_for(store: &Store, config: &AdapterConfig) -> Arc<AcpPool> {
    let mut pool_config = PoolConfig::default();
    pool_config.adapters.insert(config.name.clone(), config.clone());
    AcpPool::new(store.clone(), EventBroker::new().sink(), pool_config)
}

/// The `fleet_session` + `fleet_acp_session` pair `fleet/acp_session_create`
/// writes, minus the RPC layer.
async fn seed_session(store: &Store, session_key: &str, provider: &str, cwd: &str) -> String {
    let scope_key = format!("session:{session_key}");
    let event = NewFleetEvent {
        event_id: format!("acp-session-create:{session_key}"),
        session_key: session_key.to_string(),
        observed_at: 1,
        authority: ObservationAuthority::Authoritative,
        event_type: "acp_session_created".to_string(),
        payload: "{}".to_string(),
        patch: FleetSessionPatch {
            provider: Some("acp".to_string()),
            cwd: Some(cwd.to_string()),
            management_state: Some("MANAGED".to_string()),
            capabilities: Some(r#"{"send_prompt":true,"interrupt":true}"#.to_string()),
            lifecycle_state: Some("IDLE".to_string()),
            ..FleetSessionPatch::default()
        },
    };
    FleetAcpSessionRepo::insert_with_fleet_session(
        store.pool(),
        &NewFleetAcpSession {
            session_key: session_key.to_string(),
            scope_key: scope_key.clone(),
            provider: provider.to_string(),
            cwd: cwd.to_string(),
            permission_mode: "default".to_string(),
            state: "IDLE".to_string(),
            created_at: 1,
            last_active_at: 1,
        },
        &event,
    )
    .await
    .expect("seed acp session");
    scope_key
}

/// Persist one operator message to `session_key` and hand it to the pool.
async fn ask(
    store: &Store,
    pool: &Arc<AcpPool>,
    session_key: &str,
    id: &str,
    text: &str,
) -> String {
    let scope_key = format!("session:{session_key}");
    FleetMessageRepo::insert_message_with_deliveries(
        store.pool(),
        &NewFleetMessage {
            id: id.to_string(),
            request_id: None,
            request_fingerprint: None,
            scope_key,
            origin_message_id: None,
            sender: "operator".to_string(),
            kind: "user".to_string(),
            body: text.to_string(),
            created_at: 1,
        },
        &[session_key.to_string()],
    )
    .await
    .expect("persist the prompt");
    assert_eq!(
        pool.submit_prompt(session_key, id, text).await,
        SubmitOutcome::Queued
    );
    id.to_string()
}

/// Wait out a REAL model turn (minutes, not milliseconds) and return the leg.
async fn await_terminal(store: &Store, message_id: &str, session_key: &str) -> (String, String) {
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        let leg = FleetMessageRepo::deliveries_for_message(store.pool(), message_id)
            .await
            .expect("deliveries")
            .into_iter()
            .find(|leg| leg.session_key == session_key);
        if let Some(leg) = leg {
            if leg.state != "PENDING" {
                return (leg.state, leg.detail.unwrap_or_default());
            }
        }
        assert!(
            Instant::now() < deadline,
            "the real adapter never resolved {message_id}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// The agent's reply to `message_id`, as the chat timeline has it.
async fn reply_to(store: &Store, message_id: &str) -> String {
    FleetMessageRepo::list_by_origin(store.pool(), message_id, 0, 10)
        .await
        .expect("replies")
        .into_iter()
        .map(|row| row.body)
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

async fn transcript_kinds(store: &Store, session_key: &str) -> Vec<(String, String)> {
    FleetProviderEventRepo::list_by_session_after(store.pool(), session_key, 0, 1000)
        .await
        .expect("transcript")
        .into_iter()
        .map(|row| (row.event_type, row.raw_payload))
        .collect()
}

/// The plan's Phase 6 manual step, automated: tell the agent a secret word,
/// SIGKILL the adapter, throw the daemon's pool away entirely, boot a new one
/// over the same store, and ask for the word back.
///
/// The boot scan runs in between, exactly as a restarted daemon runs it, so
/// this also proves convergence leaves the scope USABLE rather than merely
/// tidy.
async fn a_secret_word_survives_a_daemon_and_adapter_kill(adapter: &str, key: &str) {
    let Some(config) = gated(adapter) else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().to_string();
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_session(&store, key, adapter, &cwd).await;

    let first = pool_for(&store, &config);
    let told = ask(
        &store,
        &first,
        key,
        "msg-secret",
        &format!("Remember this secret word and reply with just the word: {SECRET}"),
    )
    .await;
    let (state, detail) = await_terminal(&store, &told, key).await;
    assert_eq!(state, "DELIVERED", "{detail}");

    // SIGKILL the adapter, then drop the pool: between them, that is the whole
    // of "the daemon and its adapter both died".
    assert!(first.kill_provider(adapter).await, "the adapter was killed");
    drop(first);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A fresh daemon: boot scan first, then a brand-new pool over the same store.
    ainb_hangar_daemon::acp_pool::converge_dirty_sessions_at_boot(
        store.pool(),
        &EventBroker::new().sink(),
    )
    .await;
    let second = pool_for(&store, &config);
    let asked = ask(
        &store,
        &second,
        key,
        "msg-recall",
        "What was the secret word? Reply with just the word.",
    )
    .await;
    let (state, detail) = await_terminal(&store, &asked, key).await;
    assert_eq!(state, "DELIVERED", "{detail}");
    assert!(
        reply_to(&store, &asked).await.contains(SECRET),
        "the resumed session did not recall the secret word (path: {detail})"
    );
    eprintln!("{adapter} resumed by: {detail}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real adapter + credentials"]
async fn claude_agent_acp_recalls_a_secret_word_after_a_daemon_and_adapter_kill() {
    a_secret_word_survives_a_daemon_and_adapter_kill(CLAUDE_ADAPTER, "acp:real-claude").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real adapter + credentials"]
async fn codex_acp_recalls_a_secret_word_after_a_daemon_and_adapter_kill() {
    a_secret_word_survives_a_daemon_and_adapter_kill(CODEX_ADAPTER, "acp:real-codex").await;
}

/// R5's whole point: the design must not DEPEND on `session/load`. Fabricate an
/// adapter session id the real adapter has never issued, so the load provably
/// cannot succeed, and require the re-prime path to answer the SAME question
/// from persisted history alone.
///
/// The fabricated id is a well-formed UUID on purpose. That is the case the
/// gate re-run measured: claude answers a typed `-32002` and codex an opaque
/// `-32603` carrying "no rollout found", and BOTH must classify as rebuild.
async fn a_forced_load_failure_falls_back_to_reprime(adapter: &str, key: &str) {
    let Some(config) = gated(adapter) else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let cwd = dir.path().to_string_lossy().to_string();
    let store = Store::open_in(dir.path()).await.expect("store");
    seed_session(&store, key, adapter, &cwd).await;

    let pool = pool_for(&store, &config);
    let told = ask(
        &store,
        &pool,
        key,
        "msg-secret",
        &format!("Remember this secret word and reply with just the word: {SECRET}"),
    )
    .await;
    let (state, detail) = await_terminal(&store, &told, key).await;
    assert_eq!(state, "DELIVERED", "{detail}");

    assert!(pool.kill_provider(adapter).await);
    drop(pool);
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The forced failure: a well-formed uuid no adapter has ever heard of.
    FleetAcpSessionRepo::set_acp_session_id(
        store.pool(),
        key,
        Some("00000000-0000-4000-8000-00000000dead"),
    )
    .await
    .expect("fabricate an unknown adapter session id");

    let pool = pool_for(&store, &config);
    let asked = ask(
        &store,
        &pool,
        key,
        "msg-recall",
        "What was the secret word? Reply with just the word.",
    )
    .await;
    let (state, detail) = await_terminal(&store, &asked, key).await;
    assert_eq!(state, "DELIVERED", "{detail}");
    // A delivered leg leads with the adapter's stop reason; the resume path is
    // appended after it, and that is the half this probe is about.
    assert!(
        detail.ends_with("; resume=reprimed"),
        "the load could not have succeeded, so the rebuild leg must have run: {detail}"
    );
    assert!(
        transcript_kinds(&store, key).await.iter().any(|(kind, payload)| {
            kind == "acp.context_rebuilt" && payload.contains("\"mode\":\"reprimed\"")
        }),
        "the transcript records the rebuilt context"
    );
    assert!(
        reply_to(&store, &asked).await.contains(SECRET),
        "the re-primed context was not enough to answer"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real adapter + credentials"]
async fn claude_agent_acp_falls_back_to_reprime_on_a_forced_load_failure() {
    a_forced_load_failure_falls_back_to_reprime(CLAUDE_ADAPTER, "acp:real-claude-reprime").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "real adapter + credentials"]
async fn codex_acp_falls_back_to_reprime_on_a_forced_load_failure() {
    a_forced_load_failure_falls_back_to_reprime(CODEX_ADAPTER, "acp:real-codex-reprime").await;
}
