//! A scripted ACP adapter for CI. FIXTURE, not a real adapter.
//!
//! Speaks newline-delimited JSON-RPC on stdio, exactly like `claude-agent-acp`
//! and `codex-acp` do, but every answer comes from an environment variable
//! instead of a model. That is what makes the client's invariants (mode
//! pinning, env allowlist, handler-before-load, `-32602` surfacing) and the
//! pool's invariants (session demux, deadline, convergence) assertable on a
//! machine with no adapters and no credentials.
//!
//! It is CONCURRENT by construction: every `session/prompt` runs on its own
//! thread behind a shared stdout lock. A multiplexed pool hosts several
//! sessions on one process, so a fixture that answered prompts one at a time
//! could not express "session A hangs while session B completes", which is
//! exactly the I16 deadline case.
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
//! | `FAKE_ACP_CHUNK_DELAY_MS` | sleep this long between chunks (paced streaming, I12) |
//! | `FAKE_ACP_LOAD_REPLAY` | emit N updates BEFORE answering `session/load` (the replay-ordering probe) |
//! | `FAKE_ACP_LOAD_REPLAY_TAIL` | emit N MORE replay updates AFTER the `session/load` reply |
//! | `FAKE_ACP_LOAD_TAIL_DELAY_MS` | how long after that reply the tail is emitted |
//! | `FAKE_ACP_MODE_REVERT_ON_LOAD` | `currentModeId` returned by `session/load` (the ambient-mode revert BOTH real adapters do) |
//! | `FAKE_ACP_LOAD_UNKNOWN_SESSION` | `claude` \| `codex` \| `opaque`: fail `session/load` in that adapter's unknown-session shape |
//! | `FAKE_ACP_MODE_FLIP` | emit a `current_mode_update` carrying this mode at the END of every prompt turn (the mid-conversation flip) |
//! | `FAKE_ACP_SESSION_PREFIX` | prefix for minted adapter session ids (default `fake-session`) |
//! | `FAKE_ACP_HANG_SESSIONS` | comma list of adapter session ids (or `*`) whose prompt NEVER answers |
//! | `FAKE_ACP_PERMISSION_SESSIONS` | comma list of adapter session ids (or `*`) that raise one `session/request_permission` per turn |
//! | `FAKE_ACP_PERMISSION_COUNT` | raise N CONCURRENT permissions per turn instead of one (default 1) |
//! | `FAKE_ACP_PERMISSION_NO_ALLOW` | offer only reject-flavoured options, so `Approve` has nothing to select |
//! | `FAKE_ACP_DIE_AFTER_CHUNKS` | emit the turn's updates, then exit WITHOUT replying to `session/prompt` |
//! | `FAKE_ACP_FAIL_TURN_SESSIONS` | comma list (or `*`) whose prompt answers `stopReason: refusal` |
//! | `FAKE_ACP_STOP_REASON` | `stopReason` answered by every non-refused prompt (default `end_turn`; e.g. `max_tokens`) |
//! | `FAKE_ACP_HANG_PROMPTS` | comma list (or `*`) of prompt TEXTS whose turn never answers |
//! | `FAKE_ACP_ECHO_PROMPT` | append the prompt text to the final agent message |
//! | `FAKE_ACP_GHOST_SESSION` | per turn, emit one `session/update` AND one `session/request_permission` for THIS adapter session id, which no client session owns |
//! | `FAKE_ACP_RPC_LOG` | append every `spawn`/`new`/`prompt`/`permission`/`cancel`/`close` this process observed to this path |
//! | `FAKE_ACP_DIE_ON_SESSION_NEW` | marker path: the FIRST process to reach `session/new` exits without replying |

