//! Slow fixture plugin: identical to `fixture_plugin` except render
//! sleeps 200ms before replying — used by tripwire_nonblocking to
//! prove the TUI render thread never blocks on a slow plugin.

use std::io::{BufReader, Write};
use std::time::Duration;

use ainb_plugin_protocol::framing::{MAX_BODY_BYTES, encode};
use ainb_plugin_protocol::methods;
use ainb_plugin_protocol::params::{PluginInitResult, RenderResult};
use ainb_plugin_protocol::wire_buffer::{Cell, Coord, WireBuffer};
use serde_json::{Value, json};

fn main() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = stdout.lock();

    loop {
        let body = match read_frame_sync(&mut reader) {
            Ok(Some(b)) => b,
            Ok(None) => break,
            Err(e) => {
                eprintln!("slow-fixture: read frame failed: {e}");
                break;
            }
        };
        let v: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("slow-fixture: malformed JSON: {e}");
                continue;
            }
        };
        let id = v.get("id").and_then(Value::as_u64);
        let method = v.get("method").and_then(Value::as_str).unwrap_or("");

        match method {
            methods::PLUGIN_INIT => {
                if let Some(id) = id {
                    let result = serde_json::to_value(PluginInitResult {
                        name: "slow-fixture".into(),
                        version: "0.1.0".into(),
                    })
                    .unwrap();
                    write_response(&mut writer, id, result);
                }
            }
            methods::PLUGIN_RENDER => {
                // Deliberate 200ms delay to simulate slow plugin.
                std::thread::sleep(Duration::from_millis(200));
                if let Some(id) = id {
                    let mut buf = WireBuffer::new(1, 1);
                    buf.push(Coord::new(0, 0), Cell::new("S"));
                    let result = serde_json::to_value(RenderResult {
                        buffer: buf,
                        redraw: false,
                    })
                    .unwrap();
                    write_response(&mut writer, id, result);
                }
            }
            methods::PLUGIN_SHUTDOWN => {
                std::process::exit(0);
            }
            _ => {
                if let Some(id) = id {
                    let body = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32601, "message": format!("method not found: {method}") }
                    });
                    write_value(&mut writer, &body);
                }
            }
        }
    }
}

fn write_response<W: Write>(w: &mut W, id: u64, result: Value) {
    let body = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    write_value(w, &body);
}

fn write_value<W: Write>(w: &mut W, v: &Value) {
    let bytes = serde_json::to_vec(v).expect("JSON serialize");
    let frame = encode(&bytes);
    w.write_all(&frame).ok();
    w.flush().ok();
}

fn read_frame_sync<R: std::io::BufRead>(r: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut any_byte_read = false;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line)?;
        if n == 0 {
            if any_byte_read {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "EOF in headers",
                ));
            }
            return Ok(None);
        }
        any_byte_read = true;
        if !line.ends_with("\r\n") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "header not CRLF terminated",
            ));
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing Content-Length")
            })?;
            if len > MAX_BODY_BYTES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "body too big",
                ));
            }
            let mut body = vec![0u8; len];
            r.read_exact(&mut body)?;
            return Ok(Some(body));
        }
        let Some((name, value)) = trimmed.split_once(':') else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "malformed header",
            ));
        };
        if name.trim().eq_ignore_ascii_case("Content-Length") {
            let parsed: usize = value.trim().parse().map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "non-numeric Content-Length",
                )
            })?;
            content_length = Some(parsed);
        }
    }
}
