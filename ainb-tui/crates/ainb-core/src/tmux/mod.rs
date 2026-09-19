// ABOUTME: Terminal-side tmux helpers. The tmux service layer lives in
// `ainb-app`; this module re-exports it and adds what belongs to the terminal
// host: the PTY-backed tmux client behind the preview pane and the crossterm
// key and mouse encoders that feed it.

pub use ainb_app::tmux::*;

pub mod embed_client;
pub mod embed_input;
pub mod pty_wrapper;

pub use embed_client::EmbedClient;
pub use embed_input::{encode_key_event, encode_mouse_event, rejoin_paste};
pub use pty_wrapper::PtyWrapper;
