//! The macOS menu bar, built item by item.
//!
//! Tauri's default menu carries a Close Window item bound to Cmd+W, which
//! swallows the accelerator before the webview sees it, so the shell could
//! never close a terminal tab with it. This menu is the default minus that one
//! item: Cmd+W reaches the webview, and Cmd+Q still quits.
//!
//! macOS only. Elsewhere the window has no menu bar and every accelerator
//! reaches the webview already.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem, Submenu};
use tauri::{AppHandle, Runtime};

/// The updater's menu ids, handled in `main.rs`. Until the settings page
/// carries an updates section, the menu is the updater's only surface.
pub const UPDATE_CHECK: &str = "update.check";
pub const UPDATE_INSTALL: &str = "update.install";
pub const UPDATE_ROLLBACK: &str = "update.rollback";

/// Install the menu. A failure is logged and left: a window with the default
/// menu is worth more than no window.
pub fn install<R: Runtime>(app: &AppHandle<R>) {
    if !cfg!(target_os = "macos") {
        return;
    }
    match build(app) {
        Ok(menu) => {
            if let Err(error) = app.set_menu(menu) {
                tracing::warn!(%error, "the app menu was not installed");
            }
        }
        Err(error) => tracing::warn!(%error, "the app menu was not built"),
    }
}

fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let application = Submenu::with_items(
        app,
        "Agents in a Box",
        true,
        &[
            &PredefinedMenuItem::about(app, None, None)?,
            &PredefinedMenuItem::separator(app)?,
            &MenuItem::with_id(
                app,
                UPDATE_CHECK,
                "Check for Updates...",
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(
                app,
                UPDATE_INSTALL,
                "Install Update and Restart",
                true,
                None::<&str>,
            )?,
            &MenuItem::with_id(app, UPDATE_ROLLBACK, "Roll Back Update", true, None::<&str>)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::services(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::hide(app, None)?,
            &PredefinedMenuItem::hide_others(app, None)?,
            &PredefinedMenuItem::show_all(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::quit(app, None)?,
        ],
    )?;
    // Edit is what gives the webview its native copy and paste on macOS.
    let edit = Submenu::with_items(
        app,
        "Edit",
        true,
        &[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
            &PredefinedMenuItem::separator(app)?,
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ],
    )?;
    // No Close Window: Cmd+W belongs to the tab strip.
    let window = Submenu::with_items(
        app,
        "Window",
        true,
        &[
            &PredefinedMenuItem::minimize(app, None)?,
            &PredefinedMenuItem::fullscreen(app, None)?,
        ],
    )?;
    Menu::with_items(app, &[&application, &edit, &window])
}
