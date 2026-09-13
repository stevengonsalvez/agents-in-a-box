// ABOUTME: Terminal-side tmux helpers. The tmux service layer lives in
// `ainb-app`; this module re-exports it and adds the crossterm key and mouse
// encoders, which belong to the terminal renderer.

pub use ainb_app::tmux::*;

pub mod embed_input;

pub use embed_input::{encode_key_event, encode_mouse_event};
