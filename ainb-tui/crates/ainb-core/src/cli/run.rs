// ABOUTME: CLI run command - spawn a new AI coding session
//
// Creates a new session with:
// - Optional git worktree for isolation
// - Tmux session running Claude CLI
// - Session metadata persisted for TUI compatibility

use anyhow::{Context, Result};
use chrono::Utc;
use std::path::PathBuf;
use tokio::process::Command;
use tokio::time::{Duration, Instant, sleep};
use tracing::{info, warn};
use uuid::Uuid;

use super::RunArgs;
use crate::config::CliProvider;
use crate::git::worktree_manager::WorktreeManager;
use crate::interactive::session_manager::{ModelSource, SessionMetadata, SessionStore};
use crate::models::session::{SessionAgentType, is_default_model};
use crate::tmux::TmuxSession;

/// Execute the run command
pub async fn execute(args: RunArgs) -> Result<()> {
    // Step 0: Validate provider CLI is installed
    let provider = args.tool.to_cli_provider();
    validate_provider_installed(&provider)?;

    // Step 1: Resolve repository path
    let repo_path = resolve_repo_path(&args).await?;
    info!("Using repository: {}", repo_path.display());

    // Step 2: Determine workspace name and working directory
    let workspace_name = repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();

    let work_dir: PathBuf;
    let branch_name: String;
    let session_id = Uuid::new_v4();
    // Set when the session ends up running directly in the user's checkout.
    // Kept so the warning can be REPEATED in the post-creation summary: with
    // `--attach`/`-i` this process execs into tmux, and the copy printed here
    // is buried under the creation log before the user ever reads it, in
    // exactly the invocation the warning exists to catch.
    let mut shared_checkout = false;

    // Step 3: Create worktree if requested
    if args.worktree || args.create_branch.is_some() {
        let worktree_manager =
            WorktreeManager::new().context("Failed to initialize worktree manager")?;

        let branch = args
            .create_branch
            .clone()
            .unwrap_or_else(|| format!("ainb/session-{}", &session_id.to_string()[..8]));

        info!("Creating worktree for branch: {}", branch);

        let worktree_info = worktree_manager
            .create_worktree(session_id, &repo_path, &branch, None)
            .context("Failed to create worktree")?;

        work_dir = worktree_info.path;
        branch_name = branch;

        println!("Created worktree at: {}", work_dir.display());
    } else {
        work_dir = repo_path.clone();
        branch_name =
            crate::git::current_branch_at(&repo_path).unwrap_or_else(|| "main".to_string());

        // No isolation was requested, so this session runs directly in the
        // checkout the user pointed at. Say so loudly (stderr, never a prompt,
        // never fatal): the agent shares that branch/index/working tree with
        // the user's editor and with any other session started there.
        shared_checkout = matches!(
            classify_session_root(&work_dir),
            SessionRoot::SharedCheckout
        );
        if shared_checkout {
            warn_shared_checkout(&work_dir);
        }
    }

    // Step 4: Generate session name
    let session_name = args.name.clone().unwrap_or_else(|| {
        let short_id = &session_id.to_string()[..8];
        format!("{workspace_name}-{short_id}")
    });

    // Step 5: Keep model opaque. Provider CLI owns model validation/catalog.
    let model = requested_model(args.model.as_deref());

    // Step 5.5: Wire shared MCP pool (Claude only; never blocks creation).
    // Ensures the pool daemon is up and merge-writes the worktree's
    // .mcp.json so pooled servers point at the `ainb mcp proxy` shim.
    // Any failure falls back to today's per-session behavior.
    if matches!(args.tool.to_cli_provider(), CliProvider::Claude) {
        setup_mcp_pool(&work_dir, &session_name);
    }

    // Step 6: Build Claude command
    let claude_cmd = build_agent_command(&args);

    // Step 6b: Parent linkage (event-driven plumbing). When spawned with
    // `--parent <id>`, this session is a child of an orchestrator (e.g. ATC).
    // We seed `AINB_PARENT_SESSION` into the tmux session's environment (via
    // `tmux new-session -e`), so the child's Stop hook routes completions to the
    // parent's durable inbox. We also record a durable child→parent map as a
    // restart-safe fallback.
    let mut session_env: Vec<(String, String)> = Vec::new();
    if let Some(parent_id) = args.parent.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        // Seed the live, in-band linkage: the child's Stop hook reads
        // AINB_PARENT_SESSION first and routes its completion to the parent's
        // inbox with no disk lookup. The durable child→parent map is NOT written
        // here: claude mints its own session id (we don't pass --session-id), so
        // a map keyed by ainb's Uuid would never match the id the hook reports.
        // Instead the hook self-registers the durable fallback under the
        // hook-observed id (see `fleet atc hook`), keying the map by the id any
        // later lookup actually sees.
        session_env.push((
            crate::fleet::plumbing::PARENT_ENV.to_string(),
            parent_id.to_string(),
        ));
        info!("Linked session to parent {parent_id} (event-driven inbox routing)");
    }

    // Step 7: Create tmux session
    let mut tmux = TmuxSession::new(session_name.clone(), claude_cmd.clone()).with_env(session_env);
    tmux.start(&work_dir).await.context("Failed to start tmux session")?;

    let tmux_name = tmux.name().to_string();
    info!("Started tmux session: {}", tmux_name);

    // Step 8: send the initial prompt (if any) once the input box is ready.
    // A fixed sleep loses keystrokes into Claude Code's not-yet-ready splash.
    if let Some(ref prompt) = args.prompt {
        wait_for_prompt_ready(&tmux_name, Duration::from_secs(30)).await;
        send_prompt_to_tmux(&tmux_name, prompt).await?;
    }

    // Step 9: Save session to SessionStore (TUI-compatible format)
    let agent_type = match args.tool.to_cli_provider() {
        CliProvider::Claude => SessionAgentType::Claude,
        CliProvider::Codex => SessionAgentType::Codex,
        CliProvider::Gemini => SessionAgentType::Gemini,
        CliProvider::Copilot => SessionAgentType::Copilot,
    };

    let metadata = SessionMetadata {
        session_id,
        tmux_session_name: tmux_name.clone(),
        worktree_path: work_dir.clone(),
        workspace_name: workspace_name.clone(),
        created_at: Utc::now(),
        agent_type,
        headroom_enabled: false,
        rtk_enabled: false,
        skip_permissions: Some(args.dangerously_skip_permissions),
        model: model.clone(),
        model_source: ModelSource::Raw,
        codex_model: None,
    };

    // Locked RMW (pu4): another `ainb run`/`kill` or a daemon register racing
    // this write must not lost-update the store.
    SessionStore::mutate(|store| store.upsert(metadata))
        .context("Failed to save session metadata")?;

    info!("Saved session metadata for TUI discovery");

    // Step 10: Print session info
    println!();
    println!("Session created successfully!");
    println!("  Session ID:   {session_id}");
    println!("  Tmux Session: {tmux_name}");
    println!("  Working Dir:  {}", work_dir.display());
    println!("  Branch:       {branch_name}");
    println!(
        "  Model:        {}",
        model.as_deref().unwrap_or("system default")
    );
    println!();
    println!("To attach to this session:");
    println!("  tmux attach -t {tmux_name}");
    println!();
    // Print an id prefix, NOT `session_name`. `--name` only renames the tmux
    // session; `ainb attach|status|kill` resolve their argument as a session
    // id, an id prefix, or the *workspace* name (repo-directory derived), so
    // echoing `session_name` here hands the user a handle that does not
    // resolve whenever they passed `--name`.
    println!("Or use:");
    println!("  ainb attach {}", &session_id.to_string()[..8]);
    println!();

    // Repeat the no-isolation warning as the LAST thing before attaching.
    //
    // With `--attach`/`-i` the next statement execs tmux, which takes the
    // terminal over for the whole life of the session; the pre-creation copy
    // is on screen for a few milliseconds and then buried under the creation
    // log. Emitting it here puts it immediately above the point where tmux
    // takes (and later hands back) the terminal, so it is the last thing the
    // user saw going in and the first thing they see coming out, instead of
    // being lost mid-log.
    //
    // Still advisory: no prompt, no non-zero exit.
    if shared_checkout {
        warn_shared_checkout(&work_dir);
    }

    // Step 11: Attach if requested
    if args.attach || args.interactive {
        // tmux switches to the alternate screen within milliseconds of the
        // exec below, so without a beat here the warning above is technically
        // emitted and practically unreadable: the user only meets it after
        // detaching, by which point the agent has been working in their
        // checkout for a while. A short pause is the cheapest thing that makes
        // it legible without turning an advisory into a prompt.
        if shared_checkout {
            sleep(Duration::from_secs(2)).await;
        }
        println!("Attaching to session...");
        attach_to_session(&tmux_name)?;
    }

    Ok(())
}

