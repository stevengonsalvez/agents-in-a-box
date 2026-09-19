-- Hangar v1 schema, migration 0102: the sessions.json import marker (P6d).
--
-- The boot import of ~/.agents-in-a-box/sessions.json (0101) runs once per
-- source file. Before this table the import decided "already done" per record,
-- by looking for a row with the same id or tmux name, so a session deleted from
-- the table while the file still named it came back on the next boot.
--
-- One row per source path, written in the same transaction as the imported
-- rows. Its presence means the import finished; a boot that finds it imports
-- nothing, so a row deleted after the import stays deleted. Its absence means
-- the import has not completed (the file was unreadable, unparseable, or over
-- the size cap), and clients read that as "the table is not yet authoritative".
--
-- `imported` counts rows written, `skipped` rows whose id or tmux name was
-- already present, `rejected` records that failed validation (for example a
-- session id that is not a UUID). Rejected records stay in the file untouched.

CREATE TABLE session_import (
    source_path   TEXT PRIMARY KEY NOT NULL,
    completed_at  INTEGER NOT NULL,
    imported      INTEGER NOT NULL CHECK (imported >= 0),
    skipped       INTEGER NOT NULL CHECK (skipped >= 0),
    rejected      INTEGER NOT NULL CHECK (rejected >= 0)
);
