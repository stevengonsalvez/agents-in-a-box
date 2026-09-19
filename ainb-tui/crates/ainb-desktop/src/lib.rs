//! The desktop shell's Rust side: a host of the `ainb-app` state machine, the
//! same way the terminal binary is one.
//!
//! - [`host::DesktopHost`] owns the `AppState`, applies intents through
//!   `ainb_app::dispatch`, and frames what moved for the webview.
//! - [`executor::DesktopExecutor`] carries out the effects a dispatch returns,
//!   and answers the ones this shell cannot run yet with their documented
//!   failure report.
//! - [`intent::RendererIntent`] is what the webview may send.
//! - [`clipboard`] holds the size rule a copy and a paste share.
//! - [`setup`] is the desktop-native path for the onboarding writes the
//!   webview may not run (#1175), behind a confirmation the shell owns.
//! - [`daemons_panel`] keeps the daemons collector alive while the settings
//!   page draws it.
//! - [`shell::Shell`] locks the host and the executor together for the
//!   window's commands and tick.
//! - [`sidecar`] finds or starts the bundled hangar daemon and holds this
//!   surface's presence against it.
//!
//! None of it needs a window: the Tauri binary (`app` feature) wires these to
//! channels and commands, and the tests drive them headless.

/// TypeScript for the shapes the webview sends and receives (#1158).
#[cfg(feature = "typescript-bindings")]
pub mod bindings;
pub mod clipboard;
pub mod daemons_panel;
pub mod executor;
pub mod host;
pub mod intent;
pub mod setup;
pub mod shell;
pub mod sidecar;
pub mod terminal;
pub mod updater;
