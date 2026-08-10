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
//!   ┌──────────────────────┐   gate first, always
//!   │ [`server`]           │──▶ fleet/copilot_gate ──▶ [`guardrail`] IN THE
//!   └─────────┬────────────┘    (hangar.sock)          DAEMON, which parks a
//!             │ run only                               confirm card and waits
//!             ▼
//!   ┌──────────────────────┐   reads wrapped by [`envelope`]
//!   │ [`fleet`]            │──▶ hangar.sock (ainb-hangar-client)
//!   └──────────────────────┘
//! ```
//!
//! Three rules hold the crate together, the first two from the plan's Trust
//! boundary:
//!
//! 1. The classifier decides on the tool and its arguments plus daemon-pinned
//!    turn state. Never on anything the model wrote as prose.
//! 2. Everything the tools return that was authored by another agent goes back
//!    inside part 1's fenced, escaped envelope, framed as observed data.
//! 3. The classifier RUNS in the daemon, not here. [`guardrail`] is pure and
//!    lives here because it is this crate's contract (its tool table and its
//!    argument shapes), but this process never calls it: a second live copy of
//!    the rules, running downstream of every transcript the copilot has read,
//!    is a second thing to keep in step and the easier one to soften.
//!
//! The crate is independently testable: [`fleet::FleetTools`] talks to whatever
//! socket its [`ainb_hangar_client::DaemonClient`] was built with, so
//! `tests/round_trip.rs` runs the whole path against a fake daemon.

pub mod envelope;
pub mod fleet;
pub mod guardrail;
pub mod keyfile;
pub mod server;
