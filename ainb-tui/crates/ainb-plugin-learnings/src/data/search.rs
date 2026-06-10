//! Semantic search: shell `qmd query --json` behind a trait.
//!
//! The Search tab runs the user's query through QMD's hybrid search. The shell
//! call sits behind the [`QmdSearch`] trait so tests inject a captured sample
//! (`tests/fixtures/kb/qmd_query_sample.json`) without spawning a subprocess;
//! [`QmdCli`] is the real runner used at runtime. The wire shape is fixed by
//! `qmd query --json` (a JSON array of `{docid, score, file, title, snippet}`,
//! captured 2026-06-04) and parsed by [`parse_qmd_json`].
//!
//! **Two search modes, one wire shape.** `qmd` exposes two ranked-search paths
//! that emit the SAME `--json` row shape, so both reuse [`parse_qmd_json`]:
//!
//! - [`SearchMode::Bm25`] → `qmd search <q> --json`: BM25 full-text only, NO
//!   LLM, effectively instant. Used for the two-stage "fast paint".
//! - [`SearchMode::Semantic`] → `qmd query <q> --json`: the hybrid
//!   LLM-expansion + rerank path. Higher quality, but cold-slow (~14 s while
//!   `qmd` runs its query-expansion). Swapped in to replace the BM25 paint once
//!   it lands.
//!
//! Crucially, neither call runs on the plugin's dispatch thread — the Search
//! tab fires them on a worker thread and polls for the result, so a slow (or
//! hung) `qmd` never freezes the Learnings pane (see [`crate::ui`] search).
//!
//! **Cancellation.** A superseded or timed-out search must not leave its `qmd`
//! child burning CPU on a result nobody will read (the semantic path's LLM
//! expansion runs ~14 s cold). [`SearchCancel`] is the kill handle: [`QmdCli`]
//! registers each spawned child with it, and the plugin cancels it on
//! supersede / timeout, SIGKILLing the live child. The `*_cancellable` trait
//! methods default-delegate to the plain ones so test fakes (which spawn
//! nothing) stay source-compatible and ignore the handle.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::Deserialize;

use super::error::DataError;

/// A ranked semantic-search hit.
///
/// Maps the `qmd query --json` row to the design's `SearchHit{id, score,
/// title}`, keeping `file` as the locator the Detail pane uses to open the
/// underlying note.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    /// QMD doc id (e.g. `#114073`).
    pub id: String,
    /// Relevance score (higher = more relevant).
    pub score: f64,
    /// Document title.
    pub title: String,
    /// `qmd://...` locator, if present in the row.
    pub file: Option<String>,
}

/// Raw `qmd query --json` row.
#[derive(Debug, Deserialize)]
struct RawHit {
    #[serde(default)]
    docid: String,
    #[serde(default)]
    score: f64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    file: Option<String>,
}

/// Parse a `qmd query --json` payload into ranked [`SearchHit`]s.
///
/// The payload is a JSON array; rows are returned in the order QMD ranked them
/// (it sorts by score, so no re-sort here). Malformed JSON is a typed
/// [`DataError::Json`].
pub fn parse_qmd_json(json: &str) -> Result<Vec<SearchHit>, DataError> {
    let rows: Vec<RawHit> = serde_json::from_str(json).map_err(|e| DataError::Json {
        path: "<qmd output>".into(),
        detail: e.to_string(),
    })?;
    Ok(rows
        .into_iter()
        .map(|r| SearchHit {
            id: r.docid,
            score: r.score,
            title: r.title,
            file: r.file,
        })
        .collect())
}

/// Which `qmd` search path to run. Both emit the same `--json` row shape, so
/// [`parse_qmd_json`] is reusable across both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchMode {
    /// `qmd search <q> --json` — BM25 full-text, NO LLM, effectively instant.
    /// The two-stage "fast paint" first stage.
    Bm25,
    /// `qmd query <q> --json` — hybrid LLM-expansion + rerank. Higher quality
    /// but cold-slow; the default and the second-stage swap-in.
    #[default]
    Semantic,
}

