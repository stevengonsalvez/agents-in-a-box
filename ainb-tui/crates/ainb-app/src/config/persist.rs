// ABOUTME: Writes the stores an `Effect::Persist` names. A host calls this
// after the step that changed a store has finished; the reducer only queues
// the effect, so no reducer arm waits on disk.

use crate::app::effect::Persist;
use crate::config::{AppConfig, OnboardingConfig};

/// Write `persist` to its store, or say why it could not be written.
///
/// # Errors
///
/// The store's own write error, as text for a report.
pub fn write(persist: &Persist) -> Result<(), String> {
    match persist {
        Persist::AppConfig { config, keys } => {
            config.0.save_keys(keys).map_err(|error| error.to_string())
        }
        Persist::ConfigExternalKeys(edits) => {
            AppConfig::save_external_keys(edits).map_err(|error| error.to_string())
        }
        Persist::Favorites(store) => store.0.save().map_err(|error| error.to_string()),
        Persist::SessionLabels(store) => store.0.save().map_err(|error| error.to_string()),
        Persist::Onboarding(record) => record.0.save().map_err(|error| error.to_string()),
        Persist::OnboardingGitDirectories(directories) => {
            // A record that exists but does not load is left alone: writing a
            // default over it would lose everything but the directories.
            let mut record = OnboardingConfig::load().map_err(|error| error.to_string())?;
            record.git_directories.clone_from(directories);
            record.save().map_err(|error| error.to_string())
        }
        Persist::SessionHeadroom {
            tmux_session,
            expected,
            enabled,
        } => {
            let mut moved = false;
            // P6e: through the process's session source. The compare-and-set
            // runs inside the resolver's read-modify-write, against the
            // table on the daemon, so a moved value is still never overwritten.
            crate::cli::util::mutate_session_store(|store| {
                if let Some(meta) = store.sessions.get_mut(tmux_session) {
                    if meta.headroom_enabled == *expected {
                        meta.headroom_enabled = *enabled;
                    } else {
                        moved = true;
                    }
                }
            })
            .map_err(|error| error.to_string())?;
            if moved {
                return Err(format!(
                    "the Headroom switch for '{tmux_session}' changed since it was read"
                ));
            }
            Ok(())
        }
    }
}
