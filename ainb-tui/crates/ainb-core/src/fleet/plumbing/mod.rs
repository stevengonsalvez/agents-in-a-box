// ABOUTME: Durable orchestration plumbing — the event-driven substrate ATC consumes.
//
// This is SHARED session-lifecycle infrastructure (consumed by ATC, the phone
// bridge, and ainb itself), deliberately living in `fleet::plumbing` rather than
// inside the ATC instance code: ATC merely *consumes* it. It upgrades ATC from
// poll-mode (wait for the next heartbeat) to event-driven (learn the instant a
// child finishes).
//
// The pieces, mirroring agent-deck's hooks → event log → durable inbox →
// synchronous Stop-drain chain (see explainers/agent-deck-vs-ainb-fleet.html):
//
// - `atomic`  — crash-safe atomic write+fsync, the durability primitive.
// - `paths`   — `~/.agents-in-a-box/inbox/` layout (honours `AINB_HOME`).
// - `inbox`   — durable per-parent JSONL inbox: last-wins-per-child, fsync'd,
//               atomic rewrite, turn-fingerprint exactly-once, dead-letter sink.
// - `drain`   — pure Stop-drain decision: empty-inbox fast path + the
//               consecutive-block budget; emits Claude's `{"decision":"block"}`.
// - `hooks`   — idempotent read-preserve-modify-write merge of the lifecycle
//               hook set into `~/.claude/settings.json` (preserves reflect +
//               notifyd hooks).
//
// `commit_completion` is the one routing entry point a finishing child uses: it
// resolves the parent and commits to that parent's inbox, dead-lettering when
// the parent is unresolvable.
//
// NOTE: the former `status/<session_id>.json` lifecycle files (the `status`
// module) are RETIRED. The hook no longer writes them (Wave 2 replaced the
// write with an append to the event log) and no reader consumes them (Wave 4
// migrated `fleet needs` / the ATC heartbeat onto the event-sourced
// `current_state` table). Coarse per-session lifecycle is now `current_state`
// (kind RUNNING/DONE/…), read via `fleet::read::current_state`.

pub mod atomic;
pub mod drain;
pub mod hooks;
pub mod inbox;
pub mod parent;
pub mod paths;
pub mod settings;

use std::path::Path;

use anyhow::Result;

pub use drain::{BlockBudget, DEFAULT_BLOCK_BUDGET, StopDecision, decide, format_completions};
pub use inbox::{Inbox, InboxRecord, dead_letter_in};
pub use parent::{PARENT_ENV, record_parent_in, resolve_parent_in};

/// Commit a finished child's completion to its parent's inbox under `home`.
///
/// Routing: a non-empty `parent_id` routes to that parent's durable inbox; an
/// empty/blank parent (the child has no recorded parent — it is a leaf or was
/// spawned outside ATC) means the completion has no consumer, so it is sent to
/// the dead-letter sink rather than silently dropped. Returns `true` when it was
/// routed to a live parent inbox, `false` when dead-lettered.
pub fn commit_completion(home: &Path, record: &InboxRecord) -> Result<bool> {
    let inbox_dir = paths::inbox_dir_in(home);
    if record.parent_id.trim().is_empty() {
        dead_letter_in(&inbox_dir, record)?;
        return Ok(false);
    }
    let inbox = Inbox::open_in(&inbox_dir, &record.parent_id);
    inbox.commit(record)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn commit_completion_routes_to_parent_inbox() {
        let home = TempDir::new().unwrap();
        let rec = InboxRecord::new("child", "parent", "done", "Stop", 1);
        let routed = commit_completion(home.path(), &rec).unwrap();
        assert!(routed, "should route to live parent");
        let inbox = Inbox::open_in(&paths::inbox_dir_in(home.path()), "parent");
        assert_eq!(inbox.drain().unwrap().len(), 1);
    }

    #[test]
    fn commit_completion_dead_letters_orphan() {
        let home = TempDir::new().unwrap();
        let rec = InboxRecord::new("child", "", "done", "Stop", 1);
        let routed = commit_completion(home.path(), &rec).unwrap();
        assert!(!routed, "orphan should dead-letter");
        let dl = inbox::dead_letter_path_in(&paths::inbox_dir_in(home.path()));
        assert!(dl.exists());
        assert!(std::fs::read_to_string(dl).unwrap().contains("child"));
    }
}
