// ABOUTME: Interactive session manager for host-based Docker-free sessions
//
// Manages the lifecycle of Interactive mode sessions which run directly on the host:
// - Creates git worktrees for branch isolation
// - Starts tmux sessions for terminal multiplexing
// - Runs claude CLI directly on the host
// - Discovers existing sessions by scanning tmux
//
// This manager is completely independent of Docker and ContainerManager,
// enabling lightweight, fast development workflows.

#![allow(dead_code)]

use crate::audit::{self, AuditResult, AuditTrigger};
use crate::git::WorktreeManager;
use crate::models::{
    CodexModel, Session, SessionAgentType, SessionMode, SessionStatus, is_default_model,
};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::process::Command;
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Error, Debug)]
pub enum InteractiveSessionError {
    #[error("Worktree error: {0}")]
    Worktree(#[from] crate::git::WorktreeError),

    #[error("Tmux error: {0}")]
    Tmux(String),

    #[error("Session not found: {0}")]
    SessionNotFound(Uuid),

    #[error("Session already exists: {0}")]
    SessionAlreadyExists(Uuid),

    #[error("Invalid session state: {0}")]
    InvalidState(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Error: {0}")]
    Other(#[from] anyhow::Error),
}

/// Represents an active Interactive mode session
#[derive(Debug, Clone)]
pub struct InteractiveSession {
    pub session_id: Uuid,
    pub worktree_path: PathBuf,
    pub source_repository: PathBuf, // The original git repository path
    pub tmux_session_name: String,
    pub branch_name: String,
    pub workspace_name: String,
    pub created_at: DateTime<Utc>,
    pub agent_type: SessionAgentType, // The AI agent or shell for this session
    pub skip_permissions: bool,       // Provider-specific dangerous/yolo launch flag
    pub model: Option<String>,        // Raw provider model ID passed through to the CLI
    pub headroom_enabled: bool,       // Route this session's CLI through the local Headroom proxy
    pub rtk_enabled: bool,            // RTK PreToolUse hook wired in session's worktree
    pub codex_thread_id: Option<String>, // Exact shared app-server thread for Codex sessions
}

/// How the persisted model value should be interpreted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelSource {
    /// Metadata written before provider model IDs were stored verbatim.
    #[default]
    LegacyTyped,
    /// Provider model ID supplied by the user and passed through unchanged.
    Raw,
}

/// Persisted session metadata for discovery across restarts.
///
/// This solves the branch-mismatch problem: when a user changes branches in a worktree,
/// the old tmux session name no longer matches the current branch. By persisting the
/// mapping between session_id, tmux_session_name, and worktree_path, we can reliably
/// rediscover sessions even after branch changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub session_id: Uuid,
    pub tmux_session_name: String,
    pub worktree_path: PathBuf,
    pub workspace_name: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub agent_type: SessionAgentType,
    #[serde(default)]
    pub headroom_enabled: bool,
    #[serde(default)]
    pub rtk_enabled: bool,
    /// Persisted launch settings so a Stopped session resumes with the SAME
    /// flags it was created with. `skip_permissions` is `Option`: `None`
    /// (legacy metadata written before this field existed) → default to yolo
    /// (`--dangerously-skip-permissions`) on resume; `Some(v)` preserves the
    /// exact value the session was started with.
    #[serde(default)]
    pub skip_permissions: Option<bool>,
    #[serde(default)]
    pub model: Option<String>,
    /// Distinguishes legacy typed model names from raw provider model IDs.
    #[serde(default)]
    pub model_source: ModelSource,
    /// Legacy Codex-only field. New writes use provider-agnostic `model`.
    #[serde(default)]
    pub codex_model: Option<CodexModel>,
    /// Exact daemon-owned remote thread. Missing legacy values migrate lazily.
    #[serde(default)]
    pub codex_thread_id: Option<String>,
}

/// Whether shared Codex remote control can work in this process's hangar home.
///
/// `ensure_hangar_daemon` never spawns a daemon into an ephemeral home, and
/// reports that rather than failing, because its other caller (the TUI) is right
/// to ignore it. Here the daemon is connected to in the very next statement, so
/// the skip has to be acted on: ignored, it arrives as a socket-level connect
/// error with nothing in it about the home.
///
/// Acting on it means DEGRADING, not failing. An ephemeral home costs the
/// session exactly one feature, the shared remote thread; a plain `codex` in
/// the pane is what a session with the feature switched off already runs.
/// Failing instead turned `ainb run --tool codex` under an isolated `$HOME`
/// from a working launch into a hard error, and the error path then deleted the
/// worktree the run had just created. That is worse than the leak it prevents.
///
/// [`crate::cli::hangar::DaemonAutostart::Failed`] deliberately passes through.
/// A failed spawn is already logged and may still have left a usable daemon
/// (another process's, on a durable home); only the ephemeral-home skip is a
/// promise that no daemon will EVER appear for this home.
///
/// `home` is a closure so the path is resolved only on the degrading path.
fn shared_remote_control_available(
    outcome: crate::cli::hangar::DaemonAutostart,
    home: impl FnOnce() -> String,
) -> bool {
    if outcome == crate::cli::hangar::DaemonAutostart::SkippedEphemeralHome {
        warn!("{}", ephemeral_hangar_home_warning(&home()));
        return false;
    }
    true
}

/// What the user is told when an ephemeral home costs them shared remote
/// control, built here rather than inline in the `warn!` so the sentence that
/// names the remedy is a value a test can read.
fn ephemeral_hangar_home_warning(home: &str) -> String {
    format!(
        "hangar home {home} is ephemeral; set AINB_HANGAR_HOME to a durable path to use \
         Codex shared remote control - launching this session without it"
    )
}

/// The hangar home this process resolved, for the message above.
fn resolved_hangar_home() -> String {
    ainb_hangar_daemon::hangar_dir().map_or_else(
        |_| "<unresolved>".to_string(),
        |home| home.display().to_string(),
    )
}

/// Create or resume one exact shared Codex app-server thread for Interactive.
///
/// `Ok(None)` is a successful launch WITHOUT shared remote control: the reason
/// has already been warned about, and the session runs the provider CLI
/// directly, which is the same path a non-Codex session and a Codex session
/// with the feature disabled take. Callers must not treat it as a failure, and
/// in particular must not roll back a worktree over it.
pub(crate) async fn ensure_codex_remote_thread(
    session_id: Uuid,
    cwd: &std::path::Path,
    model: Option<&str>,
    skip_permissions: bool,
    headroom_enabled: bool,
    existing_thread_id: Option<String>,
) -> anyhow::Result<Option<ainb_hangar_proto::fleet::CodexSessionEnsureResult>> {
    if headroom_enabled {
        anyhow::bail!(
            "Codex Headroom is unavailable with shared remote control; disable Headroom for this session"
        );
    }
    ensure_codex_remote_thread_with(
        // Ephemeral: `ainb run` prints its summary and exits, while the daemon
        // has to keep serving the tmux session it just created.
        || {
            crate::cli::hangar::ensure_hangar_daemon(
                crate::cli::hangar::LauncherLifetime::Ephemeral,
            )
        },
        resolved_hangar_home,
        ainb_hangar_proto::fleet::CodexSessionEnsureParams {
            session_id: session_id.to_string(),
            cwd: cwd.display().to_string(),
            model: model.map(str::to_owned),
            thread_id: existing_thread_id,
            skip_permissions,
        },
    )
    .await
}

/// [`ensure_codex_remote_thread`] with its two impure inputs passed in: the
/// autostart outcome and the home to name when that outcome is a skip.
///
/// The seam is what makes the degrade testable at all. The ephemeral branch
/// returns BEFORE the daemon connect, and that ordering is the whole claim:
/// checking [`shared_remote_control_available`] on its own would leave "and
/// then the launch still succeeds" untested, which is how a degrade that is
/// computed and then ignored still passes its suite.
async fn ensure_codex_remote_thread_with(
    autostart: impl FnOnce() -> crate::cli::hangar::DaemonAutostart,
    home: impl FnOnce() -> String,
    params: ainb_hangar_proto::fleet::CodexSessionEnsureParams,
) -> anyhow::Result<Option<ainb_hangar_proto::fleet::CodexSessionEnsureResult>> {
    if !shared_remote_control_available(autostart(), home) {
        return Ok(None);
    }
    let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
        .map_err(|error| anyhow::anyhow!("connect to Ainb Codex runtime: {error}"))?;
    client.codex_session_ensure(params).await.map(Some).map_err(|error| {
        let message = format_codex_remote_control_failure(&error.to_string());
        warn!(error = %error, "{message}");
        anyhow::Error::msg(error.to_string()).context(message)
    })
}

/// Short actionable TUI message for a shared Codex bridge failure.
///
/// The detailed transport error remains in daemon and client logs. Showing it in
/// the three-line notification hid the user action behind nested RPC context.
fn format_codex_remote_control_failure(cause: &str) -> &'static str {
    if cause.contains("WebSocket handshake")
        || cause.contains("Handshake not finished")
        || cause.contains("invalid token")
    {
        "Codex bridge conflict. Restart Ainb, then retry."
    } else if cause.contains("timed out") || cause.contains("still starting") {
        "Codex bridge starting. Retry session in 5 seconds."
    } else if cause.contains("not found") || cause.contains("cannot generate app-server schema") {
        "Codex unavailable. Update Codex, then retry."
    } else {
        "Codex remote control unavailable. Check Hangar logs, then retry."
    }
}

