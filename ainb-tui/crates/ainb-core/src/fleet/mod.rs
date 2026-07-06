// ABOUTME: Fleet orchestration core — multi-session discovery, state reading, and send routing.
//
// Provides the building blocks for the `ainb fleet …` CLI subcommands
// (broadcast / sequence / needs / daemon / standup). The CLI dispatchers live
// under `crate::cli::fleet`; this module owns the pure logic.
//
// Layers:
// - `discover/` — unified session enumeration across ainb, claude-peers, bg jobs
// - `read/`     — state signals from tmux panes + JSONL transcripts + error regex
// - `send/`     — prompt delivery via claude-peers broker (preferred) or tmux send-keys
// - `types`     — Session / SessionState / Signal records shared across layers
//
// `discover`, `enrich_cache`, `read::{errors,jsonl_tail,needs,tmux_pane}`,
// `send`, and `types` were EXTRACTED into `ainb-fleet-core` so the hangar
// daemon can reuse the classifier and the verified send path without a crate
// cycle (`ainb` depends on `ainb-hangar-daemon`). They are re-exported here so
// every `crate::fleet::…` path in the TUI/CLI keeps resolving unchanged.

#![allow(missing_docs)]

pub mod atc;
pub mod bridge;
pub mod control;
pub mod daemons;
pub mod plumbing;
pub mod read;

pub use ainb_fleet_core::fleet::{discover, enrich_cache, send, types};

pub use types::{
    AinbSession, Block, BrokerPeer, Liveness, SendOutcome, Session, SessionSource, SessionState,
    Signal,
};
