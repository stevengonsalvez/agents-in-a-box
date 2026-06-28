//! ainb-notifyd — the ainb-owned notification daemon.
//!
//! Listens on a Unix domain socket at `~/.agents-in-a-box/notify.sock`,
//! receives newline-delimited JSON envelopes from the `ainb-hooks` bash
//! script (and any other producer), persists every envelope to a SQLite
//! database, and broadcasts the row to in-process subscribers (e.g. the
//! ainb-tui Inbox screen via SQLite polling).
//!
//! The daemon also:
//!
//! - writes a PID file at `~/.agents-in-a-box/notify.pid` and handles
//!   `SIGTERM` / `SIGINT` gracefully (removes socket + PID, exits 0);
//! - replays + clears `~/.agents-in-a-box/notify.fallback.jsonl` on
//!   startup so events that hit the hook script while the daemon was
//!   down are recovered;
//! - runs a lightweight retention sweep on each insert (delete rows
//!   older than `retention_days` and over the `max_rows` cap, oldest
//!   first).
//!
//! See `.agents/specs/2026-05-27-ainb-hooks-plugin-stub-spec.md` for
//! the full design.

#![deny(missing_docs)]
#![allow(clippy::too_many_lines)]

pub mod cli;
pub mod envelope;
pub mod fallback;
pub mod install;
pub mod osnotify;
pub mod paths;
pub mod pid;
pub mod procs;
pub mod store;

mod listener;

pub use envelope::{Envelope, EnvelopeError};
pub use fallback::FallbackFile;
pub use install::{
    Agent, ClaudeRegister, InstallPrompt, InstallRecord, InstallReport, StatusRow, dismiss_prompt,
    embedded_plugin_version, install as install_for, install_under_home, prompt_state, status,
    uninstall,
};
pub use listener::{RunConfig, run_daemon};
pub use osnotify::{AlertKind, classify_attention};
pub use paths::Paths;
pub use pid::PidFile;
pub use procs::{
    ClassifiedDaemon, DaemonClass, NotifydProc, classify as classify_daemons, scan as scan_daemons,
};
pub use store::{NotificationRecord, RetentionPolicy, Store, StoreError};
