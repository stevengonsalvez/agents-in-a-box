//! Beads CLI adapter (P2.2).
//!
//! Hangar mirrors issues into Stevie's existing [beads] tracker by shelling out
//! to the `bd` binary and parsing its `--json` output — there is no library
//! link, so the adapter survives `bd` version bumps as long as the JSON shape
//! holds. [`BdClient`] is the single entry point; it owns the resolved binary
//! path, the `BEADS_DIR` it always passes explicitly, and an O_EXCL pidfile
//! [`lock::BdLock`] that serialises concurrent writes against the same beads
//! root (`bd` occasionally races when two writes hit the same git head).
//!
//! # `BEADS_DIR` discipline
//!
//! Every invocation sets `BEADS_DIR` from [`BdClient::beads_dir`] — the adapter
//! **never** relies on the caller's `pwd`. This matters in a git worktree, where
//! `bd` cannot infer the beads root from the cwd (see the project's
//! `reference_beads_worktree` note). Construct the client with an explicit,
//! absolute `BEADS_DIR`.
//!
//! [beads]: https://github.com/steveyegge/beads

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::Deserialize;

pub mod cli;
#[cfg(any(test, feature = "test-support"))]
pub mod fake_bd;
pub mod lock;

use lock::BdLock;

/// A beads issue id (`bd`'s opaque string PK, e.g. `agents-in-a-box-174.3.2`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BdId(String);

impl BdId {
    /// Borrow the inner id string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for BdId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for BdId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl std::fmt::Display for BdId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A beads issue lifecycle status.
///
/// Mirrors the `status` strings `bd` emits. Anything the adapter does not
/// recognise maps to [`BdStatus::Other`] (carrying the raw token) rather than
/// failing the parse — `bd` may grow new states the sync layer treats as
/// advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BdStatus {
    /// `"open"` — newly created, not yet started.
    Open,
    /// `"in_progress"` — actively being worked.
    InProgress,
    /// `"blocked"` — waiting on a dependency.
    Blocked,
    /// `"closed"` — terminal; maps to Hangar `done`.
    Closed,
    /// Any other status token `bd` reported, kept verbatim.
    Other(String),
}

impl BdStatus {
    /// Parse a `bd` status token, never failing (unknown → [`BdStatus::Other`]).
    fn parse(s: &str) -> Self {
        match s {
            "open" => Self::Open,
            "in_progress" => Self::InProgress,
            "blocked" => Self::Blocked,
            "closed" => Self::Closed,
            other => Self::Other(other.to_string()),
        }
    }
}

/// One beads issue as parsed from `bd ... --json`.
///
/// Only the fields the sync layer needs are kept; `bd`'s JSON carries many more
/// (priority, owner, dependencies, …) which `serde` discards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BdIssue {
    /// The issue's beads id.
    pub id: BdId,
    /// Human-readable title.
    pub title: String,
    /// Lifecycle status.
    pub status: BdStatus,
    /// Assignee string, or `None` when unassigned.
    pub assignee: Option<String>,
    /// Labels attached to the issue.
    pub labels: Vec<String>,
    /// Last-modified timestamp reported by `bd`.
    pub updated_at: DateTime<Utc>,
}

/// The raw JSON object `bd` emits per issue (a superset; extra fields ignored).
///
/// Field notes (verified against `bd 0.49.0`):
/// - `assignee` is present **only** when the issue was created/updated with
///   `--assignee`; it carries the crosswalk value (e.g. `agent:ag_x`) the
///   sync layer round-trips. It is distinct from `owner` (the bd account that
///   authored the row, e.g. an email) which the adapter deliberately ignores —
///   `owner` is not the polymorphic-assignee field.
/// - `labels` is absent from `bd create`'s object response (only `list`/`show`/
///   `close` echo labels back), hence `#[serde(default)]`.
#[derive(Debug, Deserialize)]
struct BdJsonIssue {
    id: String,
    title: String,
    status: String,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    labels: Vec<String>,
    updated_at: DateTime<Utc>,
}

