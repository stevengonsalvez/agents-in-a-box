//! `ainb plugin tail <plugin_id>` — host tracing tap with plugin-id filter.
//!
//! Installs a [`tracing_subscriber::Layer`] that visits every event the
//! runtime emits, filters down to events whose `plugin` field matches
//! the requested id, then prints one line per event:
//!
//! ```text
//! [2026-05-10T20:55:00.123Z] INFO  plugin_task plugin=burndown init ok
//! ```
//!
//! Why a custom Layer rather than `RUST_LOG=ainb_plugin_runtime`:
//! a single host can host many plugins, all logging through the same
//! target. Filtering by plugin id requires inspecting the structured
//! field, not the target name.
//!
//! Flags (matched by the dispatch contract):
//!
//! - `--level <l>` — drop events below this level. Accepts trace,
//!   debug, info, warn, error (case-insensitive). Defaults to debug
//!   so plugin authors see init/handshake traffic.
//! - `--since <ts>` — RFC-3339 timestamp; events older than this are
//!   suppressed. Live-only, since tracing doesn't persist history,
//!   but a future-timestamp value lets a CI gate skip startup noise
//!   until the gate's expected window opens.
//! - `--duration <secs>` — how long to tail (default 30s).
//!
//! Keep tracing setup tightly scoped — the host process may already
//! have a global subscriber installed by [`crate::lib::init_tracing`].
//! We use [`tracing_subscriber::registry::Registry::set_default`] so
//! the subscriber lives only for the duration of `tail::run` and
//! reverts on drop. **Caveat**: this routes the runtime's own events
//! into our layer; that's the whole point.

use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ainb_plugin_runtime::PluginId;
use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, FixedOffset, Utc};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::registry::Registry;
use tracing_subscriber::Layer;

/// Default window when the user passes no `--duration`.
const DEFAULT_DURATION_SECS: u64 = 30;

/// Entry point invoked from the `ainb plugin tail` clap dispatcher.
pub async fn run(
    plugin_id: &str,
    level: &str,
    since: Option<&str>,
    duration_secs: Option<u64>,
) -> Result<()> {
    let level = parse_level(level)?;
    let since = match since {
        Some(s) => Some(parse_since(s)?),
        None => None,
    };
    let duration = Duration::from_secs(duration_secs.unwrap_or(DEFAULT_DURATION_SECS));

    let (runtime, handle, _outcome) = crate::plugins::init_plugin_runtime()
        .map_err(|e| anyhow!("plugin runtime init failed: {e}"))?;

    let outcome = tail_with_handle(&handle, plugin_id, level, since, duration).await;

    // Same drop dance as `watch::run` — moving the inner runtime onto a
    // blocking thread keeps tokio's "Cannot drop a runtime in a context
    // where blocking is not allowed" panic from biting us.
    tokio::task::spawn_blocking(move || drop(runtime))
        .await
        .ok();
    outcome
}

/// Inner helper, exposed for tests that want to drive the layer without
/// re-running runtime discovery.
pub(crate) async fn tail_with_handle(
    handle: &ainb_plugin_runtime::RuntimeHandle,
    plugin_id: &str,
    level: Level,
    since: Option<DateTime<FixedOffset>>,
    duration: Duration,
) -> Result<()> {
    let pid = PluginId::from(plugin_id);
    if !handle
        .registered_plugins()
        .iter()
        .any(|p| p.id == pid)
    {
        bail!(
            "plugin '{plugin_id}' is not registered — try 'ainb plugin list'"
        );
    }

    let layer = PluginTailLayer::new(plugin_id.to_string(), level, since);
    let subscriber = Registry::default().with(layer);
    let guard = tracing::subscriber::set_default(subscriber);

    println!(
        "tailing plugin {plugin_id} level>={level} for {duration:?} (Ctrl-C to exit)",
    );

    tokio::time::sleep(duration).await;
    drop(guard);
    Ok(())
}

fn parse_level(s: &str) -> Result<Level> {
    Level::from_str(s).map_err(|_| {
        anyhow!(
            "invalid log level '{s}' — expected one of: trace, debug, info, warn, error"
        )
    })
}

fn parse_since(s: &str) -> Result<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s).map_err(|e| {
        anyhow!(
            "invalid --since '{s}' (expected RFC-3339, e.g. 2026-05-10T20:00:00Z): {e}"
        )
    })
}

/// Custom layer: filters tracing events by the structured `plugin`
/// field and emits a one-line text record per match.
struct PluginTailLayer {
    target_plugin: String,
    level: Level,
    since: Option<DateTime<FixedOffset>>,
    /// Counter is publicly readable from tests so they can assert
    /// emission without scraping stdout.
    matched: Arc<Mutex<usize>>,
}

impl PluginTailLayer {
    fn new(target_plugin: String, level: Level, since: Option<DateTime<FixedOffset>>) -> Self {
        Self {
            target_plugin,
            level,
            since,
            matched: Arc::new(Mutex::new(0)),
        }
    }
}