/// Kill handle for ONE in-flight two-stage search lineage.
///
/// Shared between the plugin (which cancels on supersede / timeout / teardown)
/// and the worker's [`QmdCli`] (which registers each spawned `qmd` child so
/// the cancelling side can SIGKILL it). Without this, a timed-out or
/// superseded semantic `qmd query` (LLM expansion, ~14 s cold) keeps running
/// as an orphan whose result is discarded.
///
/// Test fakes never touch it: the [`QmdSearch`] `*_cancellable` methods
/// default-delegate to the plain methods, ignoring the handle — a
/// deterministic fake has nothing to kill.
///
/// The cancelled flag and the child slot live under ONE mutex so the
/// cancel-vs-register race is settled by construction: a child registered
/// after [`Self::cancel`] is killed at registration, and a `cancel` after
/// registration finds the child in the slot and kills it.
#[derive(Debug, Default)]
pub struct SearchCancel {
    inner: Mutex<CancelInner>,
}

/// State guarded by the [`SearchCancel`] mutex.
#[derive(Debug, Default)]
struct CancelInner {
    /// Set once the owning search was superseded / timed out / torn down.
    cancelled: bool,
    /// The live `qmd` child, present while one is running. One child at a
    /// time: the worker registers the BM25 child, reclaims it, then registers
    /// the semantic child.
    child: Option<Child>,
}

impl SearchCancel {
    /// A fresh, un-cancelled handle with no registered child.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` once [`Self::cancel`] has run. The worker checks this between
    /// the BM25 and semantic stages to skip the slow pass entirely when the
    /// query was superseded mid-flight.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// The registered live child's pid, if a `qmd` child is currently running.
    /// Observability seam for the kill-on-timeout / kill-before-spawn tests.
    #[must_use]
    pub fn child_id(&self) -> Option<u32> {
        self.lock().child.as_ref().map(Child::id)
    }

    /// Cancel the search: mark the handle cancelled and SIGKILL + reap the
    /// registered `qmd` child (if one is running). Idempotent. A child
    /// registered AFTER this call is killed at registration, so the kill
    /// cannot be raced by an in-progress spawn.
    pub fn cancel(&self) {
        let child = {
            let mut inner = self.lock();
            inner.cancelled = true;
            inner.child.take()
        };
        if let Some(child) = child {
            kill_and_reap(child);
        }
    }

    /// Register a freshly-spawned child. If the search was already cancelled,
    /// the child is killed + reaped immediately and `Err` is returned so the
    /// caller bails without reading its output.
    fn register(&self, child: Child) -> Result<(), DataError> {
        let mut inner = self.lock();
        if inner.cancelled {
            drop(inner);
            kill_and_reap(child);
            return Err(cancelled_error());
        }
        inner.child = Some(child);
        Ok(())
    }

    /// Reclaim the registered child for reaping (the worker's post-read step).
    /// `None` means the cancel side already killed + reaped it.
    fn take_child(&self) -> Option<Child> {
        self.lock().child.take()
    }

