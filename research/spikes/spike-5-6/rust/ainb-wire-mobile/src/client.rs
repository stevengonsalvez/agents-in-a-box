//! The phone-side session: WebSocket, Noise IK initiator, AEAD-framed JSON-RPC
//! and an in-crate heartbeat. Everything here is exported through uniffi; the
//! TypeScript side never sees a key schedule, a nonce or a frame header.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ainb_hangar_proto::auth::{DeviceInfo, HelloParams, HelloResult};
use ainb_hangar_proto::fleet::{FleetReplayState, FleetSubscribeParams, FleetSubscribeResult};
use ainb_hangar_proto::protocol::{catalogue_strings, ProtocolRange};
use ainb_hangar_proto::{jsonrpc_version, methods, RpcId, RpcRequest, RpcResponse};
use futures_util::{SinkExt, StreamExt};
use tokio::runtime::Runtime;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;

use crate::wire::{self, Frame, Opcode, Reassembler};

const RPC_TIMEOUT: Duration = Duration::from_secs(10);

fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("ainb-wire")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum WireError {
    #[error("connect failed: {message}")]
    Connect { message: String },
    #[error("noise handshake failed: {message}")]
    Handshake { message: String },
    #[error("protocol error: {message}")]
    Protocol { message: String },
    #[error("rpc error {code}: {message}")]
    Rpc { code: i32, message: String },
    #[error("session closed: {reason}")]
    Closed { reason: String },
}

fn proto(e: impl std::fmt::Display) -> WireError {
    WireError::Protocol { message: e.to_string() }
}

#[derive(uniffi::Record)]
pub struct DeviceKeypair {
    pub private_key: Vec<u8>,
    pub public_key: Vec<u8>,
}

#[derive(uniffi::Record, Clone)]
pub struct ConnectConfig {
    pub url: String,
    pub host_public_key: Vec<u8>,
    pub device_private_key: Vec<u8>,
    pub transport: String,
    pub host_id: String,
    /// 0 disables the heartbeat.
    pub heartbeat_interval_ms: u64,
}

#[derive(uniffi::Record)]
pub struct SessionStats {
    pub ws_connect_ms: f64,
    pub noise_handshake_ms: f64,
    pub heartbeats_sent: u64,
    pub pongs_received: u64,
    pub last_pong_rtt_ms: f64,
    pub closed: bool,
    pub close_reason: String,
}

#[derive(uniffi::Record)]
pub struct HelloSummary {
    pub selected_protocol: u32,
    pub capabilities: u32,
    pub daemon_version: String,
    pub rtt_ms: f64,
}

#[derive(uniffi::Record)]
pub struct SubscribeSummary {
    pub head_revision: i64,
    pub sessions: u32,
    pub replay_events: u32,
    pub replay_state: String,
    pub rtt_ms: f64,
}

#[uniffi::export]
pub fn generate_device_keypair() -> DeviceKeypair {
    let (private_key, public_key) = wire::generate_keypair();
    DeviceKeypair { private_key, public_key }
}

#[uniffi::export]
pub fn wire_version() -> String {
    format!("ainb-wire-mobile {} ({})", env!("CARGO_PKG_VERSION"), wire::NOISE_PATTERN)
}

#[derive(uniffi::Object)]
pub struct WireSession {
    transport: Mutex<snow::TransportState>,
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Mutex<HashMap<i64, oneshot::Sender<RpcResponse>>>,
    next_id: AtomicI64,
    started: Instant,
    ws_connect_ms: f64,
    noise_handshake_ms: f64,
    heartbeats_sent: AtomicU64,
    pongs_received: AtomicU64,
    last_pong_rtt_us: AtomicU64,
    closed: AtomicBool,
    close_reason: Mutex<String>,
}

#[uniffi::export]
pub async fn connect(config: ConnectConfig) -> Result<Arc<WireSession>, WireError> {
    rt().spawn(connect_inner(config)).await.map_err(proto)?
}

