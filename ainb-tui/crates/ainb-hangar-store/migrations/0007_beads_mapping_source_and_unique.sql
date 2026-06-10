-- Hangar v1 schema, migration 0007: re-shape `beads_mapping` for the P2 sync
-- adapter.
--
-- The P0 (0005) `beads_mapping` table was a placeholder: composite PK
-- `(hangar_id, bd_id)`, INTEGER epoch-ms `last_synced`, and no `source` column.
-- P2.1 needs three changes that the placeholder cannot express, so this is a
-- forward-only rebuild (the table carries no rows yet — P2 owns the only writer):
--
-- 1. `source` discriminant (`hangar` | `swarm`). Per
--    `feedback_swarm_leader_owns_bd_tracking`, swarm-sourced rows skip the
--    outbound mirror, so the discriminant must live on the row.
-- 2. Independent UNIQUE on EACH id side, not a composite PK. The sync adapter
--    looks a row up by hangar_id OR by bd_id, and each correlation is 1:1 in
--    both directions, so a single pair-uniqueness PK is too weak — it would let
--    one hangar_id map to two bd_ids (and vice versa).
-- 3. `last_synced` as ISO-8601 TEXT (UTC, second precision) rather than INTEGER
--    ms. P2 compares it against `DateTime<Utc>` last-writer-wins timestamps and
--    round-trips at second precision; TEXT keeps the column human-legible in
--    `bd`/sqlite debugging.
--
-- `hangar_kind` / `bd_kind` are CHECK-constrained to the issue/task set so the
-- discriminants stay honest. The primary key is `hangar_id` (the local entity is
-- the anchor); the bd side gets its own UNIQUE index.

DROP TABLE IF EXISTS beads_mapping;

CREATE TABLE beads_mapping (
    hangar_id   TEXT NOT NULL PRIMARY KEY,
    bd_id       TEXT NOT NULL UNIQUE,
    hangar_kind TEXT NOT NULL CHECK (hangar_kind IN ('issue','task')),
    bd_kind     TEXT NOT NULL CHECK (bd_kind IN ('issue','task')),
    source      TEXT NOT NULL CHECK (source IN ('hangar','swarm')),
    last_synced TEXT NOT NULL
);

-- Reconcile delta queries scan by `last_synced >= t`; index it.
CREATE INDEX idx_beads_mapping_last_synced ON beads_mapping (last_synced);
