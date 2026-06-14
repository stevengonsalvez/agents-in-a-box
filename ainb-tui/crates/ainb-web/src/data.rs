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
    /// `ainb --format json fleet needs` — ASK/ERR/IDLE/WAIT cards.
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
    /// `ainb --format json fleet needs` — ASK/ERR/IDLE/WAIT cards.
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
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // serde_json::Value isn't Hash; hash its compact string form.
        sessions.to_string().hash(&mut h);
        needs.to_string().hash(&mut h);
        cost.to_string().hash(&mut h);
        h.finish()
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
            // Required surfaces fail loudly.
            let sessions = self.run_json(&["list"], false).await?;
            let needs = self.run_json(&["fleet", "needs"], false).await?;
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
}
