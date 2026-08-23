//! Tripwire: pressing `[s]` on the offline panel must not trap the user.
//!
//! The reported bug: go to the Hangar screen, start the daemon, and the screen
//! freezes — no key works, `q` and `Esc` included, so there is no way out.
//!
//! The cause is a layering trap rather than a slow function. The SDK dispatches
//! `plugin/handle_key` INLINE on its reader loop, and that dispatch takes the
//! same per-plugin mutex that `plugin/render` holds for the whole render
//! future. `[s]` used to poll-wait up to three seconds inside that render for
//! the spawned `ainb hangar daemon start` to report. For the whole window the
//! reader loop was parked, so every subsequent frame — keys included — went
//! unread. `q` in particular is doubly affected: it reduces to a deferred close
//! request that is itself drained in `render`.
//!
//! So the property under test is not "the start is fast". It is: WHILE a start
//! is in flight, the plugin still reads and services keys. This drives the real
//! [`HangarPlugin`] behind the real `ainb-plugin-sdk-rust` [`Server`] over the
//! genuine Content-Length framing, with a deliberately slow starter, and
//! asserts a `q` sent immediately after `[s]` is acted on well inside the
//! window the old code would have been deaf for.
//!
//! Falsifiable: revert the starter to a blocking poll-wait and the `q` cannot
//! be serviced until it finishes, blowing the budget below.

use std::time::{Duration, Instant};

use ainb_plugin_hangar::plugin::HangarPlugin;
use ainb_plugin_hangar::shell::{DaemonStarter, StartVerdict};
use ainb_plugin_protocol::{framing, methods};
use ainb_plugin_sdk::Server;
use tokio::io::{AsyncWriteExt, BufReader};

/// How long the fake `ainb hangar daemon start` takes to report. Comfortably
/// longer than the budget below, so a blocking implementation cannot pass by
/// being quick.
const START_COST: Duration = Duration::from_secs(3);

/// How long `q` gets to be serviced after `[s]`. The old blocking path could
/// not answer for [`START_COST`]; anything close to instant is correct.
const ESCAPE_BUDGET: Duration = Duration::from_millis(500);

/// Outer guard so a regression fails rather than hanging the suite.
const OUTER_BUDGET: Duration = Duration::from_secs(30);

/// A starter that takes [`START_COST`] to produce its verdict — the shape of
/// the real one, which spawns the CLI and waits for its exit status.
#[derive(Debug)]
struct SlowStarter;

/// Proof the `[s]` action actually reached the starter. Without this the test
/// could pass by never dispatching a start at all.
static STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl DaemonStarter for SlowStarter {
    fn start(&self) -> StartVerdict {
        STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::thread::spawn(move || {
            std::thread::sleep(START_COST);
            let _ = tx.send(Ok(()));
        });
        rx
    }
}

fn host_frame(body: &serde_json::Value) -> Vec<u8> {
    framing::encode(&serde_json::to_vec(body).unwrap())
}

/// Read one Content-Length frame body. `None` on EOF.
async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> Option<serde_json::Value> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        let n = r.read_line(&mut line).await.ok()?;
        if n == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches("\r\n");
        if trimmed.is_empty() {
            let len = content_length?;
            let mut body = vec![0u8; len];
            r.read_exact(&mut body).await.ok()?;
            return serde_json::from_slice(&body).ok();
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("Content-Length") {
                content_length = value.trim().parse().ok();
            }
        }
    }
}

/// A `plugin/handle_key` notification for a bare character press. Built from
/// the real wire types so the shape cannot drift from what the SDK decodes.
fn key_frame(ch: char) -> serde_json::Value {
    let key = ainb_plugin_protocol::params::KeyEvent {
        code: ainb_plugin_protocol::params::KeyCode::Char { ch },
        mods: 0,
        kind: ainb_plugin_protocol::params::KeyKind::Press,
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": methods::PLUGIN_HANDLE_KEY,
        "params": { "screen_id": "hangar", "key": key, "generation": 1 }
    })
}

fn render_frame(id: i64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": methods::PLUGIN_RENDER,
        "params": { "viewport": {"width": 120, "height": 30}, "generation": 0 }
    })
}