pub async fn connect_inner(config: ConnectConfig) -> Result<Arc<WireSession>, WireError> {
    let t0 = Instant::now();
    let (ws, _) = tokio_tungstenite::connect_async(config.url.as_str())
        .await
        .map_err(|e| WireError::Connect { message: e.to_string() })?;
    let ws_connect_ms = ms(t0.elapsed());
    let (mut sink, mut stream) = ws.split();

    let t1 = Instant::now();
    let hs_err = |e: snow::Error| WireError::Handshake { message: e.to_string() };
    let mut hs = wire::initiator(&config.device_private_key, &config.host_public_key, &config.transport, &config.host_id).map_err(hs_err)?;
    let mut buf = vec![0u8; wire::NOISE_MAX];
    let n = hs.write_message(&[], &mut buf).map_err(hs_err)?;
    sink.send(Message::Binary(buf[..n].to_vec())).await.map_err(|e| WireError::Handshake { message: e.to_string() })?;
    let reply = loop {
        match stream.next().await {
            Some(Ok(Message::Binary(b))) => break b,
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(other)) => return Err(WireError::Handshake { message: format!("unexpected {other:?}") }),
            Some(Err(e)) => return Err(WireError::Handshake { message: e.to_string() }),
            None => return Err(WireError::Handshake { message: "peer closed during handshake (prologue or key mismatch)".into() }),
        }
    };
    hs.read_message(&reply, &mut buf).map_err(hs_err)?;
    let transport = hs.into_transport_mode().map_err(hs_err)?;
    let noise_handshake_ms = ms(t1.elapsed());

    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let session = Arc::new(WireSession {
        transport: Mutex::new(transport),
        out: out_tx,
        pending: Mutex::new(HashMap::new()),
        next_id: AtomicI64::new(1),
        started: Instant::now(),
        ws_connect_ms,
        noise_handshake_ms,
        heartbeats_sent: AtomicU64::new(0),
        pongs_received: AtomicU64::new(0),
        last_pong_rtt_us: AtomicU64::new(0),
        closed: AtomicBool::new(false),
        close_reason: Mutex::new(String::new()),
    });

    let writer = Arc::clone(&session);
    tokio::spawn(async move {
        while let Some(bytes) = out_rx.recv().await {
            // An empty buffer is the close signal: Noise ciphertext is never empty.
            if bytes.is_empty() {
                break;
            }
            if let Err(e) = sink.send(Message::Binary(bytes)).await {
                writer.mark_closed(format!("write: {e}"));
                break;
            }
        }
        let _ = sink.close().await;
    });

    let reader = Arc::clone(&session);
    tokio::spawn(async move {
        let mut reasm = Reassembler::default();
        let reason = loop {
            match stream.next().await {
                Some(Ok(Message::Binary(b))) => {
                    if let Err(e) = reader.on_ciphertext(&b, &mut reasm) {
                        break format!("read: {e}");
                    }
                }
                Some(Ok(Message::Close(c))) => break format!("close frame {c:?}"),
                Some(Ok(_)) => {}
                Some(Err(e)) => break format!("ws: {e}"),
                None => break "eof".into(),
            }
        };
        reader.mark_closed(reason);
    });

    if config.heartbeat_interval_ms > 0 {
        let hb = Arc::downgrade(&session);
        let every = Duration::from_millis(config.heartbeat_interval_ms);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            loop {
                tick.tick().await;
                let Some(s) = hb.upgrade() else { break };
                if s.closed.load(Ordering::SeqCst) {
                    break;
                }
                let elapsed_us = s.started.elapsed().as_micros() as u64;
                let seq = s.heartbeats_sent.fetch_add(1, Ordering::SeqCst);
                let frame = Frame { opcode: Opcode::Ping, flags: wire::FLAG_FIN, stream_id: 0, seq, payload: elapsed_us.to_le_bytes().to_vec() };
                if s.send_frame(&frame).is_err() {
                    break;
                }
            }
        });
    }

    Ok(session)
}

impl WireSession {
    fn mark_closed(&self, reason: String) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            *self.close_reason.lock().unwrap() = reason;
        }
        self.pending.lock().unwrap().clear();
    }

    fn send_frame(&self, frame: &Frame) -> Result<(), WireError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(WireError::Closed { reason: self.close_reason.lock().unwrap().clone() });
        }
        let plain = frame.encode();
        let mut cipher = vec![0u8; plain.len() + 16];
        // Encrypt and enqueue under one lock so nonce order equals wire order.
        let mut t = self.transport.lock().unwrap();
        let n = t.write_message(&plain, &mut cipher).map_err(proto)?;
        cipher.truncate(n);
        self.out.send(cipher).map_err(|_| WireError::Closed { reason: "writer gone".into() })
    }

    fn on_ciphertext(&self, cipher: &[u8], reasm: &mut Reassembler) -> Result<(), String> {
        let mut plain = vec![0u8; cipher.len()];
        let n = self.transport.lock().unwrap().read_message(cipher, &mut plain).map_err(|e| e.to_string())?;
        let frame = Frame::decode(&plain[..n])?;
        match frame.opcode {
            Opcode::Pong => {
                let sent = u64::from_le_bytes(frame.payload.get(..8).ok_or("short pong")?.try_into().unwrap());
                let now = self.started.elapsed().as_micros() as u64;
                self.last_pong_rtt_us.store(now.saturating_sub(sent), Ordering::SeqCst);
                self.pongs_received.fetch_add(1, Ordering::SeqCst);
            }
            Opcode::Rpc => {
                if let Some(msg) = reasm.push(&frame) {
                    let resp: RpcResponse = serde_json::from_slice(wire::lsp_decode(&msg)?).map_err(|e| e.to_string())?;
                    if let RpcId::Number(id) = resp.id {
                        if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                            let _ = tx.send(resp);
                        }
                    }
                }
            }
            Opcode::Ping | Opcode::StreamEnd => {}
        }
        Ok(())
    }

    async fn request(&self, method: &str, params: serde_json::Value) -> Result<(serde_json::Value, f64), WireError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let req = RpcRequest { jsonrpc: jsonrpc_version(), id: RpcId::Number(id), method: method.to_string(), params };
        let body = serde_json::to_vec(&req).map_err(proto)?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let t0 = Instant::now();
        for frame in wire::rpc_frames(&wire::lsp_encode(&body)) {
            self.send_frame(&frame)?;
        }
        let resp = tokio::time::timeout(RPC_TIMEOUT, rx)
            .await
            .map_err(|_| WireError::Protocol { message: format!("{method} timed out") })?
            .map_err(|_| WireError::Closed { reason: self.close_reason.lock().unwrap().clone() })?;
        let rtt = ms(t0.elapsed());
        match (resp.result, resp.error) {
            (_, Some(e)) => Err(WireError::Rpc { code: e.code, message: e.message }),
            (Some(r), None) => Ok((r, rtt)),
            (None, None) => Ok((serde_json::Value::Null, rtt)),
        }
    }
}