use std::collections::HashMap;
use std::io::{BufRead, Stdout, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// stdout, shared by every prompt thread. One lock per whole line, so two
/// concurrent turns can never interleave half a JSON frame.
type SharedOut = Arc<Mutex<Stdout>>;

/// Replies to the requests THIS process issued (only `session/request_permission`
/// today), routed by JSON-RPC id back to the thread that is blocked on them.
type Pending = Arc<Mutex<HashMap<String, Sender<serde_json::Value>>>>;

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

    record("spawn");
    let out: SharedOut = Arc::new(Mutex::new(std::io::stdout()));
    let pending: Pending = Arc::default();
    let sessions = AtomicI64::new(0);
    let mut threads = Vec::new();

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message): Result<serde_json::Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        let Some(method) = message["method"].as_str() else {
            // No method: this is a REPLY to a request we issued. Route it to
            // the blocked thread; an unknown id is simply dropped.
            route_reply(&pending, &message);
            continue;
        };
        let Some(id) = message.get("id") else {
            // Notifications ($/cancel_request, session/cancel) need no reply,
            // but WHICH session was cancelled is exactly what I16 asserts, and
            // it is invisible from the store.
            if method == "session/cancel" {
                record(&format!(
                    "cancel:{}",
                    message["params"]["sessionId"].as_str().unwrap_or_default()
                ));
            }
            continue;
        };
        if method == "session/prompt" {
            // Its own thread: a hung session must not block its siblings on the
            // same process (the multiplex case the pool exists to serve).
            let out = Arc::clone(&out);
            let pending = Arc::clone(&pending);
            let id = id.clone();
            let params = message["params"].clone();
            threads.push(std::thread::spawn(move || {
                prompt(&out, &pending, &id, &params);
            }));
            continue;
        }
        handle(&out, &sessions, method, id, &message["params"]);
    }
    // stdin is closed: nobody will answer an outstanding
    // `session/request_permission` and nobody will read another chunk. Wake
    // every thread that is deliberately blocked forever rather than joining
    // threads that can never finish on their own. A fixture that outlives its
    // test holds the harness's stdout pipe open, and the run then hangs with no
    // output instead of reporting the failure that stranded it, which is
    // exactly the run where the evidence matters. The client is gone either
    // way, so cutting these turns short costs nothing.
    if let Ok(mut map) = pending.lock() {
        map.clear();
    }
    for thread in &threads {
        thread.thread().unpark();
    }
    for thread in threads {
        let _ = thread.join();
    }
}

fn route_reply(pending: &Pending, message: &serde_json::Value) {
    let key = message["id"].to_string();
    let waiter = pending.lock().ok().and_then(|mut map| map.remove(&key));
    if let Some(waiter) = waiter {
        let _ = waiter.send(message.clone());
    }
}

fn handle(
    out: &SharedOut,
    sessions: &AtomicI64,
    method: &str,
    id: &serde_json::Value,
    params: &serde_json::Value,
) {
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
            // I6's pre-write window: die with the request in hand and no reply
            // on the wire, so the client learns the transport closed BEFORE any
            // `session/prompt` was issued. The marker file makes it fire for the
            // first process only, so the pool's one legal requeue can succeed.
            if let Some(marker) = var("FAKE_ACP_DIE_ON_SESSION_NEW") {
                if !std::path::Path::new(&marker).exists() {
                    let _ = std::fs::write(&marker, "died");
                    record("die:session_new");
                    std::process::exit(1);
                }
            }
            // DISTINCT per session: a fixture that answered one id for every
            // `session/new` would make a cross-attribution bug invisible,
            // because both sessions would legitimately share a demux key.
            let ordinal = sessions.fetch_add(1, Ordering::SeqCst) + 1;
            let prefix =
                var("FAKE_ACP_SESSION_PREFIX").unwrap_or_else(|| "fake-session".to_string());
            let mut result = serde_json::json!({"sessionId": format!("{prefix}-{ordinal}")});
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
            record(&format!("new:{prefix}-{ordinal}"));
            respond(out, id, &result);
        }
        "session/load" => {
            record(&format!("load:{session_id}"));
            // The two unknown-session shapes the gate re-run measured, so the
            // client's taxonomy can be tested without either real adapter.
            match var("FAKE_ACP_LOAD_UNKNOWN_SESSION").as_deref() {
                // claude-agent-acp 0.65.0: TYPED, with the uri it could not find.
                Some("claude") => {
                    return respond_error_with_data(
                        out,
                        id,
                        -32002,
                        "Resource not found",
                        &serde_json::json!({"uri": format!("acp://session/{session_id}")}),
                    );
                }
                // codex-acp 1.1.10: opaque -32603, the needle only in data.details.
                Some("codex") => {
                    return respond_error_with_data(
                        out,
                        id,
                        -32603,
                        "Internal error",
                        &serde_json::json!({
                            "details": format!("no rollout found for thread id {session_id}"),
                        }),
                    );
                }
                // A -32603 that is NOT an unknown session: must NOT be rebuilt.
                Some(_) => {
                    return respond_error_with_data(
                        out,
                        id,
                        -32603,
                        "Internal error",
                        &serde_json::json!({"details": "the adapter fell over"}),
                    );
                }
                None => {}
            }
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
                // BOTH real adapters revert a pinned mode to ambient on load
                // (gate re-run 2026-08-06), so the fixture can too, INDEPENDENTLY
                // of what session/new reported. Without a separate knob a test
                // could not express "new pinned it, load lost it".
                let mode = var("FAKE_ACP_MODE_REVERT_ON_LOAD")
                    .or_else(|| var("FAKE_ACP_MODE_ON_NEW"))
                    .unwrap_or_else(|| "default".to_string());
                result["modes"] = serde_json::json!({
                    "currentModeId": mode,
                    "availableModes": [
                        {"id": "default", "name": "Default"},
                        {"id": "bypassPermissions", "name": "Bypass"},
                    ],
                });
            }
            respond(out, id, &result);
            emit_load_tail(out, &session_id);
        }
        "session/set_config_option" => {
            record(&format!(
                "config:{session_id}:{}={}",
                params["configId"].as_str().unwrap_or_default(),
                params["value"].as_str().unwrap_or_default()
            ));
            // `configOptions` is REQUIRED on the reply; `{}` is a parse error,
            // and the client surfaces parse errors rather than swallowing them.
            respond(out, id, &serde_json::json!({"configOptions": []}));
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
        "session/close" => {
            record(&format!("close:{session_id}"));
            respond(out, id, &serde_json::json!({}));
        }
        _ => respond_error(out, id, -32601, "method not found"),
    }
}