/// Wait briefly for the freshly started remote terminal to publish its exact
/// thread identity through the daemon's app-server event stream.
///
/// `Ok(None)` carries the same meaning as in [`ensure_codex_remote_thread`]:
/// shared remote control is unavailable and the session runs without it.
pub(crate) async fn claim_codex_remote_thread(
    session_id: Uuid,
    cwd: &std::path::Path,
    model: Option<&str>,
    skip_permissions: bool,
    headroom_enabled: bool,
) -> anyhow::Result<Option<ainb_hangar_proto::fleet::CodexSessionEnsureResult>> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let Some(remote) = ensure_codex_remote_thread(
            session_id,
            cwd,
            model,
            skip_permissions,
            headroom_enabled,
            None,
        )
        .await?
        else {
            // Shared remote control is unavailable, so there is no thread to
            // wait for. Report that once rather than spending the 10s deadline
            // re-asking a question whose answer cannot change.
            return Ok(None);
        };
        if remote.thread_id.is_some() {
            return Ok(Some(remote));
        }
        if tokio::time::Instant::now() >= deadline {
            anyhow::bail!("Codex started but did not publish a remote thread within 10 seconds");
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// Archive and forget a remote thread created by a failed fresh launch.
pub(crate) async fn discard_codex_remote_thread(session_id: Uuid) -> anyhow::Result<()> {
    let client = crate::fleet::bridge::daemon::DaemonClient::from_env()
        .map_err(|error| anyhow::anyhow!("connect to Ainb Codex runtime: {error}"))?;
    client
        .codex_session_discard(ainb_hangar_proto::fleet::CodexSessionDiscardParams {
            session_id: session_id.to_string(),
        })
        .await
        .map_err(|error| anyhow::anyhow!("discard failed Codex thread: {error}"))?;
    Ok(())
}

/// Build the argv for a managed (remote app-server) Codex session.
///
/// Extracted from the launcher so the flags that keep breaking are testable
/// without spawning tmux. Every element here is load-bearing:
///
/// * `-C <working_dir>` — under `--remote` the TUI ignores the pane cwd and
///   adopts the app-server's, so without this every session runs in the
///   daemon's tree rather than its own worktree.
/// * `--dangerously-bypass-hook-trust` — Ainb's own `notifyd install` rewrites
///   `~/.codex/hooks.json`, invalidating Codex's positional hook hashes and
///   parking the launch on a "Hooks need review" modal.
/// * `resume <thread_id>` — joins the thread Ainb already started, so the CLI
///   and any other client on that app-server (the phone) share ONE
///   conversation. Without it each client starts its own thread on the same
///   cwd and neither sees the other's turns.
pub(crate) fn codex_remote_command(
    provider: &crate::config::CliProvider,
    remote: &ainb_hangar_proto::fleet::CodexSessionEnsureResult,
    working_dir: &std::path::Path,
    model: Option<&str>,
    skip_permissions: bool,
) -> Vec<String> {
    let mut command = vec![
        provider.command().to_string(),
        "-c".to_string(),
        "check_for_update_on_startup=false".to_string(),
        "--disable".to_string(),
        "apps".to_string(),
        "--dangerously-bypass-hook-trust".to_string(),
        "--remote".to_string(),
        remote.endpoint.clone(),
        "-C".to_string(),
        working_dir.display().to_string(),
    ];
    if let Some(model) = model.filter(|model| !is_default_model(model)) {
        // A retiring model shows a blocking migration modal, which stalls the
        // launch exactly like the trust and hook gates. Follow the upgrade the
        // provider itself advertises rather than arguing with it.
        command.extend(["--model".to_string(), migrated_codex_model(model)]);
    }
    if skip_permissions {
        command.push(provider.skip_permissions_flag().to_string());
    }
    if let Some(thread_id) = remote.thread_id.as_ref() {
        command.extend(["resume".to_string(), thread_id.clone()]);
    }
    command
}

/// Log what the pane was showing when a launch failed.
///
/// Best-effort and never fatal: this runs on a path that is already failing,
/// so a capture error must not mask the original problem.
async fn log_failed_launch_pane(exact_target: &str) {
    let Ok(output) = Command::new("tmux")
        .args(["capture-pane", "-p", "-t", exact_target])
        .output()
        .await
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let pane = String::from_utf8_lossy(&output.stdout);
    // The tail is where a modal or error sits; the head is the banner.
    let tail: Vec<&str> = pane.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    if tail.is_empty() {
        warn!("Failed launch pane was empty; the CLI produced no output at all");
        return;
    }
    let shown = tail.iter().rev().take(12).rev().copied().collect::<Vec<_>>().join(" | ");
    warn!("Failed launch pane (last lines): {shown}");
}

/// Record `trust_level = "trusted"` for a worktree Ainb created, in the Codex
/// home the app-server reads.
///
/// Codex asks "Do you trust the contents of this directory?" for any directory
/// it has not seen, and under `--remote` there is no CLI flag that suppresses
/// it (`-C` is `--cd`; the only trust flag is `--dangerously-bypass-hook-trust`,
/// which covers hooks, not directories). The modal blocks before a thread is
/// created, so the launch stalls and Ainb reports "Codex failed to start".
///
/// SCOPE, deliberately narrow: this writes exactly one key,
/// `[projects."<path>"] trust_level`, and only for a path Ainb created itself.
/// It must never touch `model`, `approval_policy`, sandbox settings,
/// `mcp_servers`, `hooks`, or anything else in the user's file. `toml_edit`
/// preserves their formatting, comments and every other entry byte for byte.
///
/// Best-effort: a failure here only means the user answers one prompt, so it
/// must never fail a launch.
pub(crate) fn trust_codex_project_dir(worktree: &std::path::Path) {
    let Some(config) = codex_config_path() else {
        return;
    };
    let existing = match std::fs::read_to_string(&config) {
        Ok(text) => text,
        // No config yet: Codex will create one. Writing a bare [projects] table
        // from under it risks fighting its own first-run write.
        Err(_) => return,
    };
    let Ok(mut doc) = existing.parse::<toml_edit::DocumentMut>() else {
        warn!("Codex config is not valid TOML; not recording worktree trust");
        return;
    };
    let key = worktree.display().to_string();
    // Already trusted (by Codex or by us) -- nothing to write.
    if doc
        .get("projects")
        .and_then(|projects| projects.get(&key))
        .and_then(|entry| entry.get("trust_level"))
        .and_then(|level| level.as_str())
        == Some("trusted")
    {
        return;
    }
    let projects = doc.entry("projects").or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(projects) = projects.as_table_mut() else {
        return;
    };
    projects.set_implicit(true);
    let entry = projects.entry(&key).or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(entry) = entry.as_table_mut() else {
        return;
    };
    entry["trust_level"] = toml_edit::value("trusted");
    if let Err(error) = write_atomic_config(&config, &doc.to_string()) {
        warn!("Could not record worktree trust in the Codex config: {error}");
        return;
    }
    info!("Recorded Codex trust for the worktree Ainb created: {key}");
}

/// `<CODEX_HOME>/config.toml`, honouring `CODEX_HOME`.
fn codex_config_path() -> Option<PathBuf> {
    let home = match std::env::var_os("CODEX_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => dirs::home_dir()?.join(".codex"),
    };
    Some(home.join("config.toml"))
}

/// Temp-file + rename, so a crash mid-write cannot truncate the user's config.
fn write_atomic_config(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.ainb-tmp");
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Substitute a retiring Codex model with the one the provider names.
///
/// `<CODEX_HOME>/models_cache.json` carries, per model, an `upgrade` block with
/// the replacement `model` and a `retirement_at`. When it is present, launching
/// the old slug shows a blocking migration modal, so the session never starts.
/// Codex owns this data and updates it server-side; we read it rather than
/// hard-coding a mapping that would go stale.
///
/// When the cache cannot answer - absent on a fresh machine, unreadable, or
/// pruned of the entry once a model is fully gone - this falls back to
/// [`ainb_model_rates::retired_codex_replacement`], the small dated table of ids
/// known to be dead. Returns the input unchanged only when neither source has
/// anything to say. Never fails a launch on its own.
pub(crate) fn migrated_codex_model(model: &str) -> String {
    if let Some((replacement, source)) = codex_cache_upgrade(model) {
        warn!(
            "Codex is retiring '{model}'; launching '{replacement}' instead \
             (per {source})"
        );
        return replacement;
    }

    // The cache had nothing to say. It can be absent (fresh machine),
    // unreadable, stale, or pruned of the entry once the model is fully gone,
    // so fall back to the small dated table of ids we know are dead. Warn
    // either way: the user pinned a model and is not getting it, and for
    // gpt-5.4-mini the replacement costs more.
    match ainb_model_rates::retired_codex_replacement(model) {
        Some(replacement) => {
            warn!(
                "'{model}' is retired and the Codex models cache offers no \
                 upgrade; launching '{replacement}' instead"
            );
            replacement.to_string()
        }
        None => model.to_string(),
    }
}

/// The replacement `<CODEX_HOME>/models_cache.json` advertises for `model`,
/// with the cache path that supplied it, or `None` when the cache is absent,
/// unreadable, unparseable, or carries no upgrade for this model.
fn codex_cache_upgrade(model: &str) -> Option<(String, String)> {
    let cache = codex_models_cache_path()?;
    let text = std::fs::read_to_string(&cache).ok()?;
    let parsed = serde_json::from_str::<serde_json::Value>(&text).ok()?;
    let replacement = parsed
        .get("models")
        .and_then(|models| models.as_array())?
        .iter()
        .find(|entry| entry.get("slug").and_then(|s| s.as_str()) == Some(model))
        .and_then(|entry| entry.get("upgrade"))
        .filter(|upgrade| !upgrade.is_null())
        .and_then(|upgrade| upgrade.get("model"))
        .and_then(|model| model.as_str())
        .filter(|replacement| *replacement != model)?;
    Some((replacement.to_string(), cache.display().to_string()))
}

/// `<CODEX_HOME>/models_cache.json`, honouring `CODEX_HOME`.
fn codex_models_cache_path() -> Option<PathBuf> {
    let home = match std::env::var_os("CODEX_HOME") {
        Some(home) if !home.is_empty() => PathBuf::from(home),
        _ => dirs::home_dir()?.join(".codex"),
    };
    Some(home.join("models_cache.json"))
}

/// Roll back resources created before a fresh Interactive session is registered.
pub(crate) async fn rollback_failed_interactive_launch(
    session_id: Uuid,
    exact_tmux_name: Option<&str>,
    worktree_manager: Option<&WorktreeManager>,
) {
    if let Some(tmux_name) = exact_tmux_name {
        let exact_target = format!("={tmux_name}");
        // Read the pane BEFORE killing it. Every way a CLI can stall before it
        // is ready -- a directory-trust modal, a hooks-need-review modal, a
        // model-migration prompt, a login screen, a config parse error --
        // collapses into the same "failed to start" message, and the one place
        // the actual reason is written is the pane we are about to destroy.
        log_failed_launch_pane(&exact_target).await;
        match Command::new("tmux").args(["kill-session", "-t", &exact_target]).output().await {
            Ok(output) if output.status.success() => {
                info!("Rolled back failed launch tmux session: {tmux_name}");
            }
            Ok(output) => warn!(
                "Failed to roll back tmux session '{}': {}",
                tmux_name,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) => warn!("Failed to run tmux cleanup for '{tmux_name}': {error}"),
        }
    }

    if let Some(worktree_manager) = worktree_manager {
        match worktree_manager.remove_worktree(session_id) {
            Ok(()) => info!("Rolled back failed launch worktree: {session_id}"),
            Err(crate::git::WorktreeError::NotFound(_)) => {}
            Err(error) => warn!("Failed to roll back worktree for {session_id}: {error}"),
        }
    }

    if let Err(error) = SessionStore::mutate(|store| {
        if let Some(tmux_name) = exact_tmux_name {
            store.remove_by_tmux_name(tmux_name);
        }
        store.remove_by_session_id(session_id);
    }) {
        warn!("Failed to purge failed launch metadata for {session_id}: {error}");
    }
}

pub(crate) fn persist_codex_thread_id(session_id: Uuid, thread_id: String) -> anyhow::Result<()> {
    SessionStore::mutate(|store| {
        if let Some(metadata) =
            store.sessions.values_mut().find(|metadata| metadata.session_id == session_id)
        {
            metadata.codex_thread_id = Some(thread_id);
        }
    })?;
    Ok(())
}

impl SessionMetadata {
    /// Resolve the raw model value used for launch/restart.
    ///
    /// New metadata stores the exact CLI value in `model` and marks it `Raw`.
    /// Legacy enum values serialized as strings are translated to historical IDs.
    /// Old Codex records stored their model in `codex_model`, so retain that
    /// fallback until the on-disk corpus has naturally migrated.
    pub fn launch_model(&self) -> Option<String> {
        if let Some(model) = self.model.as_deref() {
            return match self.model_source {
                ModelSource::Raw => normalize_raw_model(model),
                ModelSource::LegacyTyped => normalize_legacy_model(self.agent_type, model),
            };
        }

        self.codex_model.and_then(|model| model.cli_value()).map(str::to_string)
    }

    /// The workspace name to DISPLAY for this session.
    ///
    /// Re-derived from `worktree_path` via
    /// [`InteractiveSessionManager::workspace_name_for`], never read from the
    /// persisted `workspace_name` field. `ainb run` records the `--repo`
    /// basename, which is the wrong answer whenever the session is rooted at a
    /// subdirectory of a checkout: the TUI (which always re-derives) would say
    /// `myrepo` while `ainb list` said `subdir`, for the same session.
    ///
    /// The persisted field is kept as the creation-time record and is what a
    /// legacy entry still resolves against in
    /// [`crate::cli::util::find_session_in_store`].
    ///
    /// It is ALSO the fallback for a session whose worktree is GONE from disk.
    /// The derivation can only answer `(broken)` there, the TUI drops those
    /// rows from its tree entirely (so there is no TUI value to agree with),
    /// and `ainb list` is exactly the command a user runs to decide what to
    /// clean up. Collapsing every deleted worktree onto one undifferentiated
    /// `(broken)` throws away the only human-readable identifier those rows
    /// still carry.
    ///
    /// A root that still EXISTS and yet resolves to nothing is a different
    /// animal: a live directory sitting outside any usable repository. That
    /// one keeps saying `(broken)`, because it is the case the operator can
    /// still act on and the one the spawn skills tell them to look for.
    #[must_use]
    pub fn display_workspace_name(&self) -> String {
        let derived = InteractiveSessionManager::workspace_name_for(&self.worktree_path);
        if derived == InteractiveSessionManager::BROKEN_WORKSPACE_NAME
            && !self.worktree_path.exists()
            && !self.workspace_name.trim().is_empty()
        {
            return self.workspace_name.clone();
        }
        derived
    }
}

fn normalize_raw_model(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if is_default_model(trimmed) {
        return None;
    }

    Some(trimmed.to_string())
}

fn normalize_legacy_model(agent_type: SessionAgentType, value: &str) -> Option<String> {
    let trimmed = value.trim();
    if is_default_model(trimmed) || trimmed == "SystemDefault" {
        return None;
    }

    let legacy = match (agent_type, trimmed) {
        (SessionAgentType::Claude, "Fable") => Some("claude-fable-5"),
        (SessionAgentType::Claude, "Opus") => Some("claude-opus-4-8"),
        (SessionAgentType::Claude, "Opus47") => Some("claude-opus-4-7"),
        (SessionAgentType::Claude, "Opus46") => Some("claude-opus-4-6"),
        (SessionAgentType::Claude, "Sonnet") => Some("claude-sonnet-4-6"),
        (SessionAgentType::Claude, "Haiku") => Some("claude-haiku-4-5"),
        (SessionAgentType::Claude, "OpusPlan") => Some("opusplan"),
        _ => None,
    };

    Some(legacy.unwrap_or(trimmed).to_string())
}

/// Storage for all persisted session metadata
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SessionStore {
    pub sessions: HashMap<String, SessionMetadata>, // keyed by tmux_session_name
}

/// RAII guard holding the cross-process advisory lock over `sessions.json`.
///
/// Returned by [`SessionStore::lock`]. The lock is released when this drops, so
/// keep the guard alive for the entire load-mutate-save window and let it fall
/// out of scope afterwards. The wrapped file handle is intentionally opaque —
/// its only role is to own the `flock`.
#[must_use = "the sessions.json lock is released as soon as the guard is dropped"]
pub struct SessionStoreGuard {
    _file: std::fs::File,
}

impl SessionStore {
    /// Load session store from disk
    pub fn load() -> Self {
        let path = Self::storage_path();
        if !path.exists() {
            debug!(
                "No sessions.json found at {:?}, returning empty store",
                path
            );
            return Self::default();
        }

        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<SessionStore>(&content) {
                Ok(store) => {
                    debug!("Loaded {} sessions from {:?}", store.sessions.len(), path);
                    store
                }
                Err(e) => {
                    warn!(
                        "Failed to parse sessions.json: {}, returning empty store",
                        e
                    );
                    Self::default()
                }
            },
            Err(e) => {
                warn!("Failed to read sessions.json: {}, returning empty store", e);
                Self::default()
            }
        }
    }

    /// Acquire the cross-process advisory lock guarding `sessions.json` and
    /// hold it across a load-mutate-save.
    ///
    /// This is the SAME lock the hangar daemon takes in
    /// `ainb_fleet_core::session_registry::register_session_at`, so an
    /// interactive `ainb run` / `ainb kill` / recovery mutation and a daemon
    /// task registration serialise against each other instead of racing a
    /// naked read-modify-write. Hold the returned guard for the WHOLE window —
    /// load, inspect, mutate, save — then drop it (the lock releases on drop).
    /// Prefer [`SessionStore::mutate`] for the common upsert/remove sites; reach
    /// for the raw guard only when the mutation is interleaved with decisions
    /// the closure can't express (e.g. emitting host notifications on an
    /// early-return path).
    ///
    /// # Errors
    ///
    /// Returns an error if the store directory can't be created or the lock
    /// can't be acquired.
    pub fn lock() -> Result<SessionStoreGuard, std::io::Error> {
        let path = Self::storage_path();
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let file = ainb_fleet_core::session_registry::lock_sessions_store_at(dir)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        Ok(SessionStoreGuard { _file: file })
    }

    /// Locked read-modify-write: take the [`lock`](Self::lock), load the store
    /// FRESH under it, apply `f`, and save — atomically with respect to every
    /// other locked writer. The load happens inside the lock, so `f` always
    /// observes the latest on-disk state (no lost update).
    ///
    /// Use this for the upsert/remove lifecycle sites. When the mutation needs
    /// to short-circuit and do host-side work (notifications) on some paths,
    /// hold [`lock`](Self::lock) directly instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the lock can't be acquired or the save fails.
    pub fn mutate<F>(f: F) -> Result<(), std::io::Error>
    where
        F: FnOnce(&mut Self),
    {
        let _guard = Self::lock()?;
        let mut store = Self::load();
        f(&mut store);
        store.save()
    }

    /// Save session store to disk
    pub fn save(&self) -> Result<(), std::io::Error> {
        let path = Self::storage_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Atomic write: tmp + rename so a crash / full disk mid-write can't
        // truncate the store and lose every tracked session. With the proxy
        // watchdog, session-create and the `H` downgrade all writing here,
        // an in-place truncating write would widen the corruption window.
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, content)?;
        if let Err(e) = std::fs::rename(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        debug!("Saved {} sessions to {:?}", self.sessions.len(), path);
        Ok(())
    }

    /// Get the storage file path
    ///
    /// Honors the `AINB_HOME` environment variable as an override for the base
    /// directory (otherwise the user's home dir). This keeps the production path
    /// at `~/.agents-in-a-box/sessions.json` while letting tests point the store
    /// at an isolated temp directory.
    pub fn storage_path() -> PathBuf {
        let base = std::env::var_os("AINB_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
        base.join(".agents-in-a-box").join("sessions.json")
    }

    /// Add or update a session
    pub fn upsert(&mut self, metadata: SessionMetadata) {
        self.sessions.insert(metadata.tmux_session_name.clone(), metadata);
    }

    /// Remove a session by tmux name
    pub fn remove_by_tmux_name(&mut self, tmux_name: &str) {
        self.sessions.remove(tmux_name);
    }

    /// Remove a session by session_id
    pub fn remove_by_session_id(&mut self, session_id: Uuid) {
        self.sessions.retain(|_, v| v.session_id != session_id);
    }

    /// Find session by tmux name
    pub fn find_by_tmux_name(&self, tmux_name: &str) -> Option<&SessionMetadata> {
        self.sessions.get(tmux_name)
    }

    /// Get an iterator over all tracked sessions (tmux_name -> metadata)
    pub fn sessions(&self) -> &HashMap<String, SessionMetadata> {
        &self.sessions
    }

    /// Get all tmux session names that are tracked
    pub fn tracked_tmux_names(&self) -> Vec<&str> {
        self.sessions.keys().map(|s| s.as_str()).collect()
    }
}

/// Default port for the ainb-managed Headroom compression proxy.
pub const HEADROOM_DEFAULT_PORT: u16 = 8787;

/// Base URL of the local Headroom proxy. One resolver with
/// [`crate::headroom::proxy_port`], so the URL a session is pointed at and the
/// port the proxy is spawned on cannot disagree.
pub fn headroom_base_url() -> String {
    format!("http://127.0.0.1:{}", crate::headroom::proxy_port())
}

/// Shell `export … && ` prefix that routes a session's CLI through the local
/// Headroom proxy. Empty when disabled or for providers that can't be proxied
/// (Gemini/Copilot). One source of truth for both initial launch
/// (`build_env_setup_for_provider`) and restart, so the two never drift.
pub fn headroom_env_prefix(agent_type: SessionAgentType, enabled: bool) -> String {
    if !enabled {
        return String::new();
    }
    match agent_type {
        SessionAgentType::Claude => {
            format!("export ANTHROPIC_BASE_URL='{}' && ", headroom_base_url())
        }
        SessionAgentType::Codex => {
            format!("export OPENAI_BASE_URL='{}/v1' && ", headroom_base_url())
        }
        _ => String::new(),
    }
}

/// Manager for Interactive mode sessions (host-based, no Docker)
pub struct InteractiveSessionManager {
    worktree_manager: WorktreeManager,
    active_sessions: HashMap<Uuid, InteractiveSession>,
}

impl InteractiveSessionManager {
    /// Create a new Interactive session manager
    ///
    /// NOTE: This does NOT require Docker, unlike SessionLifecycleManager
    pub fn new() -> Result<Self, InteractiveSessionError> {
        let worktree_manager = WorktreeManager::new().map_err(|e| {
            InteractiveSessionError::InvalidState(format!(
                "Failed to create worktree manager: {}",
                e
            ))
        })?;

        Ok(Self {
            worktree_manager,
            active_sessions: HashMap::new(),
        })
    }

    /// Create a new Interactive session with worktree and tmux
    ///
    /// # Arguments
    /// * `session_id` - Unique identifier for the session
    /// * `workspace_name` - Name of the workspace
    /// * `workspace_path` - Path to the git repository
    /// * `branch_name` - Branch name to create worktree for
    /// * `base_branch` - Optional base branch to branch from
    ///
    /// # Returns
    /// * `Result<InteractiveSession>` - The created session or an error
    pub async fn create_session(
        &mut self,
        session_id: Uuid,
        workspace_name: String,
        workspace_path: PathBuf,
        branch_name: String,
        base_branch: Option<String>,
        skip_permissions: bool,
        agent_type: SessionAgentType,
        model: Option<String>,
        headroom_enabled: bool,
        rtk_enabled: bool,
    ) -> Result<InteractiveSession, InteractiveSessionError> {
        info!(
            "Creating Interactive session {} for branch '{}' in workspace '{}' (agent={:?}, model={:?}, skip_permissions={})",
            session_id, branch_name, workspace_name, agent_type, model, skip_permissions
        );

        // Check if session already exists
        if self.active_sessions.contains_key(&session_id) {
            return Err(InteractiveSessionError::SessionAlreadyExists(session_id));
        }

        // Step 1: Create git worktree
        info!("Creating worktree for branch '{}'", branch_name);
        let worktree_info = self.worktree_manager.create_worktree(
            session_id,
            &workspace_path,
            &branch_name,
            base_branch.as_deref(),
        )?;

        info!("Created worktree at: {}", worktree_info.path.display());

        // Step 1b: Wire RTK project-local hook (best-effort — failure is
        // non-fatal). Claude only: the hook lives in `.claude/settings.json`,
        // which Codex/Gemini/Copilot never read — wiring it for them writes a
        // file nothing consumes.
        if rtk_enabled && agent_type == SessionAgentType::Claude {
            if let Err(e) = wire_rtk_project_hook(&worktree_info.path) {
                warn!(
                    "Failed to wire RTK hook in worktree: {} — session launches without RTK",
                    e
                );
            }
        }

        let codex_remote = if agent_type == SessionAgentType::Codex {
            match ensure_codex_remote_thread(
                session_id,
                &worktree_info.path,
                model.as_deref(),
                skip_permissions,
                headroom_enabled,
                None,
            )
            .await
            {
                Ok(remote) => remote,
                Err(error) => {
                    rollback_failed_interactive_launch(
                        session_id,
                        None,
                        Some(&self.worktree_manager),
                    )
                    .await;
                    return Err(error
                        .context("Codex failed to start; AINB ran failed-session cleanup")
                        .into());
                }
            }
        } else {
            None
        };

        // Step 2: Create tmux session name (format: tmux_{folder}_{branch})
        let worktree_folder = Self::extract_worktree_folder(&worktree_info.path);
        let tmux_session_name = Self::generate_tmux_name(&worktree_folder, &branch_name);

        // Step 3: Start tmux session
        info!("Starting tmux session: {}", tmux_session_name);
        if let Err(error) = self.start_tmux_session(&tmux_session_name, &worktree_info.path).await {
            rollback_failed_interactive_launch(
                session_id,
                Some(&tmux_session_name),
                Some(&self.worktree_manager),
            )
            .await;
            return Err(error);
        }

        // Step 4: Start CLI in tmux session (for AI agent types)
        match agent_type {
            SessionAgentType::Claude
            | SessionAgentType::Codex
            | SessionAgentType::Gemini
            | SessionAgentType::Copilot
            | SessionAgentType::Antigravity => {
                info!(
                    "Starting {:?} CLI in tmux session (model={:?}, skip_permissions={})",
                    agent_type, model, skip_permissions
                );
                if let Err(error) = self
                    .start_cli_in_tmux(
                        &tmux_session_name,
                        &worktree_info.path,
                        skip_permissions,
                        model.clone(),
                        agent_type,
                        None,
                        false, // resume_requested: fresh launch
                        headroom_enabled,
                        codex_remote.as_ref(),
                    )
                    .await
                {
                    rollback_failed_interactive_launch(
                        session_id,
                        Some(&tmux_session_name),
                        Some(&self.worktree_manager),
                    )
                    .await;
                    return Err(error);
                }
            }
            _ => {
                info!("Skipping CLI for agent type: {:?}", agent_type);
            }
        }

        let codex_remote = match codex_remote {
            Some(remote) if remote.thread_id.is_none() => match claim_codex_remote_thread(
                session_id,
                &worktree_info.path,
                model.as_deref(),
                skip_permissions,
                headroom_enabled,
            )
            .await
            {
                Ok(remote) => remote,
                Err(error) => {
                    rollback_failed_interactive_launch(
                        session_id,
                        Some(&tmux_session_name),
                        Some(&self.worktree_manager),
                    )
                    .await;
                    return Err(error
                        .context("Codex failed to start; AINB ran failed-session cleanup")
                        .into());
                }
            },
            remote => remote,
        };

        // Step 5: Create session record
        let created_at = Utc::now();
        let session = InteractiveSession {
            session_id,
            worktree_path: worktree_info.path.clone(),
            source_repository: worktree_info.source_repository.clone(),
            tmux_session_name: tmux_session_name.clone(),
            branch_name: branch_name.clone(),
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
            skip_permissions,
            model: model.clone(),
            headroom_enabled,
            rtk_enabled,
            codex_thread_id: codex_remote.as_ref().and_then(|remote| remote.thread_id.clone()),
        };

        self.active_sessions.insert(session_id, session.clone());

        // Step 6: Persist session metadata to sessions.json for discovery across restarts
        let metadata = SessionMetadata {
            session_id,
            tmux_session_name: tmux_session_name.clone(),
            worktree_path: worktree_info.path.clone(),
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
            headroom_enabled,
            rtk_enabled,
            skip_permissions: Some(skip_permissions),
            model,
            model_source: ModelSource::Raw,
            codex_model: None,
            codex_thread_id: codex_remote.and_then(|remote| remote.thread_id),
        };
        // Locked RMW so a concurrent `ainb kill` / recovery / daemon register
        // can't lost-update this upsert (pu4).
        if let Err(e) = SessionStore::mutate(|store| store.upsert(metadata)) {
            warn!("Failed to persist session metadata: {}", e);
            // Continue anyway - session is still usable, just won't survive restarts gracefully
        }

        info!("Successfully created Interactive session {}", session_id);

        // Audit log the session creation
        audit::audit_session_created(
            session_id,
            &tmux_session_name,
            &worktree_info.path.display().to_string(),
            &branch_name,
            AuditTrigger::UserKeypress("Enter".to_string()),
            AuditResult::Success,
        );

        Ok(session)
    }

    /// Create an Interactive session using an existing worktree
    ///
    /// This is used for remote repository flows where the worktree has already been
    /// created from the bare cache. Unlike `create_session()`, this skips worktree creation.
    ///
    /// # Arguments
    /// * `session_id` - Unique identifier for the session
    /// * `workspace_name` - Name of the workspace (for display)
    /// * `existing_worktree_path` - Path to the already-created worktree
    /// * `source_repo_path` - Path to the source repository (bare cache for remote repos)
    /// * `branch_name` - Branch name for the session
    /// * `skip_permissions` - Whether to skip permission prompts in claude CLI
    /// * `agent_type` - Type of agent (Claude, Shell, etc.)
    /// * `model` - Claude model to use (only for Claude agent)
    ///
    /// # Returns
    /// * `Result<InteractiveSession>` - The created session or an error
    pub async fn create_session_with_worktree(
        &mut self,
        session_id: Uuid,
        workspace_name: String,
        existing_worktree_path: PathBuf,
        source_repo_path: PathBuf,
        branch_name: String,
        skip_permissions: bool,
        agent_type: SessionAgentType,
        model: Option<String>,
        headroom_enabled: bool,
        rtk_enabled: bool,
    ) -> Result<InteractiveSession, InteractiveSessionError> {
        info!(
            "Creating Interactive session {} with existing worktree at '{}' (agent={:?}, model={:?})",
            session_id,
            existing_worktree_path.display(),
            agent_type,
            model
        );

        // Check if session already exists
        if self.active_sessions.contains_key(&session_id) {
            return Err(InteractiveSessionError::SessionAlreadyExists(session_id));
        }

        // Verify the worktree exists
        if !existing_worktree_path.exists() {
            return Err(InteractiveSessionError::Worktree(
                crate::git::WorktreeError::NotFound(existing_worktree_path.display().to_string()),
            ));
        }

        info!(
            "Using existing worktree at: {}",
            existing_worktree_path.display()
        );

        // Create session-based symlink for easy lookup
        let session_path =
            self.worktree_manager.base_dir().join("by-session").join(session_id.to_string());
        if !session_path.exists() {
            if let Some(parent) = session_path.parent() {
                std::fs::create_dir_all(parent).ok();
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(&existing_worktree_path, &session_path).ok();
        }

        // Step 0b: Wire RTK project-local hook (best-effort — failure is
        // non-fatal). Claude only: see create_session — `.claude/settings.json`
        // is a Claude Code surface, ignored by other agents.
        if rtk_enabled && agent_type == SessionAgentType::Claude {
            if let Err(e) = wire_rtk_project_hook(&existing_worktree_path) {
                warn!(
                    "Failed to wire RTK hook in worktree: {} — session launches without RTK",
                    e
                );
            }
        }

        let codex_remote = if agent_type == SessionAgentType::Codex {
            match ensure_codex_remote_thread(
                session_id,
                &existing_worktree_path,
                model.as_deref(),
                skip_permissions,
                headroom_enabled,
                None,
            )
            .await
            {
                Ok(remote) => remote,
                Err(error) => {
                    rollback_failed_interactive_launch(
                        session_id,
                        None,
                        Some(&self.worktree_manager),
                    )
                    .await;
                    return Err(error
                        .context("Codex failed to start; AINB ran failed-session cleanup")
                        .into());
                }
            }
        } else {
            None
        };

        // Step 1: Create tmux session name (format: tmux_{folder}_{branch})
        let worktree_folder = Self::extract_worktree_folder(&existing_worktree_path);
        let tmux_session_name = Self::generate_tmux_name(&worktree_folder, &branch_name);

        // Step 2: Start tmux session
        info!("Starting tmux session: {}", tmux_session_name);
        if let Err(error) =
            self.start_tmux_session(&tmux_session_name, &existing_worktree_path).await
        {
            rollback_failed_interactive_launch(
                session_id,
                Some(&tmux_session_name),
                Some(&self.worktree_manager),
            )
            .await;
            return Err(error);
        }

        // Step 3: Start CLI in tmux session (for AI agent types)
        match agent_type {
            SessionAgentType::Claude
            | SessionAgentType::Codex
            | SessionAgentType::Gemini
            | SessionAgentType::Copilot
            | SessionAgentType::Antigravity => {
                info!(
                    "Starting {:?} CLI in tmux session (model={:?}, skip_permissions={})",
                    agent_type, model, skip_permissions
                );
                if let Err(error) = self
                    .start_cli_in_tmux(
                        &tmux_session_name,
                        &existing_worktree_path,
                        skip_permissions,
                        model.clone(),
                        agent_type,
                        None,
                        false, // resume_requested: fresh launch
                        headroom_enabled,
                        codex_remote.as_ref(),
                    )
                    .await
                {
                    rollback_failed_interactive_launch(
                        session_id,
                        Some(&tmux_session_name),
                        Some(&self.worktree_manager),
                    )
                    .await;
                    return Err(error);
                }
            }
            _ => {
                info!("Skipping CLI for agent type: {:?}", agent_type);
            }
        }

        let codex_remote = match codex_remote {
            Some(remote) if remote.thread_id.is_none() => match claim_codex_remote_thread(
                session_id,
                &existing_worktree_path,
                model.as_deref(),
                skip_permissions,
                headroom_enabled,
            )
            .await
            {
                Ok(remote) => remote,
                Err(error) => {
                    rollback_failed_interactive_launch(
                        session_id,
                        Some(&tmux_session_name),
                        Some(&self.worktree_manager),
                    )
                    .await;
                    return Err(error
                        .context("Codex failed to start; AINB ran failed-session cleanup")
                        .into());
                }
            },
            remote => remote,
        };

        // Step 4: Create session record
        let created_at = Utc::now();
        let worktree_path_clone = existing_worktree_path.clone();
        let session = InteractiveSession {
            session_id,
            worktree_path: existing_worktree_path,
            source_repository: source_repo_path,
            tmux_session_name: tmux_session_name.clone(),
            branch_name: branch_name.clone(),
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
            skip_permissions,
            model: model.clone(),
            headroom_enabled,
            rtk_enabled,
            codex_thread_id: codex_remote.as_ref().and_then(|remote| remote.thread_id.clone()),
        };

        self.active_sessions.insert(session_id, session.clone());

        // Step 5: Persist session metadata to sessions.json for discovery across restarts
        let metadata = SessionMetadata {
            session_id,
            tmux_session_name: tmux_session_name.clone(),
            worktree_path: worktree_path_clone,
            workspace_name: workspace_name.clone(),
            created_at,
            agent_type,
            headroom_enabled,
            rtk_enabled,
            skip_permissions: Some(skip_permissions),
            model,
            model_source: ModelSource::Raw,
            codex_model: None,
            codex_thread_id: codex_remote.and_then(|remote| remote.thread_id),
        };
        // Locked RMW (pu4): serialise against concurrent kill/recovery writers.
        if let Err(e) = SessionStore::mutate(|store| store.upsert(metadata)) {
            warn!("Failed to persist session metadata: {}", e);
        }

        info!(
            "Successfully created Interactive session {} with existing worktree",
            session_id
        );

        // Audit log the session creation
        audit::audit_session_created(
            session_id,
            &tmux_session_name,
            &session.worktree_path.display().to_string(),
            &branch_name,
            AuditTrigger::UserKeypress("Enter".to_string()),
            AuditResult::Success,
        );

        Ok(session)
    }

    /// Discover and list all active Interactive sessions by scanning tmux
    ///
    /// This enables stateless recovery - we can discover sessions created in
    /// previous app instances by matching tmux session names to worktrees.
    ///
    /// # Returns
    /// * `Result<Vec<InteractiveSession>>` - List of discovered sessions
    pub async fn list_sessions(
        &mut self,
    ) -> Result<Vec<InteractiveSession>, InteractiveSessionError> {
        info!("Discovering Interactive sessions from tmux");

        // Get all tmux sessions
        let output = Command::new("tmux")
            .args(["list-sessions", "-F", "#{session_name}"])
            .output()
            .await?;

        if !output.status.success() {
            // No tmux server running or no sessions
            debug!("No tmux sessions found (tmux might not be running)");
            return Ok(Vec::new());
        }

        let tmux_sessions = String::from_utf8_lossy(&output.stdout);
        let mut discovered_sessions = Vec::new();

        // Filter for our tmux sessions (prefix: tmux_)
        for tmux_name in tmux_sessions.lines() {
            if !tmux_name.starts_with("tmux_") {
                continue;
            }

            debug!("Found tmux session: {}", tmux_name);

            // Try to find corresponding worktree
            if let Ok(session) = self.discover_session_from_tmux(tmux_name).await {
                discovered_sessions.push(session);
            }
        }

        info!(
            "Discovered {} Interactive sessions",
            discovered_sessions.len()
        );
        Ok(discovered_sessions)
    }

    /// Discover a session from a tmux session name
    ///
    /// Uses a two-phase approach:
    /// 1. First, try to find the session in sessions.json (handles branch-mismatch case)
    /// 2. If not found, fall back to reverse-engineering the branch name from tmux session name
    async fn discover_session_from_tmux(
        &self,
        tmux_name: &str,
    ) -> Result<InteractiveSession, InteractiveSessionError> {
        // Phase 1: Try to find session in persisted sessions.json
        // This handles the branch-mismatch case where the user changed branches in the worktree
        let store = SessionStore::load();
        if let Some(metadata) = store.find_by_tmux_name(tmux_name) {
            // Verify the worktree still exists
            if metadata.worktree_path.exists() {
                debug!(
                    "Found session {} in sessions.json for tmux {}",
                    metadata.session_id, tmux_name
                );

                // Try to get current branch name from the worktree
                let branch_name = Self::get_current_branch(&metadata.worktree_path)
                    .unwrap_or_else(|| "unknown".to_string());

                // Try to get source repository from worktree
                let source_repository = Self::get_source_repository(&metadata.worktree_path)
                    .unwrap_or_else(|| metadata.worktree_path.clone());

                // Use persisted agent_type, fall back to tmux process detection
                let agent_type = if metadata.agent_type == SessionAgentType::Claude {
                    // Could be a real Claude session or a legacy default — try detecting from tmux
                    Self::detect_agent_from_tmux(tmux_name).await.unwrap_or(metadata.agent_type)
                } else {
                    metadata.agent_type
                };

                // Re-derive workspace_name from current paths rather than trusting
                // the persisted value — older sessions.json entries may carry the
                // full sanitized worktree dir name as workspace_name.
                let workspace_name =
                    Self::derive_workspace_name(&metadata.worktree_path, &source_repository);

                return Ok(InteractiveSession {
                    session_id: metadata.session_id,
                    worktree_path: metadata.worktree_path.clone(),
                    source_repository,
                    tmux_session_name: tmux_name.to_string(),
                    branch_name,
                    workspace_name,
                    created_at: metadata.created_at,
                    agent_type,
                    skip_permissions: metadata.skip_permissions.unwrap_or(true),
                    model: metadata.launch_model(),
                    headroom_enabled: metadata.headroom_enabled,
                    rtk_enabled: metadata.rtk_enabled,
                    codex_thread_id: metadata.codex_thread_id.clone(),
                });
            } else {
                debug!(
                    "Session {} in sessions.json but worktree no longer exists at {:?}",
                    metadata.session_id, metadata.worktree_path
                );
            }
        }

        // Phase 2: Fall back to branch-name matching (original logic)
        // Remove "tmux_" prefix and reverse sanitization
        let sanitized = tmux_name.strip_prefix("tmux_").unwrap_or(tmux_name);
        let branch_guess = sanitized.replace('_', "/");

        // Try to find worktree with matching branch
        // Use list_all_worktrees() which scans by-session directory with UUID symlinks
        let worktrees = self.worktree_manager.list_all_worktrees().map_err(|e| {
            InteractiveSessionError::InvalidState(format!("Failed to list worktrees: {}", e))
        })?;

        for (session_id, worktree) in worktrees {
            // Try matching both new format (tmux_{folder}_{branch}) and legacy format (tmux_{branch})
            let worktree_folder = Self::extract_worktree_folder(&worktree.path);
            let matches_new_format =
                Self::generate_tmux_name(&worktree_folder, &worktree.branch_name) == tmux_name;
            let matches_legacy_format =
                Self::generate_tmux_name_legacy(&worktree.branch_name) == tmux_name;
            let matches_branch_guess = worktree.branch_name.contains(&branch_guess);

            if matches_new_format || matches_legacy_format || matches_branch_guess {
                let workspace_name =
                    Self::derive_workspace_name(&worktree.path, &worktree.source_repository);

                // Try to detect agent from tmux process, default to Claude
                let agent_type = Self::detect_agent_from_tmux(tmux_name)
                    .await
                    .unwrap_or(SessionAgentType::Claude);

                return Ok(InteractiveSession {
                    session_id,
                    worktree_path: worktree.path,
                    source_repository: worktree.source_repository,
                    tmux_session_name: tmux_name.to_string(),
                    branch_name: worktree.branch_name,
                    workspace_name,
                    created_at: Utc::now(),
                    agent_type,
                    skip_permissions: true,
                    model: None,
                    headroom_enabled: false,
                    rtk_enabled: false,
                    codex_thread_id: None,
                });
            }
        }

        Err(InteractiveSessionError::InvalidState(format!(
            "No matching worktree found for tmux session {}",
            tmux_name
        )))
    }

    /// Detect agent type by inspecting the running process in a tmux session
    ///
    /// Runs `tmux list-panes -t <session> -F '#{pane_current_command}'` to get the
    /// active process, then matches it against known CLI commands.
    async fn detect_agent_from_tmux(tmux_name: &str) -> Option<SessionAgentType> {
        let output = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                tmux_name,
                "-F",
                "#{pane_current_command}",
            ])
            .output()
            .await
            .ok()?;

        if !output.status.success() {
            return None;
        }

        let commands = String::from_utf8_lossy(&output.stdout);
        for line in commands.lines() {
            let cmd = line.trim().to_lowercase();
            if cmd.contains("claude") {
                return Some(SessionAgentType::Claude);
            } else if cmd.contains("codex") {
                return Some(SessionAgentType::Codex);
            } else if cmd.contains("agy") || cmd.contains("antigravity") {
                return Some(SessionAgentType::Antigravity);
            } else if cmd.contains("gemini") {
                return Some(SessionAgentType::Gemini);
            } else if cmd.contains("copilot") {
                return Some(SessionAgentType::Copilot);
            }
        }

        None
    }

    /// Get the current branch name from a worktree path.
    ///
    /// Delegates to the one shared implementation
    /// ([`crate::git::current_branch_at`]), which DISCOVERS the owning
    /// repository rather than requiring `worktree_path` to be a repository
    /// root. A session rooted at a subdirectory of a checkout (the shape
    /// `get_source_repository` now resolves) would otherwise render its branch
    /// as "unknown".
    ///
    /// Associated fn, not a method: it reads nothing from `self`, and keeping
    /// it callable without a manager instance is what lets the unit tests
    /// exercise it against real git state.
    fn get_current_branch(worktree_path: &Path) -> Option<String> {
        crate::git::current_branch_at(worktree_path)
    }

    /// Sentinel workspace name for sessions whose worktree directory is on
    /// disk but sits inside NO git repository at all (e.g. a worktree emptied
    /// except for a leftover cache like `.vite/`). Callers also use this as
    /// the workspace bucket key so every broken session collapses into one
    /// row instead of fanning out.
    ///
    /// It explicitly does NOT mean "not a linked git worktree": a plain clone
    /// (`.git` is a directory) and a subdirectory of a clone are both valid
    /// session roots and must render with their real repository name.
    pub const BROKEN_WORKSPACE_NAME: &'static str = "(broken)";

    /// Derive a workspace display name from a worktree path.
    ///
    /// Why: only the legacy `<repo>--<hash>--<session>` worktree layout encodes
    /// the repo in the directory name. Newer flat-format paths (no `--`) made
    /// `path.split("--").next()` return the entire directory, producing bogus
    /// workspace groups like `shotclubhouse_shotclubhouse_temp_debug`.
    ///
    /// When the worktree is broken (inside no git repository), callers fall
    /// back to passing `worktree_path` itself as `source_repository`. Detect
    /// that collapse and surface `(broken)` instead of the sanitized
    /// doubled-prefix dir name, otherwise a dead worktree dir appears as a
    /// real workspace.
    ///
    /// The collapse alone is NOT proof of breakage: a session created directly
    /// in a plain checkout (`ainb run --repo <clone>` with no `--worktree`)
    /// legitimately has `source_repository == worktree_path`. We therefore
    /// re-run the repository lookup and only claim `(broken)` when it genuinely
    /// finds nothing.
    ///
    /// An ATC control directory is checked FIRST, before every other rule. It
    /// is a real, healthy session root that is deliberately not a git worktree,
    /// so it would otherwise fall all the way through to `(broken)` and become
    /// indistinguishable from a dead worktree. Running the check first (rather
    /// than just ahead of the sentinel) also keeps an instance whose name
    /// happens to contain `--` from being chopped by the legacy-layout rule:
    /// `sanitize_instance_name` permits `-`, and the ATC shape is the stronger
    /// signal.
    pub fn derive_workspace_name(worktree_path: &Path, source_repository: &Path) -> String {
        if let Some(instance) = Self::atc_instance_name(worktree_path) {
            return format!("atc:{instance}");
        }

        let from_dir = worktree_path
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|name| name.contains("--"))
            .and_then(|name| name.split("--").next());

        if let Some(name) = from_dir {
            return name.to_string();
        }

        // Broken-worktree sentinel: the loaders fall back to
        // `source_repository = worktree_path` whenever `get_source_repository`
        // returns None. We only treat that collapse as "broken" when both
        // paths actually have a basename (so `/` + `/` still yields "unknown")
        // AND the lookup really does come up empty: a plain checkout resolves
        // to itself, which produces the same collapsed shape but is valid.
        if source_repository == worktree_path
            && worktree_path.file_name().is_some()
            && Self::get_source_repository(worktree_path).is_none()
        {
            return Self::BROKEN_WORKSPACE_NAME.to_string();
        }

        source_repository
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    }

    /// Recognise an ATC control directory and return its instance name.
    ///
    /// ATC spawns its session with `--repo <atc_root>/<name>`, so the session
    /// root is the instance directory itself: a direct child of the ATC root
    /// carrying a `meta.json`. That is deliberately NOT a git worktree, which
    /// is why the ordinary derivation has nothing to name it with.
    ///
    /// The root is resolved through [`crate::fleet::atc::paths::atc_root`], the
    /// same function the rest of ATC uses, so `AINB_HOME` is honoured and no
    /// home path is baked in. An unresolvable home (no `$AINB_HOME`, no home
    /// directory) is simply "not ATC" rather than an error: this is a render
    /// path and must never fail.
    ///
    /// The `meta.json` requirement is what keeps the check narrow. A stray
    /// directory under the ATC root that was never provisioned is not claimed.
    fn atc_instance_name(worktree_path: &Path) -> Option<String> {
        // A git checkout is never an ATC control dir, whatever it sits under.
        // Without this, a real repository that happened to live beside the ATC
        // instances would take the ATC name while its bucket key still came
        // from the repository lookup, so the two would disagree about the same
        // row. ATC dirs have no `.git` at all, so this costs nothing.
        if worktree_path.join(".git").exists() {
            return None;
        }
        let root = crate::fleet::atc::paths::atc_root().ok()?;
        if let Some(name) = crate::fleet::atc::instance_name_for_cwd_in(&root, worktree_path) {
            return Some(name);
        }
        // The parent comparison is literal, so a symlinked home (macOS
        // `/tmp` -> `/private/tmp`, the shape tempdir-based tests produce)
        // misses even though the two paths are the same directory. Retry once
        // against the canonical forms before giving up.
        let root = root.canonicalize().ok()?;
        let path = worktree_path.canonicalize().ok()?;
        crate::fleet::atc::instance_name_for_cwd_in(&root, &path)
    }

    /// THE workspace-name derivation: repository lookup + naming in one call.
    ///
    /// Every surface that displays a workspace name must go through this, so
    /// the TUI's session list and `ainb list` / `ainb status` can never
    /// disagree about what a session is called. The TUI's two loaders already
    /// compose exactly these two steps; the CLI used to print the value
    /// PERSISTED at creation time (the `--repo` basename), which differs for
    /// any session rooted at a subdirectory of a checkout.
    ///
    /// Derived from the path on every read, never written back: a render path
    /// that mutates `sessions.json` would rewrite persisted state as a side
    /// effect of drawing a frame, and would still be wrong for any surface
    /// that had not been drawn yet.
    #[must_use]
    pub fn workspace_name_for(worktree_path: &Path) -> String {
        let source_repository = Self::get_source_repository(worktree_path)
            .unwrap_or_else(|| worktree_path.to_path_buf());
        Self::derive_workspace_name(worktree_path, &source_repository)
    }

    /// Resolve the git repository root that owns `worktree_path`.
    ///
    /// Three on-disk shapes are recognised, checked from `worktree_path`
    /// upwards through its ancestors:
    ///
    /// 1. `<dir>/.git` is a FILE: a linked git worktree. The file holds a
    ///    `gitdir: <main>/.git/worktrees/<name>` pointer; follow it back to
    ///    the main repository (or the bare repo dir).
    /// 2. `<dir>/.git` is a DIRECTORY: a plain checkout. `<dir>` IS the
    ///    repository root.
    /// 3. Neither: keep walking up. This covers a session rooted at a
    ///    SUBDIRECTORY of a checkout, which is what
    ///    `ainb run --repo <clone>/subdir` produces.
    ///
    /// Returns `None` only when no ancestor is inside any git repository, i.e.
    /// the directory is genuinely repo-less. Callers treat that (and only
    /// that) as [`Self::BROKEN_WORKSPACE_NAME`].
    ///
    /// Note: the ancestor walk attributes a session to whatever repository
    /// contains it, which is exactly what `git` itself would report from that
    /// directory. Running a session inside e.g. a git-tracked `$HOME` will
    /// therefore group it under that repository rather than reporting it
    /// broken. The walk is bounded by the filesystem root.
    ///
    /// ABSOLUTE PATHS ONLY. A relative path is rejected outright rather than
    /// walked, because `Path::ancestors()` on a relative path terminates at
    /// `""`, and `Path::new("").join(".git")` is `".git"`, which the
    /// filesystem resolves against the PROCESS's current directory. The walk
    /// would then silently attribute the session to whatever repository the
    /// user happened to run `ainb` from, which has nothing to do with the
    /// session. Every real caller (the session store, the worktree manager)
    /// holds an absolute path, so this rejects only nonsense input.
    pub fn get_source_repository(worktree_path: &Path) -> Option<PathBuf> {
        if !worktree_path.is_absolute() {
            return None;
        }
        for candidate in worktree_path.ancestors() {
            let git_entry = candidate.join(".git");
            if git_entry.is_file() {
                // Linked worktree: resolve through the gitdir pointer. A
                // malformed pointer is a real breakage, so stop here rather
                // than silently attributing the session to an ancestor repo.
                return Self::source_repository_from_gitdir_pointer(&git_entry);
            }
            if git_entry.is_dir() {
                return Some(candidate.to_path_buf());
            }
        }
        None
    }

    /// Follow a linked worktree's `.git` pointer file back to its main repo.
    ///
    /// A pointer that PARSES but names a gitdir that is gone (the main repo was
    /// deleted) resolves to nothing. Path arithmetic alone would happily hand
    /// back the vanished repo's basename, so a session whose repository no
    /// longer exists would render as a healthy row named after it. Requiring
    /// the target to exist is what makes `(broken)` mean what the docs say it
    /// means: not inside any usable repository.
    fn source_repository_from_gitdir_pointer(git_file: &Path) -> Option<PathBuf> {
        let content = std::fs::read_to_string(git_file).ok()?;
        // Format: "gitdir: /path/to/main/repo/.git/worktrees/name"
        let gitdir = content.trim().strip_prefix("gitdir: ")?;

        // Navigate from .git/worktrees/name to the main repo
        let worktree_git_path = PathBuf::from(gitdir);
        if !worktree_git_path.exists() {
            return None;
        }
        let main_git = worktree_git_path.parent()?.parent()?.parent()?;

        // The main git dir might be .git or bare repo
        if main_git.file_name()? == ".git" {
            main_git.parent().map(|p| p.to_path_buf())
        } else {
            Some(main_git.to_path_buf())
        }
    }

    /// Remove an Interactive session (cleanup tmux and worktree)
    ///
    /// # Arguments
    /// * `session_id` - UUID of the session to remove
    ///
    /// # Returns
    /// * `Result<()>` - Success or an error
    pub async fn remove_session(
        &mut self,
        session_id: Uuid,
    ) -> Result<(), InteractiveSessionError> {
        info!(
            ">>> InteractiveSessionManager::remove_session() START: {}",
            session_id
        );

        // Try to get session from active_sessions first
        let session_opt = self.active_sessions.remove(&session_id);
        info!("Session in active_sessions: {}", session_opt.is_some());

        // Step 1: Resolve the tmux session name, then kill it.
        //
        // Resolution order: in-memory session → worktree-derived name → the
        // persisted sessions.json entry. The store fallback is what makes
        // orphaned records deletable: an entry whose worktree/symlink is already
        // gone (e.g. a shared worktree removed by a sibling session, or a record
        // imported without a `by-session/<uuid>` symlink) still carries its
        // `tmux_session_name`, so we can kill the tmux session and — crucially —
        // always fall through to the store cleanup in Step 3 instead of bailing.
        let tmux_session_name: Option<String> = if let Some(ref session) = session_opt {
            info!(
                "Using tmux session name from memory: {}",
                session.tmux_session_name
            );
            Some(session.tmux_session_name.clone())
        } else {
            // Try to get worktree info and derive tmux session name
            info!("Session not in memory, discovering from worktree");
            match self.worktree_manager.get_worktree_info(session_id) {
                Ok(worktree) => {
                    info!("Found worktree with branch: {}", worktree.branch_name);
                    let worktree_folder = Self::extract_worktree_folder(&worktree.path);
                    let tmux_name =
                        Self::generate_tmux_name(&worktree_folder, &worktree.branch_name);
                    let legacy_name = Self::generate_tmux_name_legacy(&worktree.branch_name);
                    // Check if new format session exists, otherwise try legacy
                    let check_new = std::process::Command::new("tmux")
                        .args(["has-session", "-t", &tmux_name])
                        .output();
                    let final_name = if check_new.map(|o| o.status.success()).unwrap_or(false) {
                        info!("Found tmux session with new format: {}", tmux_name);
                        tmux_name
                    } else {
                        info!("Trying legacy tmux session name: {}", legacy_name);
                        legacy_name
                    };
                    Some(final_name)
                }
                Err(e) => {
                    // Couldn't resolve from the worktree (it's gone / never had a
                    // symlink). Fall back to the persisted store so we can still
                    // kill tmux and purge the record. Do NOT bail — bailing here is
                    // exactly what left orphaned entries stuck in the UI.
                    warn!(
                        "Could not derive tmux name from worktree for session {} ({}); \
                         falling back to sessions.json",
                        session_id, e
                    );
                    let store_name = SessionStore::load()
                        .sessions()
                        .values()
                        .find(|m| m.session_id == session_id)
                        .map(|m| m.tmux_session_name.clone());
                    if let Some(ref n) = store_name {
                        info!("Resolved tmux name from sessions.json: {}", n);
                    } else {
                        info!(
                            "No sessions.json entry for {} either — proceeding to cleanup by id",
                            session_id
                        );
                    }
                    store_name
                }
            }
        };

        if let Some(ref name) = tmux_session_name {
            info!("Attempting to kill tmux session: {}", name);
            // `=name` forces an exact target: bare `-t name` resolves exact, then
            // prefix, then fnmatch, so deleting "feat-auth" would kill a live
            // "feat-auth-2".
            let output = Command::new("tmux")
                .args(["kill-session", "-t", &format!("={name}")])
                .output()
                .await?;

            if output.status.success() {
                info!("Successfully killed tmux session: {}", name);
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("Failed to kill tmux session '{}': {}", name, stderr);
                // Continue anyway - session might already be dead
            }
        } else {
            info!(
                "No tmux session name to kill for {} — skipping tmux step",
                session_id
            );
        }

        // Step 2: Remove worktree
        //
        // Worktree removal must NEVER block the sessions.json cleanup in Step 3.
        // If it did, any session whose worktree is already gone (already deleted,
        // a shared worktree removed by a sibling session, or an orphaned entry that
        // never had a `by-session/<uuid>` symlink) could never be removed from the
        // store — every delete would die here and the record would reappear in the
        // UI on the next reload. A `NotFound` worktree already satisfies the
        // post-condition (no worktree on disk), so we treat it as success; for any
        // other error we log and still continue so the UI record is always cleared.
        info!("Attempting to remove worktree for session {}", session_id);
        match self.worktree_manager.remove_worktree(session_id) {
            Ok(()) => info!("Successfully removed worktree for session {}", session_id),
            Err(crate::git::WorktreeError::NotFound(path)) => {
                info!(
                    "Worktree for session {} already gone ({}) — continuing to store cleanup",
                    session_id, path
                );
            }
            Err(e) => {
                // Non-fatal: leave any on-disk worktree for separate GC, but still
                // purge the sessions.json record so the session leaves the UI.
                warn!(
                    "Failed to remove worktree for session {} ({}) — continuing to store cleanup",
                    session_id, e
                );
            }
        }

        // Step 3: Remove from sessions.json under the cross-process lock (pu4)
        // so a concurrent create/register can't resurrect the entry we delete.
        // Best-effort lock: if it can't be taken we still clean up (unlocked)
        // rather than leak the metadata — the guard is held across load+save and
        // dropped before the reap read below.
        let lock_guard = SessionStore::lock()
            .map_err(|e| {
                warn!("Failed to lock sessions.json for removal: {e}; proceeding unlocked");
            })
            .ok();
        let mut store = SessionStore::load();
        if let Some(ref name) = tmux_session_name {
            store.remove_by_tmux_name(name);
        }
        store.remove_by_session_id(session_id); // Also remove by ID in case tmux name changed
        if let Err(e) = store.save() {
            warn!("Failed to update sessions.json after removal: {}", e);
            // Continue anyway - removal was successful
        }
        drop(lock_guard);

        // Idle-reap: if no Headroom-enabled sessions remain, stop the shared
        // proxy so it doesn't linger after the last consumer is gone.
        // `headroom::stop()` is a no-op when the proxy wasn't ainb-spawned (no
        // pid file), so a user's own `headroom proxy` is never touched.
        let remaining_headroom = store.sessions.values().filter(|m| m.headroom_enabled).count();
        if remaining_headroom == 0 {
            info!("No Headroom sessions remain — reaping shared proxy");
            crate::headroom::stop();
        }

        info!(
            "<<< InteractiveSessionManager::remove_session() COMPLETE: {}",
            session_id
        );
        Ok(())
    }

    /// Check if a session is still alive (tmux session exists)
    ///
    /// # Arguments
    /// * `session_id` - UUID of the session to check
    ///
    /// # Returns
    /// * `Result<bool>` - True if session is alive, false otherwise
    pub async fn is_session_alive(
        &self,
        session_id: Uuid,
    ) -> Result<bool, InteractiveSessionError> {
        let session = self
            .active_sessions
            .get(&session_id)
            .ok_or(InteractiveSessionError::SessionNotFound(session_id))?;

        let output = Command::new("tmux")
            .args(["has-session", "-t", &session.tmux_session_name])
            .output()
            .await?;

        Ok(output.status.success())
    }

    /// Get a session by ID
    pub fn get_session(&self, session_id: Uuid) -> Option<&InteractiveSession> {
        self.active_sessions.get(&session_id)
    }

    /// Get all active sessions
    pub fn get_all_sessions(&self) -> Vec<&InteractiveSession> {
        self.active_sessions.values().collect()
    }

    // ===== Private Helper Methods =====

    /// Generate a tmux session name from worktree folder and branch name
    ///
    /// Format: tmux_{folder}_{branch}
    /// Sanitizes both folder and branch to be tmux-compatible
    fn generate_tmux_name(worktree_folder: &str, branch_name: &str) -> String {
        let sanitized_folder = worktree_folder
            .replace(' ', "_")
            .replace('.', "_")
            .replace('/', "_")
            .replace(':', "_");
        let sanitized_branch = branch_name
            .replace(' ', "_")
            .replace('.', "_")
            .replace('/', "_")
            .replace(':', "_");
        format!("tmux_{}_{}", sanitized_folder, sanitized_branch)
    }

    /// Generate legacy tmux session name (branch only) for backwards compatibility
    fn generate_tmux_name_legacy(branch_name: &str) -> String {
        let sanitized = branch_name
            .replace(' ', "_")
            .replace('.', "_")
            .replace('/', "_")
            .replace(':', "_");
        format!("tmux_{}", sanitized)
    }

    /// Extract folder name from a worktree path
    fn extract_worktree_folder(path: &Path) -> String {
        path.file_name().and_then(|n| n.to_str()).unwrap_or("session").to_string()
    }

    /// Start a new tmux session
    async fn start_tmux_session(
        &self,
        session_name: &str,
        work_dir: &Path,
    ) -> Result<(), InteractiveSessionError> {
        // Check if session already exists
        // Both targets are exact. `has-session -t name` prefix-matches, so a live
        // "feat-auth-2" would answer for "feat-auth" and the kill below would
        // then destroy it.
        let exact_target = format!("={session_name}");
        let check =
            Command::new("tmux").args(["has-session", "-t", &exact_target]).output().await?;

        if check.status.success() {
            warn!(
                "Tmux session '{}' already exists, killing it first",
                session_name
            );
            Command::new("tmux")
                .args(["kill-session", "-t", &exact_target])
                .output()
                .await?;
        }

        // Create new detached tmux session
        let output = Command::new("tmux")
            .args([
                "new-session",
                "-d", // Detached
                "-s",
                session_name,
                "-c",
                work_dir.to_str().context("Invalid work directory path")?,
                "-x",
                "120", // Width
                "-y",
                "40", // Height
            ])
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(InteractiveSessionError::Tmux(format!(
                "Failed to create tmux session '{}': {}",
                session_name, stderr
            )));
        }

        // Configure tmux session
        self.configure_tmux_session(session_name).await?;

        info!("Started tmux session: {}", session_name);
        Ok(())
    }

    /// Configure tmux session settings
    async fn configure_tmux_session(
        &self,
        session_name: &str,
    ) -> Result<(), InteractiveSessionError> {
        // Set history limit
        Command::new("tmux")
            .args(["set-option", "-t", session_name, "history-limit", "50000"])
            .status()
            .await?;

        // Enable mouse scrolling
        Command::new("tmux")
            .args(["set-option", "-t", session_name, "mouse", "on"])
            .status()
            .await?;

        // Configure clipboard integration
        crate::tmux::configure_clipboard(session_name).await.map_err(|e| {
            InteractiveSessionError::Tmux(format!("Failed to configure clipboard: {}", e))
        })?;

        // macOS: Configure reattach-to-user-namespace for audio/clipboard access
        // Uses centralized function with shell validation and proper error handling
        if let Err(e) = crate::tmux::configure_macos_user_namespace(session_name).await {
            warn!(
                "Failed to configure macOS user namespace for session {}: {}",
                session_name, e
            );
            // Continue anyway - this is optional functionality
        }

        Ok(())
    }

    /// Wait for the shell prompt to be ready in a tmux session
    ///
    /// Polls the tmux pane content until a shell prompt character appears,
    /// indicating the shell has initialized and is ready to receive commands.
    async fn wait_for_shell_ready(
        &self,
        session_name: &str,
    ) -> Result<(), InteractiveSessionError> {
        use tokio::time::{Duration, sleep};

        debug!("Waiting for shell prompt in session {}", session_name);

        // Two-stage detection:
        //   1. Wait for a prompt indicator (`$ % > #`) in the captured pane.
        //   2. Once seen, require the pane content to be STABLE — identical
        //      across 3 consecutive captures spanning ≥600ms — before
        //      declaring the shell ready. This catches heavy `.zshrc` setups
        //      (Powerlevel10k, conda init, NVM, etc.) that emit greeting
        //      lines AFTER the first prompt char appears, swallowing any
        //      keystrokes typed during the gap.
        //
        // Stevie 2026-05-27: previous 3s flat poll fired `claude` keystrokes
        // mid-`.zshrc`; the shell ate them and the user landed on an empty
        // zsh prompt, not claude. Stable-detection is the durable fix.
        const POLL_INTERVAL_MS: u64 = 100;
        const MAX_ATTEMPTS: usize = 150; // 15s hard cap
        const STABILITY_REQUIRED_POLLS: usize = 6; // 6 * 100ms = 600ms idle

        let mut last_content = String::new();
        let mut stable_count = 0usize;
        let mut saw_prompt = false;

        for attempt in 0..MAX_ATTEMPTS {
            let output = Command::new("tmux")
                .args(["capture-pane", "-t", session_name, "-p"])
                .output()
                .await?;
            let content = String::from_utf8_lossy(&output.stdout).to_string();

            let has_prompt = content.contains('$')
                || content.contains('%')
                || content.contains('>')
                || content.contains('#');

            if has_prompt {
                saw_prompt = true;
            }

            if saw_prompt {
                if content == last_content {
                    stable_count += 1;
                    if stable_count >= STABILITY_REQUIRED_POLLS {
                        debug!(
                            "Shell ready in session {} (stable for {} polls after {} attempts)",
                            session_name,
                            stable_count,
                            attempt + 1
                        );
                        // Safety pad — give the rendered prompt a moment to
                        // accept input even after stability is declared.
                        sleep(Duration::from_millis(200)).await;
                        return Ok(());
                    }
                } else {
                    stable_count = 0;
                    last_content = content;
                }
            } else {
                last_content = content;
            }

            sleep(Duration::from_millis(POLL_INTERVAL_MS)).await;
        }

        warn!(
            "Timeout waiting for shell to settle in session {} ({}ms); proceeding with extra pad",
            session_name,
            MAX_ATTEMPTS as u64 * POLL_INTERVAL_MS
        );
        // Even if we never declared stability, give the shell a 1s grace
        // period — better to wait a bit than lose keystrokes.
        sleep(Duration::from_millis(1000)).await;
        Ok(())
    }

    /// Assemble the provider CLI argv for a launch or resume. Pure (no I/O) so
    /// the resume/model/skip-permissions branching is unit-testable.
    ///
    /// `has_history` is `true` when the caller found a prior conversation for
    /// this cwd (gates Claude's `--continue`). See `start_cli_in_tmux` for the
    /// full resume semantics.
    pub(crate) fn build_cli_cmd_parts(
        provider: &crate::config::CliProvider,
        agent_type: SessionAgentType,
        skip_permissions: bool,
        model: Option<&str>,
        resume_requested: bool,
        has_history: bool,
    ) -> Vec<String> {
        let mut cmd_parts = vec![provider.command().to_string()];
        if agent_type == SessionAgentType::Codex {
            cmd_parts.extend([
                "-c".to_string(),
                "check_for_update_on_startup=false".to_string(),
            ]);
        }

        // Codex resume is a subcommand form. Global config overrides precede
        // `resume --last`; model and permission flags follow it as resume
        // subcommand options. `--last` continues the most recent
        // session in the current cwd (worktrees are 1-session-per-dir, so that
        // is "the last session"). Codex owns the cwd filtering.
        let codex_resume = agent_type == SessionAgentType::Codex && resume_requested;
        if codex_resume {
            cmd_parts.push("resume".to_string());
            cmd_parts.push("--last".to_string());
        }

        // AINB does not own provider model catalogs. Pass any non-default raw
        // value through unchanged; provider CLI performs validation.
        //
        // The one exception is a RETIRED Codex id. Persisted session metadata
        // and saved presets both reach this builder as opaque strings, so a
        // session created before a retirement would resume straight into the
        // provider's blocking migration modal and never start.
        if matches!(
            agent_type,
            SessionAgentType::Claude | SessionAgentType::Codex | SessionAgentType::Antigravity
        ) {
            if let Some(model) = model.map(str::trim).filter(|model| !is_default_model(model)) {
                let model = if agent_type == SessionAgentType::Codex {
                    migrated_codex_model(model)
                } else {
                    model.to_string()
                };
                cmd_parts.push("--model".to_string());
                cmd_parts.push(model);
            }
        }

        // Add skip permissions flag if specified (provider-specific). Valid
        // both for a fresh launch and after `codex resume --last`.
        if skip_permissions {
            cmd_parts.push(provider.skip_permissions_flag().to_string());
        }

        // Claude resume: `--continue` re-opens the most recent conversation in
        // the cwd (worktrees are 1-session-per-dir -> "the last session"). Only
        // add it when the caller found prior history, as `claude --continue`
        // with no history errors and leaves a dead pane. NOTE: the current
        // claude CLI `--resume` takes a *session id*, not a path, so the old
        // `--resume <jsonl path>` silently fell through to the interactive
        // picker instead of resuming - `--continue` is the correct "resume
        // latest" for a cwd-scoped session.
        if agent_type == SessionAgentType::Claude && resume_requested && has_history {
            cmd_parts.push("--continue".to_string());
        }

        // Copilot resume: `--continue` re-opens the most recent copilot session.
        // Unguarded (no cheap cwd-history probe exists yet) - mirrors codex's
        // tradeoff. NOTE: copilot's "most recent session" is not strictly
        // cwd-scoped the way claude's is, so with several copilot worktrees this
        // can resume the globally-newest session; acceptable for the first pass.
        if agent_type == SessionAgentType::Copilot && resume_requested {
            cmd_parts.push("--continue".to_string());
        }

        // Antigravity resume: `--continue` re-opens the most recent session.
        if agent_type == SessionAgentType::Antigravity && resume_requested {
            cmd_parts.push("--continue".to_string());
        }

        cmd_parts
    }

    /// Start AI CLI in the tmux session (Claude, Codex, Antigravity, or Gemini)
    ///
    /// **Resume** (`resume_requested == true`): re-open the most recent
    /// conversation in the session's cwd instead of starting fresh. Worktrees
    /// are 1-session-per-dir, so "most recent in cwd" == "the last session".
    ///   * Claude: emits `--continue`, but only when `resume_transcript.is_some()`
    ///     (the caller found prior history) - `claude --continue` with no
    ///     history errors and leaves a dead pane. `resume_transcript`'s *path*
    ///     is no longer used as an argument (the current claude CLI `--resume`
    ///     wants a session id, not a path); its presence is the history guard.
    ///   * Codex: emits the `resume --last` subcommand form.
    ///   * Copilot: emits `--continue` (most recent copilot session; unguarded).
    ///   * Antigravity: emits `--continue`.
    ///   * Gemini: no resume flag wired - always starts fresh.
    ///
    /// **Model flag emission:**
    ///   * Claude / Codex / Antigravity: pass the raw persisted value through unchanged.
    ///     `None`, empty, or `default` omits the flag.
    ///   * Gemini / Copilot: never emit `--model`.
    pub async fn start_cli_in_tmux(
        &self,
        session_name: &str,
        working_dir: &std::path::Path,
        skip_permissions: bool,
        model: Option<String>,
        agent_type: SessionAgentType,
        resume_transcript: Option<PathBuf>,
        resume_requested: bool,
        headroom_enabled: bool,
        codex_remote: Option<&ainb_hangar_proto::fleet::CodexSessionEnsureResult>,
    ) -> Result<(), InteractiveSessionError> {
        use crate::config::CliProvider;

        // No more send-keys + wait_for_shell_ready dance. Heavy `.zshrc`
        // setups (Powerlevel10k instant-prompt + conda + nvm + ...) can stream
        // content for 15+s, racing keystrokes that get eaten mid-startup.
        // Stevie 2026-05-27 hit this twice. Cure: `tmux respawn-pane -k`
        // REPLACES the pane's shell process with the CLI command directly.
        // No shell, no .zshrc, no race. Env (PATH etc.) is inherited from
        // the ainb-tui process which inherited Stevie's interactive shell.

        // Determine CLI provider from agent type
        let provider = match agent_type {
            SessionAgentType::Claude => CliProvider::Claude,
            SessionAgentType::Codex => CliProvider::Codex,
            SessionAgentType::Gemini => CliProvider::Gemini,
            SessionAgentType::Copilot => CliProvider::Copilot,
            SessionAgentType::Antigravity => CliProvider::Antigravity,
            _ => return Ok(()), // Shell and other types don't need CLI
        };

        // Ensure the shared Headroom proxy is running before we build the env
        // that points the CLI at it. On error we log a warning and continue —
        // the session still launches, just without working compression.
        //
        // #951 (Claude Code daemon bypass): NOT reproduced in ainb's launch
        // model. We respawn the pane with `export ANTHROPIC_BASE_URL=… && exec
        // claude`, so the CLI process inherits the override directly. Live-
        // verified 2026-06-19: routing a call through the proxy incremented its
        // request counter, so the env reaches the upstream request. The
        // headroom-issue fix (killing Claude Code's global daemon) is
        // deliberately NOT done here — it would disrupt unrelated sessions to
        // solve a problem we cannot reproduce. The statusline reflects ACTUAL
        // routing, so any real bypass would surface there rather than silently.
        //
        // Idle-reap is handled in `remove_session()` (stops the shared proxy
        // when the last Headroom session closes).
        // The toggle being on is *intent*; routing only happens if the proxy is
        // actually healthy. If ensure fails (binary missing, port 8787 taken,
        // crash), DEGRADE to direct — do NOT inject a base URL pointing at a
        // dead port, which would brick the session with connection-refused.
        let mut headroom_active = headroom_enabled
            && matches!(
                agent_type,
                SessionAgentType::Claude | SessionAgentType::Codex
            );
        if headroom_active {
            if let Err(e) = crate::headroom::ensure_proxy_running().await {
                warn!("headroom proxy unavailable — running DIRECT, no compression: {e}");
                headroom_active = false;
            }
        }

        // Build environment setup for API key injection. `headroom_active`
        // (not the raw toggle) decides injection, so a failed proxy degrades to
        // direct rather than a dead-port URL.
        let env_setup = Self::build_env_setup_for_provider(agent_type, headroom_active);

        // Build the CLI command with appropriate flags. Pure assembly lives in
        // `build_cli_cmd_parts` (unit-tested); `resume_transcript.is_some()` is
        // the "has prior history" guard for Claude's `--continue`.
        let cmd_parts = if let Some(remote) = codex_remote {
            if agent_type != SessionAgentType::Codex {
                return Err(InteractiveSessionError::Other(anyhow::anyhow!(
                    "remote Codex thread used for a non-Codex session"
                )));
            }
            // `-C <working_dir>` is REQUIRED, not redundant. Under `--remote`
            // the TUI ignores the pane's cwd and adopts the app-server's
            // working directory, so dropping `-C` silently runs every session
            // in whatever tree the daemon happens to sit in rather than the
            // session's worktree. Verified on codex 0.149.1: a pane opened in
            // one git repo reported the daemon's directory and branch instead.
            //
            // `--dangerously-bypass-hook-trust` addresses a launch stall Ainb
            // causes itself: Codex pins each hook by POSITION
            // (`<file>:<event>:<group>:<idx>`), and `ainb notifyd install`
            // rewrites ~/.codex/hooks.json, so our own installer invalidates
            // those hashes and parks the launch on a blocking "Hooks need
            // review" modal. The TUI then never creates a thread and our 10s
            // claim deadline reports "Codex failed to start". Ainb wrote those
            // hooks; re-confirming them from a detached tmux pane is not a
            // decision the user can act on.
            // Trust the worktree before launching: `-C` into a directory Codex
            // has not seen shows a blocking modal, and under `--remote` no flag
            // suppresses it.
            trust_codex_project_dir(working_dir);
            codex_remote_command(
                &provider,
                remote,
                working_dir,
                model.as_deref(),
                skip_permissions,
            )
        } else {
            Self::build_cli_cmd_parts(
                &provider,
                agent_type,
                skip_permissions,
                model.as_deref(),
                resume_requested,
                resume_transcript.is_some(),
            )
        };

        let cli_cmd = cmd_parts.join(" ");

        info!(
            "Starting {} with command: {}",
            provider.display_name(),
            cli_cmd
        );

        // Target the session by NAME only — tmux resolves to the active pane
        // of the active window, regardless of `base-index` / `pane-base-index`.
        // Hardcoding `:0` broke on Stevie's config (base-index 1) with
        // "can't find window: 0" — the actual root cause of the empty
        // sessions (2026-05-27), not the shell race.
        let target = session_name.to_string();

        // First: set `remain-on-exit` so the pane stays visible if the CLI
        // exits/crashes (e.g. claude can't auth, binary not on PATH) — the
        // user sees the error instead of an empty closed pane.
        let _ = Command::new("tmux")
            .args([
                "set-option",
                "-w", // remain-on-exit is a window option
                "-t",
                &target,
                "remain-on-exit",
                "on",
            ])
            .output()
            .await;

        // Then: respawn-pane KILLS the pane's current process (the still-
        // loading shell) and starts the CLI command directly. The pane's
        // cwd from `tmux new-session -c <work_dir>` is preserved. Env (PATH,
        // ANTHROPIC_API_KEY, etc.) inherits from the ainb-tui process.
        //
        // If `env_setup` contains an inline export (legacy path for API-key
        // injection), we have to wrap in `sh -c '...'` since `respawn-pane`
        // takes a single command, not a shell line. Without env_setup we
        // pass argv directly for max speed and no shell-parsing surprises.
        // `-c <working_dir>` states the cwd instead of inheriting it. The pane
        // already carries it from `tmux new-session -c`, but the Codex launch no
        // longer passes `-C`, so the cwd is now load-bearing rather than
        // belt-and-braces: if a pane ever came from elsewhere, Codex would open
        // on the wrong tree silently.
        let working_dir = working_dir.to_string_lossy().into_owned();
        let output = if env_setup.trim().is_empty() {
            // Pure argv path — fastest, no shell.
            let mut tmux_args: Vec<String> = vec![
                "respawn-pane".to_string(),
                "-k".to_string(), // Kill current pane process first
                "-c".to_string(),
                working_dir.clone(),
                "-t".to_string(),
                target.clone(),
            ];
            tmux_args.extend(cmd_parts.iter().cloned());
            Command::new("tmux").args(&tmux_args).output().await?
        } else {
            // Env-setup path: wrap in `sh -c` so the inline `export FOO=bar; …`
            // gets evaluated. Use sh (not zsh) — no .zshrc, no race.
            let escaped_cmd = cmd_parts
                .iter()
                .map(|part| shell_escape::escape(part.into()).into_owned())
                .collect::<Vec<_>>()
                .join(" ");
            let full_line = format!("{env_setup}exec {escaped_cmd}");
            Command::new("tmux")
                .args([
                    "respawn-pane",
                    "-k",
                    "-c",
                    &working_dir,
                    "-t",
                    &target,
                    "sh",
                    "-c",
                    &full_line,
                ])
                .output()
                .await?
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(InteractiveSessionError::Tmux(format!(
                "Failed to start {} in tmux: {}",
                provider.display_name(),
                stderr
            )));
        }

        info!(
            "Started {} CLI in tmux session: {} (model={:?}, skip_permissions={})",
            provider.display_name(),
            session_name,
            model,
            skip_permissions
        );
        Ok(())
    }

    /// Build environment setup for injecting API key if using ApiKey auth mode
    fn build_env_setup() -> String {
        use crate::config::{AppConfig, ClaudeAuthProvider};
        use crate::credentials;

        // Check auth provider from config
        let auth_provider = AppConfig::load()
            .map(|c| c.authentication.claude_provider.clone())
            .unwrap_or(ClaudeAuthProvider::SystemAuth);

        // Only inject API key if using ApiKey auth mode (not Pro/Max subscription)
        if matches!(auth_provider, ClaudeAuthProvider::ApiKey) {
            if let Ok(Some(api_key)) = credentials::get_anthropic_api_key() {
                info!("Injecting ANTHROPIC_API_KEY for API key auth mode");
                return format!("export ANTHROPIC_API_KEY='{}' && ", api_key);
            } else {
                warn!("ApiKey auth mode configured but no API key found in keychain");
            }
        }

        String::new()
    }

    /// Build environment setup for injecting API key based on provider
    fn build_env_setup_for_provider(
        agent_type: SessionAgentType,
        headroom_enabled: bool,
    ) -> String {
        use crate::credentials;

        let headroom_prefix = headroom_env_prefix(agent_type, headroom_enabled);

        let base = match agent_type {
            SessionAgentType::Claude => Self::build_env_setup(),
            SessionAgentType::Codex => {
                if let Ok(Some(api_key)) = credentials::get_openai_api_key() {
                    info!("Injecting OPENAI_API_KEY for Codex CLI");
                    format!("export OPENAI_API_KEY='{}' && ", api_key)
                } else {
                    String::new()
                }
            }
            SessionAgentType::Gemini | SessionAgentType::Antigravity => {
                if let Ok(Some(api_key)) = credentials::get_gemini_api_key() {
                    info!("Injecting GEMINI_API_KEY for {:?} CLI", agent_type);
                    format!("export GEMINI_API_KEY='{}' && ", api_key)
                } else {
                    String::new()
                }
            }
            SessionAgentType::Copilot => {
                // Copilot authenticates via `gh`/device flow by default; if the
                // user stored a PAT in onboarding, inject it as GITHUB_TOKEN.
                if let Ok(Some(pat)) =
                    credentials::get_credential(credentials::CredentialKey::GithubPat)
                {
                    info!("Injecting GITHUB_TOKEN for Copilot CLI");
                    format!("export GITHUB_TOKEN='{}' && ", pat)
                } else {
                    String::new()
                }
            }
            _ => String::new(),
        };

        // Point the agent's OTLP exporter at the local Alloy collector. The
        // spawned pane runs `sh -c` (non-interactive — never sources the
        // shell rc that would otherwise export these), so inject directly.
        // Empty when OTEL isn't configured.
        let otel_prefix = crate::otel::session_otlp_exports();

        format!("{headroom_prefix}{otel_prefix}{base}")
    }
}

