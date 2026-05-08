//! Built-in `copilot` provider — GitHub Copilot CLI.

use super::Provider;

#[derive(Debug, Default)]
pub struct CopilotProvider;

impl Provider for CopilotProvider {
    fn id(&self) -> &'static str { "copilot" }
    fn display_name(&self) -> &'static str { "GitHub Copilot" }
    fn command(&self) -> &'static str { "copilot" }
    fn api_key_env_var(&self) -> Option<&'static str> { Some("GITHUB_TOKEN") }
    fn skip_permissions_flag(&self) -> Option<&'static str> { Some("--yolo") }
    fn install_docs_url(&self) -> &'static str { "https://githubnext.com/projects/copilot-cli" }
}
