//! A scripted ACP adapter for CI. FIXTURE, not a real adapter.
//!
//! Speaks newline-delimited JSON-RPC on stdio, exactly like `claude-agent-acp`
//! and `codex-acp` do, but every answer comes from an environment variable
//! instead of a model. That is what makes the client's invariants (mode
//! pinning, env allowlist, handler-before-load, `-32602` surfacing) assertable
//! on a machine with no adapters and no credentials.
//!
//! | Variable | Effect |
//! |---|---|
//! | `FAKE_ACP_ENV_DUMP` | write the process environment to this path as JSON, then continue |
//! | `FAKE_ACP_NO_LOAD` | advertise `loadSession: false` |
//! | `FAKE_ACP_MODE_ON_NEW` | `currentModeId` returned by `session/new` (default `default`) |
//! | `FAKE_ACP_NO_MODES` | omit `modes` from `session/new` entirely |
//! | `FAKE_ACP_MODE_ECHO` | mode echoed by `current_mode_update` after `session/set_mode` (default: the requested one) |
//! | `FAKE_ACP_REFUSE_SET_MODE` | answer `session/set_mode` with `-32602` |
//! | `FAKE_ACP_SCRIPT` | ndjson file of `session/update` payloads to emit per prompt |
//! | `FAKE_ACP_CHUNKS` | emit N `agent_message_chunk` updates per prompt instead of a script |
//! | `FAKE_ACP_LOAD_REPLAY` | emit N updates BEFORE answering `session/load` (the replay-ordering probe) |
//! | `FAKE_ACP_MODE_FLIP` | emit a `current_mode_update` carrying this mode at the END of every prompt turn (the mid-conversation flip) |

use std::io::{BufRead, BufWriter, Write};

fn main() {
    if let Ok(path) = std::env::var("FAKE_ACP_ENV_DUMP") {
        let env: serde_json::Map<String, serde_json::Value> = std::env::vars()
            .map(|(name, value)| (name, serde_json::Value::String(value)))
            .collect();
        let _ = std::fs::write(
            path,
            serde_json::to_string(&serde_json::Value::Object(env)).unwrap_or_default(),
        );
    }

    let stdin = std::io::stdin();
    let mut out = BufWriter::new(std::io::stdout());
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message): Result<serde_json::Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        let (Some(method), Some(id)) = (message["method"].as_str(), message.get("id")) else {
            // Notifications ($/cancel_request, session/cancel) need no reply.
            continue;
        };
        handle(&mut out, method, id, &message["params"]);
    }
}

fn handle<W: Write>(out: &mut W, method: &str, id: &serde_json::Value, params: &serde_json::Value) {
    let session_id = params["sessionId"].as_str().unwrap_or("fake-session").to_string();
    match method {
        "initialize" => respond(
            out,
            id,
            &serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": !flag("FAKE_ACP_NO_LOAD")},
                "authMethods": [],
                "agentInfo": {"name": "fake-acp-adapter", "version": "0.0.0-fixture"},
            }),
        ),
        "session/new" => {
            let mut result = serde_json::json!({"sessionId": "fake-session-1"});
            if !flag("FAKE_ACP_NO_MODES") {
                let mode = var("FAKE_ACP_MODE_ON_NEW").unwrap_or_else(|| "default".to_string());
                result["modes"] = serde_json::json!({
                    "currentModeId": mode,
                    "availableModes": [
                        {"id": "default", "name": "Default"},
                        {"id": "plan", "name": "Plan"},
                        {"id": "bypassPermissions", "name": "Bypass"},
                    ],
                });
            }
            respond(out, id, &result);
        }
        "session/load" => {
            // The replay arrives BEFORE the reply, which is the exact ordering
            // that breaks a client registering its handler after the call.
            for index in 0..count("FAKE_ACP_LOAD_REPLAY") {
                notify_update(
                    out,
                    &session_id,
                    &serde_json::json!({
                        "sessionUpdate": "agent_message_chunk",
                        "content": {"type": "text", "text": format!("replay-{index} ")},
                    }),
                );
            }
            let mut result = serde_json::json!({});
            if !flag("FAKE_ACP_NO_MODES") {
                let mode = var("FAKE_ACP_MODE_ON_NEW").unwrap_or_else(|| "default".to_string());
                result["modes"] = serde_json::json!({
                    "currentModeId": mode,
                    "availableModes": [{"id": "default", "name": "Default"}],
                });
            }
            respond(out, id, &result);
        }
        "session/set_mode" => {
            if flag("FAKE_ACP_REFUSE_SET_MODE") {
                respond_error(out, id, -32602, "unknown mode id");
                return;
            }
            respond(out, id, &serde_json::json!({}));
            let echo = var("FAKE_ACP_MODE_ECHO")
                .unwrap_or_else(|| params["modeId"].as_str().unwrap_or("default").to_string());
            notify_update(
                out,
                &session_id,
                &serde_json::json!({"sessionUpdate": "current_mode_update", "currentModeId": echo}),
            );
        }
        "session/prompt" => {
            emit_turn(out, &session_id);
            flip_mode(out, &session_id);
            respond(out, id, &serde_json::json!({"stopReason": "end_turn"}));
        }
        "session/close" => respond(out, id, &serde_json::json!({})),
        _ => respond_error(out, id, -32601, "method not found"),
    }
}

fn emit_turn<W: Write>(out: &mut W, session_id: &str) {
    if let Some(path) = var("FAKE_ACP_SCRIPT") {
        let script = std::fs::read_to_string(path).unwrap_or_default();
        for line in script.lines().filter(|line| !line.trim().is_empty()) {
            if let Ok(update) = serde_json::from_str::<serde_json::Value>(line) {
                notify_update(out, session_id, &update);
            }
        }
        return;
    }
    for index in 0..count("FAKE_ACP_CHUNKS") {
        notify_update(
            out,
            session_id,
            &serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": format!("chunk-{index} ")},
            }),
        );
    }
}

/// A live session changing permission regime mid conversation, long after the
/// spawn-time assertion passed.
fn flip_mode<W: Write>(out: &mut W, session_id: &str) {
    if let Some(mode) = var("FAKE_ACP_MODE_FLIP") {
        notify_update(
            out,
            session_id,
            &serde_json::json!({"sessionUpdate": "current_mode_update", "currentModeId": mode}),
        );
    }
}

fn notify_update<W: Write>(out: &mut W, session_id: &str, update: &serde_json::Value) {
    write_line(
        out,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": session_id, "update": update},
        }),
    );
}

fn respond<W: Write>(out: &mut W, id: &serde_json::Value, result: &serde_json::Value) {
    write_line(
        out,
        &serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
    );
}

fn respond_error<W: Write>(out: &mut W, id: &serde_json::Value, code: i32, message: &str) {
    write_line(
        out,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }),
    );
}

fn write_line<W: Write>(out: &mut W, message: &serde_json::Value) {
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn flag(name: &str) -> bool {
    var(name).is_some_and(|value| value != "0")
}

fn count(name: &str) -> usize {
    var(name).and_then(|value| value.parse().ok()).unwrap_or(0)
}
