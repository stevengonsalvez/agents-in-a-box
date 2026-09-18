//! Dashboard data sources.
//!
//! The dashboard never re-implements data access — it drives the *same*
//! `ainb --format json …` commands the CLI and TUI expose, so the browser view
//! can never drift from the terminal view. [`DataSource`] abstracts that so
//! tests can inject a deterministic fake instead of spawning subprocesses.

use std::ffi::OsString;
use std::future::Future;
use std::pin::Pin;

use ainb_app::wire::web::{WebCost, WebNeedCard};
use serde_json::Value;

/// The sessions + needs pair fetched on the fast poll cadence (cost excluded).
/// Returned by [`DataSource::core`].
#[derive(Debug, Clone)]
pub struct CoreSnapshot {
    /// `ainb --format json list --frame`: the live session list, as rows
    /// projected from the redacted Sessions frame (#1056).
    pub sessions: Value,
    /// The daemon `attention/list` inbox as allow-listed web cards (#1081).
    pub needs: Vec<WebNeedCard>,
}

/// A `'static` boxed future, the return shape of [`DataSource::core`]. Keeping
/// the trait boxed-future (rather than `async fn`) makes it object-safe so the
/// router can hold a `dyn DataSource`.
pub type CoreFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CoreSnapshot, DataError>> + Send + 'a>>;

/// A `'static` boxed future, the return shape of [`DataSource::cost`].
pub type CostFuture<'a> = Pin<Box<dyn Future<Output = Option<WebCost>> + Send + 'a>>;

/// A snapshot of everything the dashboard renders, as JSON values. `sessions`
/// and `needs` are projections through `ainb_app::wire::web` allow-lists, so a
/// field the CLI or the daemon adds does NOT reach the browser until the
/// projection names it and the key-path fixture locks it (#1056, #1081).
/// `cost` is projected the same way (#1113), and typed so nothing can bypass
/// the projection (#1119).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FleetSnapshot {
    /// `ainb --format json list --frame`: the live session list, as rows
    /// projected from the redacted Sessions frame (#1056).
    pub sessions: Value,
    /// The daemon `attention/list` inbox mapped to ASK/ERR/WAIT cards (D18),
    /// projected by `ainb_app::wire::web::need_cards`: no `cwd`, no raw
    /// request, payload text scrubbed (#1081). Each card carries `attentionId`
    /// so an ASK can be answered via `POST /api/answer`. Typed, so nothing can
    /// put a daemon card here without going through the projection.
    pub needs: Vec<WebNeedCard>,
    /// `ainb --format json fleet cost`, projected by
    /// `ainb_app::wire::web::cost_panel` to the totals, models and groups the
    /// dashboard draws: no per-session rows, no cwd (#1113). `null` when the
    /// verb is absent from this build or fails, so the dashboard degrades
    /// gracefully instead of failing. Typed, like `needs`, so nothing can put a
    /// raw report here without going through the projection (#1119).
    pub cost: Option<WebCost>,
    /// Content fingerprint, used by the SSE layer to suppress duplicate pushes
    /// when nothing changed. Skipped from the API payload — it's internal.
    #[serde(skip)]
    pub(crate) fingerprint: u64,
}

impl FleetSnapshot {
    /// Assemble a full snapshot from a fast-cadence [`CoreSnapshot`] and a
    /// (possibly stale) cost panel, recomputing the fingerprint. Used by the
    /// poller to stitch fresh sessions/needs onto the last-known cost on ticks
    /// that skip the slow cost fetch.
    #[must_use]
    pub fn from_parts(core: CoreSnapshot, cost: Option<WebCost>) -> Self {
        let needs = serde_json::to_value(&core.needs).unwrap_or(Value::Null);
        let cost_value = serde_json::to_value(&cost).unwrap_or(Value::Null);
        let fingerprint = Self::compute_fingerprint(&core.sessions, &needs, &cost_value);
        Self {
            sessions: core.sessions,
            needs: core.needs,
            cost,
            fingerprint,
        }
    }

