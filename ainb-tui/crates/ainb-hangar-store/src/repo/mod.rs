//! Typed repository wrappers over the Hangar schema.
//!
//! Each sub-module is a thin, stateless sqlx layer over one logical group of
//! tables. Methods take a [`sqlx::SqlitePool`] borrow (handed out by
//! [`crate::Store::pool`]) rather than owning a connection, so a single pool is
//! shared across every repo call.

pub mod agent;
pub mod agent_runtime;
pub mod issue;
pub mod skill;
pub mod task;
