//! Dashboard data sources.
//!
//! The dashboard never re-implements data access — it drives the *same*
//! `ainb --format json …` commands the CLI and TUI expose, so the browser view
//! can never drift from the terminal view. [`DataSource`] abstracts that so
//! tests can inject a deterministic fake instead of spawning subprocesses.

use std::ffi::OsString;
use std::future::Future;
use std::pin::Pin;

use serde_json::Value;

/// A `'static` boxed future, the return shape of [`DataSource::snapshot`].
/// Keeping the trait boxed-future (rather than `async fn`) makes it
/// object-safe so the router can hold a `dyn DataSource`.
pub type SnapshotFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FleetSnapshot, DataError>> + Send + 'a>>;

/// The sessions + needs pair fetched on the fast poll cadence (cost excluded).
/// Returned by [`DataSource::core`].
#[derive(Debug, Clone)]
pub struct CoreSnapshot {
    /// `ainb --format json list` — the live session list.
    pub sessions: Value,
    /// The daemon `attention/list` inbox mapped to ASK/ERR/WAIT cards (D18).
    pub needs: Value,
}

/// A `'static` boxed future, the return shape of [`DataSource::core`].
pub type CoreFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CoreSnapshot, DataError>> + Send + 'a>>;

/// A `'static` boxed future, the return shape of [`DataSource::cost`].
pub type CostFuture<'a> = Pin<Box<dyn Future<Output = Value> + Send + 'a>>;

/// A snapshot of everything the dashboard renders, as opaque JSON values
/// proxied straight from the underlying `ainb` commands. Keeping these as
/// [`Value`] (rather than re-deriving the CLI's structs) means new fields the
/// CLI adds flow through to the frontend with no code change here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FleetSnapshot {
    /// `ainb --format json list` — the live session list.
    pub sessions: Value,
    /// The daemon `attention/list` inbox mapped to ASK/ERR/WAIT cards (D18).
    /// Each card carries `attentionId` so an ASK can be answered via
    /// `POST /api/answer`.
    pub needs: Value,
    /// `ainb --format json fleet cost` — cost rollups. `null` when the verb is
    /// absent from this build (cost-surface not yet merged) so the dashboard
    /// degrades gracefully instead of failing.
    pub cost: Value,
    /// Content fingerprint, used by the SSE layer to suppress duplicate pushes
    /// when nothing changed. Skipped from the API payload — it's internal.
    #[serde(skip)]
    pub fingerprint: u64,
}

impl FleetSnapshot {
    /// Assemble a full snapshot from a fast-cadence [`CoreSnapshot`] and a
    /// (possibly stale) cost value, recomputing the fingerprint. Used by the
    /// poller to stitch fresh sessions/needs onto the last-known cost on ticks
    /// that skip the slow cost fetch.
    #[must_use]
    pub fn from_parts(core: CoreSnapshot, cost: Value) -> Self {
        let fingerprint = Self::compute_fingerprint(&core.sessions, &core.needs, &cost);
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
/// data rolls up slowly. [`snapshot`](DataSource::snapshot) composes both for
/// the cold-cache fallback path.
pub trait DataSource: Send + Sync + 'static {
    /// Build a fresh full snapshot of the current fleet state (sessions, needs,
    /// and cost). Used only on a cold cache before the poller's first tick.
    fn snapshot(&self) -> SnapshotFuture<'_>;

    /// Fetch only the fast-cadence surfaces (sessions + needs). These are cheap
    /// relative to cost and need ~2s freshness, so the poller refreshes them on
    /// every tick.
    fn core(&self) -> CoreFuture<'_>;

    /// Fetch the slow-cadence cost rollup, best-effort: any failure or absent
    /// verb resolves to [`Value::Null`] so the dashboard degrades gracefully.
    /// The poller calls this only every Nth tick.
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
            return Err(DataError::CommandFailed {
                verb,
                detail: if stderr.is_empty() {
                    format!("exited with {}", output.status)
                } else {
                    stderr
                },
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

/// Fetch the open attention inbox from the daemon (`attention/list`, fleet-wide)
/// and map it to the dashboard's `needs` cards (D18). Best-effort: a
/// down / unreachable daemon (or a token that hasn't been minted yet) degrades
/// to an empty list so the dashboard still renders sessions instead of failing
/// the whole poll. This is the read half of the web-on-the-bus retarget — the
/// old `ainb fleet needs` subprocess (which cold-booted a plugin runtime and
/// capture-paned every session) is gone.
async fn daemon_needs() -> Value {
    match crate::daemon::DaemonClient::from_env() {
        Ok(client) => match client.attention_list_fleet().await {
            Ok(rows) => crate::daemon::attention_to_needs(&rows),
            Err(e) => {
                tracing::debug!(error = %e, "attention/list unavailable; needs degrades to empty");
                Value::Array(Vec::new())
            }
        },
        Err(e) => {
            tracing::debug!(error = %e, "daemon client unavailable; needs degrades to empty");
            Value::Array(Vec::new())
        }
    }
}

impl DataSource for AinbCliSource {
    fn snapshot(&self) -> SnapshotFuture<'_> {
        Box::pin(async move {
            // Required surfaces fail loudly; cost is best-effort.
            let core = self.core().await?;
            let cost = self.cost().await;
            Ok(FleetSnapshot::from_parts(core, cost))
        })
    }

    fn core(&self) -> CoreFuture<'_> {
        Box::pin(async move {
            // Sessions still come from `ainb list` (the daemon exposes no
            // host-session snapshot RPC). Needs now read the daemon's attention
            // inbox instead of the old `ainb fleet needs` capture-pane poll (D18).
            let sessions = self.run_json(&["list"], false).await?;
            let needs = daemon_needs().await;
            Ok(CoreSnapshot { sessions, needs })
        })
    }

    fn cost(&self) -> CostFuture<'_> {
        Box::pin(async move {
            // Cost is best-effort: `run_json(.., allow_absent=true)` already
            // resolves spawn errors / non-zero exits to `Value::Null`, so any
            // residual error here also degrades to `Null` rather than failing.
            self.run_json(&["fleet", "cost"], true).await.unwrap_or(Value::Null)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
