//! Built-in `claude` provider — Anthropic Claude Code CLI.

use super::{AtcControl, Provider};

#[derive(Debug, Default)]
pub struct ClaudeProvider;

impl Provider for ClaudeProvider {
    fn id(&self) -> &'static str {
        "claude"
    }
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }
    fn command(&self) -> &'static str {
        "claude"
    }
    fn api_key_env_var(&self) -> Option<&'static str> {
        Some("ANTHROPIC_API_KEY")
    }
    fn skip_permissions_flag(&self) -> Option<&'static str> {
        Some("--dangerously-skip-permissions")
    }
    fn install_docs_url(&self) -> &'static str {
        "https://docs.anthropic.com/en/docs/claude-code"
    }
    /// The original — and default — ATC full-mode brain. `ainb run --tool claude`
    /// gives a resident tmux session, `fleet send` injects the heartbeat into it,
    /// and Claude Code reads `CLAUDE.md` out of the instance dir.
    fn atc_control(&self) -> AtcControl {
        AtcControl::supported("CLAUDE.md")
    }
}