/// Convert InteractiveSession to Session model for UI
impl InteractiveSession {
    pub fn to_session_model(&self) -> Session {
        let mut session = Session::new_with_options(
            self.workspace_name.clone(),
            self.worktree_path.to_string_lossy().to_string(),
            self.skip_permissions,
            SessionMode::Interactive,
            None, // boss_prompt
            self.agent_type,
            self.model.clone(),
        );

        session.id = self.session_id;
        session.branch_name = self.branch_name.clone();
        session.tmux_session_name = Some(self.tmux_session_name.clone());
        session.container_id = None; // No Docker container
        session.status = SessionStatus::Running; // If tmux session exists, it's running
        session.created_at = self.created_at;

        session
    }
}

/// Write an RTK `PreToolUse` hook entry into `<worktree>/.claude/settings.json`.
///
/// Merge-only, idempotent: reads existing JSON, appends only when no rtk hook
/// is already present, and preserves all other settings (never clobbers).
/// Best-effort — call sites log warn on error but must not propagate it.
fn wire_rtk_project_hook(worktree: &std::path::Path) -> anyhow::Result<()> {
    let Some(cmd) = crate::rtk::project_hook_command() else {
        warn!("rtk binary not found — skipping project hook wiring");
        return Ok(());
    };
    wire_rtk_project_hook_with_cmd(worktree, &cmd)
}

