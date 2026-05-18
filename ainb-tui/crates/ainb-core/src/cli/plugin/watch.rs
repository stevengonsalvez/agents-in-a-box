//! `ainb plugin watch <plugin_id>` — live runtime introspection.
//!
//! Spins up the plugin runtime, looks up the requested plugin, and runs
//! a non-blocking poll loop printing one line per observed event:
//!
//! - lifecycle transitions (`Idle → Spawning → Running` etc.)
//! - publishes on every snapshot topic the plugin's manifest declares
//!   under `[provides].snapshots` *and* `[subscribes].snapshots`
//! - render outputs the runtime has cached for the plugin
//!
//! Architectural lint per Phase 7 plan §7a:
//! [`RuntimeHandle::try_recv_render`] and
//! [`RuntimeHandle::lifecycle_state`] are sync + non-blocking. The CLI
//! owns the tokio runtime end-to-end (we *can* `.await`), but the
//! observation surface is the same one the TUI render thread uses, so
//! the same constraints exercise it the way the TUI will.
//!
//! Output format: one line per event, prefixed with an ISO-8601
//! timestamp. JSON output emits a structured record per event suitable
//! for piping to `jq`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ainb_plugin_runtime::{LifecycleState, PluginId, RuntimeHandle};
use anyhow::{Result, anyhow};
use chrono::Utc;
use serde::Serialize;

/// Poll interval. The TUI render loop uses ~16ms; we don't need to be
/// that hot here — a 100ms cadence keeps the output readable while
/// staying responsive to lifecycle transitions.
const POLL_INTERVAL_MS: u64 = 100;

/// Default duration when the user passes no `--duration`. Picked so an
/// unattended `ainb plugin watch` doesn't hang forever in CI.
const DEFAULT_DURATION_SECS: u64 = 30;

/// Entry point invoked from the `ainb plugin watch` clap dispatcher.
pub async fn run(plugin_id: &str, duration_secs: Option<u64>) -> Result<()> {
    let (runtime, handle, _outcome) = crate::plugins::init_plugin_runtime()
        .map_err(|e| anyhow!("plugin runtime init failed: {e}"))?;

    let outcome = run_with_handle(&handle, plugin_id, duration_secs).await;

    // Drop the owning runtime off the async context — dropping it while
    // we're still inside the outer tokio runtime hits the "Cannot drop a
    // runtime in a context where blocking is not allowed" panic.
    tokio::task::spawn_blocking(move || drop(runtime)).await.ok();
    outcome
}

/// Inner helper: same flow as [`run`] but takes an already-built
/// [`RuntimeHandle`]. Lets tests exercise the lookup path without
/// owning the [`Runtime`] across an `await`.
pub(crate) async fn run_with_handle(
    handle: &RuntimeHandle,
    plugin_id: &str,
    duration_secs: Option<u64>,
) -> Result<()> {
    let pid = PluginId::from(plugin_id);
    let registered = handle.registered_plugins();
    let target = registered.iter().find(|p| p.id == pid).cloned().ok_or_else(|| {
        anyhow!("plugin '{plugin_id}' is not registered — try 'ainb plugin list'")
    })?;

    print_header(&target);

    let topics: Vec<String> = target
        .manifest
        .subscribes
        .snapshots
        .iter()
        .chain(target.manifest.provides.snapshots.iter())
        .cloned()
        .collect();

    let duration = Duration::from_secs(duration_secs.unwrap_or(DEFAULT_DURATION_SECS));
    poll_loop(handle, &pid, &topics, duration).await;
    Ok(())
}

fn print_header(target: &ainb_plugin_runtime::RegisteredPlugin) {
    println!(
        "watching plugin {name} v{ver} (abi={abi}) — Ctrl-C to exit",
        name = target.manifest.plugin.name,
        ver = target.manifest.plugin.version,
        abi = target.manifest.plugin.abi_version,
    );
    if !target.manifest.subscribes.snapshots.is_empty() {
        println!(
            "  subscribes: {}",
            target.manifest.subscribes.snapshots.join(", ")
        );
    }
    if !target.manifest.provides.snapshots.is_empty() {
        println!(
            "  publishes: {}",
            target.manifest.provides.snapshots.join(", ")
        );
    }
}

