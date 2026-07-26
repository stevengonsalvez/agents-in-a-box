-- Hangar v1 schema, migration 0053: per-member ROLE on a squad membership and
-- per-squad user-authored INSTRUCTIONS.
--
-- Multica parity (gap #25): migration 084 (`squad_member.role TEXT DEFAULT ''`)
-- and 088 (`squad.instructions TEXT DEFAULT ''`). The third leg of #25 —
-- squad archive (multica 085) — already landed in migration 0052.
--
-- Both columns exist to be READ BY THE SQUAD LEADER'S CLAIM-TIME BRIEFING
-- (`ainb-hangar-daemon/src/squad_briefing.rs`), which today renders a roster with
-- no roles and omits the `## Squad Instructions` section because these columns
-- did not exist. This migration is what turns that documented divergence off —
-- neither column is a stored no-op.
--
-- `role` is FREE TEXT with no CHECK (multica parity, D2): it is a human label
-- like "owns the migrations" that the leader matches a task against, not a
-- vocabulary. NOT NULL DEFAULT '' so "no role" is the empty string rather than a
-- NULL every reader must unwrap; '' is also the "omit this fragment" sentinel the
-- roster renders against, mirroring multica's blank-omit for instructions.
--
-- DEVIATION (D1): the LEADER gets no `squad_member` row. Multica auto-adds the
-- leader as a member with role='leader'; hangar keeps the leader in
-- `squad.leader_type`/`leader_id` and renders a synthetic self-row, so `role`
-- describes MEMBERS only.
--
-- DEVIATION (D3): `role` does NOT gate dispatch. `SquadRepo::member_agent_ids`
-- (the fan-out query) stays role-blind — every agent member is still dispatched
-- to. Role is advisory metadata the leader reads, matching multica, where nothing
-- filters on it either. Selective routing is parity item #16.
--
-- Both statements are `ALTER TABLE ... ADD COLUMN` with a CONSTANT literal
-- default → O(1) catalog changes in SQLite (no table rewrite), safe on a
-- populated DB, the same shape 0047/0050/0051/0052 used. Every pre-existing
-- membership reads `role = ''` and every pre-existing squad reads
-- `instructions = ''`, i.e. byte-identical briefing output to today.

ALTER TABLE squad_member ADD COLUMN role TEXT NOT NULL DEFAULT '';

ALTER TABLE squad ADD COLUMN instructions TEXT NOT NULL DEFAULT '';
