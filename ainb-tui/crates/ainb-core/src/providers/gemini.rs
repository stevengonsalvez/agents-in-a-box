//! Built-in `gemini` provider — Google Gemini CLI.

use super::Provider;

#[derive(Debug, Default)]
pub struct GeminiProvider;

impl Provider for GeminiProvider {
    fn id(&self) -> &'static str { "gemini" }
    fn display_name(&self) -> &'static str { "Google Gemini" }
    fn command(&self) -> &'static str { "gemini" }
    fn api_key_env_var(&self) -> Option<&'static str> { Some("GEMINI_API_KEY") }
    fn skip_permissions_flag(&self) -> Option<&'static str> { Some("-y") }
    fn install_docs_url(&self) -> &'static str { "https://github.com/google-gemini/gemini-cli" }
}
