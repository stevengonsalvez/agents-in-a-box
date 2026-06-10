// ABOUTME: Protocol-aware JSON-RPC multiplexer for one shared MCP child.
//
// Pure state machine (no IO) so it unit-tests without sockets or processes.
// The proxy feeds it lines from clients/the child; it returns routing
// outcomes the proxy executes.
//
// Responsibilities beyond the agent-deck byte-pipe design:
//   * per-client request-id rewrite to a mux-global counter (collision-free)
//   * InitializeResult cache — the child sees exactly ONE initialize; every
//     later client's initialize is answered from cache
//   * `notifications/initialized` forwarded to the child only once
//   * progress notifications routed to the owning client via progressToken
//   * `notifications/cancelled` requestId remapped to the proxy id
//
// Unknown traffic (server-initiated requests, unmapped notifications) is
// broadcast to all clients — documented v1 limitation, matches agent-deck.

use serde_json::Value;
use std::collections::HashMap;

pub type ClientId = u64;

/// What the proxy must do with a processed line.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    ToChild(String),
    ToClient(ClientId, String),
    Broadcast(String),
}

#[derive(Debug)]
struct Pending {
    client: ClientId,
    original_id: Value,
    progress_token: Option<String>,
    is_initialize: bool,
}

#[derive(Debug)]
enum InitState {
    NotStarted,
    /// Initialize forwarded to the child; later initializes queue here as
    /// (client, original id) and are answered when the response lands.
    Pending { waiters: Vec<(ClientId, Value)> },
    Done { result: Value },
}

pub struct Mux {
    next_id: i64,
    id_map: HashMap<i64, Pending>,
    /// canonical progressToken JSON -> owning client
    progress_tokens: HashMap<String, ClientId>,
    init: InitState,
    initialized_notified: bool,
}

impl Default for Mux {
    fn default() -> Self {
        Self::new()
    }
}

