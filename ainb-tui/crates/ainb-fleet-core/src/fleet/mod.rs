// ABOUTME: Fleet-core module root — the extracted lower layers of the fleet
// stack (types, enrich cache, read/classify, send, discover).
//
// The module is deliberately named `fleet` so the files moved out of
// `ainb-core` keep their original `crate::fleet::…` self-references and needed
// no path rewrites. `ainb-core::fleet` re-exports these back so the TUI/CLI is
// unchanged; the hangar daemon consumes them directly.

#![allow(missing_docs)]

pub mod discover;
pub mod enrich_cache;
pub mod read;
pub mod repo_clone;
pub mod repo_roster;
pub mod send;
pub mod session_registry;
pub mod types;

pub use types::{
    AinbSession, Block, BrokerPeer, Liveness, SendOutcome, Session, SessionSource, SessionState,
    Signal,
};
