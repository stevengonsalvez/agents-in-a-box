-- Hangar v1 schema, migration 0045: stamp the dispatching SQUAD onto the task row.
--
-- Mirrors multica migration 127 (`agent_task_queue.squad_id`): a squad-dispatched
-- task (the leader brief + every fanned-out member, or a CLI leader-only assign)
-- records WHICH squad dispatched it, so the daemon can key a claim-time leader
-- briefing injection off the row without re-deriving the squad from the issue.
--
-- Distinct from `issue.squad_id` (migration 0035, the durable CARD assignment axis):
-- that says "this card is owned by a squad"; THIS says "this specific task row was
-- produced by a squad dispatch". A single-agent (non-squad) task reads back NULL.
--
-- No FK (hot queue table; `PRAGMA foreign_keys` is off in this crate — see 0002/0035);
-- referential integrity of the squad ref is a service-layer concern, exactly as
-- `issue.squad_id` and the task actor/repo refs are. ALTER ... ADD COLUMN with no
-- default is an O(1) catalog change in SQLite (no table rewrite), safe on populated
-- DBs (mirrors 0031/0032/0033/0035). Every pre-existing task reads back squad_id = NULL.

ALTER TABLE agent_task_queue
    ADD COLUMN squad_id TEXT;