impl Mux {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            id_map: HashMap::new(),
            progress_tokens: HashMap::new(),
            init: InitState::NotStarted,
            initialized_notified: false,
        }
    }

    /// Child died / restarting: drop in-flight state but KEEP the init cache
    /// invalid (a fresh child needs a fresh initialize).
    pub fn reset_for_child_restart(&mut self) {
        self.id_map.clear();
        self.progress_tokens.clear();
        self.init = InitState::NotStarted;
        self.initialized_notified = false;
    }

    pub fn on_client_disconnect(&mut self, client: ClientId) {
        self.id_map.retain(|_, p| p.client != client);
        self.progress_tokens.retain(|_, c| *c != client);
        if let InitState::Pending { waiters } = &mut self.init {
            waiters.retain(|(c, _)| *c != client);
        }
    }

    pub fn on_client_line(&mut self, client: ClientId, line: &str) -> Vec<Outcome> {
        let Ok(mut msg) = serde_json::from_str::<Value>(line) else {
            // Unparseable: pass through untouched (agent-deck behavior).
            return vec![Outcome::ToChild(line.to_string())];
        };

        let method = msg.get("method").and_then(Value::as_str).map(str::to_string);
        let has_id = msg.get("id").is_some_and(|v| !v.is_null());

        match (method.as_deref(), has_id) {
            (Some("initialize"), true) => self.client_initialize(client, msg),
            (Some("notifications/initialized"), false) => {
                if self.initialized_notified {
                    vec![]
                } else {
                    self.initialized_notified = true;
                    vec![Outcome::ToChild(line.to_string())]
                }
            }
            (Some("notifications/cancelled"), false) => {
                self.remap_cancelled(client, &mut msg);
                vec![Outcome::ToChild(msg.to_string())]
            }
            (Some(_), true) => {
                let original_id = msg["id"].clone();
                let progress_token = extract_progress_token(&msg);
                let proxy_id = self.assign_id(Pending {
                    client,
                    original_id,
                    progress_token: progress_token.clone(),
                    is_initialize: false,
                });
                if let Some(token) = progress_token {
                    self.progress_tokens.insert(token, client);
                }
                msg["id"] = Value::from(proxy_id);
                vec![Outcome::ToChild(msg.to_string())]
            }
            // Other notifications, and client responses to server-initiated
            // requests (which were broadcast unmapped): pass through.
            _ => vec![Outcome::ToChild(line.to_string())],
        }
    }

    pub fn on_child_line(&mut self, line: &str) -> Vec<Outcome> {
        let Ok(mut msg) = serde_json::from_str::<Value>(line) else {
            return vec![Outcome::Broadcast(line.to_string())];
        };

        // Notification from the child?
        if msg.get("id").is_none_or(Value::is_null) {
            if msg.get("method").and_then(Value::as_str) == Some("notifications/progress") {
                if let Some(token) = msg
                    .get("params")
                    .and_then(|p| p.get("progressToken"))
                    .map(canonical_token)
                {
                    if let Some(owner) = self.progress_tokens.get(&token) {
                        return vec![Outcome::ToClient(*owner, line.to_string())];
                    }
                }
            }
            return vec![Outcome::Broadcast(line.to_string())];
        }

        // Response (or server-initiated request carrying an id we never assigned).
        let Some(proxy_id) = msg.get("id").and_then(Value::as_i64) else {
            return vec![Outcome::Broadcast(line.to_string())];
        };
        let Some(pending) = self.id_map.remove(&proxy_id) else {
            return vec![Outcome::Broadcast(line.to_string())];
        };

        if let Some(token) = &pending.progress_token {
            self.progress_tokens.remove(token);
        }

        if pending.is_initialize {
            return self.child_initialize_response(pending, msg);
        }

        msg["id"] = pending.original_id.clone();
        vec![Outcome::ToClient(pending.client, msg.to_string())]
    }

    fn client_initialize(&mut self, client: ClientId, mut msg: Value) -> Vec<Outcome> {
        let original_id = msg["id"].clone();
        match &mut self.init {
            InitState::Done { result } => {
                let mut response = result.clone();
                response["id"] = original_id;
                vec![Outcome::ToClient(client, response.to_string())]
            }
            InitState::Pending { waiters } => {
                waiters.push((client, original_id));
                vec![]
            }
            InitState::NotStarted => {
                let proxy_id = self.assign_id(Pending {
                    client,
                    original_id,
                    progress_token: None,
                    is_initialize: true,
                });
                self.init = InitState::Pending { waiters: Vec::new() };
                msg["id"] = Value::from(proxy_id);
                vec![Outcome::ToChild(msg.to_string())]
            }
        }
    }

    fn child_initialize_response(&mut self, pending: Pending, mut msg: Value) -> Vec<Outcome> {
        let waiters = match std::mem::replace(&mut self.init, InitState::NotStarted) {
            InitState::Pending { waiters } => waiters,
            other => {
                self.init = other;
                Vec::new()
            }
        };

        let is_error = msg.get("error").is_some();
        if !is_error {
            self.init = InitState::Done { result: msg.clone() };
        }
        // On error InitState stays NotStarted so the next initialize retries.

        let mut out = Vec::with_capacity(1 + waiters.len());
        msg["id"] = pending.original_id.clone();
        out.push(Outcome::ToClient(pending.client, msg.clone().to_string()));
        for (client, original_id) in waiters {
            let mut reply = msg.clone();
            reply["id"] = original_id;
            out.push(Outcome::ToClient(client, reply.to_string()));
        }
        out
    }

    /// `notifications/cancelled` carries `params.requestId` = the CLIENT's id.
    /// The child only knows the proxy id — remap it (scoped to this client to
    /// avoid cross-client collisions on identical original ids).
    fn remap_cancelled(&self, client: ClientId, msg: &mut Value) {
        let Some(request_id) = msg.get("params").and_then(|p| p.get("requestId")).cloned() else {
            return;
        };
        let proxy_id = self
            .id_map
            .iter()
            .find(|(_, p)| p.client == client && p.original_id == request_id)
            .map(|(k, _)| *k);
        if let Some(proxy_id) = proxy_id {
            msg["params"]["requestId"] = Value::from(proxy_id);
        }
    }

    fn assign_id(&mut self, pending: Pending) -> i64 {
        self.next_id += 1;
        self.id_map.insert(self.next_id, pending);
        self.next_id
    }

    /// Number of in-flight requests (for status/debug).
    pub fn in_flight(&self) -> usize {
        self.id_map.len()
    }
}