    /// Compute a stable fingerprint over the rendered payload. Two snapshots
    /// with identical sessions/needs/cost hash equal, so the SSE stream only
    /// emits on real change.
    #[must_use]
    pub fn compute_fingerprint(sessions: &Value, needs: &Value, cost: &Value) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // `serde_json::Value` isn't `Hash`, but it doesn't need to be: walk it
        // structurally into the hasher. This avoids three full `to_string()`
        // re-serializations of the entire snapshot on every poll tick (every
        // ~2s, forever) — we feed bytes straight into the hasher with no
        // intermediate `String` allocations.
        hash_value(sessions, &mut h);
        hash_value(needs, &mut h);
        hash_value(cost, &mut h);
        h.finish()
    }
}

/// Feed a [`serde_json::Value`] into a hasher structurally, with no
/// intermediate string allocation. A leading discriminant byte per node keeps
/// distinct shapes from colliding (e.g. the string `"1"` vs the number `1`, or
/// `[]` vs `{}`), and object keys are hashed in their stored (insertion/sorted)
/// order — stable for a given serialization, which is all the fingerprint needs.
fn hash_value<H: std::hash::Hasher>(v: &Value, h: &mut H) {
    use std::hash::Hash;
    match v {
        Value::Null => h.write_u8(0),
        Value::Bool(b) => {
            h.write_u8(1);
            h.write_u8(u8::from(*b));
        }
        Value::Number(n) => {
            h.write_u8(2);
            // The number's textual form distinguishes int/float/precision
            // without depending on `f64` round-tripping; hash its bytes.
            h.write(n.to_string().as_bytes());
        }
        Value::String(s) => {
            h.write_u8(3);
            s.hash(h);
        }
        Value::Array(items) => {
            h.write_u8(4);
            h.write_usize(items.len());
            for item in items {
                hash_value(item, h);
            }
        }
        Value::Object(map) => {
            h.write_u8(5);
            h.write_usize(map.len());
            for (k, val) in map {
                k.hash(h);
                hash_value(val, h);
            }
        }
    }
}

/// Error returned when a data source cannot produce a snapshot.
#[derive(Debug, thiserror::Error)]
pub enum DataError {
    /// The underlying `ainb` command failed to spawn or exited non-zero.
    #[error("`ainb {verb}` failed: {detail}")]
    CommandFailed {
        /// The verb we tried to run, e.g. `list` or `fleet needs`.
        verb: String,
        /// Human-readable failure detail (stderr / spawn error).
        detail: String,
    },
    /// The command produced output that wasn't valid JSON.
    #[error("`ainb {verb}` produced invalid JSON: {detail}")]
    InvalidJson {
        /// The verb whose output failed to parse.
        verb: String,
        /// Parse error detail.
        detail: String,
    },
}

/// Produces [`FleetSnapshot`]s on demand. Implemented by [`AinbCliSource`] in
/// production and by a fake in tests. Object-safe via boxed futures.
///
/// The poller fetches [`core`](DataSource::core) (sessions + needs) on the fast
/// cadence and [`cost`](DataSource::cost) on a slower one, because
/// `ainb fleet cost` cold-boots the burndown plugin runtime per call and cost
/// data rolls up slowly. A cold-cache request reads `core` only (#1055).
pub trait DataSource: Send + Sync + 'static {
    /// Fetch only the fast-cadence surfaces (sessions + needs). These are cheap
    /// relative to cost and need ~2s freshness, so the poller refreshes them on
    /// every tick.
    fn core(&self) -> CoreFuture<'_>;

    /// Fetch the slow-cadence cost panel, best-effort: any failure or absent
    /// verb resolves to `None` so the dashboard degrades gracefully.
    /// The cost task calls this on its own cadence, under a timeout.
    fn cost(&self) -> CostFuture<'_>;
}

