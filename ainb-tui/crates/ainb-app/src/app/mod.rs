// ABOUTME: The renderer-agnostic application state machine: AppState and its
// versioned sections, the event reducer, the keymap and the loaders that feed
// them. Renderers draw from it; `ainb-core::app` re-exports it and adds the
// terminal's UiState, attach handler and screen registry.

pub mod effect;
pub mod event_bus;
// The reducer's event enum and the handler that applies it stay behind
// `dispatch`. `test-support` opens them to integration tests that assert on
// resolved events directly; the normal dependency graph never enables it,
// which `tests/renderer_free.rs` checks.
#[cfg(feature = "test-support")]
pub mod events;
#[cfg(not(feature = "test-support"))]
mod events;
pub mod intent;
pub mod keymap;
pub mod keymap_defaults;
pub mod keymap_toml;
pub mod plugin_action;
pub mod pointer;
pub mod reports;
pub mod screens;
pub mod sections;
pub mod session_loader;
pub mod snapshot;
pub mod state;
pub mod versioned;

pub use effect::{
    EditorPath, Effect, Persist, PluginInput, Snapshot, TerminalTarget, TmuxSessionName,
    ToolTerminal,
};
#[cfg(any(test, feature = "test-support"))]
pub use events::{AppEvent, EventHandler};
// The wire sample state opens the Ctrl+K popup through the real reducer.
#[cfg(not(any(test, feature = "test-support")))]
pub(crate) use events::{AppEvent, EventHandler};
pub use events::{
    NoRenderer, RendererHost, is_in_text_input_context, skill_manager_overlay_open,
    slash_command_intent,
};
pub use intent::{Args, Btn, Intent, Pos, dispatch};
pub use keymap::{Chord, CommandId, Key, Keymap, Mods};
pub use screens::ScreenId;
pub use session_loader::SessionLoader;
pub use state::AppState;
pub use versioned::{SectionId, Versioned};