/// `bd`'s `--json` payload, which is shaped per verb: the single-issue write
/// verbs (`create`) emit a bare object, while the read/collection verbs
/// (`list`/`show`) — and `close` — emit an array. This untagged envelope
/// accepts either so a verb's shape is decoded without the call site guessing
/// wrong (verified against `bd 0.49.0`: `create` is an object, the rest are
/// arrays).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum BdJsonEnvelope {
    /// A bare object (e.g. `bd create`).
    One(BdJsonIssue),
    /// An array of objects (e.g. `bd list`/`show`/`close`).
    Many(Vec<BdJsonIssue>),
}

impl BdJsonEnvelope {
    /// Flatten either shape into a list of issues.
    fn into_issues(self) -> Vec<BdIssue> {
        match self {
            Self::One(j) => vec![j.into()],
            Self::Many(js) => js.into_iter().map(BdIssue::from).collect(),
        }
    }
}

impl From<BdJsonIssue> for BdIssue {
    fn from(j: BdJsonIssue) -> Self {
        Self {
            id: BdId(j.id),
            title: j.title,
            status: BdStatus::parse(&j.status),
            // `bd` emits `""` for unassigned in some versions; normalise to None.
            assignee: j.assignee.filter(|a| !a.is_empty()),
            labels: j.labels,
            updated_at: j.updated_at,
        }
    }
}

/// Arguments for [`BdClient::create`].
#[derive(Debug, Clone)]
pub struct BdCreateArgs {
    /// Issue title (required).
    pub title: String,
    /// Optional long-form description.
    pub description: Option<String>,
    /// Labels to attach (comma-joined into a single `--labels foo,bar` flag).
    pub labels: Vec<String>,
    /// Optional assignee string (already crosswalked by the caller).
    pub assignee: Option<String>,
}

/// Filter for [`BdClient::list`].
///
/// `bd list -l <label>` per label, optionally bounded by `--since`. An empty
/// filter lists everything `bd` returns by default.
///
/// # Closed issues
///
/// `bd list` **excludes closed issues by default** (verified against
/// `bd 0.49.0`): a plain `bd list -l hangar-v1` after a `bd close` returns `[]`.
/// The sync layer must therefore set [`include_closed`](Self::include_closed)
/// whenever the *closure itself* is the signal it is looking for — namely the
/// inbound poll (P2.4) and the reconcile pass (P2.5), which detect a `bd`-side
/// close and drag the Hangar issue to `done`. Without `--all` a closure is
/// invisible and the round-trip silently never completes.
#[derive(Debug, Clone, Default)]
pub struct BdListFilter {
    /// Restrict to issues carrying every one of these labels.
    pub labels: Vec<String>,
    /// Only issues updated at/after this instant (advisory; `bd`-side filter).
    pub since: Option<DateTime<Utc>>,
    /// Include closed issues in the result (emits `--all`).
    ///
    /// `false` mirrors `bd`'s default (open issues only). Set `true` when the
    /// caller must observe closures — the inbound/reconcile sync paths.
    pub include_closed: bool,
}

