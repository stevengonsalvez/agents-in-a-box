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

#![allow(missing_docs)]

pub mod discover;
pub mod enrich_cache;
pub mod read;
pub mod send;
pub mod types;

pub use types::{
    AinbSession, Block, BrokerPeer, Liveness, SendOutcome, Session, SessionSource, SessionState,
    Signal,
};