fn extract_progress_token(msg: &Value) -> Option<String> {
    msg.get("params")
        .and_then(|p| p.get("_meta"))
        .and_then(|m| m.get("progressToken"))
        .map(canonical_token)
}

/// progressToken may be a string or number; canonicalize as its JSON text.
fn canonical_token(v: &Value) -> String {
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(v: Value) -> String {
        v.to_string()
    }

    fn parse(s: &str) -> Value {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn rewrites_colliding_ids_and_routes_responses_back() {
        let mut mux = Mux::new();

        // Both clients use id:1 (Claude Code always starts at 1).
        let out_a = mux.on_client_line(1, &line(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})));
        let out_b = mux.on_client_line(2, &line(json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})));

        let Outcome::ToChild(fwd_a) = &out_a[0] else { panic!("{out_a:?}") };
        let Outcome::ToChild(fwd_b) = &out_b[0] else { panic!("{out_b:?}") };
        let id_a = parse(fwd_a)["id"].as_i64().unwrap();
        let id_b = parse(fwd_b)["id"].as_i64().unwrap();
        assert_ne!(id_a, id_b, "proxy ids must be unique");

        // Child answers B first.
        let out = mux.on_child_line(&line(json!({"jsonrpc":"2.0","id":id_b,"result":{"tools":[]}})));
        assert_eq!(out.len(), 1);
        let Outcome::ToClient(client, resp) = &out[0] else { panic!("{out:?}") };
        assert_eq!(*client, 2);
        assert_eq!(parse(resp)["id"], json!(1), "original id restored");

        let out = mux.on_child_line(&line(json!({"jsonrpc":"2.0","id":id_a,"result":{"tools":[]}})));
        let Outcome::ToClient(client, _) = &out[0] else { panic!("{out:?}") };
        assert_eq!(*client, 1);
        assert_eq!(mux.in_flight(), 0);
    }

    #[test]
    fn initialize_hits_child_once_then_served_from_cache() {
        let mut mux = Mux::new();

        let init = |id: i64| line(json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}));

        // First client: forwarded.
        let out = mux.on_client_line(1, &init(1));
        let Outcome::ToChild(fwd) = &out[0] else { panic!("{out:?}") };
        let proxy_id = parse(fwd)["id"].as_i64().unwrap();

        // Second client initializes while the first is still pending: queued.
        let out = mux.on_client_line(2, &init(7));
        assert!(out.is_empty(), "pending waiter must not re-forward: {out:?}");

        // Child responds once → both clients answered with THEIR ids.
        let out = mux.on_child_line(&line(json!({
            "jsonrpc":"2.0","id":proxy_id,
            "result":{"capabilities":{},"serverInfo":{"name":"x"}}
        })));
        assert_eq!(out.len(), 2);
        let Outcome::ToClient(c1, r1) = &out[0] else { panic!() };
        let Outcome::ToClient(c2, r2) = &out[1] else { panic!() };
        assert_eq!((*c1, parse(r1)["id"].clone()), (1, json!(1)));
        assert_eq!((*c2, parse(r2)["id"].clone()), (2, json!(7)));

        // Third client initializes later: served from cache, child untouched.
        let out = mux.on_client_line(3, &init(42));
        assert_eq!(out.len(), 1);
        let Outcome::ToClient(c3, r3) = &out[0] else { panic!("{out:?}") };
        assert_eq!(*c3, 3);
        let resp = parse(r3);
        assert_eq!(resp["id"], json!(42));
        assert_eq!(resp["result"]["serverInfo"]["name"], json!("x"));
    }

    #[test]
    fn initialized_notification_forwarded_only_once() {
        let mut mux = Mux::new();
        let note = line(json!({"jsonrpc":"2.0","method":"notifications/initialized"}));
        assert_eq!(mux.on_client_line(1, &note).len(), 1);
        assert!(mux.on_client_line(2, &note).is_empty());
        assert!(mux.on_client_line(3, &note).is_empty());
    }

    #[test]
    fn initialize_error_is_not_cached() {
        let mut mux = Mux::new();
        let out = mux.on_client_line(1, &line(json!({"jsonrpc":"2.0","id":1,"method":"initialize"})));
        let Outcome::ToChild(fwd) = &out[0] else { panic!() };
        let proxy_id = parse(fwd)["id"].as_i64().unwrap();

        let out = mux.on_child_line(&line(json!({"jsonrpc":"2.0","id":proxy_id,"error":{"code":-1,"message":"boom"}})));
        assert_eq!(out.len(), 1, "error routed to requester");

        // Next initialize must be forwarded again, not served from cache.
        let out = mux.on_client_line(2, &line(json!({"jsonrpc":"2.0","id":1,"method":"initialize"})));
        assert!(matches!(out[0], Outcome::ToChild(_)), "{out:?}");
    }

    #[test]
    fn progress_notifications_route_to_token_owner_only() {
        let mut mux = Mux::new();

        let req = json!({
            "jsonrpc":"2.0","id":5,"method":"tools/call",
            "params":{"name":"t","_meta":{"progressToken":"tok-1"}}
        });
        let out = mux.on_client_line(1, &line(req));
        let Outcome::ToChild(fwd) = &out[0] else { panic!() };
        let proxy_id = parse(fwd)["id"].as_i64().unwrap();

        // Another client with no token in flight.
        mux.on_client_line(2, &line(json!({"jsonrpc":"2.0","id":5,"method":"tools/list"})));

        let progress = line(json!({
            "jsonrpc":"2.0","method":"notifications/progress",
            "params":{"progressToken":"tok-1","progress":50}
        }));
        let out = mux.on_child_line(&progress);
        assert_eq!(out, vec![Outcome::ToClient(1, progress.clone())]);

        // After the owning request completes, the token unregisters and the
        // same notification broadcasts (unknown token fallback).
        mux.on_child_line(&line(json!({"jsonrpc":"2.0","id":proxy_id,"result":{}})));
        let out = mux.on_child_line(&progress);
        assert!(matches!(out[0], Outcome::Broadcast(_)), "{out:?}");
    }

    #[test]
    fn unknown_id_and_unparseable_child_lines_broadcast() {
        let mut mux = Mux::new();
        let out = mux.on_child_line("not json");
        assert!(matches!(out[0], Outcome::Broadcast(_)));
        let out = mux.on_child_line(&line(json!({"jsonrpc":"2.0","id":999,"method":"roots/list"})));
        assert!(matches!(out[0], Outcome::Broadcast(_)));
    }

    #[test]
    fn cancelled_notification_remaps_request_id() {
        let mut mux = Mux::new();
        let out = mux.on_client_line(1, &line(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{}})));
        let Outcome::ToChild(fwd) = &out[0] else { panic!() };
        let proxy_id = parse(fwd)["id"].as_i64().unwrap();

        let out = mux.on_client_line(
            1,
            &line(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":3}})),
        );
        let Outcome::ToChild(fwd) = &out[0] else { panic!("{out:?}") };
        assert_eq!(parse(fwd)["params"]["requestId"].as_i64().unwrap(), proxy_id);
    }

    #[test]
    fn disconnect_cleans_client_state() {
        let mut mux = Mux::new();
        mux.on_client_line(
            1,
            &line(json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"_meta":{"progressToken":"t"}}})),
        );
        assert_eq!(mux.in_flight(), 1);
        mux.on_client_disconnect(1);
        assert_eq!(mux.in_flight(), 0);
        // Token gone → progress broadcasts instead of routing to dead client.
        let out = mux.on_child_line(&line(json!({
            "jsonrpc":"2.0","method":"notifications/progress","params":{"progressToken":"t"}
        })));
        assert!(matches!(out[0], Outcome::Broadcast(_)));
    }
}
