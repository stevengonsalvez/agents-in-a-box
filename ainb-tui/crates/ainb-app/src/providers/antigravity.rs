//! Built-in `antigravity` provider: Google Antigravity CLI.

use super::Provider;

#[derive(Debug, Default)]
pub struct AntigravityProvider;

impl Provider for AntigravityProvider {
    fn id(&self) -> &'static str {
        "antigravity"
    }
    fn display_name(&self) -> &'static str {
        "Google Antigravity"
    }
    fn command(&self) -> &'static str {
        "agy"
    }
    fn api_key_env_var(&self) -> Option<&'static str> {
        Some("GEMINI_API_KEY")
    }
    fn skip_permissions_flag(&self) -> Option<&'static str> {
        Some("--dangerously-skip-permissions")
    }
    fn install_docs_url(&self) -> &'static str {
        "https://github.com/google/antigravity"
    }
}