/// One turn, on its own thread.
fn prompt(out: &SharedOut, pending: &Pending, id: &serde_json::Value, params: &serde_json::Value) {
    let session_id = params["sessionId"].as_str().unwrap_or("fake-session").to_string();
    let text = params["prompt"][0]["text"].as_str().unwrap_or_default().to_string();
    // Recorded BEFORE any hang: "how many prompts did the adapter ever see" is
    // the only honest way to assert I6's no-resend half.
    record(&format!("prompt:{session_id}:{text}"));

    if selected("FAKE_ACP_HANG_SESSIONS", &session_id) || selected("FAKE_ACP_HANG_PROMPTS", &text) {
        // Never answers. The pool's turn deadline is the only thing that ends
        // this turn, which is precisely what I16 asserts.
        emit_turn(out, &session_id, params);
        std::thread::park();
        return;
    }

    // A chunk (and a blocked permission) for a session id NOBODY owns. The
    // demux must drop the chunk rather than hand it to a neighbour on the same
    // process, and must ANSWER the permission rather than leave this adapter
    // blocked on a request nothing will ever reply to.
    if let Some(ghost) = var("FAKE_ACP_GHOST_SESSION") {
        notify_update(
            out,
            &ghost,
            &serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "ghost-text "},
            }),
        );
        request_permission(out, pending, &ghost);
    }

    if selected("FAKE_ACP_PERMISSION_SESSIONS", &session_id) {
        // CONCURRENT by construction: every ask blocks its own thread, so all N
        // are outstanding at once. A client that can only answer the newest
        // deadlocks here on the older ones instead of quietly passing.
        let asks = count("FAKE_ACP_PERMISSION_COUNT").max(1);
        let threads: Vec<_> = (0..asks)
            .map(|_| {
                let out = Arc::clone(out);
                let pending = Arc::clone(pending);
                let session_id = session_id.clone();
                std::thread::spawn(move || request_permission(&out, &pending, &session_id))
            })
            .collect();
        for thread in threads {
            let _ = thread.join();
        }
    }
    emit_turn(out, &session_id, params);
    // Die with the turn's updates already on the wire and no reply behind them:
    // the client must commit what it read before it observes the exit.
    if flag("FAKE_ACP_DIE_AFTER_CHUNKS") {
        record(&format!("die_after_chunks:{session_id}"));
        std::process::exit(1);
    }
    flip_mode(out, &session_id);
    let stop = if selected("FAKE_ACP_FAIL_TURN_SESSIONS", &session_id) {
        "refusal".to_string()
    } else {
        var("FAKE_ACP_STOP_REASON").unwrap_or_else(|| "end_turn".to_string())
    };
    respond(out, id, &serde_json::json!({"stopReason": stop}));
}

/// Issue one `session/request_permission` and BLOCK this turn until the client
/// answers it, exactly like a real adapter waiting on a human.
fn request_permission(out: &SharedOut, pending: &Pending, session_id: &str) {
    static NEXT_ID: AtomicI64 = AtomicI64::new(9000);
    let request_id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let (tx, rx): (Sender<serde_json::Value>, Receiver<serde_json::Value>) = channel();
    if let Ok(mut map) = pending.lock() {
        map.insert(serde_json::json!(request_id).to_string(), tx);
    }
    // A real adapter is free to offer no allow-flavoured option at all, and a
    // client that treats "the first option" as approval would then approve by
    // selecting a REJECT.
    let mut options = vec![serde_json::json!(
        {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"}
    )];
    if !flag("FAKE_ACP_PERMISSION_NO_ALLOW") {
        options.insert(
            0,
            serde_json::json!({"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"}),
        );
    }
    write_line(
        out,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": session_id,
                "toolCall": {"toolCallId": "tool-1", "title": "rm -rf /tmp/fixture"},
                "options": options,
            },
        }),
    );
    let reply = rx.recv().unwrap_or(serde_json::Value::Null);
    let outcome = reply["result"]["outcome"]["outcome"]
        .as_str()
        .unwrap_or("cancelled")
        .to_string();
    let option = reply["result"]["outcome"]["optionId"].as_str().unwrap_or("").to_string();
    // The only honest evidence that the BLOCKED adapter was answered: an
    // unrouted permission's own `session/update` is dropped by the demux, so
    // the transcript can never show it.
    record(&format!("permission:{session_id}:{outcome}"));
    notify_update(
        out,
        session_id,
        &serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": format!("permission:{outcome}:{option} ")},
        }),
    );
}

