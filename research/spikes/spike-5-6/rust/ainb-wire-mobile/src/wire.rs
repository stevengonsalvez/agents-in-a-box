//! Transport layers shared by the phone client and the scratch peer.
//!
//! One WebSocket binary message carries one Noise transport message, which
//! decrypts to one frame: the 16-byte header from spike 3 plus a payload.
//! `Rpc` payloads are the daemon's LSP `Content-Length` bytes, fragmented with
//! the `FIN` flag when a message exceeds one Noise message.

use snow::{Builder, HandshakeState};

pub const MAGIC: u8 = 0x74;
pub const FRAMING_VERSION: u8 = 1;
pub const HEADER_LEN: usize = 16;
pub const FLAG_FIN: u8 = 0x01;

/// Noise caps a transport message at 65535 bytes including the 16-byte tag.
pub const NOISE_MAX: usize = 65_535;
pub const MAX_FRAME_PAYLOAD: usize = NOISE_MAX - 16 - HEADER_LEN;

pub const NOISE_PATTERN: &str = "Noise_IK_25519_ChaChaPoly_BLAKE2s";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Opcode {
    Rpc = 20,
    StreamEnd = 21,
    /// Added by spike 5/6: application heartbeat inside the Noise session.
    Ping = 22,
    Pong = 23,
}

impl Opcode {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            20 => Some(Self::Rpc),
            21 => Some(Self::StreamEnd),
            22 => Some(Self::Ping),
            23 => Some(Self::Pong),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub opcode: Opcode,
    pub flags: u8,
    pub stream_id: u32,
    pub seq: u64,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        out.push(MAGIC);
        out.push(FRAMING_VERSION);
        out.push(self.opcode as u8);
        out.push(self.flags);
        out.extend_from_slice(&self.stream_id.to_le_bytes());
        out.extend_from_slice(&((self.seq >> 32) as u32).to_le_bytes());
        out.extend_from_slice(&(self.seq as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    pub fn decode(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < HEADER_LEN {
            return Err(format!("frame too short: {} bytes", buf.len()));
        }
        if buf[0] != MAGIC || buf[1] != FRAMING_VERSION {
            return Err(format!("bad magic/version {:#x}/{}", buf[0], buf[1]));
        }
        let opcode = Opcode::from_u8(buf[2]).ok_or_else(|| format!("unknown opcode {}", buf[2]))?;
        let u32_at = |i: usize| u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        Ok(Self {
            opcode,
            flags: buf[3],
            stream_id: u32_at(4),
            seq: (u64::from(u32_at(8)) << 32) | u64::from(u32_at(12)),
            payload: buf[HEADER_LEN..].to_vec(),
        })
    }
}

/// Split one logical `Rpc` message into frames that each fit a Noise message.
pub fn rpc_frames(bytes: &[u8]) -> Vec<Frame> {
    let chunks: Vec<&[u8]> = if bytes.is_empty() { vec![&[]] } else { bytes.chunks(MAX_FRAME_PAYLOAD).collect() };
    let last = chunks.len() - 1;
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| Frame {
            opcode: Opcode::Rpc,
            flags: if i == last { FLAG_FIN } else { 0 },
            stream_id: 0,
            seq: i as u64,
            payload: c.to_vec(),
        })
        .collect()
}

/// The spike 3 prologue: length-prefixed named fields mixed into the handshake
/// hash, so a disagreement on carrier or host id fails at Noise message 1.
pub fn prologue(transport: &str, host_id: &str) -> Vec<u8> {
    let fields = [
        ("protocol", "ainb-peer-ws"),
        ("framing", "1"),
        ("payload_kinds", "binary"),
        ("initiator", "client"),
        ("responder", "host"),
        ("transport", transport),
        ("host_id", host_id),
    ];
    let mut out = Vec::new();
    for (k, v) in fields {
        for part in [k, v] {
            out.extend_from_slice(&(part.len() as u16).to_be_bytes());
            out.extend_from_slice(part.as_bytes());
        }
    }
    out
}

pub fn initiator(device_private: &[u8], host_public: &[u8], transport: &str, host_id: &str) -> Result<HandshakeState, snow::Error> {
    let p = prologue(transport, host_id);
    Builder::new(NOISE_PATTERN.parse()?)
        .local_private_key(device_private)
        .remote_public_key(host_public)
        .prologue(&p)
        .build_initiator()
}

pub fn responder(host_private: &[u8], transport: &str, host_id: &str) -> Result<HandshakeState, snow::Error> {
    let p = prologue(transport, host_id);
    Builder::new(NOISE_PATTERN.parse()?)
        .local_private_key(host_private)
        .prologue(&p)
        .build_responder()
}

pub fn generate_keypair() -> (Vec<u8>, Vec<u8>) {
    let kp = Builder::new(NOISE_PATTERN.parse().expect("static pattern parses"))
        .generate_keypair()
        .expect("x25519 keygen");
    (kp.private, kp.public)
}

/// LSP `Content-Length` framing, the daemon's unix-socket framing.
pub fn lsp_encode(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

pub fn lsp_decode(buf: &[u8]) -> Result<&[u8], String> {
    let sep = buf.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no LSP header terminator")?;
    let header = std::str::from_utf8(&buf[..sep]).map_err(|e| e.to_string())?;
    let len: usize = header
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length:"))
        .ok_or("no Content-Length")?
        .trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| e.to_string())?;
    let body = &buf[sep + 4..];
    if body.len() != len {
        return Err(format!("Content-Length {len} but body {}", body.len()));
    }
    Ok(body)
}

/// Reassembles `Rpc` fragments until `FIN`.
#[derive(Default)]
pub struct Reassembler {
    buf: Vec<u8>,
}

impl Reassembler {
    pub fn push(&mut self, frame: &Frame) -> Option<Vec<u8>> {
        self.buf.extend_from_slice(&frame.payload);
        (frame.flags & FLAG_FIN != 0).then(|| std::mem::take(&mut self.buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_and_fragmentation() {
        let big = vec![7u8; MAX_FRAME_PAYLOAD * 2 + 5];
        let frames = rpc_frames(&big);
        assert_eq!(frames.len(), 3);
        let mut r = Reassembler::default();
        let mut out = None;
        for f in &frames {
            let decoded = Frame::decode(&f.encode()).unwrap();
            assert!(decoded.encode().len() <= NOISE_MAX - 16);
            out = r.push(&decoded);
        }
        assert_eq!(out.unwrap(), big);
        let body = br#"{"jsonrpc":"2.0"}"#;
        assert_eq!(lsp_decode(&lsp_encode(body)).unwrap(), body);
    }

    #[test]
    fn prologue_mismatch_fails_at_message_one() {
        let (hpriv, hpub) = generate_keypair();
        let (dpriv, _) = generate_keypair();
        let mut buf = [0u8; 1024];
        let mut ok = [0u8; 1024];
        for (transport, host_id, expect_ok) in [("lan", "host-aaaaaaaaaaaa", true), ("tailnet", "host-aaaaaaaaaaaa", false), ("lan", "host-bbbbbbbbbbbb", false)] {
            let mut i = initiator(&dpriv, &hpub, transport, host_id).unwrap();
            let mut r = responder(&hpriv, "lan", "host-aaaaaaaaaaaa").unwrap();
            let n = i.write_message(&[], &mut buf).unwrap();
            assert_eq!(r.read_message(&buf[..n], &mut ok).is_ok(), expect_ok, "{transport} {host_id}");
        }
    }
}
