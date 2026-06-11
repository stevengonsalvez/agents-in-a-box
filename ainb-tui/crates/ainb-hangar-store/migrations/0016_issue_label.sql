-- Hangar v1 schema, migration 0016: a first-class issue-label entity.
--
-- Migration 0014 added a denormalized `issue.labels` JSON column — the minimal
-- persistence the create flow needed to carry the labels an issue was created
-- with. This migration promotes labels to a real workspace-scoped entity with an
-- attach/detach join, the source of truth the e38.10 RPCs mutate:
--
--   - `label`        a workspace-scoped label definition: a `(workspace_id,
--                    name)` pair plus an optional hex `color`. The
--                    `(workspace_id, name)` UNIQUE index is the resolve-or-reject
--                    guard the attach RPC relies on (a label name resolves to
--                    exactly one row per tenant), mirroring `skill`'s
--                    `idx_skill_workspace_name` (migration 0008).
--
--   - `issue_label`  the attach join — a `(issue_id, label_id)` composite PK so
--                    a label attaches to an issue at most once, mirroring
--                    `agent_skill` (migration 0002). The PK doubles as the
--                    idempotency guard the attach path leans on
--                    (`INSERT ... ON CONFLICT DO NOTHING`).
--
-- # Reconciliation with the `issue.labels` JSON column (migration 0014)
--
-- The `label` table is the SOURCE OF TRUTH. The `issue.labels` JSON column is
-- KEPT as a denormalized read-cache: the `hangar/issues_list` snapshot already
-- reads `issue.labels` into `IssueRow.labels`, which the plugin chips render
-- straight from. Re-deriving every issue's labels by JOINing this new table on
-- each list pull would rewrite that read path for no behavioural gain; instead
-- the attach/detach RPCs (the only writers of the join) keep the JSON cache in
-- sync inside the same transaction, so `issues_list` stays correct without a
-- join. Dropping the column would force `issues_list` to grow a per-issue join;
-- keeping it as a cache leaves the hot read path byte-identical.
--
-- There is deliberately NO foreign key from `issue_label.issue_id` /
-- `issue_label.label_id` to their parents: `PRAGMA foreign_keys` is off in this
-- crate (see 0002/0003), so referential integrity of the join — and the cascade
-- on issue/label deletion — is a service-layer concern enforced by `LabelRepo`,
-- exactly as `agent_skill` and `skill_file` are. The `(workspace_id, name)`
-- UNIQUE index plus the composite PK are the only engine-enforced invariants.
--
-- `CREATE TABLE` / `CREATE INDEX` are catalog-only changes (no table rewrite),
-- so this is safe + O(1) on a populated database. Every pre-existing issue keeps
-- its `issue.labels` JSON snapshot untouched and simply owns no `issue_label`
-- rows until a label is attached.

CREATE TABLE label (
    id           TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES workspace(id),
    name         TEXT NOT NULL,
    color        TEXT
);

-- Resolve-or-reject guard: a label name maps to exactly one row per tenant, so
-- the attach RPC can resolve a `(workspace, name)` pair to a single label (and
-- the upsert-on-attach path can `ON CONFLICT (workspace_id, name)`).
CREATE UNIQUE INDEX idx_label_workspace_name ON label(workspace_id, name);

CREATE TABLE issue_label (
    issue_id TEXT NOT NULL REFERENCES issue(id),
    label_id TEXT NOT NULL REFERENCES label(id),
    PRIMARY KEY (issue_id, label_id)
);

-- Reverse lookup: "which issues carry this label" (used when a label is renamed
-- or removed). The forward direction is served by the composite PK's leading
-- `issue_id`.
CREATE INDEX idx_issue_label_label ON issue_label(label_id);
