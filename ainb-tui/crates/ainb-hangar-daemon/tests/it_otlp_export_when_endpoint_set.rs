//! P8.2 OTLP export tripwire (feature-gated).
//!
//! Proves that, **with the `otlp` feature on and `OTEL_EXPORTER_OTLP_ENDPOINT`
//! pointed at a receiver**, [`ainb_hangar_daemon::observability::install`]
//! attaches an OpenTelemetry layer that exports emitted spans to that endpoint.
//!
//! Mechanics: a tiny hand-rolled HTTP receiver (a raw `tokio` TCP listener — no
//! extra dev-dep) accepts the OTLP/HTTP `POST /v1/traces`, reads the request
//! body, and hands it back over a channel. The test installs the subscriber with
//! the endpoint set to that listener, emits one `info_span!("p8_2_probe",
//! probe_id = "abc")`, and force-flushes via the explicit tracer-provider
//! shutdown (never relying on Drop inside the tokio runtime). It then asserts the
//! receiver got a POST whose protobuf body carries the `probe_id` key and the
//! `abc` value as UTF-8 byte runs.
//!
//! Gated behind `#![cfg(feature = "otlp")]`: without the feature the OTEL stack
//! is not even linked, so this file compiles to nothing on a default build.
#![cfg(feature = "otlp")]

use std::io::Write;
use std::path::Path;
use std::sync::mpsc;

use ainb_hangar_store::test_support::with_isolated_home;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Spin a minimal HTTP/1.1 receiver on a random loopback port.
///
/// Returns the bound `http://127.0.0.1:<port>` base URL and a channel that
/// yields the raw bytes of the first request body received. The OTLP/HTTP
/// exporter POSTs protobuf-encoded spans to `<base>/v1/traces`; we don't decode
/// the protobuf, we just capture the body bytes and grep them for the probe
/// attribute strings, which survive verbatim inside the protobuf.
async fn spawn_otlp_receiver() -> (String, mpsc::Receiver<Vec<u8>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback receiver");
    let port = listener.local_addr().expect("local addr").port();
    let (tx, rx) = mpsc::channel::<Vec<u8>>();

    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let tx = tx.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                // Read until we have headers + the full Content-Length body.
                let mut content_len: Option<usize> = None;
                let mut header_end: Option<usize> = None;
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&chunk[..n]);
                            if header_end.is_none() {
                                if let Some(pos) =
                                    buf.windows(4).position(|w| w == b"\r\n\r\n")
                                {
                                    header_end = Some(pos + 4);
                                    let head = String::from_utf8_lossy(&buf[..pos]);
                                    for line in head.lines() {
                                        if let Some(v) =
                                            line.to_ascii_lowercase().strip_prefix("content-length:")
                                        {
                                            content_len = v.trim().parse().ok();
                                        }
                                    }
                                }
                            }
                            if let (Some(he), Some(cl)) = (header_end, content_len) {
                                if buf.len() >= he + cl {
                                    let body = buf[he..he + cl].to_vec();
                                    let _ = tx.send(body);
                                    break;
                                }
                            }
                        }
                    }
                }
                // OTLP/HTTP success response: empty `200 OK`.
                let _ = sock
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                    .await;
                let _ = sock.flush().await;
            });
        }
    });

    (format!("http://127.0.0.1:{port}"), rx)
}

#[tokio::test(flavor = "multi_thread")]
async fn otlp_layer_exports_span_to_endpoint_when_set() {
    let (base, rx) = spawn_otlp_receiver().await;

    with_isolated_home(|home: &Path| {
        let log_dir = home.join("hangar").join("logs");

        // SAFETY: single-threaded section under the test_support env lock held by
        // `with_isolated_home`; no other thread reads OTEL env concurrently.
        std::env::set_var("OTEL_EXPORTER_OTLP_ENDPOINT", &base);

        let mut opts = ainb_hangar_daemon::observability::ObservabilityOpts::new(log_dir);
        opts.stderr = false;
        opts.otlp = Some(ainb_hangar_daemon::observability::OtlpOpts::new(base.clone()));

        let guard =
            ainb_hangar_daemon::observability::install(opts).expect("install subscriber + otlp");

        // Emit one probe span inside the subscriber's scope.
        {
            let span = tracing::info_span!("p8_2_probe", probe_id = "abc");
            let _enter = span.enter();
            tracing::info!("probe event");
        }

        // Force-flush + tear down the exporter (explicit, never Drop-in-runtime).
        guard.shutdown();

        std::env::remove_var("OTEL_EXPORTER_OTLP_ENDPOINT");
    });

    // The exporter is on its own runtime/thread; give it a moment to POST.
    let body = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("receiver got an OTLP POST body");

    let hay = String::from_utf8_lossy(&body);
    assert!(
        hay.contains("probe_id"),
        "exported OTLP body should carry the `probe_id` attribute key"
    );
    assert!(
        hay.contains("abc"),
        "exported OTLP body should carry the `abc` attribute value"
    );
    // Quiet the unused-import lint when assertions above short-circuit compile.
    let _ = std::io::stderr().flush();
}
