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
    let process = AdapterProcess::spawn(&config, tx, permission_sink())
        .await
        .expect("spawn real adapter");
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
    let first = AdapterProcess::spawn(&config, tx, permission_sink())
        .await
        .expect("spawn real adapter");
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
    let second = AdapterProcess::spawn(&config, tx, permission_sink())
        .await
        .expect("respawn real adapter");
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

/// A permission sink nothing reads: these suites drive the protocol legs, not
/// R8's answer path (that lives in the daemon's pool tests). Dropping the
/// receiver would answer every ask `Cancelled`, so the sender is leaked
/// deliberately to keep the fixture's behaviour unchanged.
fn permission_sink() -> tokio::sync::mpsc::UnboundedSender<ainb_acp::client::PermissionRequest> {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    std::mem::forget(rx);
    tx
}

/// Move 1 probe A0 (`docs/hangar/renovation/move1-acp-tasks.md` §3c): does
/// `claude-agent-acp` surface the `AskUserQuestion` tool as a
/// `session/request_permission` carrying the question's options, or does it
/// only appear as a `ToolCall` update the client is never asked about?
///
/// The exit criterion for move 1 hangs on the answer, so this is a PROBE, not
/// an assertion of either: it drives one turn, logs every `session/update` and
/// every permission request raw (to stderr, and to `AINB_ACP_PROBE_LOG` when
/// set), answers any permission (the option named "blue" when offered, else
/// the first) so the turn can finish, and prints one `A0 VERDICT:` line:
/// `REQUEST_PERMISSION` / `TOOL CALL ONLY` / `NOT OFFERED`, with the agent's
/// own reply as evidence. It only fails when the turn cannot run to
/// completion, because that is the "could not run" outcome.
///
/// Recorded 2026-09-02 against `@zed-industries/claude-agent-acp` 0.23.1:
/// NOT OFFERED. The adapter's `session/new` passes
/// `disallowedTools: ["AskUserQuestion"]` to the SDK (`dist/acp-agent.js`,
/// "Disable this for now, not a great way to expose this over ACP"), and the
/// agent's own `ToolSearch` for it returns no match. Ordinary tool permissions
/// (a `Skill` call here) DO arrive as `session/request_permission` with
/// `allow_always` / `allow` / `reject`, and answering one unblocks the turn.
#[tokio::test]
#[ignore = "real adapter + credentials"]
async fn claude_agent_acp_ask_user_question_probe() {
    let Some(config) = gated(CLAUDE_ADAPTER) else {
        return;
    };
    let cwd = tempfile::tempdir().expect("probe cwd");
    // claude-agent-acp merges `~/.claude/settings.json` (user) under the cwd's
    // `.claude/settings.json` (project) and refuses `session/new` outright when
    // the merged `permissions.defaultMode` is an alias it does not know (the
    // operator's `auto` is one, observed on 0.23.1). A project-level override
    // in the throwaway cwd keeps the probe off the operator's global config.
    std::fs::create_dir_all(cwd.path().join(".claude")).expect("probe .claude dir");
    std::fs::write(
        cwd.path().join(".claude/settings.json"),
        r#"{"permissions":{"defaultMode":"default"}}"#,
    )
    .expect("probe project settings");
    let (tx, mut rx) = mpsc::unbounded_channel();
    let (permission_tx, mut permission_rx) = mpsc::unbounded_channel();
    let process = AdapterProcess::spawn(&config, tx, permission_tx)
        .await
        .expect("spawn real adapter");
    let mut log = vec![format!(
        "agentInfo {:?} mode {}",
        process.info(),
        config.permission_mode
    )];
    let session = process.new_session(cwd.path()).await.expect("session/new");

    let prompt = "Use the AskUserQuestion tool to ask me exactly one question, \
                  \"Which colour?\", with exactly two options: \"red\" and \"blue\". \
                  Do not use any other tool. After I answer, reply with only the \
                  option I chose, in lower case, and nothing else.";
    let turn = process.prompt(&session, prompt);
    tokio::pin!(turn);
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(180));
    tokio::pin!(deadline);

    // Counted only when the tool call itself is named AskUserQuestion (its
    // `title` or `_meta.claudeCode.toolName`), never when another tool merely
    // mentions it: the agent's own ToolSearch for the tool must not count as
    // the tool.
    let mut permissions = 0usize;
    let mut ask_permissions = 0usize;
    let mut ask_tool_calls = 0usize;
    let mut answered_with = None;
    let mut agent_text = String::new();
    let outcome = loop {
        tokio::select! {
            result = &mut turn => break Some(result),
            Some(notification) = rx.recv() => {
                let value = serde_json::to_value(&notification).unwrap_or_default();
                let update = &value["update"];
                if names_tool(update, "AskUserQuestion") {
                    ask_tool_calls += 1;
                }
                if update["sessionUpdate"] == "agent_message_chunk" {
                    agent_text.push_str(update["content"]["text"].as_str().unwrap_or_default());
                }
                log.push(format!("update {value}"));
            }
            Some(permission) = permission_rx.recv() => {
                permissions += 1;
                let tool_call = serde_json::to_value(&permission.request.tool_call).unwrap_or_default();
                if names_tool(&tool_call, "AskUserQuestion") {
                    ask_permissions += 1;
                }
                let options = permission.options_wire();
                log.push(format!("request_permission tool_call={tool_call} options={options:?}"));
                // Prefer the option that is NOT the first one offered, so the
                // final reply proves the agent acted on the pick rather than on
                // a highlighted default (defect 26's bug class).
                let pick = options
                    .iter()
                    .find(|o| o["name"].as_str().is_some_and(|n| n.to_lowercase().contains("blue")))
                    .or_else(|| options.first())
                    .and_then(|o| o["optionId"].as_str().map(str::to_string));
                match pick {
                    Some(id) => {
                        log.push(format!("answered optionId={id}"));
                        answered_with = Some(id.clone());
                        permission.answer_selected(&id).expect("answer the permission");
                    }
                    None => {
                        log.push("answered cancelled (no options offered)".to_string());
                        permission.answer_cancelled().expect("cancel the permission");
                    }
                }
            }
            () = &mut deadline => break None,
        }
    };
    // Late notifications can trail the prompt reply; keep them in the log
    // (bounded, so a chatty adapter cannot keep the probe alive forever).
    let mut trailing = 0usize;
    while trailing < 200 {
        let Ok(Some(notification)) =
            tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv()).await
        else {
            break;
        };
        trailing += 1;
        log.push(format!(
            "update {}",
            serde_json::to_string(&notification).unwrap_or_default()
        ));
    }
    log.push(format!(
        "turn outcome {outcome:?} (trailing notifications: {trailing})"
    ));

    let verdict = match (ask_permissions, ask_tool_calls) {
        (0, 0) => format!(
            "NOT OFFERED: no AskUserQuestion tool call and no request_permission for it \
             ({permissions} request_permission(s) for other tools, answered {answered_with:?}); \
             agent said: {agent_text:?}"
        ),
        (0, n) => format!(
            "TOOL CALL ONLY: AskUserQuestion in {n} tool_call update(s), no request_permission \
             for it; agent said: {agent_text:?}"
        ),
        (p, n) => format!(
            "REQUEST_PERMISSION: {p} request(s) for AskUserQuestion ({n} tool_call update(s)), \
             answered with {answered_with:?}; agent said: {agent_text:?}"
        ),
    };
    log.push(format!("A0 VERDICT: {verdict}"));
    let text = log.join("\n");
    eprintln!("{text}");
    if let Ok(path) = std::env::var("AINB_ACP_PROBE_LOG") {
        std::fs::write(&path, format!("{text}\n")).expect("write AINB_ACP_PROBE_LOG");
    }
    assert!(
        outcome.is_some(),
        "the probe turn did not complete within 180s"
    );
}

/// True when a `tool_call` / `tool_call_update` payload (or a permission's
/// `tool_call`) is for `tool`: claude-agent-acp puts the Claude Code tool name
/// in `title` and again in `_meta.claudeCode.toolName`.
fn names_tool(tool_call: &serde_json::Value, tool: &str) -> bool {
    tool_call["title"] == tool || tool_call["_meta"]["claudeCode"]["toolName"] == tool
}