impl<S> Layer<S> for PluginTailLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        if *metadata.level() > self.level {
            return;
        }

        let now = Utc::now();
        if let Some(since) = self.since {
            if now < since.with_timezone(&Utc) {
                return;
            }
        }

        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        if visitor.plugin.as_deref() != Some(&self.target_plugin) {
            return;
        }

        let line = format_event(metadata, &visitor, now);
        // Surface to stdout so `ainb plugin tail … | jq` works after a
        // future JSON-output flag; today text only.
        println!("{line}");

        if let Ok(mut g) = self.matched.lock() {
            *g += 1;
        }
    }
}

#[derive(Default)]
struct FieldVisitor {
    plugin: Option<String>,
    message: String,
    extras: Vec<(String, String)>,
}

impl Visit for FieldVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "plugin" => self.plugin = Some(value.to_owned()),
            "message" => value.clone_into(&mut self.message),
            other => self.extras.push((other.to_owned(), value.to_owned())),
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        // tracing renders `display`/`Display`-formatted values via
        // record_debug too, with the printable form already wrapped.
        let unwrapped = rendered.trim_matches('"').to_string();
        match field.name() {
            "plugin" => self.plugin = Some(unwrapped),
            "message" => self.message = unwrapped,
            other => self.extras.push((other.to_owned(), unwrapped)),
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.extras
            .push((field.name().to_owned(), value.to_string()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.extras
            .push((field.name().to_owned(), value.to_string()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.extras
            .push((field.name().to_owned(), value.to_string()));
    }
}

fn format_event(
    metadata: &tracing::Metadata<'_>,
    visitor: &FieldVisitor,
    now: DateTime<Utc>,
) -> String {
    let extras = if visitor.extras.is_empty() {
        String::new()
    } else {
        let mut s = String::new();
        for (k, v) in &visitor.extras {
            use std::fmt::Write as _;
            let _ = write!(s, " {k}={v}");
        }
        s
    };
    format!(
        "[{ts}] {level:5} {target} plugin={plugin}{extras} {msg}",
        ts = now.to_rfc3339(),
        level = metadata.level(),
        target = metadata.target(),
        plugin = visitor.plugin.as_deref().unwrap_or("?"),
        extras = extras,
        msg = visitor.message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn parse_level_accepts_canonical_names() {
        assert_eq!(parse_level("debug").unwrap(), Level::DEBUG);
        assert_eq!(parse_level("INFO").unwrap(), Level::INFO);
        assert_eq!(parse_level("Warn").unwrap(), Level::WARN);
        assert!(parse_level("loud").is_err());
    }

    #[test]
    fn parse_since_accepts_rfc3339() {
        let parsed = parse_since("2026-05-10T20:00:00Z").unwrap();
        assert_eq!(parsed.to_rfc3339(), "2026-05-10T20:00:00+00:00");
        assert!(parse_since("yesterday").is_err());
    }

    #[test]
    fn layer_filters_by_plugin_id() {
        let layer = PluginTailLayer::new("burndown".into(), Level::TRACE, None);
        let counter = layer.matched.clone();
        let subscriber = tracing_subscriber::Registry::default().with(layer);
        let _g = tracing::subscriber::set_default(subscriber);

        tracing::info!(plugin = "burndown", "init ok");
        tracing::info!(plugin = "session-reader", "ignored");
        tracing::warn!(plugin = "burndown", "render slow");

        assert_eq!(*counter.lock().unwrap(), 2);
    }

    #[test]
    fn layer_filters_below_level() {
        let layer = PluginTailLayer::new("burndown".into(), Level::WARN, None);
        let counter = layer.matched.clone();
        let subscriber = tracing_subscriber::Registry::default().with(layer);
        let _g = tracing::subscriber::set_default(subscriber);

        tracing::debug!(plugin = "burndown", "noisy");
        tracing::info!(plugin = "burndown", "still noisy");
        tracing::warn!(plugin = "burndown", "matters");
        tracing::error!(plugin = "burndown", "matters more");

        assert_eq!(*counter.lock().unwrap(), 2);
    }

    #[test]
    fn layer_skips_events_before_since() {
        let future_since = (Utc::now() + chrono::Duration::seconds(60))
            .with_timezone(&FixedOffset::east_opt(0).unwrap());
        let layer = PluginTailLayer::new("burndown".into(), Level::TRACE, Some(future_since));
        let counter = layer.matched.clone();
        let subscriber = tracing_subscriber::Registry::default().with(layer);
        let _g = tracing::subscriber::set_default(subscriber);

        tracing::info!(plugin = "burndown", "current");

        // since is in the future → event is suppressed.
        assert_eq!(*counter.lock().unwrap(), 0);
    }

    #[test]
    fn unknown_plugin_yields_actionable_error() {
        // Drive the helper directly so we don't invoke the
        // init_plugin_runtime → spawn_blocking dance from inside the
        // test's tokio runtime. The bail-on-unknown-plugin path is the
        // same code regardless.
        use ainb_plugin_runtime::Runtime;
        let (runtime, handle) = Runtime::new().expect("runtime");
        let test_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let res = test_rt.block_on(tail_with_handle(
            &handle,
            "nonexistent",
            Level::INFO,
            None,
            Duration::from_secs(1),
        ));
        drop(runtime);
        let err = res.expect_err("expected error for unknown plugin");
        assert!(
            format!("{err:#}").contains("not registered"),
            "msg: {err:#}"
        );
    }
}
