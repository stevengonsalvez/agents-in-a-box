// ABOUTME: Main application structure and state management for the TUI

pub mod attach_handler;
pub mod event_bus;
pub mod events;
pub mod registry;
pub mod screens;
pub mod session_loader;
pub mod snapshot;
pub mod state;
pub mod usage_event_bridge;

pub use attach_handler::AttachHandler;
pub use events::EventHandler;
pub use registry::ScreenRegistry;
pub use screens::{Screen, ScreenId};
pub use session_loader::SessionLoader;
pub use state::{App, AppState};
