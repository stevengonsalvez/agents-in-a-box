-- Hangar v1 schema, migration 0017: squads, membership, and leader routing.
--
-- A `squad` is a workspace-scoped group of actors with a designated LEADER. It is
-- the first construct that lets work be assigned to a *team* rather than a single
-- actor (the e38.17 parity gap: `ActorKind` was only `{Member, Agent}` and no
-- squad table existed). Two tables land:
--
--   - `squad`         a workspace-scoped squad: a `(workspace_id, name)` pair
--                     plus the squad's LEADER as a polymorphic actor-ref
--                     (`leader_type` / `leader_id`). The `(workspace_id, name)`
--                     UNIQUE index is the resolve-or-reject guard the create RPC
--                     relies on (a squad name resolves to exactly one row per
--                     tenant), mirroring `label`'s `idx_label_workspace_name`
--                     (migration 0016) and `skill`'s `idx_skill_workspace_name`
--                     (migration 0008).
--
--   - `squad_member`  the membership join — a `(squad_id, member_type,
--                     member_id)` composite PK so an actor belongs to a squad at
--                     most once. Members are the same polymorphic actor-ref
--                     pattern (`member:<id>` / `agent:<id>`) the `issue` /
--                     `comment` tables already use (migration 0003).
--
-- # Leader routing (how a squad-assigned task reaches an actor)
--
-- A squad does NOT introduce a new `ActorKind`. Extending `ActorKind` with a
-- `Squad` variant would ripple into the `assignee_type` / `creator_type` /
-- `author_type` CHECK constraints on three tables and every `ActorKind` matcher
-- in the codebase, for no behavioural gain at v1. Instead a squad routes through
-- its LEADER actor-ref: the leader is a real actor (`agent:<id>` for an
-- automated leader), and the existing task queue is already `agent_id`-keyed and
-- claimed per-runtime. `SquadRepo::leader_agent_id(squad)` resolves a squad to
-- its leader's agent id, so a task enqueued for a squad carries the leader's
-- `agent_id` and the EXISTING claim/dispatch path (`ClaimTaskService`) routes it
-- to the leader agent with no claim-path change. This is the simplest behaviour
-- that actually takes effect — routing is real, not a stored no-op.
--
-- The `leader_type` CHECK keeps the leader discriminant honest (`member` /
-- `agent`), mirroring `issue.assignee_type`. There is deliberately NO foreign key
-- on the `_id` half of any actor-ref: `PRAGMA foreign_keys` is off in this crate
-- (see 0002/0003), the actor may live in either the `member` or `agent` table,
-- and SQLite cannot express a conditional FK. Referential integrity of the `_id`
-- half is a service-layer concern (`SquadRepo`), exactly as `issue` / `comment`
-- actor-refs are. The `(workspace_id, name)` UNIQUE index, the composite PK, the
-- two `_type` CHECKs, and the `squad_id` FK to `squad` are the engine-enforced
-- invariants.
--
-- `CREATE TABLE` / `CREATE INDEX` are catalog-only changes (no table rewrite),
-- so this is safe + O(1) on a populated database: every pre-existing row is
-- untouched and the workspace simply owns no squads until one is created.

CREATE TABLE squad (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    name         TEXT NOT NULL,
    leader_type  TEXT NOT NULL CHECK (leader_type IN ('member','agent')),
    leader_id    TEXT NOT NULL,
    created_at   INTEGER NOT NULL
);

-- Resolve-or-reject guard: a squad name maps to exactly one row per tenant, so
-- the create RPC can reject a duplicate name and a caller can resolve a
-- `(workspace, name)` pair to a single squad.
CREATE UNIQUE INDEX idx_squad_workspace_name ON squad(workspace_id, name);

CREATE TABLE squad_member (
    squad_id    TEXT NOT NULL REFERENCES squad(id),
    member_type TEXT NOT NULL CHECK (member_type IN ('member','agent')),
    member_id   TEXT NOT NULL,
    PRIMARY KEY (squad_id, member_type, member_id)
);