    /// Lock the inner state, recovering from poisoning (a panicked worker must
    /// not wedge the cancel path — the state stays coherent either way).
    fn lock(&self) -> MutexGuard<'_, CancelInner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// SIGKILL + reap a `qmd` child. Errors are deliberately ignored: the child
/// may already have exited, and the goal — "this process is not running and
/// not a zombie" — holds either way.
fn kill_and_reap(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The typed error a cancelled search surfaces. Only ever observed by the
/// worker thread — the plugin has already superseded / timed out the query,
/// so any result carrying this error is dropped by token mismatch.
fn cancelled_error() -> DataError {
    DataError::Subprocess("qmd search cancelled (superseded or timed out)".into())
}

/// Abstraction over the `qmd` shell so search is testable without a subprocess.
///
/// Implementors return the raw `qmd query --json` stdout for a query; the
/// caller ([`search`]) parses it. Tests supply a fake returning a captured
/// sample; production uses [`QmdCli`].
pub trait QmdSearch {
    /// Run `qmd query <query> --json` against `collection` using the sqlite
    /// `index` path, returning raw stdout JSON. The semantic (hybrid) path.
    fn run_query(&self, query: &str, collection: &str, index: &str) -> Result<String, DataError>;

    /// Run the BM25 full-text path (`qmd search <query> --json`). Defaults to
    /// [`Self::run_query`] so existing fakes (which only implement `run_query`)
    /// keep working unchanged — they simply return the same payload for both
    /// modes, which is exactly what a deterministic test fake wants. The real
    /// [`QmdCli`] overrides this to shell the faster `qmd search` sub-command.
    fn run_bm25(&self, query: &str, collection: &str, index: &str) -> Result<String, DataError> {
        self.run_query(query, collection, index)
    }

    /// Cancellable variant of [`Self::run_query`]: same semantics, but the
    /// runner SHOULD register any spawned subprocess with `cancel` (so a
    /// superseded or timed-out search can kill it) and bail early when the
    /// handle is already cancelled. Defaults to the plain [`Self::run_query`],
    /// ignoring the handle, so existing fakes stay source-compatible.
    fn run_query_cancellable(
        &self,
        query: &str,
        collection: &str,
        index: &str,
        cancel: &SearchCancel,
    ) -> Result<String, DataError> {
        let _ = cancel;
        self.run_query(query, collection, index)
    }

    /// Cancellable variant of [`Self::run_bm25`] — see
    /// [`Self::run_query_cancellable`] for the contract; same no-op default.
    fn run_bm25_cancellable(
        &self,
        query: &str,
        collection: &str,
        index: &str,
        cancel: &SearchCancel,
    ) -> Result<String, DataError> {
        let _ = cancel;
        self.run_bm25(query, collection, index)
    }
}

/// Run a search through any [`QmdSearch`] runner and parse the result.
///
/// Uncancellable convenience over [`search_cancellable`] (a throwaway handle
/// nobody cancels): the behaviour every existing call site had. `mode` selects
/// the BM25 fast path vs the semantic rerank path — both parse through
/// [`parse_qmd_json`].
pub fn search(
    runner: &dyn QmdSearch,
    query: &str,
    collection: &str,
    index: &str,
    mode: SearchMode,
) -> Result<Vec<SearchHit>, DataError> {
    search_cancellable(runner, query, collection, index, mode, &SearchCancel::new())
}

/// Run a search through any [`QmdSearch`] runner with a kill handle.
///
/// The entry point the plugin's search worker calls: `cancel` lets a
/// superseded / timed-out search SIGKILL the in-flight `qmd` child instead of
/// orphaning it. Fakes ignore the handle (trait default), so tests are
/// unaffected by which entry point runs.
pub fn search_cancellable(
    runner: &dyn QmdSearch,
    query: &str,
    collection: &str,
    index: &str,
    mode: SearchMode,
    cancel: &SearchCancel,
) -> Result<Vec<SearchHit>, DataError> {
    let raw = match mode {
        SearchMode::Bm25 => runner.run_bm25_cancellable(query, collection, index, cancel),
        SearchMode::Semantic => runner.run_query_cancellable(query, collection, index, cancel),
    }?;
    parse_qmd_json(&raw)
}

/// The real `qmd` runner: shells `qmd query <q> --json -c <collection>`.
///
/// **On `--index`:** QMD's `--index` flag takes a *named* index, not a sqlite
/// path, and passing the configured `qmd_index` path through it triggers a bug
/// in QMD's own `setIndexName` (verified live 2026-06-04). QMD already resolves
/// its index from its own config, so the runner does **not** forward `--index`;
/// the configured `index` value is threaded through the trait for the Search
/// tab's display + future use, but the CLI relies on QMD's default index. The
/// live smoke test exercises this end-to-end.
#[derive(Debug, Default, Clone)]
pub struct QmdCli {
    /// Binary name / path to invoke (defaults to `qmd` on `PATH`).
    binary: Option<String>,
}

impl QmdCli {
    /// Override the `qmd` binary (test seam / non-PATH installs).
    #[must_use]
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: Some(binary.into()),
        }
    }

    fn program(&self) -> &str {
        self.binary.as_deref().unwrap_or("qmd")
    }

    /// Spawn `qmd` with the given pre-built argument list, capturing stdout as
    /// the parsed JSON payload. Shared by the BM25 (`search`) and semantic
    /// (`query`) paths so both surface the same typed subprocess errors.
    ///
    /// Unlike `Command::output()` (which hides the child handle and cannot be
    /// interrupted), this spawns the child and REGISTERS it with `cancel` so a
    /// superseding / timed-out search can SIGKILL it mid-run. The pipes are
    /// drained the same way `output()` would (stderr on a helper thread, so a
    /// chatty child can't fill its stderr buffer and deadlock the stdout
    /// read), and the child is always reaped — by this reader on the happy
    /// path, or by [`SearchCancel::cancel`] on the kill path.
    fn run_args_cancellable(
        &self,
        args: &[&str],
        cancel: &SearchCancel,
    ) -> Result<String, DataError> {
        if cancel.is_cancelled() {
            return Err(cancelled_error());
        }
        let mut child = Command::new(self.program())
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| DataError::Subprocess(format!("spawning {}: {e}", self.program())))?;
        // Detach the pipes BEFORE registering: once the child sits in the
        // cancel slot another thread may kill + reap it at any moment, but
        // these read handles stay valid regardless (EOF on death).
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        cancel.register(child)?;