fn emit_turn(out: &SharedOut, session_id: &str, params: &serde_json::Value) {
    let delay = std::time::Duration::from_millis(count("FAKE_ACP_CHUNK_DELAY_MS") as u64);
    if let Some(path) = var("FAKE_ACP_SCRIPT") {
        let script = std::fs::read_to_string(path).unwrap_or_default();
        for line in script.lines().filter(|line| !line.trim().is_empty()) {
            if let Ok(update) = serde_json::from_str::<serde_json::Value>(line) {
                notify_update(out, session_id, &update);
                std::thread::sleep(delay);
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
        std::thread::sleep(delay);
    }
    if flag("FAKE_ACP_ECHO_PROMPT") {
        let echo = params["prompt"][0]["text"].as_str().unwrap_or_default().to_string();
        notify_update(
            out,
            session_id,
            &serde_json::json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": format!("echo:{echo}")},
            }),
        );
    }
}

/// The part of the `session/load` replay that arrives AFTER its own reply.
///
/// A client cannot assume the whole replay precedes the reply: the adapter is
/// free to still be flushing, and the daemon's own demux hands notifications to
/// a different task than the one awaiting the reply, so a tail can surface at
/// any point up to the next prompt. Emitted on its own thread so the fixture's
/// read loop keeps serving while the tail is pending.
fn emit_load_tail(out: &SharedOut, session_id: &str) {
    let tail = count("FAKE_ACP_LOAD_REPLAY_TAIL");
    if tail == 0 {
        return;
    }
    let out = Arc::clone(out);
    let session_id = session_id.to_string();
    let delay = std::time::Duration::from_millis(count("FAKE_ACP_LOAD_TAIL_DELAY_MS") as u64);
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        for index in 0..tail {
            notify_update(
                &out,
                &session_id,
                &serde_json::json!({
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": format!("tail-replay-{index} ")},
                }),
            );
        }
    });
}

/// A live session changing permission regime mid conversation, long after the
/// spawn-time assertion passed.
fn flip_mode(out: &SharedOut, session_id: &str) {
    if let Some(mode) = var("FAKE_ACP_MODE_FLIP") {
        notify_update(
            out,
            session_id,
            &serde_json::json!({"sessionUpdate": "current_mode_update", "currentModeId": mode}),
        );
    }
}

fn notify_update(out: &SharedOut, session_id: &str, update: &serde_json::Value) {
    write_line(
        out,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": session_id, "update": update},
        }),
    );
}

fn respond(out: &SharedOut, id: &serde_json::Value, result: &serde_json::Value) {
    write_line(
        out,
        &serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}),
    );
}

fn respond_error(out: &SharedOut, id: &serde_json::Value, code: i32, message: &str) {
    write_line(
        out,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message},
        }),
    );
}

/// An error carrying `data`, which is where BOTH real adapters put the only
/// detail that says "unknown session" rather than "something broke".
fn respond_error_with_data(
    out: &SharedOut,
    id: &serde_json::Value,
    code: i32,
    message: &str,
    data: &serde_json::Value,
) {
    write_line(
        out,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message, "data": data},
        }),
    );
}

fn write_line(out: &SharedOut, message: &serde_json::Value) {
    let Ok(mut out) = out.lock() else { return };
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

/// Append one observed RPC to `FAKE_ACP_RPC_LOG`, when set.
///
/// The store answers "what did the pool decide"; only this answers "what did
/// the adapter actually receive", which is the whole of I6's no-resend claim
/// and of the deadline's "the cancel named A's session, not B's".
///
/// Reopened per line: prompts run on their own threads and a process may die
/// mid-turn, so an unflushed shared handle would lose exactly the evidence.
fn record(line: &str) {
    let Some(path) = var("FAKE_ACP_RPC_LOG") else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
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

/// Whether `session_id` is named by a comma list variable (`*` means all).
fn selected(name: &str, session_id: &str) -> bool {
    var(name).is_some_and(|list| {
        list.split(',').map(str::trim).any(|entry| entry == "*" || entry == session_id)
    })
}
