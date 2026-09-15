-- Hangar v1 schema, migration 0100: the daemon's own identity (spec D11, #1066).
--
-- Every `host_id` column before this one (0097 `mutation_ledger`, 0099
-- `fleet_session` and `fleet_event`) was a placeholder defaulted to 'local',
-- because no daemon had an identity to write. This is that identity: a ULID
-- minted once, at the first boot that applies this migration, and never
-- derived from the hostname. A backup restore keeps it; a fresh install mints a
-- new one.
--
-- # One row, by construction
--
-- `singleton` is the primary key and can only be 1, so a second mint cannot
-- insert a second identity: `INSERT OR IGNORE` against it is the whole
-- "mint once" guarantee, and it holds across two daemons racing on one home.
--
-- # Why the id is not minted here
--
-- SQLite has no ULID function, and a `randomblob` stand-in would be a second id
-- format for one fact. The daemon mints the ULID at boot
-- (`DaemonIdentityRepo::mint_or_read`) and, in the same transaction, adopts
-- every `fleet_session` row still named 'local'. `fleet_event` can be measured
-- in gigabytes (see 0099), so its 'local' rows are adopted after boot in
-- bounded batches rather than in one statement with the daemon down.
--
-- # The columns R1 fills
--
-- `host_static_pubkey` and `display_name` are NULL until R1 pairs hosts: the
-- Noise IK static key is generated with the pairing, not with the id.

CREATE TABLE daemon_identity (
    singleton          INTEGER PRIMARY KEY CHECK (singleton = 1),
    -- The ULID this daemon names itself on every row and in `auth/hello`:
    -- 26 characters, every one from the Crockford base32 alphabet. The
    -- NOT GLOB form rejects a bad character anywhere; a plain
    -- `GLOB '[0-9A-HJKMNP-TV-Z]*'` would check only the first.
    host_id            TEXT NOT NULL CHECK (
        length(host_id) = 26 AND host_id NOT GLOB '*[^0-9A-HJKMNP-TV-Z]*'
    ),
    -- The Noise IK static public key. NULL until R1 generates one.
    host_static_pubkey BLOB,
    -- The operator-facing host label. NULL until R1 lets an operator set one.
    display_name       TEXT,
    -- Unix milliseconds of the mint.
    created_at         INTEGER NOT NULL
);