        let stderr_drain = std::thread::spawn(move || {
            let mut buf = String::new();
            if let Some(mut pipe) = stderr {
                let _ = pipe.read_to_string(&mut buf);
            }
            buf
        });
        let mut out = Vec::new();
        let read_result = match stdout {
            Some(mut pipe) => pipe.read_to_end(&mut out).map(|_| ()),
            None => Ok(()),
        };
        let stderr_text = stderr_drain.join().unwrap_or_default();

        // Reclaim the child for reaping. `None` means the cancel side already
        // killed + reaped it while we were reading (the read ended at EOF).
        let Some(mut child) = cancel.take_child() else {
            return Err(cancelled_error());
        };
        if let Err(e) = read_result {
            kill_and_reap(child);
            return Err(DataError::Subprocess(format!(
                "reading {} stdout: {e}",
                self.program()
            )));
        }
        let status = child
            .wait()
            .map_err(|e| DataError::Subprocess(format!("waiting on {}: {e}", self.program())))?;
        if !status.success() {
            return Err(DataError::Subprocess(format!(
                "qmd exited {status}: {}",
                stderr_text.trim()
            )));
        }
        String::from_utf8(out)
            .map_err(|e| DataError::Subprocess(format!("qmd stdout not utf-8: {e}")))
    }
}

/// Build the semantic-path argv: `query <q> --json [-c <collection>] -C 20`.
///
/// The `-C 20` candidate-limit caps how many BM25 candidates the LLM reranks,
/// trimming the cold rerank time without losing the top results.
fn query_args<'a>(query: &'a str, collection: &'a str) -> Vec<&'a str> {
    let mut args = vec!["query", query, "--json"];
    if !collection.is_empty() {
        args.extend_from_slice(&["-c", collection]);
    }
    args.extend_from_slice(&["-C", "20"]);
    args
}

/// Build the BM25-path argv: `search <q> --json [-c <collection>]` — full-text
/// only, no LLM expansion / rerank, so it returns effectively instantly. Same
/// `--json` row shape as `query`.
fn bm25_args<'a>(query: &'a str, collection: &'a str) -> Vec<&'a str> {
    let mut args = vec!["search", query, "--json"];
    if !collection.is_empty() {
        args.extend_from_slice(&["-c", collection]);
    }
    args
}

impl QmdSearch for QmdCli {
    fn run_query(&self, query: &str, collection: &str, index: &str) -> Result<String, DataError> {
        // The plain path is the cancellable path with a throwaway handle —
        // one spawn/read/reap implementation, no second code path to drift.
        self.run_query_cancellable(query, collection, index, &SearchCancel::new())
    }

    fn run_bm25(&self, query: &str, collection: &str, index: &str) -> Result<String, DataError> {
        self.run_bm25_cancellable(query, collection, index, &SearchCancel::new())
    }

    fn run_query_cancellable(
        &self,
        query: &str,
        collection: &str,
        _index: &str,
        cancel: &SearchCancel,
    ) -> Result<String, DataError> {
        // `_index` is intentionally not forwarded — see the type doc: QMD's
        // `--index` takes a named index (not the configured sqlite path) and a
        // path-valued `--index` trips a bug in QMD itself. QMD resolves its
        // index from its own config.
        self.run_args_cancellable(&query_args(query, collection), cancel)
    }

    fn run_bm25_cancellable(
        &self,
        query: &str,
        collection: &str,
        _index: &str,
        cancel: &SearchCancel,
    ) -> Result<String, DataError> {
        // `_index` unused for the same reason as the semantic path.
        self.run_args_cancellable(&bm25_args(query, collection), cancel)
    }
}

/// Test-support process helpers, shared by the in-module cancellation tests
/// here and the plugin's kill-on-timeout / kill-before-spawn tests
/// (re-exported through `crate::data`). Unix-only by project decision.
#[cfg(test)]
pub(crate) mod test_proc {
    use std::path::{Path, PathBuf};

