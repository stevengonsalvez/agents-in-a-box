-- 0075: one-time prune of the tmux transport flip-flop artefacts written by the
-- fleet-restore churn bug (fixed in PR #554).
--
-- WHAT HAPPENED. `restore_tmux_transport` matched rows by (tmux_target,
-- fingerprint) while the missing-sweep matched by session_key. When a pane's
-- session_key changed but the pane did not -- which is what provider
-- re-detection does, since the provider is part of SessionKey::legacy -- the
-- orphaned predecessor row satisfied both loops and flip-flopped: restore set it
-- HEALTHY, the sweep's skip (needing EXITED *and* UNAVAILABLE) then missed, and
-- it was marked UNAVAILABLE again. Two applied events per orphan per three-second
-- tick, forever. Measured on a real profile: 41,200 fleet_event rows, of which
-- 19,269 tmux_missing and 18,254 tmux_available were pure no-op churn.
--
-- WHY THIS IS SAFE FOR REPLAY CURSORS. `revision` is INTEGER PRIMARY KEY
-- AUTOINCREMENT (0044), so a deleted revision is never reused, and every replay
-- path is a `revision > ?` range scan that simply skips holes. A client parked
-- past the new head is already handled by the CursorAhead snapshot_reset. State
-- is served by the snapshot, not rebuilt from replay -- the plugin only calls
-- observe_revision per replay row -- so removing ledger noise cannot corrupt a
-- projection. Fewer rows also means reconnects land on Complete rather than
-- ReplayLimitExceeded more often.
--
-- SCOPE. One-time, deliberately NOT an ongoing retention policy: after #554 this
-- table records real transitions only. The RAW provider ledger
-- (`fleet_provider_event`) is a different table and stays never-trimmed by the
-- decision recorded in PR #548.
--
-- Each session keeps its NEWEST transport event so provenance survives, and the
-- ledger head is never deleted so no client can be left pointing past the end.
DELETE FROM fleet_event
WHERE event_type IN ('tmux_missing', 'tmux_available')
  AND revision < (SELECT MAX(revision) FROM fleet_event)
  AND revision NOT IN (
      SELECT MAX(revision) FROM fleet_event
      WHERE event_type IN ('tmux_missing', 'tmux_available')
      GROUP BY session_key
  );