/// One observed event row.
#[derive(Debug, Serialize)]
pub(crate) struct Event {
    pub(crate) timestamp: String,
    pub(crate) plugin: String,
    pub(crate) kind: String,
    pub(crate) detail: String,
}

impl Event {
    fn print_line(&self) {
        println!(
            "[{ts}] {kind:18} plugin={plugin:16} {detail}",
            ts = self.timestamp,
            kind = self.kind,
            plugin = self.plugin,
            detail = self.detail,
        );
    }
}

async fn poll_loop(handle: &RuntimeHandle, pid: &PluginId, topics: &[String], duration: Duration) {
    let deadline = Instant::now() + duration;
    let mut last_state: Option<LifecycleState> = handle.lifecycle_state(pid);
    if let Some(s) = last_state {
        emit_event(pid, "lifecycle_initial", &format!("{s:?}"));
    }
    let mut last_snapshots: HashMap<String, Vec<u8>> = HashMap::new();
    for t in topics {
        if let Some(b) = handle.snapshot_get(t) {
            emit_event(
                pid,
                "snapshot_initial",
                &format!("topic={t} size={}", b.len()),
            );
            last_snapshots.insert(t.clone(), b.to_vec());
        }
    }

    loop {
        if Instant::now() >= deadline {
            emit_event(pid, "watch_deadline", &format!("watched for {duration:?}"));
            return;
        }

        // Lifecycle transitions.
        let now_state = handle.lifecycle_state(pid);
        if now_state != last_state {
            let from = last_state.map_or_else(|| "<gone>".into(), |s| format!("{s:?}"));
            let to = now_state.map_or_else(|| "<gone>".into(), |s| format!("{s:?}"));
            emit_event(pid, "lifecycle", &format!("{from} -> {to}"));
            last_state = now_state;
        }

        // Snapshot updates.
        for t in topics {
            let cur = handle.snapshot_get(t).map(|b| b.to_vec());
            let prior = last_snapshots.get(t);
            if cur.as_ref() != prior {
                let detail = match &cur {
                    Some(b) => format!("topic={t} size={}", b.len()),
                    None => format!("topic={t} <cleared>"),
                };
                emit_event(pid, "snapshot_publish", &detail);
                if let Some(b) = cur {
                    last_snapshots.insert(t.clone(), b);
                } else {
                    last_snapshots.remove(t);
                }
            }
        }

        // Cached render output (the TUI consumes this; surface it so
        // operators can see the plugin is producing frames).
        if let Some(buf) = handle.try_recv_render(pid) {
            emit_event(
                pid,
                "render",
                &format!("cells={} {}x{}", buf.cells.len(), buf.width, buf.height),
            );
        }

        tokio::time::sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
    }
}

fn emit_event(pid: &PluginId, kind: &str, detail: &str) {
    let ev = Event {
        timestamp: Utc::now().to_rfc3339(),
        plugin: pid.to_string(),
        kind: kind.into(),
        detail: detail.into(),
    };
    ev.print_line();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ainb_plugin_runtime::Runtime;

    #[test]
    fn unknown_plugin_yields_actionable_error() {
        // Build a runtime with zero plugins registered and assert
        // run_with_handle surfaces an operator-actionable error. We
        // drive the async helper on a fresh test-side tokio runtime so
        // we don't tangle with the runtime owned by the
        // [`ainb_plugin_runtime::Runtime`] under test.
        let (runtime, handle) = Runtime::new().expect("runtime");
        let test_rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let result = test_rt.block_on(run_with_handle(&handle, "nonexistent", Some(1)));
        // Drop the plugin runtime off the test thread before the test
        // tokio runtime tears down, mirroring the dance run() does.
        drop(runtime);
        let err = result.expect_err("expected error for unknown plugin");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("not registered"),
            "error should suggest 'ainb plugin list', got: {msg}"
        );
    }

    #[test]
    fn event_print_line_does_not_panic_on_empty_detail() {
        // A regression smoke — the println! formatter must not blow up
        // on empty fields.
        let ev = Event {
            timestamp: "1970-01-01T00:00:00Z".into(),
            plugin: "x".into(),
            kind: "render".into(),
            detail: String::new(),
        };
        ev.print_line();
    }
}
