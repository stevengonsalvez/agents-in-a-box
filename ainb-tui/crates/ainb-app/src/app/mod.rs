// ABOUTME: The renderer-agnostic application state machine: AppState and its
// versioned sections, the event reducer, the keymap and the loaders that feed
// them. Renderers draw from it; `ainb-core::app` re-exports it and adds the
// terminal's UiState, attach handler and screen registry.

pub mod event_bus;
pub mod events;
pub mod intent;
pub mod keymap;
pub mod keymap_defaults;
pub mod keymap_toml;
pub mod screens;
pub mod sections;
pub mod session_loader;
pub mod snapshot;
pub mod state;
pub mod versioned;

pub use events::{EventHandler, NoRenderer, RendererHost};
pub use intent::{Args, Btn, Intent, Pos, dispatch};
pub use keymap::{Chord, CommandId, Key, Keymap, Mods};
pub use screens::ScreenId;
pub use session_loader::SessionLoader;
pub use state::{App, AppState};
pub use versioned::{SectionId, Versioned};
