// ABOUTME: Terminal half of the application: UiState (scroll, geometry, mouse),
// the tty attach handler and the screen registry. The state machine itself lives
// in `ainb_app::app` and is re-exported here, so `crate::app::state` and friends
// keep resolving.

pub use ainb_app::app::*;

pub mod attach_handler;
pub mod mouse;
pub mod registry;
pub mod screens;
pub mod terminal_keys;
pub mod ui_state;

pub use attach_handler::AttachHandler;
pub use registry::ScreenRegistry;
pub use screens::{Screen, ScreenId};