/// Inner merge, parameterised on the resolved hook `cmd` so tests can exercise
/// the real read-merge-write path without rtk on PATH.
fn wire_rtk_project_hook_with_cmd(worktree: &std::path::Path, cmd: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;

    let claude_dir = worktree.join(".claude");
    let settings_path = claude_dir.join("settings.json");

    let mut root: serde_json::Value = if settings_path.exists() {
        let raw = std::fs::read_to_string(&settings_path)
            .with_context(|| format!("read {}", settings_path.display()))?;
        match serde_json::from_str(&raw) {
            Ok(v) => v,
            // Never overwrite a settings.json we couldn't parse — it may hold
            // the user's own hooks/config. Leave it untouched; the session
            // just launches without the rtk hook.
            Err(e) => {
                warn!(
                    "{} is not valid JSON ({e}) — leaving it untouched, session launches without rtk hook",
                    settings_path.display()
                );
                return Ok(());
            }
        }
    } else {
        serde_json::Value::Object(serde_json::Map::new())
    };

    // Ensure hooks.PreToolUse is an array.
    let pre_tool_use = root
        .as_object_mut()
        .context("settings.json root is not an object")?
        .entry("hooks")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()))
        .as_object_mut()
        .context("hooks is not an object")?
        .entry("PreToolUse")
        .or_insert_with(|| serde_json::Value::Array(vec![]));

    let arr = pre_tool_use.as_array_mut().context("PreToolUse is not an array")?;

    // Idempotent: skip if an rtk hook is already wired. Match the exact command
    // or any command containing `rtk hook claude` (the install path may differ)
    // — tighter than a bare "hook claude" substring, which would false-match
    // unrelated user commands.
    let already_present = arr.iter().any(|entry| {
        entry.get("hooks").and_then(|h| h.as_array()).map_or(false, |hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map_or(false, |c| c == cmd || c.contains("rtk hook claude"))
            })
        })
    });

    // Nothing to add → don't touch the file (avoids mtime churn / reformatting
    // a user's compact JSON on every launch).
    if already_present {
        return Ok(());
    }

    arr.push(serde_json::json!({
        "matcher": "Bash",
        "hooks": [{"type": "command", "command": cmd}]
    }));

    std::fs::create_dir_all(&claude_dir)
        .with_context(|| format!("create dir {}", claude_dir.display()))?;

    let content = serde_json::to_string_pretty(&root).context("serialize settings.json")?;

    // Atomic write: tmp in the same dir + rename, so a crash / full disk
    // mid-write can't truncate a settings.json that may hold unrelated user
    // hooks. rename(2) is atomic on POSIX within a filesystem.
    let tmp = settings_path.with_extension("json.tmp");
    if let Err(e) =
        std::fs::write(&tmp, &content).and_then(|_| std::fs::rename(&tmp, &settings_path))
    {
        let _ = std::fs::remove_file(&tmp);
        warn!(
            "failed to write {}: {} — session launches without rtk hook",
            settings_path.display(),
            e
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    /// The managed Codex argv, flag by flag.
    ///
    /// Each of these was a real outage today, so they are pinned rather than
    /// trusted to survive future edits.
    #[test]
    fn codex_remote_command_carries_every_load_bearing_flag() {
        use ainb_hangar_proto::fleet::CodexSessionEnsureResult;
        let provider = crate::config::CliProvider::Codex;
        let worktree = std::path::Path::new("/w/agents-in-a-box--f-x--abc123");

        let with_thread = CodexSessionEnsureResult {
            thread_id: Some("01a03ff4-efbb".to_string()),
            endpoint: "unix:///tmp/app-server-control.sock".to_string(),
        };
        let argv = super::codex_remote_command(&provider, &with_thread, worktree, None, true);
        let joined = argv.join(" ");

        // Joins OUR thread, so the CLI and the phone share one conversation.
        let at = argv.iter().position(|a| a == "resume").expect("resume missing");
        assert_eq!(argv.get(at + 1).map(String::as_str), Some("01a03ff4-efbb"));

        // Runs in the session's own worktree, not the daemon's tree.
        let cd = argv.iter().position(|a| a == "-C").expect("-C missing");
        assert_eq!(
            argv.get(cd + 1).map(String::as_str),
            Some(worktree.to_str().unwrap())
        );

        assert!(
            joined.contains("--dangerously-bypass-hook-trust"),
            "hook trust: {joined}"
        );
        assert!(joined.contains("--remote unix:///tmp/app-server-control.sock"));
        assert!(joined.contains("check_for_update_on_startup=false"));

        // No thread id yet (the fallback claim path) must NOT emit a bare
        // `resume`, which would resume an arbitrary thread.
        let without = CodexSessionEnsureResult {
            thread_id: None,
            endpoint: "unix:///tmp/app-server-control.sock".to_string(),
        };
        let argv = super::codex_remote_command(&provider, &without, worktree, None, false);
        assert!(!argv.iter().any(|a| a == "resume"), "bare resume: {argv:?}");
        // …and still lands in the right worktree.
        let cd = argv.iter().position(|a| a == "-C").expect("-C missing");
        assert_eq!(
            argv.get(cd + 1).map(String::as_str),
            Some(worktree.to_str().unwrap())
        );

        // skip_permissions emits the provider's own flag, not a hard-coded one.
        let yolo = super::codex_remote_command(&provider, &without, worktree, None, true).join(" ");
        assert!(
            yolo.contains(provider.skip_permissions_flag()),
            "skip flag: {yolo}"
        );
        let safe =
            super::codex_remote_command(&provider, &without, worktree, None, false).join(" ");
        assert!(
            !safe.contains(provider.skip_permissions_flag()),
            "leaked skip: {safe}"
        );

        // `--disable apps` travels as two adjacent tokens, not a fused string.
        let d = argv.iter().position(|a| a == "--disable").expect("--disable missing");
        assert_eq!(argv.get(d + 1).map(String::as_str), Some("apps"));
    }

    /// The `--model` branch: default-ish values omitted, real ones passed, and
    /// a retiring slug rewritten rather than launched.
    ///
    /// Launching a retiring model shows a blocking deprecation modal, one of
    /// the three stalls this argv exists to avoid.
    #[test]
    fn codex_remote_command_filters_and_migrates_the_model() {
        use ainb_hangar_proto::fleet::CodexSessionEnsureResult;
        let provider = crate::config::CliProvider::Codex;
        let worktree = std::path::Path::new("/w/tree");
        let remote = CodexSessionEnsureResult {
            thread_id: None,
            endpoint: "unix:///tmp/s.sock".to_string(),
        };
        let argv_for = |model: Option<&str>| {
            super::codex_remote_command(&provider, &remote, worktree, model, false)
        };

        // Default-ish models must not emit --model: a literal "default" would
        // send a slug that does not exist.
        for omitted in [None, Some(""), Some("default")] {
            let argv = argv_for(omitted);
            assert!(
                !argv.iter().any(|a| a == "--model"),
                "--model emitted for {omitted:?}: {argv:?}"
            );
        }

        let argv = argv_for(Some("gpt-5.6-terra"));
        let at = argv.iter().position(|a| a == "--model").expect("--model missing");
        assert_eq!(argv.get(at + 1).map(String::as_str), Some("gpt-5.6-terra"));

        // Whatever is emitted has been through the retirement rewrite, so a
        // retiring slug can never reach the launch as-is.
        assert_eq!(argv[at + 1], super::migrated_codex_model("gpt-5.6-terra"));
    }

    /// Recording worktree trust must not disturb ANY other key.
    ///
    /// This writes into the user's own `~/.codex/config.toml`, shared with
    /// Codex Desktop and the CLI. The permission is narrow on purpose: one key,
    /// `[projects."<path>"] trust_level`, for a directory Ainb created. Model,
    /// approval policy, sandbox, MCP servers and hooks are the user's.
    #[test]
    fn trusting_a_worktree_touches_only_its_own_projects_key() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        std::fs::create_dir_all(&codex).unwrap();
        let config = codex.join("config.toml");
        let original = r#"# a comment the user wrote
model = "gpt-5.6-terra"
approval_policy = "on-request"

[features]
hooks = true

[mcp_servers.hangy]
command = "hangy"

[projects."/already/trusted"]
trust_level = "trusted"
"#;
        std::fs::write(&config, original).unwrap();

        // Drive the same edit the launcher performs, against this temp home.
        let worktree = std::path::Path::new("/tmp/ainb-made-this");
        let mut doc = original.parse::<toml_edit::DocumentMut>().unwrap();
        let key = worktree.display().to_string();
        let projects =
            doc.entry("projects").or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        let projects = projects.as_table_mut().unwrap();
        projects.set_implicit(true);
        let entry = projects.entry(&key).or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
        entry.as_table_mut().unwrap()["trust_level"] = toml_edit::value("trusted");
        let updated = doc.to_string();

        // The new entry landed.
        assert!(updated.contains(r#"[projects."/tmp/ainb-made-this"]"#));

        // Everything else is byte-identical, comment included.
        assert!(updated.contains("# a comment the user wrote"));
        for untouched in [
            r#"model = "gpt-5.6-terra""#,
            r#"approval_policy = "on-request""#,
            "[features]",
            "hooks = true",
            "[mcp_servers.hangy]",
            r#"command = "hangy""#,
            r#"[projects."/already/trusted"]"#,
        ] {
            assert!(updated.contains(untouched), "clobbered: {untouched}");
        }

        // Structural check rather than a line diff: `trust_level = "trusted"`
        // already appears under /already/trusted, so a raw line diff would
        // dedupe the new one away and prove nothing.
        let before = original.parse::<toml_edit::DocumentMut>().unwrap();
        let after = updated.parse::<toml_edit::DocumentMut>().unwrap();

        // Exactly one new top-level key at most (`projects` already existed).
        let before_keys: Vec<&str> = before.as_table().iter().map(|(k, _)| k).collect();
        let after_keys: Vec<&str> = after.as_table().iter().map(|(k, _)| k).collect();
        assert_eq!(before_keys, after_keys, "top-level keys changed");

        // Every pre-existing top-level value is untouched.
        for (key, value) in before.as_table().iter() {
            if key == "projects" {
                continue;
            }
            assert_eq!(
                value.to_string(),
                after[key].to_string(),
                "`{key}` was modified"
            );
        }

        // Under [projects], exactly one entry was added and none changed.
        let before_projects: Vec<&str> =
            before["projects"].as_table().unwrap().iter().map(|(k, _)| k).collect();
        let after_projects: Vec<&str> =
            after["projects"].as_table().unwrap().iter().map(|(k, _)| k).collect();
        assert_eq!(after_projects.len(), before_projects.len() + 1);
        for existing in &before_projects {
            assert!(
                after_projects.contains(existing),
                "dropped project {existing}"
            );
        }
        assert_eq!(
            after["projects"]["/tmp/ainb-made-this"]["trust_level"].as_str(),
            Some("trusted")
        );
    }

    /// A retiring model is swapped for the replacement Codex advertises.
    ///
    /// Launching the old slug shows a blocking migration modal, so the session
    /// never starts. gpt-5.4 and gpt-5.4-mini retire 2026-08-31.
    #[test]
    fn a_retiring_model_is_replaced_by_its_advertised_upgrade() {
        let cache = serde_json::json!({
            "models": [
                {"slug": "gpt-5.6-terra", "upgrade": null},
                {"slug": "gpt-5.4", "upgrade": {
                    "model": "gpt-5.6-terra",
                    "retirement_at": "2026-08-31T19:00:00Z"
                }},
            ]
        });
        let models = cache["models"].as_array().unwrap();
        let pick = |slug: &str| -> String {
            models
                .iter()
                .find(|m| m["slug"] == slug)
                .and_then(|m| m.get("upgrade"))
                .filter(|u| !u.is_null())
                .and_then(|u| u.get("model"))
                .and_then(|m| m.as_str())
                .unwrap_or(slug)
                .to_string()
        };
        assert_eq!(
            pick("gpt-5.4"),
            "gpt-5.6-terra",
            "retiring model must be replaced"
        );
        assert_eq!(
            pick("gpt-5.6-terra"),
            "gpt-5.6-terra",
            "a current model is left alone"
        );
        assert_eq!(
            pick("gpt-unknown"),
            "gpt-unknown",
            "an unknown model is left alone"
        );
    }

    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn failed_launch_rollback_removes_only_exact_owned_resources() {
        if !Command::new("tmux")
            .arg("-V")
            .output()
            .await
            .map(|output| output.status.success())
            .unwrap_or(false)
        {
            return;
        }

        let _env = crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().expect("temp home");
        let prior_home = std::env::var_os("AINB_HOME");
        std::env::set_var("AINB_HOME", home.path());

        struct RestoreHome(Option<std::ffi::OsString>);
        impl Drop for RestoreHome {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(home) => std::env::set_var("AINB_HOME", home),
                    None => std::env::remove_var("AINB_HOME"),
                }
            }
        }
        let _restore_home = RestoreHome(prior_home);

        struct ExactTmuxCleanup(Vec<String>);
        impl Drop for ExactTmuxCleanup {
            fn drop(&mut self) {
                for name in &self.0 {
                    let exact_target = format!("={name}");
                    let _ = std::process::Command::new("tmux")
                        .args(["kill-session", "-t", &exact_target])
                        .output();
                }
            }
        }

        let session_id = Uuid::new_v4();
        let failed_tmux = format!("ainb-failed-launch-{session_id}");
        let sibling_tmux = format!("ainb-sibling-launch-{}", Uuid::new_v4());
        let prefix_tmux = format!("{failed_tmux}-still-running");
        let _tmux_cleanup = ExactTmuxCleanup(vec![
            failed_tmux.clone(),
            sibling_tmux.clone(),
            prefix_tmux.clone(),
        ]);
        for name in [&failed_tmux, &sibling_tmux, &prefix_tmux] {
            assert!(
                std::process::Command::new("tmux")
                    .args(["new-session", "-d", "-s", name])
                    .status()
                    .expect("create tmux session")
                    .success()
            );
        }

        let worktree_manager = WorktreeManager::new().expect("worktree manager");
        let worktree = worktree_manager.base_dir().join("by-name").join("failed-launch");
        std::fs::create_dir_all(&worktree).expect("create worktree");
        let session_link =
            worktree_manager.base_dir().join("by-session").join(session_id.to_string());
        std::os::unix::fs::symlink(&worktree, &session_link).expect("link worktree");

        let sibling_id = Uuid::new_v4();
        let mut store = SessionStore::default();
        for (id, name, path) in [
            (session_id, failed_tmux.clone(), worktree.clone()),
            (sibling_id, sibling_tmux.clone(), PathBuf::from("/keep")),
        ] {
            store.upsert(SessionMetadata {
                session_id: id,
                tmux_session_name: name,
                worktree_path: path,
                workspace_name: "test".to_string(),
                created_at: Utc::now(),
                agent_type: SessionAgentType::Codex,
                headroom_enabled: false,
                rtk_enabled: false,
                skip_permissions: Some(false),
                model: None,
                model_source: ModelSource::Raw,
                codex_model: None,
                codex_thread_id: None,
            });
        }
        store.save().expect("seed store");

        rollback_failed_interactive_launch(session_id, Some(&failed_tmux), Some(&worktree_manager))
            .await;

        assert!(!worktree.exists());
        assert!(std::fs::symlink_metadata(&session_link).is_err());
        assert!(
            !std::process::Command::new("tmux")
                .args(["has-session", "-t", &format!("={failed_tmux}")])
                .output()
                .expect("check failed tmux")
                .status
                .success()
        );
        assert!(
            std::process::Command::new("tmux")
                .args(["has-session", "-t", &format!("={sibling_tmux}")])
                .output()
                .expect("check sibling tmux")
                .status
                .success()
        );
        assert!(
            std::process::Command::new("tmux")
                .args(["has-session", "-t", &format!("={prefix_tmux}")])
                .output()
                .expect("check prefix tmux")
                .status
                .success()
        );

        rollback_failed_interactive_launch(session_id, Some(&failed_tmux), None).await;
        assert!(
            std::process::Command::new("tmux")
                .args(["has-session", "-t", &format!("={prefix_tmux}")])
                .output()
                .expect("check prefix tmux after missing exact target")
                .status
                .success()
        );
        let store = SessionStore::load();
        assert!(store.find_by_tmux_name(&failed_tmux).is_none());
        assert!(store.find_by_tmux_name(&sibling_tmux).is_some());
    }

    #[test]
    fn shared_remote_codex_failures_show_a_short_next_action() {
        assert_eq!(
            format_codex_remote_control_failure(
                "Codex app-server WebSocket handshake failed: invalid token"
            ),
            "Codex bridge conflict. Restart Ainb, then retry."
        );
        assert_eq!(
            format_codex_remote_control_failure("Codex manager command timed out"),
            "Codex bridge starting. Retry session in 5 seconds."
        );
        assert_eq!(
            format_codex_remote_control_failure(
                "installed Codex cannot generate app-server schema"
            ),
            "Codex unavailable. Update Codex, then retry."
        );
        assert_eq!(
            format_codex_remote_control_failure("daemon RPC rejected request"),
            "Codex remote control unavailable. Check Hangar logs, then retry."
        );
    }

    #[tokio::test]
    async fn shared_remote_codex_rejects_headroom_before_daemon_startup() {
        let error = ensure_codex_remote_thread(
            uuid::Uuid::new_v4(),
            Path::new("/worktree"),
            None,
            false,
            true,
            None,
        )
        .await
        .expect_err("Headroom cannot be routed through the shared app-server");
        assert!(error.to_string().contains("Headroom is unavailable"));
    }

    #[test]
    fn headroom_env_prefix_routes_claude_and_codex_only() {
        use crate::models::session::SessionAgentType;

        // Hold the shared env lock + force the default port so the base URL is
        // deterministic regardless of other tests mutating AINB_HEADROOM_PORT.
        let _guard = crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap();
        let old = std::env::var_os("AINB_HEADROOM_PORT");
        std::env::remove_var("AINB_HEADROOM_PORT");

        // Disabled → no injection for any provider.
        assert_eq!(headroom_env_prefix(SessionAgentType::Claude, false), "");
        assert_eq!(headroom_env_prefix(SessionAgentType::Codex, false), "");

        // Enabled → Claude routes via ANTHROPIC_BASE_URL, Codex via OPENAI_BASE_URL/v1.
        let base = headroom_base_url();
        assert_eq!(
            headroom_env_prefix(SessionAgentType::Claude, true),
            format!("export ANTHROPIC_BASE_URL='{base}' && ")
        );
        assert_eq!(
            headroom_env_prefix(SessionAgentType::Codex, true),
            format!("export OPENAI_BASE_URL='{base}/v1' && ")
        );

        // Providers Headroom can't proxy → empty even when enabled (gating).
        assert_eq!(headroom_env_prefix(SessionAgentType::Gemini, true), "");
        assert_eq!(headroom_env_prefix(SessionAgentType::Copilot, true), "");

        if let Some(v) = old {
            std::env::set_var("AINB_HEADROOM_PORT", v);
        }
    }

    #[test]
    fn headroom_base_url_honors_port_override() {
        let _guard = crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap();
        let key = "AINB_HEADROOM_PORT";
        let old = std::env::var_os(key);

        // Unset → documented default 8787.
        std::env::remove_var(key);
        assert!(headroom_base_url().ends_with(&HEADROOM_DEFAULT_PORT.to_string()));

        // Override → reflected in the base URL.
        std::env::set_var(key, "9191");
        assert!(headroom_base_url().ends_with(":9191"));

        match old {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    /// ccc / D11 schema-compat guard: a session the hangar daemon registers
    /// through the `ainb-fleet-core` seam must round-trip back through THIS
    /// crate's `SessionStore` — the same `sessions.json` file, the same schema —
    /// so `ainb list` (and thus fleet discover / standup / broadcast) sees a
    /// daemon-spawned interactive session. The daemon writes only the REQUIRED
    /// fields; the `#[serde(default)]` fields (`agent_type` / `headroom_enabled` /
    /// `rtk_enabled`) it omits must default cleanly on read. If a future field
    /// becomes non-default, this test fails — the drift alarm.
    #[test]
    fn daemon_registered_session_round_trips_through_session_store() {
        use ainb_fleet_core::session_registry::{AinbSessionRecord, register_session_at};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.json");
        let rec = AinbSessionRecord::new(
            "tmux_hangar-01HZ",
            PathBuf::from("/work/ws/01HZ/workdir"),
            "myproj",
        );
        register_session_at(&path, &rec).unwrap();

        // Read the file back through the CLI's store type — the exact `ainb list` path.
        let content = std::fs::read_to_string(&path).unwrap();
        let store: SessionStore = serde_json::from_str(&content).unwrap();
        let meta = store
            .find_by_tmux_name("tmux_hangar-01HZ")
            .expect("daemon-registered session must be readable by SessionStore");

        assert_eq!(meta.session_id, rec.session_id);
        assert_eq!(meta.tmux_session_name, "tmux_hangar-01HZ");
        assert_eq!(meta.worktree_path, PathBuf::from("/work/ws/01HZ/workdir"));
        assert_eq!(meta.workspace_name, "myproj");
        // The omitted optionals default cleanly (no drift).
        assert!(!meta.headroom_enabled);
        assert!(!meta.rtk_enabled);
    }

    #[test]
    fn test_derive_workspace_name() {
        // Legacy worktree layout: <repo>--<hash>--<session-id>
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(
                Path::new("/wt/by-name/nanoclaw--ops-main--5950b4bd"),
                Path::new("/repos/nanoclaw"),
            ),
            "nanoclaw"
        );

        // Flat layout (no `--`): must fall back to source_repository basename,
        // not return the full directory name.
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(
                Path::new("/wt/shotclubhouse_shotclubhouse_temp_debug"),
                Path::new("/repos/shotclubhouse"),
            ),
            "shotclubhouse"
        );

        // Both unusable → "unknown" (root paths have no `file_name`,
        // so the broken-sentinel branch is skipped).
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(Path::new("/"), Path::new("/")),
            "unknown"
        );

        // Broken worktree: source_repository collapsed onto worktree_path
        // (caller's get_source_repository() returned None and used the
        // worktree_path as fallback). Must surface "(broken)" rather than
        // the doubled-prefix dir name.
        let dead = Path::new("/wt/shotclubhouse_shotclubhouse_fix_all-bugs");
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(dead, dead),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME
        );
    }

    /// An ATC control directory is a healthy session root that is deliberately
    /// NOT a git worktree, so every rule in `derive_workspace_name` used to
    /// miss and it rendered as `(broken)`: a live ATC instance was
    /// indistinguishable from a dead worktree in the TUI and in `ainb list`.
    #[test]
    fn atc_control_dir_renders_as_an_atc_instance() {
        // AINB_HOME is process-global; serialise with every other env-mutating
        // test in the crate via the shared lock.
        let _env = crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap();

        let home = tempfile::tempdir().expect("tempdir");
        // Save and restore rather than blindly removing: a runner that sets
        // AINB_HOME itself would otherwise have it deleted by this test, and
        // every later test in the process would resolve against the wrong home.
        let prior_home = std::env::var_os("AINB_HOME");
        std::env::set_var("AINB_HOME", home.path());

        // `atc_root()` is `$AINB_HOME/atc`. Note this is NOT the same
        // convention as `SessionStore::storage_path()`, which treats AINB_HOME
        // as the PARENT of `.agents-in-a-box/`.
        let atc_root = home.path().join("atc");

        // A provisioned instance: a direct child of the ATC root carrying the
        // meta.json that `atc setup` writes.
        let instance = atc_root.join("main");
        std::fs::create_dir_all(&instance).unwrap();
        std::fs::write(instance.join("meta.json"), "{}").unwrap();

        // The loaders collapse `source_repository` onto `worktree_path`
        // whenever the repository lookup finds nothing, which is exactly the
        // shape an ATC dir produces.
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(&instance, &instance),
            "atc:main"
        );
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&instance),
            "atc:main"
        );

        // An instance name may legally contain `-`, so `--` is reachable. The
        // ATC check runs before the legacy `<repo>--<branch>--<id>` rule, so
        // the name survives whole instead of being chopped at the first `--`.
        let hyphenated = atc_root.join("ops--main");
        std::fs::create_dir_all(&hyphenated).unwrap();
        std::fs::write(hyphenated.join("meta.json"), "{}").unwrap();
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&hyphenated),
            "atc:ops--main"
        );

        // A directory under the ATC root that was never provisioned (no
        // meta.json) is not claimed, and still reports the sentinel.
        let stray = atc_root.join("not-an-instance");
        std::fs::create_dir_all(&stray).unwrap();
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&stray),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME
        );

        // A genuinely dead worktree elsewhere on disk is untouched.
        let dead = home.path().join("shotclubhouse_shotclubhouse_fix_all-bugs");
        std::fs::create_dir_all(&dead).unwrap();
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&dead),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME
        );

        // And an ordinary ainb worktree still derives from its own layout.
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(
                Path::new("/wt/by-name/nanoclaw--ops-main--5950b4bd"),
                Path::new("/repos/nanoclaw"),
            ),
            "nanoclaw"
        );

        // The canonical-path fallback: a symlinked home is the shape tempdir
        // fixtures produce on macOS (/tmp -> /private/tmp), and the literal
        // parent comparison misses it. Point AINB_HOME at a symlink but ask
        // about the REAL path, so only the canonicalize retry can answer.
        #[cfg(unix)]
        {
            let link = home.path().parent().unwrap().join(format!(
                "atc-symlink-{}",
                home.path().file_name().unwrap().to_string_lossy()
            ));
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(home.path(), &link).expect("symlink");
            std::env::set_var("AINB_HOME", &link);
            assert_eq!(
                InteractiveSessionManager::workspace_name_for(&instance),
                "atc:main",
                "the canonicalize fallback did not resolve a symlinked AINB_HOME"
            );
            let _ = std::fs::remove_file(&link);
        }

        match prior_home {
            Some(v) => std::env::set_var("AINB_HOME", v),
            None => std::env::remove_var("AINB_HOME"),
        }
    }

    // ── Real git fixtures ───────────────────────────────────────────────
    //
    // These build actual git state with the `git` CLI rather than hand-faking
    // `.git` files. Hand-faked fixtures are exactly how the "(broken)"
    // misclassification survived: they only ever modelled the linked-worktree
    // shape (`.git` as a FILE), so the plain-checkout shape (`.git` as a
    // DIRECTORY) was never exercised.

    use crate::test_support::{git_available, real_git_fixture};

    #[test]
    fn test_get_source_repository_real_git_shapes() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let fx = real_git_fixture();
        let (repo, worktree, repo_less) = (&fx.repo, &fx.worktree, &fx.repo_less);
        let root = fx.tmp.path().canonicalize().unwrap();

        // 1. Plain checkout resolves to ITSELF (this is the bug: it used to
        //    return None because `.git` is a directory, not a file).
        assert_eq!(
            InteractiveSessionManager::get_source_repository(&repo),
            Some(repo.clone()),
            "a plain checkout is its own source repository"
        );

        // 2. A subdirectory of a plain checkout resolves to the repo root.
        //    This is the shape in the reported bug report
        //    (`ainb run --repo <clone>/<subdir>`).
        assert_eq!(
            InteractiveSessionManager::get_source_repository(&fx.subdir),
            Some(repo.clone()),
            "a subdirectory of a checkout resolves to the repo root"
        );

        // 3. A real linked worktree still resolves through the gitdir pointer.
        let resolved = InteractiveSessionManager::get_source_repository(&worktree)
            .expect("linked worktree must resolve to its main repo");
        assert_eq!(
            &resolved.canonicalize().unwrap(),
            repo,
            "linked worktree must resolve through the gitdir pointer"
        );

        // 3b. A subdirectory inside a linked worktree resolves the same way.
        let wt_sub = worktree.join("src");
        std::fs::create_dir_all(&wt_sub).unwrap();
        let resolved_sub = InteractiveSessionManager::get_source_repository(&wt_sub)
            .expect("worktree subdir must resolve to its main repo");
        assert_eq!(&resolved_sub.canonicalize().unwrap(), repo);

        // 4. Genuinely repo-less directory resolves to nothing.
        //    (Precondition: the tempdir itself is not inside a git repo. If
        //    this trips, your TMPDIR lives inside a checkout.)
        assert_eq!(
            InteractiveSessionManager::get_source_repository(&root),
            None,
            "precondition: TMPDIR must not be inside a git repository"
        );
        assert_eq!(
            InteractiveSessionManager::get_source_repository(&repo_less),
            None,
            "a dir with no `.git` anywhere above it has no source repository"
        );

        // 5. A RELATIVE path is refused outright. `Path::ancestors()` ends at
        //    `""` and `Path::new("").join(".git")` resolves against the
        //    process's current directory, so an unguarded walk would attribute
        //    the session to whatever repository `ainb` was launched from. The
        //    CWD-dependent half of this is proven end-to-end in
        //    tests/cwd_escape_repo_lookup.rs, which chdirs into a real
        //    checkout; here we pin the contract itself.
        assert_eq!(
            InteractiveSessionManager::get_source_repository(Path::new("myrepo")),
            None,
            "a relative path has no resolvable source repository"
        );
        assert_eq!(
            InteractiveSessionManager::get_source_repository(Path::new("nested/deep")),
            None,
            "a relative path must never be walked up into the process CWD"
        );
    }

    /// F13: the branch shown for a session must come from the checkout that
    /// OWNS its directory. `git2::Repository::open` only succeeds at a
    /// repository/worktree ROOT, so a session rooted at `<clone>/<subdir>`
    /// rendered its branch as "unknown" — the very sessions the
    /// `get_source_repository` fix brings back into the tree.
    #[test]
    fn test_get_current_branch_real_git_shapes() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let fx = real_git_fixture();

        assert_eq!(
            InteractiveSessionManager::get_current_branch(&fx.repo).as_deref(),
            Some("main"),
            "a plain checkout reports its own branch"
        );

        // The regression: `Repository::open` fails here and the caller
        // substituted "unknown".
        assert_eq!(
            InteractiveSessionManager::get_current_branch(&fx.subdir).as_deref(),
            Some("main"),
            "a session rooted at <clone>/subdir reports the checkout's branch"
        );

        assert_eq!(
            InteractiveSessionManager::get_current_branch(&fx.worktree).as_deref(),
            Some("feature"),
            "a linked worktree reports ITS branch, not the main checkout's"
        );

        let wt_sub = fx.worktree.join("src");
        std::fs::create_dir_all(&wt_sub).unwrap();
        assert_eq!(
            InteractiveSessionManager::get_current_branch(&wt_sub).as_deref(),
            Some("feature"),
            "a subdir of a linked worktree reports the worktree's branch"
        );

        assert_eq!(
            InteractiveSessionManager::get_current_branch(&fx.repo_less),
            None,
            "a dir inside no git repository has no branch"
        );
    }

    #[test]
    fn test_derive_workspace_name_real_git_shapes() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let fx = real_git_fixture();
        let (repo, worktree, repo_less) = (&fx.repo, &fx.worktree, &fx.repo_less);

        // Plain checkout: callers pass `source_repository == worktree_path`
        // (get_source_repository resolves it to itself). That collapse must
        // NOT be read as broken: it is the repo, named after its directory.
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(repo, repo),
            "myrepo",
            "a plain checkout must render its real name, not (broken)"
        );

        // Subdirectory of a checkout: grouped under the owning repository.
        let src = InteractiveSessionManager::get_source_repository(&fx.subdir).unwrap();
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(&fx.subdir, &src),
            "myrepo"
        );

        // Linked worktree: unchanged, name comes from the `<repo>--<branch>--<id>`
        // directory convention.
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(worktree, repo),
            "myrepo"
        );

        // Repo-less dir: still (broken), and still collapses under the sentinel.
        assert_eq!(
            InteractiveSessionManager::derive_workspace_name(repo_less, repo_less),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME,
            "a dir inside no git repository is still (broken)"
        );
    }

    /// `workspace_name_for` must be exactly what the TUI loaders compute by
    /// hand (`get_source_repository` + fallback + `derive_workspace_name`).
    /// If these two ever diverge, the "one derivation" guarantee behind
    /// `SessionMetadata::display_workspace_name` is gone and `ainb list` starts
    /// disagreeing with the session tree again.
    #[test]
    fn workspace_name_for_matches_the_tui_longhand() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let fx = real_git_fixture();

        // The longhand, copied from `AppState::load_real_workspaces`.
        let tui_longhand = |path: &Path| {
            let source = InteractiveSessionManager::get_source_repository(path)
                .unwrap_or_else(|| path.to_path_buf());
            InteractiveSessionManager::derive_workspace_name(path, &source)
        };

        for path in [&fx.repo, &fx.subdir, &fx.worktree, &fx.repo_less] {
            assert_eq!(
                InteractiveSessionManager::workspace_name_for(path),
                tui_longhand(path),
                "derivation drift for {}",
                path.display()
            );
        }

        // And the values themselves, so a shared-but-wrong derivation still fails.
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&fx.repo),
            "myrepo"
        );
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&fx.subdir),
            "myrepo"
        );
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&fx.worktree),
            "myrepo"
        );
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&fx.repo_less),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME
        );
    }

    /// A linked worktree whose main repo has been DELETED must not keep
    /// rendering the vanished repo's name. The pointer file still parses, so
    /// path arithmetic alone would hand back `myrepo` forever.
    #[test]
    fn dangling_gitdir_pointer_resolves_to_broken() {
        if !git_available() {
            eprintln!("SKIP: git unavailable");
            return;
        }
        let fx = real_git_fixture();

        // Precondition: the intact worktree resolves to its repo.
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&fx.worktree),
            "myrepo"
        );
        assert!(fx.worktree.join(".git").is_file());

        // Delete the main repository, leaving a well-formed but dangling pointer.
        std::fs::remove_dir_all(fx.repo.join(".git")).expect("remove main gitdir");

        assert_eq!(
            InteractiveSessionManager::get_source_repository(&fx.worktree),
            None,
            "a gitdir pointer whose target is gone must not resolve"
        );

        // The NAME survives, because ainb's own worktree directories encode
        // the repo (`<repo>--<branch>--<shortid>`) and that convention is read
        // before the sentinel. Losing the repo does not make the row anonymous.
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&fx.worktree),
            "myrepo"
        );

        // A dangling pointer WITHOUT that naming convention has nothing left
        // to go on, and that is the case the `(broken)` label exists for.
        let plain_named = fx.tmp.path().join("checkout-copy");
        std::fs::create_dir_all(&plain_named).unwrap();
        std::fs::copy(fx.worktree.join(".git"), plain_named.join(".git")).unwrap();
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&plain_named),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME
        );
    }

    /// `ainb list` must not collapse every deleted-worktree row onto the same
    /// undifferentiated `(broken)` label. The persisted name is the only
    /// human-readable identifier those rows still carry, and listing them is
    /// exactly how a user decides what to clean up.
    #[test]
    fn display_workspace_name_falls_back_to_the_persisted_name() {
        let gone = std::path::PathBuf::from("/nonexistent/deleted-worktree");
        assert_eq!(
            InteractiveSessionManager::workspace_name_for(&gone),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME,
            "precondition: the derivation collapses for a vanished worktree"
        );

        let mut metadata = SessionMetadata {
            session_id: Uuid::new_v4(),
            tmux_session_name: "tmux_gone".to_string(),
            worktree_path: gone,
            workspace_name: "shotclubhouse".to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::Claude,
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: ModelSource::Raw,
            codex_model: None,
            codex_thread_id: None,
        };
        assert_eq!(metadata.display_workspace_name(), "shotclubhouse");

        // With nothing persisted there is nothing better to show.
        metadata.workspace_name = "   ".to_string();
        assert_eq!(
            metadata.display_workspace_name(),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME
        );

        // A root that still EXISTS but resolves to no repository keeps saying
        // "(broken)": that is the actionable case the spawn skills document,
        // and hiding it behind a stale persisted name would make `ainb list`
        // lie about a session the operator can still fix.
        let live_but_repoless = tempfile::tempdir().expect("tempdir");
        metadata.worktree_path = live_but_repoless.path().to_path_buf();
        metadata.workspace_name = "shotclubhouse".to_string();
        assert_eq!(
            metadata.display_workspace_name(),
            InteractiveSessionManager::BROKEN_WORKSPACE_NAME
        );
    }

    #[test]
    fn test_generate_tmux_name() {
        // New format: tmux_{folder}_{branch}
        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("myrepo--abc123", "feature/my-feature"),
            "tmux_myrepo--abc123_feature_my-feature"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("project--xyz789", "fix.bug:test"),
            "tmux_project--xyz789_fix_bug_test"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("simple-folder", "simple"),
            "tmux_simple-folder_simple"
        );

        // Test folder with special chars
        assert_eq!(
            InteractiveSessionManager::generate_tmux_name("my.repo/path:foo", "main"),
            "tmux_my_repo_path_foo_main"
        );
    }

    #[test]
    fn test_generate_tmux_name_legacy() {
        // Legacy format: tmux_{branch}
        assert_eq!(
            InteractiveSessionManager::generate_tmux_name_legacy("feature/my-feature"),
            "tmux_feature_my-feature"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name_legacy("fix.bug:test"),
            "tmux_fix_bug_test"
        );

        assert_eq!(
            InteractiveSessionManager::generate_tmux_name_legacy("simple"),
            "tmux_simple"
        );
    }

    #[test]
    fn test_extract_worktree_folder() {
        use std::path::PathBuf;

        let path = PathBuf::from("/home/user/worktrees/myrepo--abc123--uuid");
        assert_eq!(
            InteractiveSessionManager::extract_worktree_folder(&path),
            "myrepo--abc123--uuid"
        );

        let root_path = PathBuf::from("/");
        assert_eq!(
            InteractiveSessionManager::extract_worktree_folder(&root_path),
            "session" // fallback
        );
    }

    #[test]
    fn test_session_manager_creation() {
        let manager = InteractiveSessionManager::new();
        assert!(manager.is_ok(), "Should create manager without Docker");
    }

    /// Verify the SessionStore headroom flip: if a session has headroom_enabled=true,
    /// mutating the field to false and saving results in false when re-loaded.
    /// This covers the pure store-manipulation leg of `downgrade_headroom_session`
    /// without requiring tmux.
    #[test]
    fn session_store_headroom_flip_persists() {
        use crate::models::session::SessionAgentType;
        use chrono::Utc;
        use std::path::PathBuf;
        use tempfile::TempDir;

        // AINB_HOME is process-global; serialise with every other env-mutating
        // test in the crate (including `concurrent_mutate_does_not_lose_updates`)
        // via the shared lock so parallel runs don't clobber each other's home.
        let _env = crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap();

        let dir = TempDir::new().expect("tempdir");
        // Point AINB_HOME at our temp dir so SessionStore::storage_path() uses it.
        std::env::set_var("AINB_HOME", dir.path());

        let tmux_name = "ainb-test-headroom-flip".to_string();
        let session_id = uuid::Uuid::new_v4();

        // Seed: headroom_enabled = true
        let mut store = SessionStore::load();
        store.upsert(SessionMetadata {
            session_id,
            tmux_session_name: tmux_name.clone(),
            worktree_path: PathBuf::from("/tmp/fake"),
            workspace_name: "test".to_string(),
            created_at: Utc::now(),
            agent_type: SessionAgentType::Claude,
            headroom_enabled: true,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        });
        store.save().expect("save");

        // Flip: headroom_enabled = false (mirrors downgrade_headroom_session step 3)
        let mut store2 = SessionStore::load();
        let meta = store2.sessions.get(&tmux_name).expect("meta present");
        assert!(meta.headroom_enabled, "precondition: headroom was on");
        store2.sessions.get_mut(&tmux_name).unwrap().headroom_enabled = false;
        store2.save().expect("save after flip");

        // Reload and verify persistence
        let store3 = SessionStore::load();
        let reloaded = store3.sessions.get(&tmux_name).expect("still present");
        assert!(
            !reloaded.headroom_enabled,
            "headroom should be off after flip"
        );

        // Cleanup env
        std::env::remove_var("AINB_HOME");
    }

    #[tokio::test]
    async fn persisted_launch_settings_survive_live_session_discovery() {
        use crate::models::session::SessionAgentType;
        use chrono::Utc;
        use tempfile::TempDir;

        let _env = crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap();

        let dir = TempDir::new().expect("tempdir");
        std::env::set_var("AINB_HOME", dir.path());

        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let tmux_name = "tmux_persisted_launch_settings";

        SessionStore::mutate(|store| {
            store.upsert(SessionMetadata {
                session_id: uuid::Uuid::new_v4(),
                tmux_session_name: tmux_name.to_string(),
                worktree_path: worktree,
                workspace_name: "test".to_string(),
                created_at: Utc::now(),
                agent_type: SessionAgentType::Codex,
                headroom_enabled: false,
                rtk_enabled: false,
                skip_permissions: Some(true),
                model: Some("gpt-5.6-luna".to_string()),
                model_source: ModelSource::Raw,
                codex_model: Some(CodexModel::Gpt55),
                codex_thread_id: None,
            });
        })
        .expect("save");

        let manager = InteractiveSessionManager::new().expect("manager");
        let discovered = manager
            .discover_session_from_tmux(tmux_name)
            .await
            .expect("discover persisted session");
        let session = discovered.to_session_model();

        assert!(
            session.skip_permissions,
            "live discovery must preserve dangerous-permissions launch setting"
        );
        assert_eq!(
            session.model.as_deref(),
            Some("gpt-5.6-luna"),
            "live discovery must preserve model used by idle restart"
        );

        std::env::remove_var("AINB_HOME");
    }

    #[test]
    fn legacy_typed_model_metadata_resolves_to_launch_ids() {
        let claude: SessionMetadata = serde_json::from_value(serde_json::json!({
            "session_id": uuid::Uuid::new_v4(),
            "tmux_session_name": "tmux_legacy_claude",
            "worktree_path": "/tmp/legacy-claude",
            "workspace_name": "legacy",
            "created_at": "2026-07-16T00:00:00Z",
            "agent_type": "Claude",
            "model": "Opus"
        }))
        .expect("legacy Claude metadata");
        assert_eq!(claude.launch_model().as_deref(), Some("claude-opus-4-8"));

        let codex: SessionMetadata = serde_json::from_value(serde_json::json!({
            "session_id": uuid::Uuid::new_v4(),
            "tmux_session_name": "tmux_legacy_codex",
            "worktree_path": "/tmp/legacy-codex",
            "workspace_name": "legacy",
            "created_at": "2026-07-16T00:00:00Z",
            "agent_type": "Codex",
            "codex_model": "Gpt55"
        }))
        .expect("legacy Codex metadata");
        assert_eq!(codex.launch_model().as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn raw_model_metadata_preserves_provider_value() {
        let metadata: SessionMetadata = serde_json::from_value(serde_json::json!({
            "session_id": uuid::Uuid::new_v4(),
            "tmux_session_name": "tmux_raw_claude",
            "worktree_path": "/tmp/raw-claude",
            "workspace_name": "raw",
            "created_at": "2026-07-23T00:00:00Z",
            "agent_type": "Claude",
            "model": "Opus",
            "model_source": "Raw"
        }))
        .expect("raw Claude metadata");

        assert_eq!(metadata.launch_model().as_deref(), Some("Opus"));
    }

    /// pu4: `SessionStore::mutate` must serialise concurrent load-modify-save
    /// through the cross-process lock so racing writers never lost-update the
    /// store. Two thread pools each upsert a disjoint set of keys into the SAME
    /// `sessions.json`; with the naked (pre-fix) RMW the interleaving where both
    /// threads load the same base and write back only their own entry would drop
    /// updates. Under the lock every key survives.
    ///
    /// Env-guarded (`AINB_HOME` is process-global) via the shared test env lock.
    #[test]
    fn concurrent_mutate_does_not_lose_updates() {
        use crate::models::session::SessionAgentType;
        use chrono::Utc;
        use std::sync::{Arc, Barrier};
        use tempfile::TempDir;

        let _env = crate::headroom::HEADROOM_ENV_LOCK.lock().unwrap();

        let dir = TempDir::new().expect("tempdir");
        std::env::set_var("AINB_HOME", dir.path());

        let mk = |i: usize| SessionMetadata {
            session_id: uuid::Uuid::new_v4(),
            tmux_session_name: format!("tmux_pu4_{i}"),
            worktree_path: PathBuf::from(format!("/work/{i}")),
            workspace_name: format!("ws{i}"),
            created_at: Utc::now(),
            agent_type: SessionAgentType::Claude,
            headroom_enabled: false,
            rtk_enabled: false,
            skip_permissions: None,
            model: None,
            model_source: Default::default(),
            codex_model: None,
            codex_thread_id: None,
        };

        const WRITERS: usize = 12;
        let barrier = Arc::new(Barrier::new(WRITERS));
        let handles: Vec<_> = (0..WRITERS)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    SessionStore::mutate(|store| store.upsert(mk(i)))
                        .expect("locked mutate must succeed");
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let store = SessionStore::load();
        assert_eq!(
            store.sessions.len(),
            WRITERS,
            "every concurrent mutate must survive — no lost update"
        );
        for i in 0..WRITERS {
            assert!(
                store.sessions.contains_key(&format!("tmux_pu4_{i}")),
                "writer {i}'s entry was lost to a racing mutate"
            );
        }

        std::env::remove_var("AINB_HOME");
    }

    /// Two wires yield exactly one rtk entry — exercises the real function.
    #[test]
    fn wire_rtk_project_hook_merges_idempotently() {
        use tempfile::TempDir;
        let dir = TempDir::new().expect("tempdir");
        let cmd = "/usr/local/bin/rtk hook claude";

        wire_rtk_project_hook_with_cmd(dir.path(), cmd).expect("first wire");
        wire_rtk_project_hook_with_cmd(dir.path(), cmd).expect("second wire");

        let settings_path = dir.path().join(".claude/settings.json");
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        let arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 1, "two wires must produce exactly one rtk entry");
        assert_eq!(arr[0]["hooks"][0]["command"], cmd);
    }

    /// The real function appends rtk while preserving unrelated hooks + settings.
    #[test]
    fn wire_rtk_project_hook_merge_preserves_existing() {
        use tempfile::TempDir;
        let dir = TempDir::new().expect("tempdir");
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let settings_path = claude_dir.join("settings.json");

        let initial = serde_json::json!({
            "hooks": {
                "PreToolUse": [
                    {
                        "matcher": "Read",
                        "hooks": [{"type": "command", "command": "/usr/local/bin/other hook"}]
                    }
                ]
            },
            "someOtherSetting": true
        });
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&initial).unwrap(),
        )
        .unwrap();

        wire_rtk_project_hook_with_cmd(dir.path(), "/opt/rtk hook claude").expect("wire");

        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(root["someOtherSetting"], serde_json::Value::Bool(true));
        let arr = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "pre-existing hook kept + rtk appended");
        assert_eq!(arr[0]["matcher"], "Read");
        assert_eq!(arr[1]["matcher"], "Bash");
    }

    /// A settings.json we can't parse is left byte-for-byte untouched — we must
    /// never clobber a file that may hold the user's own hooks/config.
    #[test]
    fn wire_rtk_project_hook_preserves_unparseable_settings() {
        use tempfile::TempDir;
        let dir = TempDir::new().expect("tempdir");
        let claude_dir = dir.path().join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        let settings_path = claude_dir.join("settings.json");

        let garbage = "{ not valid json, // with a comment\n";
        std::fs::write(&settings_path, garbage).unwrap();

        wire_rtk_project_hook_with_cmd(dir.path(), "/opt/rtk hook claude").expect("must not error");

        let after = std::fs::read_to_string(&settings_path).unwrap();
        assert_eq!(
            after, garbage,
            "unparseable settings must be left untouched"
        );
    }

    // ---- build_cli_cmd_parts: launch/resume argv assembly ------------------

    use crate::config::CliProvider;
    use crate::models::{CodexModel, SessionAgentType};

    fn parts(
        provider: CliProvider,
        agent: SessionAgentType,
        skip: bool,
        model: Option<&str>,
        resume: bool,
        has_history: bool,
    ) -> Vec<String> {
        InteractiveSessionManager::build_cli_cmd_parts(
            &provider,
            agent,
            skip,
            model,
            resume,
            has_history,
        )
    }

    #[test]
    fn claude_fresh_launch_has_no_continue() {
        let p = parts(
            CliProvider::Claude,
            SessionAgentType::Claude,
            true,
            None,
            false,
            false,
        );
        assert_eq!(p, vec!["claude", "--dangerously-skip-permissions"]);
    }

    #[test]
    fn claude_resume_with_history_appends_continue() {
        let p = parts(
            CliProvider::Claude,
            SessionAgentType::Claude,
            true,
            None,
            true,
            true,
        );
        assert_eq!(
            p,
            vec!["claude", "--dangerously-skip-permissions", "--continue"]
        );
    }

    #[test]
    fn claude_resume_without_history_omits_continue() {
        // `claude --continue` with no prior conversation errors and leaves a
        // dead pane — the has_history guard must suppress it.
        let p = parts(
            CliProvider::Claude,
            SessionAgentType::Claude,
            true,
            None,
            true,
            false,
        );
        assert_eq!(p, vec!["claude", "--dangerously-skip-permissions"]);
    }

    #[test]
    fn claude_non_yolo_resume_has_continue_but_no_skip_flag() {
        let p = parts(
            CliProvider::Claude,
            SessionAgentType::Claude,
            false,
            None,
            true,
            true,
        );
        assert_eq!(p, vec!["claude", "--continue"]);
    }

    #[test]
    fn claude_resume_with_model_order() {
        let p = parts(
            CliProvider::Claude,
            SessionAgentType::Claude,
            true,
            Some("opus"),
            true,
            true,
        );
        assert_eq!(
            p,
            vec![
                "claude",
                "--model",
                "opus",
                "--dangerously-skip-permissions",
                "--continue",
            ]
        );
    }

    #[test]
    fn codex_resume_is_subcommand_before_flags() {
        // Global config precedes the resume subcommand; resume flags follow it.
        let p = parts(
            CliProvider::Codex,
            SessionAgentType::Codex,
            true,
            None,
            true,
            false,
        );
        assert_eq!(
            p,
            vec![
                "codex",
                "-c",
                "check_for_update_on_startup=false",
                "resume",
                "--last",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
        );
    }

    #[test]
    fn codex_resume_with_model_lands_after_subcommand() {
        let p = parts(
            CliProvider::Codex,
            SessionAgentType::Codex,
            true,
            Some("gpt-5.6-luna"),
            true,
            false,
        );
        assert_eq!(
            p,
            vec![
                "codex",
                "-c",
                "check_for_update_on_startup=false",
                "resume",
                "--last",
                "--model",
                "gpt-5.6-luna",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
        );
    }

    #[test]
    fn codex_fresh_launch_has_no_resume_subcommand() {
        let p = parts(
            CliProvider::Codex,
            SessionAgentType::Codex,
            true,
            None,
            false,
            false,
        );
        assert_eq!(
            p,
            vec![
                "codex",
                "-c",
                "check_for_update_on_startup=false",
                "--dangerously-bypass-approvals-and-sandbox",
            ]
        );
    }

    #[test]
    fn copilot_resume_appends_continue() {
        // Copilot resumes the most recent session via --continue (unguarded).
        let p = parts(
            CliProvider::Copilot,
            SessionAgentType::Copilot,
            true,
            None,
            true,
            false, // has_history irrelevant for copilot (no cwd probe)
        );
        assert_eq!(p, vec!["copilot", "--yolo", "--continue"]);
    }

    #[test]
    fn copilot_fresh_launch_has_no_continue() {
        let p = parts(
            CliProvider::Copilot,
            SessionAgentType::Copilot,
            true,
            None,
            false,
            false,
        );
        assert_eq!(p, vec!["copilot", "--yolo"]);
    }

    #[test]
    fn antigravity_fresh_launch_has_no_continue() {
        let p = parts(
            CliProvider::Antigravity,
            SessionAgentType::Antigravity,
            true,
            None,
            false,
            false,
        );
        assert_eq!(p, vec!["agy", "--dangerously-skip-permissions"]);
    }

    #[test]
    fn antigravity_resume_appends_continue() {
        let p = parts(
            CliProvider::Antigravity,
            SessionAgentType::Antigravity,
            true,
            None,
            true,
            false,
        );
        assert_eq!(
            p,
            vec!["agy", "--dangerously-skip-permissions", "--continue"]
        );
    }

    #[test]
    fn antigravity_launch_with_model() {
        let p = parts(
            CliProvider::Antigravity,
            SessionAgentType::Antigravity,
            true,
            Some("gemini-3.7-flash"),
            false,
            false,
        );
        assert_eq!(
            p,
            vec![
                "agy",
                "--model",
                "gemini-3.7-flash",
                "--dangerously-skip-permissions",
            ]
        );
    }

    #[test]
    fn antigravity_resume_with_model_and_continue() {
        let p = parts(
            CliProvider::Antigravity,
            SessionAgentType::Antigravity,
            true,
            Some("gemini-2.5-pro"),
            true,
            false,
        );
        assert_eq!(
            p,
            vec![
                "agy",
                "--model",
                "gemini-2.5-pro",
                "--dangerously-skip-permissions",
                "--continue",
            ]
        );
    }

    /// A skipped autostart must be read as "shared remote control is off for
    /// this session", and the warning must name the remedy.
    ///
    /// The gate reports the skip instead of failing, since the TUI is right to
    /// ignore it. Discarding it here turned "no daemon will ever run on this
    /// home" into a socket connect error a line later, with nothing in the
    /// message about `$AINB_HANGAR_HOME`.
    #[test]
    fn an_ephemeral_hangar_home_turns_codex_remote_control_off_with_the_remedy() {
        use crate::cli::hangar::DaemonAutostart;

        assert!(
            !super::shared_remote_control_available(DaemonAutostart::SkippedEphemeralHome, || {
                "/tmp/bj.Q9x7fk/.agents-in-a-box".to_string()
            }),
            "an ephemeral home has no daemon to connect to, now or ever"
        );

        let message = super::ephemeral_hangar_home_warning("/tmp/bj.Q9x7fk/.agents-in-a-box");
        assert!(
            message.contains("/tmp/bj.Q9x7fk/.agents-in-a-box"),
            "the warning must name the home: {message}"
        );
        assert!(
            message.contains("AINB_HANGAR_HOME"),
            "the warning must name the remedy: {message}"
        );

        // A daemon that is expected, and a spawn that merely failed, both fall
        // through: only the ephemeral-home skip promises no daemon will appear.
        for outcome in [DaemonAutostart::Started, DaemonAutostart::Failed] {
            assert!(
                super::shared_remote_control_available(outcome, || panic!(
                    "{outcome:?} must not resolve the home"
                )),
                "{outcome:?}"
            );
        }
    }

    /// The degrade itself, which the classifier above cannot show: an ephemeral
    /// home yields NO remote thread and NO error, so the caller launches the
    /// session (plain `codex` in the pane) instead of failing and rolling back
    /// the worktree it just created.
    ///
    /// Driving the seam rather than `ensure_codex_remote_thread` keeps the test
    /// off the daemon socket, and still pins the ordering that matters: the
    /// ephemeral answer returns before the connect, so no daemon is needed for
    /// this launch to succeed.
    #[tokio::test]
    async fn an_ephemeral_hangar_home_launches_codex_without_a_remote_thread() {
        use crate::cli::hangar::DaemonAutostart;

        let remote = super::ensure_codex_remote_thread_with(
            || DaemonAutostart::SkippedEphemeralHome,
            || "/tmp/bj.Q9x7fk/.agents-in-a-box".to_string(),
            ainb_hangar_proto::fleet::CodexSessionEnsureParams {
                session_id: uuid::Uuid::new_v4().to_string(),
                cwd: "/worktree".to_string(),
                model: None,
                thread_id: None,
                skip_permissions: false,
            },
        )
        .await
        .expect("an ephemeral home must not fail the launch");

        assert!(
            remote.is_none(),
            "the session must launch with no shared remote thread, exactly as one with the \
             feature disabled does"
        );
    }
}