/// Pump frames until the response to `want_id` arrives, answering any reverse
/// cap call the plugin makes along the way so it is never left waiting on us.
///
/// Returns `None` if `budget` elapses first — which is the regression: a plugin
/// blocked inside its own render never reads the next request at all.
async fn await_response<W, R>(
    host_write: &mut W,
    host_read: &mut R,
    want_id: i64,
    budget: Duration,
) -> Option<serde_json::Value>
where
    W: tokio::io::AsyncWrite + Unpin,
    R: tokio::io::AsyncBufRead + Unpin,
{
    let deadline = Instant::now() + budget;
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        let f = tokio::time::timeout(left, read_frame(host_read)).await.ok()??;
        let id = f.get("id").and_then(serde_json::Value::as_i64);
        let is_request = f.get("method").is_some();
        if id == Some(want_id) && !is_request {
            return Some(f);
        }
        // A reverse cap call (socket dial, log, publish). Refuse it politely so
        // the plugin keeps moving; none of them are what this test is about.
        if let (Some(rid), true) = (id, is_request) {
            host_write
                .write_all(&host_frame(&serde_json::json!({
                    "jsonrpc": "2.0", "id": rid,
                    "error": { "code": -32001, "message": "not available in this test" }
                })))
                .await
                .ok()?;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_plugin_stays_responsive_while_a_daemon_start_is_in_flight() {
    let outcome = tokio::time::timeout(OUTER_BUDGET, async {
        let (host, plugin) = tokio::io::duplex(1 << 20);
        let (plugin_read, plugin_write) = tokio::io::split(plugin);
        let server = tokio::spawn(
            Server::new(HangarPlugin::with_daemon_starter(Box::new(SlowStarter)))
                .run(plugin_read, plugin_write),
        );
        let (host_read, mut host_write) = tokio::io::split(host);
        let mut host_read = BufReader::new(host_read);

        host_write
            .write_all(&host_frame(&serde_json::json!({
                "jsonrpc": "2.0", "id": 1, "method": methods::PLUGIN_INIT,
                "params": {
                    "manifest_path": "/dev/null/manifest.toml",
                    "granted_capabilities": ["unix_socket_dial", "snapshot_publish", "log"],
                    "abi_version": 2,
                }
            })))
            .await
            .unwrap();
        await_response(&mut host_write, &mut host_read, 1, Duration::from_secs(10))
            .await
            .expect("plugin/init must answer");

        // The first-run danger-access modal is modal: it swallows every key but
        // `q`. Accept it so `[s]` can reach the offline panel underneath.
        host_write.write_all(&host_frame(&key_frame('y'))).await.unwrap();

        // The link is offline (there is no daemon here), which is exactly when
        // `[s]` is armed. Press it, then render — that render is where the
        // deferred start dispatches.
        host_write.write_all(&host_frame(&key_frame('s'))).await.unwrap();
        host_write.write_all(&host_frame(&render_frame(10))).await.unwrap();
        let dispatched = Instant::now();
        if await_response(&mut host_write, &mut host_read, 10, ESCAPE_BUDGET)
            .await
            .is_none()
        {
            server.abort();
            return Err(format!(
                "the render that dispatches the start did not answer within \
                 {ESCAPE_BUDGET:?} — it is waiting on the start, which is the freeze"
            ));
        }

        // The start is still running (it takes START_COST). The plugin must
        // keep reading and answering: the reader loop that serves this request
        // is the same one that dispatches every keystroke, so a plugin that
        // cannot answer here cannot service q or Esc either.
        host_write.write_all(&host_frame(&render_frame(11))).await.unwrap();
        let asked_again = Instant::now();
        let answered = await_response(&mut host_write, &mut host_read, 11, ESCAPE_BUDGET).await;
        let elapsed = asked_again.elapsed();
        server.abort();
        if answered.is_none() {
            return Err(format!(
                "the plugin went deaf for at least {ESCAPE_BUDGET:?} while the \
                 start was in flight — this is the unescapable screen"
            ));
        }
        assert_eq!(
            STARTS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the [s] action must actually have reached the starter, or this \
             test proves nothing"
        );
        assert!(
            dispatched.elapsed() < START_COST,
            "the whole exchange must complete WHILE the start is still running, \
             not after it finishes"
        );
        Ok(elapsed)
    })
    .await;

    match outcome {
        Ok(Ok(elapsed)) => {
            eprintln!("plugin answered in {elapsed:?} with a daemon start still in flight");
        }
        Ok(Err(msg)) => panic!("{msg}"),
        Err(_) => panic!(
            "the plugin never answered within {OUTER_BUDGET:?} — a start that \
             blocks the reader loop is exactly the reported freeze"
        ),
    }
}