/// Failure modes of a `bd` adapter call.
#[derive(Debug, thiserror::Error)]
pub enum BdError {
    /// The `bd` binary could not be resolved on PATH or at the given path.
    #[error("bd binary not found: {0}")]
    BdNotInstalled(String),
    /// A `show` targeted an id `bd` does not know about (empty array result).
    #[error("bd issue not found: {0}")]
    NotFound(BdId),
    /// A single-issue write verb (`create`/`close`) returned no row at all —
    /// not a lookup miss, but a malformed/empty `bd` response. Carries the verb
    /// for context so logs don't surface a misleading blank "not found".
    #[error("bd {0} returned an empty result")]
    EmptyResult(&'static str),
    /// The cross-process pidfile lock could not be acquired in time.
    #[error("timed out acquiring bd lock at {0}")]
    LockTimeout(PathBuf),
    /// `bd` exited non-zero; carries the exit code (if any) and captured stderr.
    #[error("bd exited with {exit:?}: {stderr}")]
    Cli {
        /// Process exit code, or `None` if killed by signal.
        exit: Option<i32>,
        /// Captured standard error.
        stderr: String,
    },
    /// `bd`'s stdout was not the JSON envelope the adapter expects.
    #[error("failed to parse bd json: {0}")]
    Parse(#[from] serde_json::Error),
    /// Spawning the process itself failed (distinct from a non-zero exit).
    #[error("failed to spawn bd: {0}")]
    Spawn(#[source] std::io::Error),
}

/// Adapter over the `bd` CLI.
///
/// Construct once with [`BdClient::new`] (which validates the binary exists and
/// fails fast with [`BdError::BdNotInstalled`] otherwise), then call
/// [`create`](Self::create) / [`close`](Self::close) / [`list`](Self::list) /
/// [`show`](Self::show). Every call sets `BEADS_DIR` explicitly and passes
/// `--json`.
///
/// # Worktree callers
///
/// `bd` cannot infer its data root from the cwd inside a git worktree, so
/// `beads_dir` **must** be an explicit absolute path — do not rely on the
/// process's working directory (see the module docs).
#[derive(Debug)]
pub struct BdClient {
    bin: PathBuf,
    beads_dir: PathBuf,
    lock: BdLock,
}

impl BdClient {
    /// Resolve `bin` and bind the client to `beads_dir`.
    ///
    /// `bin` may be a bare name (resolved on PATH via the `which` crate) or an
    /// explicit path. The `O_EXCL` lock lives at `{beads_dir}/.hangar-bd.lock`.
    ///
    /// # Errors
    ///
    /// Returns [`BdError::BdNotInstalled`] when `bin` cannot be resolved.
    pub fn new(bin: impl AsRef<std::path::Path>, beads_dir: PathBuf) -> Result<Self, BdError> {
        let bin = bin.as_ref();
        let resolved = which::which(bin)
            .map_err(|e| BdError::BdNotInstalled(format!("{}: {e}", bin.display())))?;
        let lock = BdLock::new(beads_dir.join(".hangar-bd.lock"));
        Ok(Self {
            bin: resolved,
            beads_dir,
            lock,
        })
    }

    /// The `BEADS_DIR` this client passes to every `bd` invocation.
    #[must_use]
    pub fn beads_dir(&self) -> &std::path::Path {
        &self.beads_dir
    }

    /// Create a new beads issue, returning the created [`BdIssue`].
    ///
    /// # Errors
    ///
    /// [`BdError::Cli`] on non-zero exit, [`BdError::Parse`] on bad JSON,
    /// [`BdError::NotFound`] if `bd` returns no row, or [`BdError::LockTimeout`].
    pub fn create(&self, args: BdCreateArgs) -> Result<BdIssue, BdError> {
        let mut cmd = cli::build_create(self.cmd(), args);
        let issues = self.run_json(&mut cmd)?;
        first_or(issues, BdError::EmptyResult("create"))
    }

    /// Close an issue (optionally with a reason), returning the updated issue.
    ///
    /// # Errors
    ///
    /// As [`create`](Self::create).
    pub fn close(&self, id: &BdId, reason: Option<&str>) -> Result<BdIssue, BdError> {
        let mut cmd = cli::build_close(self.cmd(), id, reason);
        let issues = self.run_json(&mut cmd)?;
        first_or(issues, BdError::EmptyResult("close"))
    }

    /// List issues matching `filter`.
    ///
    /// # Errors
    ///
    /// [`BdError::Cli`] / [`BdError::Parse`] / [`BdError::LockTimeout`]. Unlike
    /// the single-issue verbs, an empty result is a valid empty `Vec`.
    pub fn list(&self, filter: BdListFilter) -> Result<Vec<BdIssue>, BdError> {
        let mut cmd = cli::build_list(self.cmd(), filter);
        self.run_json(&mut cmd)
    }

    /// Show a single issue by id.
    ///
    /// # Errors
    ///
    /// [`BdError::NotFound`] when `bd` returns an empty array, plus the usual
    /// [`BdError::Cli`] / [`BdError::Parse`] / [`BdError::LockTimeout`].
    pub fn show(&self, id: &BdId) -> Result<BdIssue, BdError> {
        let mut cmd = cli::build_show(self.cmd(), id);
        let issues = self.run_json(&mut cmd)?;
        first_or(issues, BdError::NotFound(id.clone()))
    }

    /// Base command with the binary, `BEADS_DIR`, and `--json` pre-set.
    fn cmd(&self) -> std::process::Command {
        cli::base_cmd(&self.bin, &self.beads_dir)
    }

    /// Run `cmd` under the cross-process lock, classify failures, parse the
    /// JSON envelope (object *or* array — see [`BdJsonEnvelope`]) into
    /// `Vec<BdIssue>`. Single-issue verbs map the head; `list` keeps all.
    fn run_json(&self, cmd: &mut std::process::Command) -> Result<Vec<BdIssue>, BdError> {
        let _guard = self.lock.acquire()?;
        let output = cmd.output().map_err(BdError::Spawn)?;
        if !output.status.success() {
            return Err(BdError::Cli {
                exit: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            });
        }
        let envelope: BdJsonEnvelope = serde_json::from_slice(&output.stdout)?;
        Ok(envelope.into_issues())
    }
}

/// Take the head issue of a single-issue verb's result, or the verb-specific
/// `empty` error (`NotFound` for `show`, `EmptyResult` for `create`/`close`).
fn first_or(issues: Vec<BdIssue>, empty: BdError) -> Result<BdIssue, BdError> {
    issues.into_iter().next().ok_or(empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parse_maps_known_and_unknown() {
        assert_eq!(BdStatus::parse("open"), BdStatus::Open);
        assert_eq!(BdStatus::parse("in_progress"), BdStatus::InProgress);
        assert_eq!(BdStatus::parse("blocked"), BdStatus::Blocked);
        assert_eq!(BdStatus::parse("closed"), BdStatus::Closed);
        assert_eq!(
            BdStatus::parse("triaged"),
            BdStatus::Other("triaged".to_string())
        );
    }

    #[test]
    fn empty_assignee_normalises_to_none() {
        let json = fake_bd::one_issue_object("id-1", "t", "open")
            .replace(r#""assignee":"stevie""#, r#""assignee":"""#);
        let parsed: BdJsonIssue = serde_json::from_str(&json).expect("parse");
        let issue: BdIssue = parsed.into();
        assert_eq!(issue.assignee, None);
    }

    #[test]
    fn envelope_accepts_bare_object_and_array() {
        // `bd create` shape: a bare object.
        let obj = fake_bd::one_issue_object("c-1", "t", "open");
        let one: BdJsonEnvelope = serde_json::from_str(&obj).expect("parse object");
        let issues = one.into_issues();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].id, BdId::from("c-1"));

        // `bd list`/`show`/`close` shape: an array.
        let arr = fake_bd::one_issue_array("l-1", "t", "open");
        let many: BdJsonEnvelope = serde_json::from_str(&arr).expect("parse array");
        assert_eq!(many.into_issues().len(), 1);
    }

    #[test]
    fn create_object_without_labels_defaults_to_empty() {
        // The real `bd create` object carries no `labels` field.
        let obj = fake_bd::one_issue_object("c-2", "t", "open");
        assert!(!obj.contains("labels"), "fixture must omit labels: {obj}");
        let parsed: BdJsonIssue = serde_json::from_str(&obj).expect("parse");
        let issue: BdIssue = parsed.into();
        assert!(issue.labels.is_empty());
    }

    #[test]
    fn create_empty_result_is_empty_result_error() {
        let err = first_or(vec![], BdError::EmptyResult("create")).expect_err("empty");
        match err {
            BdError::EmptyResult(verb) => assert_eq!(verb, "create"),
            other => panic!("expected EmptyResult, got {other:?}"),
        }
    }

    #[test]
    fn fake_bd_happy_drives_create_through_lock() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let beads = tempfile::TempDir::new().expect("beads");
        let bin = fake_bd::happy(tmp.path(), "abc-9", "shared fixture", "open");
        let client = BdClient::new(bin, beads.path().to_path_buf()).expect("client");

        let issue = client
            .create(BdCreateArgs {
                title: "shared fixture".into(),
                description: None,
                labels: vec![],
                assignee: None,
            })
            .expect("create ok");
        assert_eq!(issue.id, BdId::from("abc-9"));
        assert_eq!(issue.status, BdStatus::Open);
    }
}
