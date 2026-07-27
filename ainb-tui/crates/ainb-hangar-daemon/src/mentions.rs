//! `@`-mention parsing over a comment body — a re-export shim.
//!
//! The grammar moved to [`ainb_hangar_core::mention`] (multica parity #2-rest)
//! so the CLI's store-direct path and the daemon's RPC path parse a body with
//! ONE implementation. The move was verbatim: [`parse_mentions`] is
//! [`ainb_hangar_core::mention::parse_handles`] under its historical name, and
//! the nine bare-grammar regression tests moved with it.
//!
//! [`parse`] is the newer, fuller scan: it also lifts multica's
//! `[@Label](mention://type/id)` link form out of the body. The *resolution*
//! half (matching a target to a real agent / member / squad in the comment's
//! workspace, gating it, and enqueuing) lives in
//! `ainb_hangar_store::service::mention`, driven from the `comment_add` handler
//! after the comment commits.

pub use ainb_hangar_core::mention::{parse, parse_handles as parse_mentions};
