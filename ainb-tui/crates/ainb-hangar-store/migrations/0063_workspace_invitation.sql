-- Hangar v1 schema, migration 0063: workspace membership INVITE lifecycle
-- (multica parity #18, reference 041_workspace_invitation.up.sql).
--
-- Until now the only way to add a human was MemberRepo::add -- the instant join
-- multica's invitation.go says CreateInvitation replaces. This table adds the
-- pending state between "someone was invited" and "someone is a member":
-- pending -> accepted | declined | expired, one live pending invite per
-- (workspace, email), 7 days to act.
--
-- Timestamps are epoch-ms INTEGER (house rule; SQLite has no temporal type), so
-- the reference's `now() + INTERVAL '7 days'` DEFAULT is computed in Rust
-- (repo::invitation::INVITE_TTL_MS) instead of in DDL.

CREATE TABLE workspace_invitation (
    id              TEXT PRIMARY KEY,
    workspace_id    TEXT NOT NULL REFERENCES workspace(id),
    inviter_id      TEXT NOT NULL REFERENCES user(id),
    invitee_email   TEXT NOT NULL,
    invitee_user_id TEXT REFERENCES user(id),
    role            TEXT NOT NULL CHECK (role IN ('admin','member')),
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending','accepted','declined','expired')),
    created_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    expires_at      INTEGER NOT NULL
);

-- One LIVE pending invite per (workspace, email). Partial, exactly like the
-- reference: an accepted/declined/expired row never blocks a re-invite. The
-- index cannot reference "now", so a past-due pending row is swept to `expired`
-- before every create (multica issue #2055) or it would block the re-invite.
CREATE UNIQUE INDEX idx_invitation_unique_pending
    ON workspace_invitation(workspace_id, invitee_email) WHERE status = 'pending';

CREATE INDEX idx_invitation_invitee_email
    ON workspace_invitation(invitee_email) WHERE status = 'pending';
CREATE INDEX idx_invitation_invitee_user
    ON workspace_invitation(invitee_user_id) WHERE status = 'pending';
CREATE INDEX idx_invitation_workspace_status
    ON workspace_invitation(workspace_id, status, created_at);