/// Best-effort shared-MCP-pool setup for a new session. Pool disabled, no
/// eligible servers, daemon spawn failure, or .mcp.json write failure all
/// degrade to per-session MCP spawning — a session must never fail to start
/// because of the pool.
fn setup_mcp_pool(work_dir: &std::path::Path, session_name: &str) {
    use crate::config::AppConfig;
    use crate::mcp_pool;

    let config = AppConfig::load().unwrap_or_default();
    if !config.mcp_pool.enabled {
        return;
    }
    let mut pooled = mcp_pool::pooled_servers(&config);

    // Auto-import: stdio servers already declared in the worktree's
    // .mcp.json join the pool too (config entries win on name conflict).
    // Users who never touched ainb config still get pooling for free.
    // Auto-import runs whatever a repo's .mcp.json declares as a pooled
    // (and later spawned) process. That matches Claude Code's own
    // project-.mcp.json trust model, but log the exact command/args loudly
    // so it's auditable — a freshly-cloned repo could declare anything.
    let known: std::collections::HashSet<String> = pooled.iter().map(|s| s.name.clone()).collect();
    for server in mcp_pool::mcp_json::parse_stdio_servers(&work_dir.join(".mcp.json")) {
        if !known.contains(&server.name) && server.resolvable_on_host() {
            warn!(
                "mcp pool: auto-importing '{}' from project .mcp.json — will pool+spawn: {} {}",
                server.name,
                server.command,
                server.args.join(" ")
            );
            pooled.push(server);
        }
    }
    if pooled.is_empty() {
        return;
    }

    if let Err(e) = mcp_pool::client::ensure_daemon() {
        warn!("mcp pool: daemon unavailable, falling back to per-session MCP: {e}");
        return;
    }
    // Teach the (possibly long-running, other-project-started) daemon every
    // server this session expects. Existing names are no-ops.
    if let Err(e) = mcp_pool::client::register_servers(&pooled) {
        warn!("mcp pool: register failed, falling back to per-session MCP: {e}");
        return;
    }
    match mcp_pool::mcp_json::write_session_mcp_json(work_dir, &pooled, Some(session_name)) {
        Ok(wired) if !wired.is_empty() => {
            println!(
                "MCP pool: shared servers wired via {}: {}",
                work_dir.join(".mcp.json").display(),
                wired.join(", ")
            );
        }
        Ok(_) => {}
        Err(e) => warn!("mcp pool: could not write .mcp.json: {e}"),
    }
}

