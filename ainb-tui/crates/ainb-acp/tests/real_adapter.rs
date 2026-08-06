//! REAL ADAPTER probes, promoted from the spike. `#[ignore]` + env gate.
//!
//! DISCLOSURE: unlike every other test in this crate, these drive the ACTUAL
//! `claude-agent-acp` / `codex-acp` binaries and consume real credentials.
//! They are never CI-required: adapter versions drift on npm and the runners
//! have no credentials. Run them by hand before a release and record the
//! adapter versions in the PR.
//!
//! ```sh
//! AINB_ACP_REAL_ADAPTERS=1 cargo test -p ainb-acp -- --ignored
//! ```
//!
//! Gates:
//! * `AINB_ACP_REAL_ADAPTERS=1` must be set (so a bare `--ignored` on a
//!   credential-less box skips instead of failing).
//! * The adapter binary must resolve on `PATH`.
//! * `AINB_ACP_REAL_MODE` overrides the mode asserted (default `default`).

use std::path::Path;

use ainb_acp::client::AdapterProcess;
use ainb_acp::config::{AdapterConfig, CLAUDE_ADAPTER, CODEX_ADAPTER};
use ainb_acp::reducer::TranscriptReducer;
use tokio::sync::mpsc;

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
        // The adapters' own credential paths. Named, not inherited.
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

/// The spike's security finding, asserted against the real thing: the
/// negotiated mode is the one we asked for, and specifically NOT the
/// `bypassPermissions` the adapter was observed inheriting from ambient state.
async fn mode_is_the_requested_one(adapter: &str) {
    let Some(config) = gated(adapter) else { return };
    let requested = config.permission_mode.clone();
    let (tx, _rx) = mpsc::unbounded_channel();
    let process = AdapterProcess::spawn(&config, tx).await.expect("spawn real adapter");
    eprintln!(
        "real adapter {adapter} reported agentInfo {:?}",
        process.info()
    );
    let session = process
        .new_session(&std::env::temp_dir())
        .await
        .expect("session/new against the real adapter");
    let observed = process.observed_mode(&session);
    assert_eq!(observed.as_deref(), Some(requested.as_str()));
    assert_ne!(observed.as_deref(), Some("bypassPermissions"));
}

#[tokio::test]
#[ignore = "real adapter + credentials"]
async fn claude_agent_acp_negotiates_the_requested_mode() {
    mode_is_the_requested_one(CLAUDE_ADAPTER).await;
}

#[tokio::test]
#[ignore = "real adapter + credentials"]
async fn codex_acp_negotiates_the_requested_mode() {
    mode_is_the_requested_one(CODEX_ADAPTER).await;
}

/// The spike's resume shape: tell the agent a secret word, drop the client,
/// respawn, `session/load` the SAME adapter session id, and ask for the word
/// back. Proves the load path recalls history AND that the replay reaches a
/// handler that was live before the load was issued.
async fn secret_word_survives_a_reload(adapter: &str) {
    let Some(config) = gated(adapter) else { return };
    let cwd = std::env::temp_dir();

    let (tx, _rx) = mpsc::unbounded_channel();
    let first = AdapterProcess::spawn(&config, tx).await.expect("spawn real adapter");
    if !first.supports_load() {
        eprintln!("skipped: {adapter} does not advertise loadSession");
        return;
    }
    let session = first.new_session(&cwd).await.expect("session/new");
    first
        .prompt(
            &session,
            "Remember this secret word and reply with just the word: kumquat",
        )
        .await
        .expect("session/prompt");
    drop(first);

    let (tx, mut rx) = mpsc::unbounded_channel();
    let second = AdapterProcess::spawn(&config, tx).await.expect("respawn real adapter");
    second.load_session(&session, Path::new(&cwd)).await.expect("session/load");

    // `session/load` replays the ENTIRE history as notifications, the secret
    // word among them. Feeding that replay to the reducer would make the
    // assertion below pass on the replay alone, no matter what the agent
    // recalled, so it is drained and discarded here. `begin_turn` then makes
    // the final message the POST-LOAD turn's agent text and nothing else.
    let replayed = drain(&mut rx).await;
    eprintln!("discarded {replayed} replayed notifications from session/load");

    let mut reducer = TranscriptReducer::new(session.clone());
    reducer.begin_turn();
    second
        .prompt(
            &session,
            "What was the secret word? Reply with just the word.",
        )
        .await
        .expect("session/prompt after load");

    while let Ok(notification) = rx.try_recv() {
        reducer.push(&notification.update);
    }
    reducer.flush();
    assert!(
        reducer.final_message().to_lowercase().contains("kumquat"),
        "reloaded session did not recall the secret word: {:?}",
        reducer.final_message()
    );
}

/// Discard everything queued, waiting out the dispatch loop rather than racing
/// it: the load reply can land before the last replayed notification does.
async fn drain(
    rx: &mut mpsc::UnboundedReceiver<agent_client_protocol::schema::v1::SessionNotification>,
) -> usize {
    let mut discarded = 0;
    while let Ok(Some(_)) =
        tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
    {
        discarded += 1;
    }
    discarded
}

#[tokio::test]
#[ignore = "real adapter + credentials"]
async fn claude_agent_acp_recalls_a_secret_word_after_reload() {
    secret_word_survives_a_reload(CLAUDE_ADAPTER).await;
}

#[tokio::test]
#[ignore = "real adapter + credentials"]
async fn codex_acp_recalls_a_secret_word_after_reload() {
    secret_word_survives_a_reload(CODEX_ADAPTER).await;
}
