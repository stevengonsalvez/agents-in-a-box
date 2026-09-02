// ABOUTME: Send-side fleet primitives: broker HTTP, tmux send-keys, route.

pub mod broker;
pub mod route;
pub mod tmux;

pub use broker::{BrokerClient, broker_health, broker_send};
pub use route::{send, tmux_delivery_preferred};
pub use tmux::{
    SendError, SendFailure, pane_has_unsubmitted_input, tmux_press_enter, tmux_send,
    tmux_send_picker_key, tmux_session_exists,
};