/// Resolve the repository path from args or current directory
async fn resolve_repo_path(args: &RunArgs) -> Result<PathBuf> {
    // Priority: --repo > --remote-repo > current directory
    if let Some(ref repo) = args.repo {
        let path = if repo.is_absolute() {
            repo.clone()
        } else {
            std::env::current_dir()?.join(repo)
        };

        if !path.exists() {
            anyhow::bail!("Repository path does not exist: {}", path.display());
        }

        return Ok(path.canonicalize()?);
    }

    if let Some(ref remote) = args.remote_repo {
        // Clone or fetch remote repository
        return clone_remote_repo(remote).await;
    }

    // Use current directory
    let current_dir = std::env::current_dir()?;

    // Verify it's a git repository
    if !current_dir.join(".git").exists() {
        anyhow::bail!(
            "Current directory is not a git repository. Use --repo or --remote-repo to specify one."
        );
    }

    Ok(current_dir)
}

/// Clone a remote repository to a local cache directory
async fn clone_remote_repo(remote: &str) -> Result<PathBuf> {
    // Normalize remote URL
    let url = if remote.starts_with("http") || remote.starts_with("git@") {
        remote.to_string()
    } else {
        // Assume GitHub shorthand: owner/repo
        format!("https://github.com/{remote}.git")
    };

    // Extract repo name for cache directory (sanitized to prevent path traversal)
    let repo_name = url
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .replace("..", "")
        .replace(['/', '\\'], "-");

    // Validate repo name is safe
    let repo_name = if repo_name.is_empty() || repo_name == "." {
        "repo".to_string()
    } else {
        repo_name
    };

    let cache_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?
        .join(".agents-in-a-box")
        .join("repo-cache");

    std::fs::create_dir_all(&cache_dir)?;

    let repo_path = cache_dir.join(repo_name);

    if repo_path.exists() {
        info!("Repository already cached, fetching updates...");
        let output = Command::new("git")
            .current_dir(&repo_path)
            .args(["fetch", "--all"])
            .output()
            .await?;

        if !output.status.success() {
            warn!(
                "Failed to fetch updates: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    } else {
        println!("Cloning {url}...");
        let output = Command::new("git").arg("clone").arg(&url).arg(&repo_path).output().await?;

        if !output.status.success() {
            anyhow::bail!(
                "Failed to clone repository: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    Ok(repo_path)
}

// The current-branch lookup lives in `crate::git::current_branch_at`. It used
// to be duplicated here with `git2::Repository::open`, which fails for a
// `--repo <clone>/<subdir>` target (open needs a repository ROOT) and made the
// session record branch "main" for whatever branch was actually checked out.

/// Normalize only AINB's no-model sentinels. Every other value stays opaque.
fn requested_model(model: Option<&str>) -> Option<String> {
    let model = model?.trim();
    (!is_default_model(model)).then(|| model.to_string())
}

/// Validate that the selected provider's CLI binary is installed and on PATH
fn validate_provider_installed(provider: &CliProvider) -> Result<()> {
    let cmd = provider.command();
    if which::which(cmd).is_err() {
        let install_url = match provider {
            CliProvider::Claude => "https://docs.anthropic.com/en/docs/claude-code",
            CliProvider::Codex => "https://github.com/openai/codex",
            CliProvider::Gemini => "https://github.com/google-gemini/gemini-cli",
            CliProvider::Copilot => "https://githubnext.com/projects/copilot-cli",
        };
        anyhow::bail!(
            "{} CLI ('{}') not found in PATH. Install it first.\nSee: {}",
            provider.display_name(),
            cmd,
            install_url,
        );
    }
    Ok(())
}

/// Build the agent CLI command with appropriate flags for the selected provider.
///
/// **Model emission semantics:**
///   * Claude / Codex — pass any non-empty, non-`default` value through
///     unchanged. Provider CLI owns model validation and future model IDs.
///   * Gemini / Copilot — never emit `--model` (those CLIs don't accept it
///     in this codebase).
fn build_agent_command(args: &RunArgs) -> String {
    let provider = args.tool.to_cli_provider();
    let mut cmd_parts = vec![provider.command().to_string()];

    match provider {
        CliProvider::Claude | CliProvider::Codex => {
            if let Some(model) = requested_model(args.model.as_deref()) {
                cmd_parts.push("--model".to_string());
                cmd_parts.push(model);
            }
        }
        CliProvider::Gemini | CliProvider::Copilot => {
            // No model flag for these providers (today).
        }
    }

    // Add permission skip flag (provider-specific)
    if args.dangerously_skip_permissions {
        cmd_parts.push(provider.skip_permissions_flag().to_string());
    }

    cmd_parts
        .iter()
        .map(|part| shell_escape::escape(part.into()).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Send a prompt to the tmux session
async fn send_prompt_to_tmux(session_name: &str, prompt: &str) -> Result<()> {
    // Send the prompt text
    let output = Command::new("tmux")
        .args(["send-keys", "-t", session_name, prompt, "C-m"])
        .output()
        .await?;

    if output.status.success() {
        info!("Sent initial prompt to session");
    } else {
        warn!(
            "Failed to send prompt: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Poll the tmux pane until the agent's input box is ready, or `timeout` elapses.
/// Best-effort: on timeout we send anyway rather than drop the prompt.
async fn wait_for_prompt_ready(session_name: &str, timeout: Duration) {
    use crate::tmux::capture::{CaptureOptions, capture_pane};
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(pane) = capture_pane(session_name, CaptureOptions::visible()).await {
            if input_box_ready(&pane) {
                return;
            }
        }
        if Instant::now() >= deadline {
            warn!("Input box not detected within {timeout:?}; sending prompt anyway");
            return;
        }
        sleep(Duration::from_millis(250)).await;
    }
}

/// Whether a captured pane shows an interactive input box ready for a prompt.
/// Recognises the footer hints the agent CLIs print once their prompt is live;
/// deliberately conservative: an empty or splash pane returns false.
fn input_box_ready(pane: &str) -> bool {
    const READY_MARKERS: [&str; 4] = [
        "? for shortcuts",  // Claude Code idle prompt
        "esc to interrupt", // Claude Code mid-turn (still accepts input)
        "Ctrl+C to exit",   // codex / others
        "for newline",      // "shift+enter for newline" style hints
    ];
    READY_MARKERS.iter().any(|marker| pane.contains(marker))
}

/// How isolated a candidate session working directory is.
///
/// Decided purely from real on-disk git state, by walking ancestors:
/// a `.git` FILE is git's gitdir pointer and only exists inside a linked
/// worktree; a `.git` DIRECTORY is the repository itself, i.e. the shared
/// checkout every other tool in that tree also writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRoot {
    /// Inside a linked git worktree, so the session has its own branch,
    /// index and working tree. Nothing to warn about.
    LinkedWorktree,
    /// The repository checkout itself (or a subdirectory of one). A session
    /// rooted here shares the branch, index and working tree with anything
    /// else operating in that checkout.
    SharedCheckout,
    /// Not inside any git repository. `ainb run` still works, but there is no
    /// worktree to isolate, so the warning would be noise.
    NotAGitRepo,
}

/// Classify `path` by walking it and its ancestors for the first `.git` entry.
///
/// The nearest `.git` wins, which is exactly how git itself resolves a
/// directory: a subdirectory of a linked worktree is still isolated, and a
/// subdirectory of a plain clone is still shared.
///
/// ABSOLUTE PATHS ONLY, for the same reason as
/// [`InteractiveSessionManager::get_source_repository`](crate::interactive::InteractiveSessionManager::get_source_repository):
/// `Path::ancestors()` on a relative path ends at `""`, and
/// `Path::new("").join(".git")` is `".git"`, which resolves against the
/// PROCESS's current directory. The walk would classify whatever tree the
/// user happened to run `ainb` from, and then warn (or stay silent) about a
/// directory that is not the session's. `resolve_repo_path` canonicalizes
/// before this is ever called, so a relative path here means a programming
/// error, and the honest answer for a path we cannot resolve is "no verdict",
/// never a warning naming the wrong tree.
#[must_use]
pub fn classify_session_root(path: &std::path::Path) -> SessionRoot {
    if !path.is_absolute() {
        return SessionRoot::NotAGitRepo;
    }
    for ancestor in path.ancestors() {
        let dot_git = ancestor.join(".git");
        if dot_git.is_file() {
            return SessionRoot::LinkedWorktree;
        }
        if dot_git.is_dir() {
            return SessionRoot::SharedCheckout;
        }
    }
    SessionRoot::NotAGitRepo
}

/// Tell the user the session they just asked for has no isolation.
///
/// stderr, not `tracing`: `ainb run` installs the JSONL file log sink, so a
/// `warn!` alone would never reach the terminal. Advisory only, creation
/// continues either way.
fn warn_shared_checkout(work_dir: &std::path::Path) {
    let dir = work_dir.display();
    eprintln!();
    eprintln!("WARNING: this session has no isolation.");
    eprintln!("  Working dir: {dir}");
    eprintln!("  It is the checkout itself, not a git worktree, so the agent shares that");
    eprintln!("  branch, index and working tree with your editor and with every other");
    eprintln!("  session started there. Concurrent edits will collide.");
    eprintln!("  Fix: re-run with --worktree, or --create-branch <name> to also cut a branch.");
    eprintln!();
    warn!("session root {dir} is a shared checkout, not an isolated worktree");
}

/// Attach to a tmux session (replaces current process)
fn attach_to_session(session_name: &str) -> Result<()> {
    use std::os::unix::process::CommandExt;

    // This replaces the current process with tmux attach
    let err = std::process::Command::new("tmux")
        .args(["attach-session", "-t", session_name])
        .exec();

    // If exec returns, it means it failed
    anyhow::bail!("Failed to attach to session: {err}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Tool;

    use crate::test_support::git_bin;

    /// Run a git command in `dir`, failing the test with git's own stderr.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new(git_bin())
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap_or_else(|e| panic!("spawn {:?} in {}: {e}", git_bin(), dir.display()));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The warn-or-not predicate, exercised against real `git init` /
    /// `git worktree add` state. Hand-faking `.git` would prove nothing:
    /// the whole point is that git writes a FILE in a linked worktree and a
    /// DIRECTORY in a plain clone.
    #[test]
    fn classify_session_root_real_git_shapes() {
        // Shells out to `git`, so it must not run while a sibling test has
        // swapped `$PATH` out from under the process (see `with_path`).
        let _path_guard = PATH_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonicalize tempdir");

        // A directory outside any repository.
        let bare_dir = root.join("no-repo");
        std::fs::create_dir(&bare_dir).unwrap();
        assert_eq!(
            classify_session_root(&bare_dir),
            SessionRoot::NotAGitRepo,
            "a dir outside any repo must not be warned about"
        );

        // A plain clone: git writes `.git` as a DIRECTORY.
        let checkout = root.join("myrepo");
        std::fs::create_dir(&checkout).unwrap();
        git(&checkout, &["init", "--initial-branch=main"]);
        git(&checkout, &["config", "user.email", "t@example.com"]);
        git(&checkout, &["config", "user.name", "t"]);
        std::fs::write(checkout.join("README.md"), "hi\n").unwrap();
        git(&checkout, &["add", "README.md"]);
        git(&checkout, &["commit", "-m", "init"]);

        assert!(checkout.join(".git").is_dir(), "precondition: plain clone");
        assert_eq!(
            classify_session_root(&checkout),
            SessionRoot::SharedCheckout,
            "the checkout root itself has no isolation"
        );

        // A subdirectory of the checkout is equally unisolated. This is the
        // exact shape that produced the original report.
        let subdir = checkout.join("sub");
        std::fs::create_dir(&subdir).unwrap();
        assert_eq!(
            classify_session_root(&subdir),
            SessionRoot::SharedCheckout,
            "a subdir of a plain checkout is still the shared working tree"
        );

        // A real linked worktree: git writes `.git` as a FILE (gitdir pointer).
        let wt = root.join("wt-feature");
        git(
            &checkout,
            &["worktree", "add", "-b", "feature", wt.to_str().unwrap()],
        );
        assert!(wt.join(".git").is_file(), "precondition: linked worktree");
        assert_eq!(
            classify_session_root(&wt),
            SessionRoot::LinkedWorktree,
            "an isolated worktree must never be warned about"
        );

        let wt_sub = wt.join("nested");
        std::fs::create_dir(&wt_sub).unwrap();
        assert_eq!(
            classify_session_root(&wt_sub),
            SessionRoot::LinkedWorktree,
            "a subdir of a linked worktree is still isolated"
        );
    }

    #[test]
    fn input_box_ready_detects_prompt_footer_not_splash() {
        assert!(input_box_ready("output line\n? for shortcuts"));
        assert!(input_box_ready("│ > │\nesc to interrupt"));
        assert!(!input_box_ready(""));
        assert!(!input_box_ready(
            "Loading…\n╭──────────╮\n│ Welcome  │\n╰──────────╯"
        ));
    }

    #[test]
    fn test_build_agent_command() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Claude,
            model: Some("sonnet".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: true,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(cmd.contains("claude"));
        assert!(
            cmd.contains("--model sonnet"),
            "AINB must pass Claude's raw model value through, got: {cmd}"
        );
        assert!(cmd.contains("--dangerously-skip-permissions"));
    }

    #[test]
    fn test_build_claude_command_passes_unknown_model_through() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Claude,
            model: Some("claude-next-9".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(
            cmd.contains("--model claude-next-9"),
            "AINB must not reject future Claude model IDs, got: {cmd}"
        );
    }

    #[test]
    fn test_build_agent_command_minimal() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Claude,
            model: Some("opus".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("--model opus"));
        assert!(!cmd.contains("--dangerously-skip-permissions"));
    }

    #[test]
    fn test_build_agent_command_system_default_omits_model_flag() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Claude,
            model: Some(String::new()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(cmd.starts_with("claude"));
        assert!(
            !cmd.contains("--model"),
            "SystemDefault must NOT emit --model, got: {cmd}"
        );
    }

    #[test]
    fn test_build_agent_command_no_model_at_all_omits_flag() {
        // `None` should behave identically to SystemDefault — no --model.
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Claude,
            model: None,
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(!cmd.contains("--model"));
    }

    #[test]
    fn test_build_codex_command_default_model_omits_flag() {
        // 2026-05 refresh: Codex CAN emit `--model`, but only when the
        // resolved CodexModel is non-default. `"sonnet"` is a Claude alias
        // that doesn't parse into any CodexModel variant → SystemDefault →
        // no `--model` flag.
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Codex,
            model: None,
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(
            cmd.starts_with("codex"),
            "Command should start with codex, got: {}",
            cmd
        );
        assert!(
            !cmd.contains("--model"),
            "Codex with default model should not have --model flag, got: {cmd}"
        );
    }

    #[test]
    fn test_build_codex_command_with_explicit_model() {
        // 2026-05 refresh: when a real CodexModel id is passed, Codex emits
        // `--model <id>` like Claude. This used to be asserted as "Codex
        // never has --model" — that assertion is gone.
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Codex,
            model: Some("gpt-5.4".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(cmd.starts_with("codex"));
        assert!(
            cmd.contains("--model gpt-5.4"),
            "Codex with explicit gpt-5.4 must emit --model, got: {cmd}"
        );
    }

    #[test]
    fn test_build_codex_command_passes_unknown_model_through() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Codex,
            model: Some("gpt-5.6-luna".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(
            cmd.contains("--model gpt-5.6-luna"),
            "AINB must not reject future Codex model IDs, got: {cmd}"
        );
    }

    #[test]
    fn test_build_codex_command_with_skip_permissions() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Codex,
            model: None,
            prompt: None,
            attach: false,
            dangerously_skip_permissions: true,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(
            cmd.starts_with("codex"),
            "Command should start with codex, got: {}",
            cmd
        );
        assert!(
            cmd.contains("--dangerously-bypass-approvals-and-sandbox"),
            "Codex skip permissions should use --dangerously-bypass-approvals-and-sandbox, got: {}",
            cmd,
        );
        assert!(
            !cmd.contains("--dangerously-skip-permissions"),
            "Codex should not use Claude's skip permissions flag"
        );
    }

    #[test]
    fn test_build_gemini_command() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Gemini,
            model: Some("sonnet".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(
            cmd.starts_with("gemini"),
            "Command should start with gemini, got: {}",
            cmd
        );
        assert!(
            !cmd.contains("--model"),
            "Gemini should not have --model flag"
        );
    }

    #[test]
    fn test_build_copilot_command() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Copilot,
            model: Some("sonnet".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert!(
            cmd.starts_with("copilot"),
            "Command should start with copilot, got: {}",
            cmd
        );
    }

    #[test]
    fn test_build_copilot_command_no_skip_permissions() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Copilot,
            model: Some("sonnet".to_string()),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
            parent: None,
        };

        let cmd = build_agent_command(&args);
        assert_eq!(
            cmd, "copilot",
            "Copilot with no flags should just be 'copilot'"
        );
    }

    /// Serialises every test that depends on `$PATH`, both the ones that swap
    /// it (`validate_provider_installed` resolves against the live
    /// environment) and the ones that shell out while it must stay intact.
    /// `cargo test` runs a binary's tests as threads of ONE process, so an
    /// unguarded PATH swap is visible to every other test.
    static PATH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Run `f` with `$PATH` set to `path`, restoring the original after.
    fn with_path<T>(path: &std::path::Path, f: impl FnOnce() -> T) -> T {
        let _guard = PATH_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let original = std::env::var_os("PATH");
        std::env::set_var("PATH", path);
        let out = f();
        match original {
            Some(p) => std::env::set_var("PATH", p),
            None => std::env::remove_var("PATH"),
        }
        out
    }

    /// Write an executable stub named `name` into `dir`.
    fn stub_binary(dir: &std::path::Path, name: &str) {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("write stub binary");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub binary");
    }

    /// `validate_provider_installed` accepts a provider whose CLI is resolvable
    /// on `$PATH`.
    ///
    /// Driven against a stub on a controlled `$PATH` rather than a real
    /// `claude` install: the old version asserted "Claude CLI should be
    /// installed on this machine", which is a statement about the developer's
    /// laptop, not about the code. It passed locally, failed on any runner
    /// without the CLI, and was papered over with a `--skip` in CI.
    #[test]
    fn validate_provider_installed_accepts_a_binary_on_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        stub_binary(dir.path(), CliProvider::Claude.command());

        let result = with_path(dir.path(), || {
            validate_provider_installed(&CliProvider::Claude)
        });
        assert!(
            result.is_ok(),
            "a `{}` on PATH must validate: {:?}",
            CliProvider::Claude.command(),
            result.err()
        );
    }

    /// The NEGATIVE half: an empty `$PATH` is rejected, with an error naming the
    /// missing binary and its install URL. Without this the positive case above
    /// could pass on a `validate_provider_installed` that returned `Ok(())`
    /// unconditionally.
    #[test]
    fn validate_provider_installed_rejects_a_binary_absent_from_path() {
        let dir = tempfile::tempdir().expect("tempdir");

        let result = with_path(dir.path(), || {
            validate_provider_installed(&CliProvider::Claude)
        });
        let err = result.expect_err("an empty PATH must not validate").to_string();
        assert!(
            err.contains("not found in PATH"),
            "error must name the PATH lookup, got: {err}"
        );
        assert!(
            err.contains("docs.anthropic.com"),
            "error must carry the install URL, got: {err}"
        );
    }

    #[test]
    fn test_validate_provider_installed_nonexistent() {
        // Use a provider struct pointing to a binary that definitely doesn't exist
        // We test via the function directly with a known-missing binary
        let result = validate_provider_installed(&CliProvider::Copilot);
        // Copilot CLI is unlikely to be installed in CI/dev - if it is, that's fine too
        // The important thing is the function doesn't panic
        if result.is_err() {
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("not found in PATH"),
                "Error should mention PATH, got: {}",
                err
            );
            assert!(
                err.contains("GitHub Copilot"),
                "Error should mention provider name, got: {}",
                err
            );
            assert!(
                err.contains("githubnext.com"),
                "Error should include install URL, got: {}",
                err
            );
        }
    }
}
