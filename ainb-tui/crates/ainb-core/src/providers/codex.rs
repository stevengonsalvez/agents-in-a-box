//! Built-in `codex` provider — OpenAI Codex CLI.

use super::Provider;

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
}
