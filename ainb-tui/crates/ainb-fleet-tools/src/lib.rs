//! The fleet copilot's MCP tool server (part-2 plan, phase A1).
//!
//! The copilot is an ACP session that receives this process through
//! `session/new mcpServers`. Its tools call BACK into the hangar daemon over
//! `hangar.sock`, so a copilot action is the same `fleet/message_send` or
//! `attention/answer` a human client issues, with the same receipts, ordering
//! and idempotency. Nothing here drives tmux or a provider directly.
//!
//! ```text
//!   copilot ACP session
//!        │ MCP stdio
//!        ▼
//!   ┌──────────────────────┐   classify first, always
//!   │ [`server`]           │──▶ [`guardrail`] Auto | Confirm | Refused
//!   └─────────┬────────────┘
//!             │ Auto only
//!             ▼
//!   ┌──────────────────────┐   reads wrapped by [`envelope`]
//!   │ [`fleet`]            │──▶ hangar.sock (ainb-hangar-client)
//!   └──────────────────────┘
//! ```
//!
//! Two rules hold the crate together, both from the plan's Trust boundary:
//!
//! 1. The classifier decides on the tool and its arguments plus daemon-pinned
//!    turn state. Never on anything the model wrote as prose.
//! 2. Everything the tools return that was authored by another agent goes back
//!    inside part 1's fenced, escaped envelope, framed as observed data.
//!
//! The crate is independently testable: [`fleet::FleetTools`] talks to whatever
//! socket its [`ainb_hangar_client::DaemonClient`] was built with, so
//! `tests/round_trip.rs` runs the whole path against a fake daemon.

pub mod envelope;
pub mod fleet;
pub mod guardrail;
pub mod keyfile;
pub mod server;
