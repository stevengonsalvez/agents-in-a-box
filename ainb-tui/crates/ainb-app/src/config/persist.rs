// ABOUTME: Writes the stores an `Effect::Persist` names. A host calls this
// after the step that changed a store has finished; the reducer only queues
// the effect, so no reducer arm waits on disk.

use crate::app::effect::Persist;
use crate::config::{AppConfig, OnboardingConfig};
use crate::interactive::session_manager::SessionStore;

/// Write `persist` to its store, or say why it could not be written.
///
/// # Errors
///
/// The store's own write error, as text for a report.
pub fn write(persist: &Persist) -> Result<(), String> {
    match persist {
        Persist::AppConfig(config) => config.0.save().map_err(|error| error.to_string()),
        Persist::Favorites(store) => store.0.save().map_err(|error| error.to_string()),
        Persist::SessionLabels(store) => store.0.save().map_err(|error| error.to_string()),
        Persist::Onboarding(record) => record.0.save().map_err(|error| error.to_string()),
        Persist::OnboardingGitDirectories(directories) => {
            let mut record = OnboardingConfig::load().unwrap_or_default();
            record.git_directories.clone_from(directories);
            record.save().map_err(|error| error.to_string())
        }
        Persist::ClaudeAuthProvider(provider) => {
            let mut config = AppConfig::load().map_err(|error| error.to_string())?;
            config.authentication.claude_provider = provider.clone();
            config.save().map_err(|error| error.to_string())
        }
        Persist::SessionHeadroom {
            tmux_session,
            enabled,
        } => SessionStore::mutate(|store| {
            if let Some(meta) = store.sessions.get_mut(tmux_session) {
                meta.headroom_enabled = *enabled;
            }
        })
        .map_err(|error| error.to_string()),
    }
}