    /// `true` while `pid` is a live (non-reaped) process — `ps -p` exit
    /// status. A SIGKILLed-and-reaped child fails this; an unreaped zombie
    /// would still pass, which is exactly why the kill path must also `wait`.
    pub(crate) fn pid_alive(pid: u32) -> bool {
        std::process::Command::new("ps")
            .args(["-p", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// Write an executable `#!/bin/sh` script with `body` into `dir` — the
    /// "slow qmd" / "failing qmd" fixtures the cancellation tests shell via
    /// [`QmdCli::with_binary`](super::QmdCli::with_binary).
    pub(crate) fn write_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        let mut perms = std::fs::metadata(&path).expect("stat script").permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod script");
        path
    }
}

#[cfg(test)]
mod tests {
    use super::test_proc::{pid_alive, write_script};
    use super::*;

    #[test]
    fn parse_empty_array_yields_no_hits() {
        assert!(parse_qmd_json("[]").expect("empty array").is_empty());
    }

    #[test]
    fn parse_rejects_non_array() {
        let err = parse_qmd_json("{\"docid\": \"x\"}").expect_err("object is not an array");
        assert!(matches!(err, DataError::Json { .. }));
    }

    #[test]
    fn qmd_cli_program_defaults_to_qmd() {
        assert_eq!(QmdCli::default().program(), "qmd");
        assert_eq!(QmdCli::with_binary("/opt/qmd").program(), "/opt/qmd");
    }

    /// The plain (uncancellable) path still collects stdout end-to-end through
    /// the new spawn/read/reap machinery — the coverage `Command::output()`
    /// used to provide.
    #[test]
    fn qmd_cli_collects_stdout_from_spawned_child() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(dir.path(), "echo-qmd.sh", "echo '[]'");
        let cli = QmdCli::with_binary(script.display().to_string());
        let raw = cli.run_query("q", "col", "idx").expect("script succeeds");
        assert_eq!(raw.trim(), "[]");
        // And the parsed entry point composes over it.
        let hits = search(&cli, "q", "col", "idx", SearchMode::Bm25).expect("parsed");
        assert!(hits.is_empty());
    }

    /// A failing child surfaces its exit status + stderr in the typed error
    /// (the contract the old `output()` path had).
    #[test]
    fn qmd_cli_failing_child_surfaces_stderr() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(dir.path(), "boom-qmd.sh", "echo boom >&2\nexit 3");
        let cli = QmdCli::with_binary(script.display().to_string());
        let err = cli.run_query("q", "", "").expect_err("non-zero exit is an error");
        let msg = err.to_string();
        assert!(msg.contains("boom"), "stderr must be surfaced: {msg}");
    }

    /// `cancel()` on a handle with a live registered child SIGKILLs + reaps it.
    #[test]
    fn cancel_kills_and_reaps_a_registered_child() {
        let child = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleeper");
        let pid = child.id();
        let cancel = SearchCancel::new();
        cancel.register(child).expect("register on a fresh handle");
        assert_eq!(cancel.child_id(), Some(pid));
        assert!(pid_alive(pid), "precondition: sleeper running");

        cancel.cancel();
        assert!(cancel.is_cancelled());
        assert_eq!(cancel.child_id(), None, "slot cleared by cancel");
        assert!(!pid_alive(pid), "child must be SIGKILLed + reaped");
    }

    /// Registering AFTER `cancel()` kills the child immediately — the
    /// cancel-vs-spawn race resolves to "dead child" whichever side wins.
    #[test]
    fn register_after_cancel_kills_the_child_immediately() {
        let cancel = SearchCancel::new();
        cancel.cancel();

        let child = Command::new("/bin/sh")
            .args(["-c", "sleep 30"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleeper");
        let pid = child.id();
        let err = cancel.register(child).expect_err("register on a cancelled handle");
        assert!(matches!(err, DataError::Subprocess(_)));
        assert!(!pid_alive(pid), "late-registered child must be killed");
    }

    /// A pre-cancelled handle short-circuits the runner before any spawn.
    #[test]
    fn cancelled_handle_short_circuits_before_spawn() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = write_script(dir.path(), "never-qmd.sh", "sleep 30");
        let cli = QmdCli::with_binary(script.display().to_string());
        let cancel = SearchCancel::new();
        cancel.cancel();
        let err = cli
            .run_query_cancellable("q", "", "", &cancel)
            .expect_err("cancelled before spawn");
        assert!(matches!(err, DataError::Subprocess(_)));
        assert_eq!(cancel.child_id(), None, "nothing was spawned");
    }
}