#[uniffi::export]
impl WireSession {
    pub async fn hello(self: Arc<Self>, token: String, device_id: String, display_name: String) -> Result<HelloSummary, WireError> {
        rt().spawn(async move {
            let params = HelloParams {
                token,
                surface: None,
                protocol: ProtocolRange::supported(),
                capabilities: catalogue_strings(),
                device: Some(DeviceInfo { device_id, display_name: Some(display_name) }),
            };
            let (v, rtt_ms) = self.request(methods::AUTH_HELLO, serde_json::to_value(params).map_err(proto)?).await?;
            let r: HelloResult = serde_json::from_value(v).map_err(proto)?;
            Ok(HelloSummary {
                selected_protocol: r.selected_or_legacy(),
                capabilities: r.capabilities.len() as u32,
                daemon_version: r.daemon_version.unwrap_or_default(),
                rtt_ms,
            })
        })
        .await
        .map_err(proto)?
    }

    pub async fn fleet_subscribe(self: Arc<Self>, after_revision: i64) -> Result<SubscribeSummary, WireError> {
        rt().spawn(async move {
            let params = serde_json::to_value(FleetSubscribeParams { after_revision }).map_err(proto)?;
            let (v, rtt_ms) = self.request(methods::FLEET_SUBSCRIBE, params).await?;
            let r: FleetSubscribeResult = serde_json::from_value(v).map_err(proto)?;
            let replay_state = match r.replay_state {
                FleetReplayState::Complete => "complete".to_string(),
                FleetReplayState::SnapshotReset { reason } => format!("snapshot_reset:{reason:?}"),
            };
            Ok(SubscribeSummary {
                head_revision: r.snapshot.head_revision,
                sessions: r.snapshot.sessions.len() as u32,
                replay_events: r.replay.len() as u32,
                replay_state,
                rtt_ms,
            })
        })
        .await
        .map_err(proto)?
    }

    /// Scratch-only method: stamps a label into the peer's log with the peer's
    /// clock, so lifecycle transitions line up with heartbeat arrivals.
    pub async fn mark(self: Arc<Self>, label: String) -> Result<f64, WireError> {
        rt().spawn(async move { self.request("spike/mark", serde_json::json!({ "label": label })).await.map(|(_, rtt)| rtt) })
            .await
            .map_err(proto)?
    }

    pub fn stats(&self) -> SessionStats {
        SessionStats {
            ws_connect_ms: self.ws_connect_ms,
            noise_handshake_ms: self.noise_handshake_ms,
            heartbeats_sent: self.heartbeats_sent.load(Ordering::SeqCst),
            pongs_received: self.pongs_received.load(Ordering::SeqCst),
            last_pong_rtt_ms: self.last_pong_rtt_us.load(Ordering::SeqCst) as f64 / 1000.0,
            closed: self.closed.load(Ordering::SeqCst),
            close_reason: self.close_reason.lock().unwrap().clone(),
        }
    }

    pub fn close(&self) {
        let _ = self.send_frame(&Frame { opcode: Opcode::StreamEnd, flags: wire::FLAG_FIN, stream_id: 0, seq: 0, payload: vec![] });
        let _ = self.out.send(Vec::new());
        self.mark_closed("closed by client".into());
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}
