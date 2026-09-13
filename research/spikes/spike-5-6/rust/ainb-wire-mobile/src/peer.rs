//! Scratch peer: the host end of the transport, standing in for R1's daemon
//! WebSocket listener. It answers `auth/hello` and `fleet/subscribe` with the
//! real proto result types, echoes heartbeats, and writes one JSON line per
//! event with its own wall clock so spike 6 can find the first missed beat.

use std::io::Write;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use ainb_hangar_proto::auth::{HelloParams, HelloResult, UNAUTHORIZED};
use ainb_hangar_proto::fleet::{FleetReplayResetReason, FleetReplayState, FleetSnapshot, FleetSubscribeParams, FleetSubscribeResult};
use ainb_hangar_proto::protocol::{catalogue_strings, ProtocolRange, PROTOCOL_VERSION};
use ainb_hangar_proto::{jsonrpc_version, methods, RpcError, RpcRequest, RpcResponse};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

use crate::wire::{self, Frame, Opcode, Reassembler};

pub struct PeerConfig {
    pub host_private_key: Vec<u8>,
    pub transport: String,
    pub host_id: String,
    pub token: String,
    pub log: Option<Arc<Mutex<std::fs::File>>>,
}

pub fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()
}

fn log(cfg: &PeerConfig, conn: u64, event: &str, extra: serde_json::Value) {
    let line = serde_json::json!({ "t_ms": now_ms() as u64, "conn": conn, "event": event, "extra": extra });
    if let Some(f) = &cfg.log {
        let mut f = f.lock().unwrap();
        let _ = writeln!(f, "{line}");
        let _ = f.flush();
    } else {
        eprintln!("{line}");
    }
}

pub async fn serve(listener: TcpListener, cfg: Arc<PeerConfig>) {
    let ids = AtomicU64::new(1);
    loop {
        let Ok((tcp, addr)) = listener.accept().await else { continue };
        let _ = tcp.set_nodelay(true);
        let conn = ids.fetch_add(1, Ordering::SeqCst);
        let cfg = Arc::clone(&cfg);
        tokio::spawn(async move {
            let reason = match handle(tcp, addr, conn, &cfg).await {
                Ok(()) => "closed".to_string(),
                Err(e) => e,
            };
            log(&cfg, conn, "disconnect", serde_json::json!({ "reason": reason }));
        });
    }
}

async fn handle(tcp: tokio::net::TcpStream, addr: SocketAddr, conn: u64, cfg: &PeerConfig) -> Result<(), String> {
    let ws = tokio_tungstenite::accept_async(tcp).await.map_err(|e| format!("ws accept: {e}"))?;
    log(cfg, conn, "ws_accept", serde_json::json!({ "addr": addr.to_string() }));
    let (mut sink, mut stream) = ws.split();

    let mut hs = wire::responder(&cfg.host_private_key, &cfg.transport, &cfg.host_id).map_err(|e| e.to_string())?;
    let msg1 = match stream.next().await {
        Some(Ok(Message::Binary(b))) => b,
        other => return Err(format!("expected noise msg 1, got {other:?}")),
    };
    let mut buf = vec![0u8; wire::NOISE_MAX];
    hs.read_message(&msg1, &mut buf).map_err(|e| format!("noise msg 1 rejected: {e}"))?;
    let n = hs.write_message(&[], &mut buf).map_err(|e| e.to_string())?;
    sink.send(Message::Binary(buf[..n].to_vec())).await.map_err(|e| e.to_string())?;
    let remote_static = hs.get_remote_static().map(|k| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, k)).unwrap_or_default();
    let mut transport = hs.into_transport_mode().map_err(|e| e.to_string())?;
    log(cfg, conn, "handshake", serde_json::json!({ "device_static": remote_static }));

    let mut authed = false;
    let mut reasm = Reassembler::default();
    while let Some(msg) = stream.next().await {
        let cipher = match msg.map_err(|e| format!("ws: {e}"))? {
            Message::Binary(b) => b,
            Message::Close(_) => return Ok(()),
            _ => continue,
        };
        let mut plain = vec![0u8; cipher.len()];
        let n = transport.read_message(&cipher, &mut plain).map_err(|e| format!("decrypt: {e}"))?;
        let frame = Frame::decode(&plain[..n])?;
        let replies = match frame.opcode {
            Opcode::Ping => {
                log(cfg, conn, "ping", serde_json::json!({ "seq": frame.seq }));
                vec![Frame { opcode: Opcode::Pong, ..frame }]
            }
            Opcode::StreamEnd => return Ok(()),
            Opcode::Pong => vec![],
            Opcode::Rpc => match reasm.push(&frame) {
                None => vec![],
                Some(bytes) => {
                    let req: RpcRequest = serde_json::from_slice(wire::lsp_decode(&bytes)?).map_err(|e| e.to_string())?;
                    let resp = dispatch(cfg, conn, &mut authed, req);
                    wire::rpc_frames(&wire::lsp_encode(&serde_json::to_vec(&resp).unwrap()))
                }
            },
        };
        for f in replies {
            let plain = f.encode();
            let mut out = vec![0u8; plain.len() + 16];
            let n = transport.write_message(&plain, &mut out).map_err(|e| e.to_string())?;
            out.truncate(n);
            sink.send(Message::Binary(out)).await.map_err(|e| e.to_string())?;
        }
    }
    Err("eof without close".into())
}

fn dispatch(cfg: &PeerConfig, conn: u64, authed: &mut bool, req: RpcRequest) -> RpcResponse {
    let ok = |v: serde_json::Value| RpcResponse { jsonrpc: jsonrpc_version(), id: req.id.clone(), result: Some(v), error: None };
    let err = |code: i32, message: &str| RpcResponse {
        jsonrpc: jsonrpc_version(),
        id: req.id.clone(),
        result: None,
        error: Some(RpcError { code, message: message.to_string(), data: None }),
    };
    match req.method.as_str() {
        methods::AUTH_HELLO => {
            let Ok(p) = serde_json::from_value::<HelloParams>(req.params.clone()) else { return err(-32602, "bad hello params") };
            if p.token != cfg.token {
                log(cfg, conn, "hello_rejected", serde_json::json!({}));
                return err(UNAUTHORIZED, "invalid daemon token");
            }
            *authed = true;
            log(cfg, conn, "hello", serde_json::json!({ "device": p.device.map(|d| d.display_name), "protocol": p.protocol }));
            ok(serde_json::to_value(HelloResult {
                protocol: ProtocolRange::supported(),
                selected: Some(PROTOCOL_VERSION),
                capabilities: catalogue_strings(),
                daemon_version: Some("spike-5-6-peerd".into()),
            })
            .unwrap())
        }
        _ if !*authed => err(UNAUTHORIZED, "hello first"),
        methods::FLEET_SUBSCRIBE => {
            let Ok(p) = serde_json::from_value::<FleetSubscribeParams>(req.params.clone()) else { return err(-32602, "bad subscribe params") };
            log(cfg, conn, "fleet_subscribe", serde_json::json!({ "after_revision": p.after_revision }));
            let result = FleetSubscribeResult {
                snapshot: FleetSnapshot { head_revision: 0, sessions: vec![] },
                replay: vec![],
                replay_state: FleetReplayState::SnapshotReset { reason: FleetReplayResetReason::Bootstrap },
            };
            ok(serde_json::to_value(result).unwrap())
        }
        "spike/mark" => {
            log(cfg, conn, "mark", req.params.clone());
            ok(serde_json::json!({ "t_ms": now_ms() as u64 }))
        }
        _ => err(-32601, "method not found"),
    }
}
