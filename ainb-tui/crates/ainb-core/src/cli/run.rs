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
use tokio::time::{Duration, sleep};
use tracing::{info, warn};
use uuid::Uuid;

use super::RunArgs;
use crate::config::CliProvider;
use crate::git::worktree_manager::WorktreeManager;
use crate::interactive::session_manager::{SessionMetadata, SessionStore};
use crate::models::ClaudeModel;
use crate::models::session::SessionAgentType;
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
        branch_name = get_current_branch(&repo_path).unwrap_or_else(|| "main".to_string());
    }

    // Step 4: Generate session name
    let session_name = args.name.clone().unwrap_or_else(|| {
        let short_id = &session_id.to_string()[..8];
        format!("{workspace_name}-{short_id}")
    });

    // Step 5: Parse model
    let model = parse_model(&args.model);

    // Step 6: Build Claude command
    let claude_cmd = build_agent_command(&args, Some(model));

    // Step 7: Create tmux session
    let mut tmux = TmuxSession::new(session_name.clone(), claude_cmd.clone());
    tmux.start(&work_dir).await.context("Failed to start tmux session")?;

    let tmux_name = tmux.name().to_string();
    info!("Started tmux session: {}", tmux_name);

    // Step 8: Send initial prompt if provided
    if let Some(ref prompt) = args.prompt {
        info!("Sending initial prompt after 2 second delay...");
        sleep(Duration::from_secs(2)).await;
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
    };

    let mut store = SessionStore::load();
    store.upsert(metadata);
    store.save().context("Failed to save session metadata")?;

    info!("Saved session metadata for TUI discovery");

    // Step 10: Print session info
    println!();
    println!("Session created successfully!");
    println!("  Session ID:   {session_id}");
    println!("  Tmux Session: {tmux_name}");
    println!("  Working Dir:  {}", work_dir.display());
    println!("  Branch:       {branch_name}");
    println!("  Model:        {}", model.display_name());
    println!();
    println!("To attach to this session:");
    println!("  tmux attach -t {tmux_name}");
    println!();
    println!("Or use:");
    println!("  ainb attach {session_name}");
    println!();

    // Step 11: Attach if requested
    if args.attach || args.interactive {
        println!("Attaching to session...");
        attach_to_session(&tmux_name)?;
    }

    Ok(())
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

/// Get the current branch name from a repository
fn get_current_branch(repo_path: &std::path::Path) -> Option<String> {
    use git2::Repository;

    let repo = Repository::open(repo_path).ok()?;
    let head = repo.head().ok()?;

    if head.is_branch() {
        head.shorthand().map(std::string::ToString::to_string)
    } else {
        head.target().map(|oid| oid.to_string()[..8].to_string())
    }
}

/// Parse model string to `ClaudeModel` enum
fn parse_model(model_str: &str) -> ClaudeModel {
    match model_str.to_lowercase().as_str() {
        "opus" | "claude-opus" | "claude-3-opus" => ClaudeModel::Opus,
        "haiku" | "claude-haiku" | "claude-3-haiku" => ClaudeModel::Haiku,
        _ => ClaudeModel::Sonnet, // Default to Sonnet
    }
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

/// Build the agent CLI command with appropriate flags for the selected provider
fn build_agent_command(args: &RunArgs, model: Option<ClaudeModel>) -> String {
    let provider = args.tool.to_cli_provider();
    let mut cmd_parts = vec![provider.command().to_string()];

    // Add model flag (Claude-only)
    if provider == CliProvider::Claude {
        if let Some(m) = model {
            cmd_parts.push("--model".to_string());
            cmd_parts.push(m.cli_value().to_string());
        }
    }

    // Add permission skip flag (provider-specific)
    if args.dangerously_skip_permissions {
        cmd_parts.push(provider.skip_permissions_flag().to_string());
    }

    cmd_parts.join(" ")
}

/// Send a prompt to the tmux session
async fn send_prompt_to_tmux(session_name: &str, prompt: &str) -> Result<()> {
    // Target pane explicitly
    let target = format!("{session_name}:0");

    // Send the prompt text
    let output = Command::new("tmux")
        .args(["send-keys", "-t", &target, prompt, "C-m"])
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

    #[test]
    fn test_parse_model() {
        assert_eq!(parse_model("sonnet"), ClaudeModel::Sonnet);
        assert_eq!(parse_model("SONNET"), ClaudeModel::Sonnet);
        assert_eq!(parse_model("opus"), ClaudeModel::Opus);
        assert_eq!(parse_model("haiku"), ClaudeModel::Haiku);
        assert_eq!(parse_model("claude-sonnet"), ClaudeModel::Sonnet);
        assert_eq!(parse_model("unknown"), ClaudeModel::Sonnet); // Default
    }

    #[test]
    fn test_build_agent_command() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Claude,
            model: "sonnet".to_string(),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: true,
            name: None,
            interactive: false,
        };

        let cmd = build_agent_command(&args, Some(ClaudeModel::Sonnet));
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("--model sonnet"));
        assert!(cmd.contains("--dangerously-skip-permissions"));
    }

    #[test]
    fn test_build_agent_command_minimal() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Claude,
            model: "opus".to_string(),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
        };

        let cmd = build_agent_command(&args, Some(ClaudeModel::Opus));
        assert!(cmd.contains("claude"));
        assert!(cmd.contains("--model opus"));
        assert!(!cmd.contains("--dangerously-skip-permissions"));
    }

    #[test]
    fn test_build_codex_command() {
        let args = RunArgs {
            remote_repo: None,
            repo: None,
            create_branch: None,
            worktree: false,
            tool: Tool::Codex,
            model: "sonnet".to_string(),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
        };

        let cmd = build_agent_command(&args, Some(ClaudeModel::Sonnet));
        assert!(
            cmd.starts_with("codex"),
            "Command should start with codex, got: {}",
            cmd
        );
        assert!(
            !cmd.contains("--model"),
            "Codex should not have --model flag"
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
            model: "sonnet".to_string(),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: true,
            name: None,
            interactive: false,
        };

        let cmd = build_agent_command(&args, Some(ClaudeModel::Sonnet));
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
            model: "sonnet".to_string(),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
        };

        let cmd = build_agent_command(&args, Some(ClaudeModel::Sonnet));
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
            model: "sonnet".to_string(),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
        };

        let cmd = build_agent_command(&args, Some(ClaudeModel::Sonnet));
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
            model: "sonnet".to_string(),
            prompt: None,
            attach: false,
            dangerously_skip_permissions: false,
            name: None,
            interactive: false,
        };

        let cmd = build_agent_command(&args, None);
        assert_eq!(
            cmd, "copilot",
            "Copilot with no flags should just be 'copilot'"
        );
    }

    #[test]
    fn test_validate_provider_installed_claude() {
        // Claude CLI should be installed on this machine
        let provider = CliProvider::Claude;
        let result = validate_provider_installed(&provider);
        assert!(
            result.is_ok(),
            "Claude CLI should be found in PATH: {:?}",
            result.err()
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
