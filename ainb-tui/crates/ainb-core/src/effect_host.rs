// ABOUTME: The terminal host's effect executor. The reducer in `ainb-app`
// queues `Effect`s; the run loop drains them once per iteration, after the
// step that queued them has finished writing state, and runs each one here.

use tracing::{error, info, warn};

use crate::app::{App, Effect};

/// Carry out one effect for the terminal host.
pub fn execute(effect: Effect, app: &mut App) {
    match effect {
        Effect::OpenEditor(path) => open_editor(app, &path),
    }
}

fn open_editor(app: &mut App, path: &std::path::Path) {
    info!("[EFFECT] Opening in editor: {:?}", path);
    let Some(editor) = resolve_editor(&app.state.config.app_config) else {
        warn!("No editor found in fallback chain");
        app.state.add_error_notification(
            "❌ No editor found. Set preferred editor in settings or install VS Code.".to_string(),
        );
        return;
    };
    info!("Opening {} in {}", path.display(), editor);
    match std::process::Command::new(&editor).arg(path).spawn() {
        Ok(_) => app.state.add_success_notification(format!("📝 Opened in {}", editor)),
        Err(e) => {
            error!("Failed to open editor: {}", e);
            app.state.add_error_notification(format!("❌ Failed to open editor: {}", e));
        }
    }
}

/// The editor to run: the configured preference, then `code`, then `$EDITOR`,
/// whichever is on `PATH` first.
fn resolve_editor(config: &crate::config::AppConfig) -> Option<String> {
    if let Some(ref editor) = config.ui_preferences.preferred_editor {
        if command_exists(editor) {
            return Some(editor.clone());
        }
    }
    if command_exists("code") {
        return Some("code".to_string());
    }
    if let Ok(editor) = std::env::var("EDITOR") {
        if command_exists(&editor) {
            return Some(editor);
        }
    }
    None
}

fn command_exists(cmd: &str) -> bool {
    std::process::Command::new("which")
        .arg(cmd)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
