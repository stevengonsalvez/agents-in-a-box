-- Hangar v1 schema, migration 0027: user-defined kanban boards (P4 / D8).
--
-- P4 turns the fixed four-column FSM board into USER-DEFINED boards: a workspace
-- owns any number of boards, each board owns any number of ordered columns, and
-- a column may (optionally) map to a task-FSM status so a card AUTO-MOVES there
-- when its work reaches that state. Three tables land:
--
--   - `board`         a workspace-scoped board: a `(workspace_id, name)` pair
--                     plus a per-board `auto_move` master toggle (the D8
--                     "per-board auto-move toggle"). The `(workspace_id, name)`
--                     UNIQUE index is the resolve-or-reject guard the create RPC
--                     relies on, mirroring `squad` (migration 0017) and `label`
--                     (migration 0016).
--
--   - `board_column`  a user-defined column with a stable surrogate `id`, an
--                     `ord` (left-to-right position; the repo keeps them
--                     contiguous `0..n`), a `name`, an optional `fsm_state`
--                     (a task-FSM status token — `queued`/`running`/`done`/… —
--                     that this column represents), and a per-column `auto_move`
--                     flag. A column with `fsm_state` set AND `auto_move` on is an
--                     AUTO-MOVE TARGET: a card whose task reaches that state jumps
--                     here. A NULL `fsm_state` is a purely manual column.
--
--   - `board_card`    an issue placed on a board in a specific column
--                     (`card = issue`, D8/§4.5). PK `(board_id, issue_id)` so an
--                     issue sits on a board at most once. `column_id` is NULLABLE:
--                     deleting a column parks its cards at `NULL` (the "unmapped
--                     pool", the edge-case contract — no data loss), rather than
--                     dropping the row.
--
-- # Why a surrogate `board_column.id` (not `(board_id, ord)` as the key)
--
-- Columns REORDER (a P4 verb): a reorder rewrites every column's `ord`. If cards
-- referenced a column by its `ord`, a reorder would silently drag cards between
-- columns. A stable surrogate `id` decouples placement (`board_card.column_id`)
-- from position (`board_column.ord`), so reordering is a pure `ord` rewrite that
-- never touches a card. `ord` carries only an ordinary (non-unique) index: the
-- repo is the sole writer and maintains contiguity, and a two-phase reorder would
-- be needed to satisfy a UNIQUE(board_id, ord) mid-swap — not worth the churn for
-- a single-writer table.
--
-- # Auto-move (the dispatch hook)
--
-- On every task FSM transition the daemon resolves the task's issue and, for each
-- board carrying that issue as a card, moves the card to the column whose
-- `fsm_state` matches the new status — but ONLY when the board's `auto_move`
-- master toggle AND the target column's `auto_move` flag are both on. This is the
-- D8 "column↔FSM-state mapping + per-board auto-move toggle" made real; the TUI
-- board reflects it on the next `hangar/boards_list` snapshot.
--
-- There is deliberately NO foreign key on `board_card.issue_id`: `PRAGMA
-- foreign_keys` is off in this crate (see 0002/0003), and the issue lives in the
-- `issue` table under the same workspace — referential integrity of the issue
-- half is a service-layer concern (`BoardRepo`), exactly as `issue`/`comment`
-- actor-refs are. The `board_id`/`column_id` FKs, the `(workspace_id, name)`
-- UNIQUE index, and the `(board_id, issue_id)` composite PK are the
-- engine-enforced invariants.
--
-- `CREATE TABLE` / `CREATE INDEX` are catalog-only changes (no table rewrite), so
-- this is safe + O(1) on a populated database: every pre-existing row is
-- untouched and a workspace simply owns no boards until one is created.

CREATE TABLE board (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    name         TEXT NOT NULL,
    auto_move    INTEGER NOT NULL DEFAULT 1,
    created_at   INTEGER NOT NULL
);

-- Resolve-or-reject guard: a board name maps to exactly one row per tenant, so
-- the create RPC can reject a duplicate name.
CREATE UNIQUE INDEX idx_board_workspace_name ON board(workspace_id, name);

CREATE TABLE board_column (
    id        TEXT PRIMARY KEY,
    board_id  TEXT NOT NULL REFERENCES board(id),
    ord       INTEGER NOT NULL,
    name      TEXT NOT NULL,
    fsm_state TEXT,
    auto_move INTEGER NOT NULL DEFAULT 0
);

-- Ordering index (non-unique — the repo maintains contiguity; a reorder is a
-- pure `ord` rewrite).
CREATE INDEX idx_board_column_board_ord ON board_column(board_id, ord);

CREATE TABLE board_card (
    board_id  TEXT NOT NULL REFERENCES board(id),
    issue_id  TEXT NOT NULL,
    column_id TEXT REFERENCES board_column(id),
    added_at  INTEGER NOT NULL,
    PRIMARY KEY (board_id, issue_id)
);

-- Scan a column's cards (the boards_list render buckets by column) and re-park a
-- deleted column's cards efficiently.
CREATE INDEX idx_board_card_column ON board_card(column_id);
