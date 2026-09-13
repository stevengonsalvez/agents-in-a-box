//! Built-in `codex` provider — OpenAI Codex CLI.

use super::{AtcControl, Provider};

#[derive(Debug, Default)]
pub struct CodexProvider;

impl Provider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }
    fn display_name(&self) -> &'static str {
        "OpenAI Codex"
    }
    fn command(&self) -> &'static str {
        "codex"
    }
    fn api_key_env_var(&self) -> Option<&'static str> {
        Some("OPENAI_API_KEY")
    }
    fn skip_permissions_flag(&self) -> Option<&'static str> {
        Some("--dangerously-bypass-approvals-and-sandbox")
    }
    fn install_docs_url(&self) -> &'static str {
        "https://github.com/openai/codex"
    }
    /// Codex hosts a resident tmux session the same way Claude does and takes the
    /// same `fleet send` injection, so full mode is real for it. It reads
    /// `AGENTS.md`, not `CLAUDE.md` — rendering the policy under the wrong name
    /// would give a brain that boots and then ignores its playbook.
    fn atc_control(&self) -> AtcControl {
        AtcControl::supported("AGENTS.md")
    }
}