/// Production data source: shells out to the `ainb` binary with
/// `--format json`. Resolves the binary from `AINB_BIN`, else the running
/// executable (so a dev `cargo run` and an installed `ainb` both work), else
/// `ainb` on `PATH`.
#[derive(Debug, Clone)]
pub struct AinbCliSource {
    bin: OsString,
}

impl Default for AinbCliSource {
    fn default() -> Self {
        Self::new()
    }
}

impl AinbCliSource {
    /// Build a source that invokes the resolved `ainb` binary.
    #[must_use]
    pub fn new() -> Self {
        let bin = std::env::var_os("AINB_BIN")
            .or_else(|| std::env::current_exe().ok().map(Into::into))
            .unwrap_or_else(|| OsString::from("ainb"));
        Self { bin }
    }

    /// Run `ainb --format json <args...>` and parse stdout as JSON.
    ///
    /// `allow_absent`: when the subcommand may legitimately not exist in this
    /// build (e.g. `fleet cost` before the cost-surface PR merges), a failure
    /// yields `Value::Null` instead of an error, so the dashboard degrades
    /// gracefully rather than blocking on an optional feature.
    async fn run_json(&self, args: &[&str], allow_absent: bool) -> Result<Value, DataError> {
        let verb = args.join(" ");
        let mut cmd = tokio::process::Command::new(&self.bin);
        cmd.arg("--format").arg("json").args(args);
        cmd.stdin(std::process::Stdio::null());
        // A caller that stops waiting (the cost task's timeout, #1055) drops this
        // future; the child is killed with it instead of running on unattended.
        cmd.kill_on_drop(true);

        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) if allow_absent => {
                tracing::debug!(verb, error = %e, "optional ainb subcommand unavailable");
                return Ok(Value::Null);
            }
            Err(e) => {
                return Err(DataError::CommandFailed {
                    verb,
                    detail: format!("spawn failed: {e}"),
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if allow_absent {
                tracing::debug!(verb, %stderr, "optional ainb subcommand returned non-zero");
                return Ok(Value::Null);
            }
            // stderr is logged, never returned: it reaches the HTTP error body,
            // and a failing verb can print a session's paths or its own input.
            tracing::warn!(verb, %stderr, status = %output.status, "ainb subcommand failed");
            return Err(DataError::CommandFailed {
                verb,
                detail: format!("exited with {}", output.status),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if trimmed.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(trimmed).map_err(|e| DataError::InvalidJson {
            verb,
            detail: e.to_string(),
        })
    }
}

/// Whether the pre-T0 read ordering is in force, read from the environment.
///
/// `ainb` owns `[fleet.status] legacy_classify_primary` and bridges it into
/// this variable at startup (`config::tunables::export_env_bridge`), because
/// this crate deliberately does not depend on `ainb-core`. Unset means the T0
/// ordering, which is the shipped default.
///
/// Accepts only the affirmative tokens: anything else, including a typo, leaves
/// the shipped behaviour in place rather than silently rolling a host back.
fn legacy_classify_primary() -> bool {
    matches!(
        std::env::var("AINB_FLEET_LEGACY_CLASSIFY_PRIMARY")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Fetch the open attention inbox from the daemon (`attention/list`, fleet-wide)
/// and map it to the dashboard's `needs` cards (D18). Best-effort: a
/// down / unreachable daemon (or a token that hasn't been minted yet) degrades
/// to an empty list so the dashboard still renders sessions instead of failing
/// the whole poll. This is the read half of the web-on-the-bus retarget: the
/// old `ainb fleet needs` subprocess (which cold-booted a plugin runtime and
/// capture-paned every session) is gone.
async fn daemon_needs() -> Vec<WebNeedCard> {
    match crate::daemon::web_client() {
        Ok(client) => match client.attention_list_fleet().await {
            Ok(rows) => {
                // D14: stamp every card from the daemon's one status read, so
                // the dashboard, `ainb fleet needs` and the TUI fleet panel
                // print the same state for the same agent. A status read that
                // fails leaves the cards unstamped rather than dropping them:
                // an inbox row with no tier is still a question worth showing.
                let empty = || ainb_hangar_proto::agent_status::AgentStatusResult {
                    rows: Vec::new(),
                    head_revision: 0,
                    unknown_events: Vec::new(),
                };
                // The T0 rollback, read from the env because this crate has no
                // config loader: `ainb` bridges
                // `[fleet.status] legacy_classify_primary` into
                // `AINB_FLEET_LEGACY_CLASSIFY_PRIMARY` at startup. Rolled back,
                // the dashboard renders the inbox cards unstamped, exactly as it
                // did before T0.
                let status = if legacy_classify_primary() {
                    empty()
                } else {
                    client.fleet_status().await.unwrap_or_else(|e| {
                        tracing::debug!(error = %e, "fleet/status unavailable; needs render unstamped");
                        empty()
                    })
                };
                // The browser gets the allow-listed card, never the daemon's:
                // `cwd` and the raw request stay behind, as section 20's frame
                // keeps them (#1081).
                let cards = crate::daemon::attention_to_needs_with_status(&rows, &status.rows);
                ainb_app::wire::web::need_cards(&cards)
            }
            Err(e) => {
                tracing::debug!(error = %e, "attention/list unavailable; needs degrades to empty");
                Vec::new()
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "daemon client unavailable; needs degrades to empty");
            Vec::new()
        }
    }
}

impl DataSource for AinbCliSource {
    fn core(&self) -> CoreFuture<'_> {
        Box::pin(async move {
            // Sessions come from `ainb list --frame` (the daemon exposes no
            // host-session snapshot RPC): the same sessions as `ainb list`, but
            // as rows projected from the redacted Sessions section frame, so a
            // label or any text the frame withholds or scrubs never reaches the
            // browser (#1056). Needs read the daemon's attention inbox instead
            // of the old `ainb fleet needs` capture-pane poll (D18).
            let sessions = self.run_json(&["list", "--frame"], false).await?;
            let needs = daemon_needs().await;
            Ok(CoreSnapshot { sessions, needs })
        })
    }

    fn cost(&self) -> CostFuture<'_> {
        Box::pin(async move {
            // Cost is best-effort: `run_json(.., allow_absent=true)` already
            // resolves spawn errors / non-zero exits to `Value::Null`, so any
            // residual error here also degrades to `None` rather than failing.
            let report = self.run_json(&["fleet", "cost"], true).await.unwrap_or(Value::Null);
            // The report's per-session rows carry absolute cwds; the browser gets
            // only the totals, models and groups the dashboard draws (#1113).
            ainb_app::wire::web::cost_panel(&report)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn a_failing_verb_reports_its_status_and_keeps_its_stderr_out_of_the_error() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("scratch dir");
        let bin = dir.path().join("ainb");
        std::fs::write(
            &bin,
            "#!/bin/sh\necho 'session at /home/op/secret-repo failed' >&2\nexit 3\n",
        )
        .expect("write the stub");
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let source = AinbCliSource {
            bin: bin.into_os_string(),
        };

        let error = source.run_json(&["list", "--frame"], false).await.expect_err("the verb fails");

        let shown = error.to_string();
        assert!(!shown.contains("secret-repo"), "{shown}");
        assert!(shown.contains("exit status: 3"), "{shown}");
    }

    fn panel_snapshot(cost: Option<WebCost>) -> FleetSnapshot {
        let core = CoreSnapshot {
            sessions: json!([]),
            needs: Vec::new(),
        };
        FleetSnapshot::from_parts(core, cost)
    }

    #[test]
    fn a_present_cost_panel_serialises_to_the_bytes_the_value_path_served() {
        let report = json!({
            "totals": {"cost_usd": 0.5, "session_count": 1, "model_count": 1,
                "bucket": {"input_tokens": 10, "output_tokens": 5, "call_count": 1, "cost_usd": 0.5}},
            "models": [{"model": "claude-sonnet", "cost_usd": 0.5, "bucket": {"call_count": 1}}],
            "groups": [{"group": "repo", "cost_usd": 0.5, "session_count": 1, "bucket": {}}],
        });
        let panel = ainb_app::wire::web::cost_panel(&report).expect("a report object");
        // Before #1119 the snapshot held the panel as a `Value`; the typed field
        // must put the same bytes on the wire and hash the same.
        let served = serde_json::to_value(&panel).expect("panel serialises");

        let snapshot = panel_snapshot(Some(panel));
        let body = serde_json::to_string(&snapshot).expect("snapshot serialises");

        let expected = format!(
            "{{\"sessions\":[],\"needs\":[],\"cost\":{}}}",
            serde_json::to_string(&served).expect("value serialises")
        );
        assert_eq!(body, expected);
        assert_eq!(
            snapshot.fingerprint,
            FleetSnapshot::compute_fingerprint(&json!([]), &json!([]), &served)
        );
    }

    #[test]
    fn an_absent_cost_panel_serialises_as_null() {
        let snapshot = panel_snapshot(None);
        let body = serde_json::to_string(&snapshot).expect("snapshot serialises");
        assert_eq!(body, r#"{"sessions":[],"needs":[],"cost":null}"#);
    }

    #[test]
    fn fingerprint_is_stable_and_change_sensitive() {
        let s1 = json!([{"id": 1}]);
        let n1 = json!([]);
        let c1 = Value::Null;
        let fp_a = FleetSnapshot::compute_fingerprint(&s1, &n1, &c1);
        let fp_b = FleetSnapshot::compute_fingerprint(&s1, &n1, &c1);
        assert_eq!(fp_a, fp_b, "identical input must hash equal");

        let s2 = json!([{"id": 2}]);
        let fp_c = FleetSnapshot::compute_fingerprint(&s2, &n1, &c1);
        assert_ne!(fp_a, fp_c, "changed sessions must change the fingerprint");
    }

    #[test]
    fn fingerprint_detects_deeply_nested_change() {
        // A change buried inside nested arrays/objects must still flip the
        // fingerprint — the structural walk has to reach the leaf.
        let base = json!([{ "session": { "ctx": { "snippet": "rate_limited" } } }]);
        let mutated = json!([{ "session": { "ctx": { "snippet": "fetch_failed" } } }]);
        let empty = Value::Null;
        let fp_base = FleetSnapshot::compute_fingerprint(&base, &empty, &empty);
        let fp_mut = FleetSnapshot::compute_fingerprint(&mutated, &empty, &empty);
        assert_ne!(fp_base, fp_mut, "a nested leaf change must change the hash");
    }

    #[test]
    fn fingerprint_does_not_confuse_string_and_number() {
        // The string "1" and the number 1 serialize differently and must hash
        // differently — the per-node discriminant byte guarantees it.
        let as_str = json!([{ "v": "1" }]);
        let as_num = json!([{ "v": 1 }]);
        let empty = Value::Null;
        assert_ne!(
            FleetSnapshot::compute_fingerprint(&as_str, &empty, &empty),
            FleetSnapshot::compute_fingerprint(&as_num, &empty, &empty),
            "string vs number must not collide",
        );
    }

    #[test]
    fn fingerprint_does_not_confuse_empty_array_and_object() {
        let arr = json!({ "x": [] });
        let obj = json!({ "x": {} });
        let empty = Value::Null;
        assert_ne!(
            FleetSnapshot::compute_fingerprint(&arr, &empty, &empty),
            FleetSnapshot::compute_fingerprint(&obj, &empty, &empty),
            "[] vs {{}} must not collide",
        );
    }
}
