-- Hangar v1 schema, migration 0018: webhook-triggered autopilots (e38.18).
--
-- An autopilot was cron+manual only (migrations 0009/0010): the scheduler fires
-- it on a `cron_expr` tick, and `autopilot_fire_now` fires it by hand. This
-- migration adds a THIRD trigger surface — an authenticated local HTTP webhook —
-- so an external system can `POST /hangar/webhook/<autopilot_id>` and fire the
-- autopilot's existing P7.4 enqueue path on demand. The the reference `webhook|api`
-- trigger family the 0009 comment deferred ("webhook / api triggers are out of
-- scope") lands here.
--
-- # Three new `autopilot` columns (ALTER, not a rewrite)
--
--   - `webhook_enabled`        INTEGER 0/1 — whether the ingress accepts a POST
--                              for this autopilot at all. A row with no secret
--                              set, or with `webhook_enabled = 0`, rejects every
--                              webhook request (so a half-configured autopilot is
--                              never firable over HTTP). Defaults to 0: every
--                              pre-existing autopilot stays webhook-OFF until an
--                              operator explicitly enables + sets a secret.
--
--   - `webhook_secret_sha256`  TEXT — the SHA-256 hex DIGEST of the per-autopilot
--                              HMAC signing secret, NOT the secret itself. This
--                              mirrors the e38.1 `daemon_socket_token` discipline
--                              (digest in the database, plaintext only in a 0600
--                              file): an attacker who reads `hangar.db` learns the
--                              digest, which cannot forge an HMAC signature. The
--                              recoverable plaintext (needed to recompute the
--                              body HMAC at verify time) lives ONLY in the 0600
--                              `{hangar_home}/hangar/webhook_secrets.json` file
--                              the daemon writes at mint/rotate time. `NULL` means
--                              no secret is set (webhook un-firable regardless of
--                              the enabled flag). The digest column is the
--                              "is a secret set / has it rotated" inspection knob
--                              + the value the rotate path compares against.
--
--   - `webhook_event_filter`   TEXT — an optional event-name filter. When set,
--                              a webhook request must carry a matching `event`
--                              (the JSON body's `event` field, or the
--                              `X-Hangar-Event` header) or the request is
--                              accepted-but-ignored (200, fires nothing). `NULL`
--                              (the default) fires on every signed request. This
--                              is the v1 collapse of the reference's richer filter
--                              expression — an exact event-name match, not a DSL.
--
-- `ALTER TABLE ... ADD COLUMN` with a constant/NULL default is a catalog-only
-- change on SQLite (no table rewrite): every pre-existing `autopilot` row is
-- untouched and simply reads the column default (webhook OFF, no secret, no
-- filter) until an operator configures it.

ALTER TABLE autopilot ADD COLUMN webhook_enabled INTEGER NOT NULL DEFAULT 0
    CHECK (webhook_enabled IN (0, 1));
ALTER TABLE autopilot ADD COLUMN webhook_secret_sha256 TEXT;
ALTER TABLE autopilot ADD COLUMN webhook_event_filter TEXT;

-- # The delivery audit log
--
-- Every webhook request the ingress processes for an autopilot records one
-- `autopilot_webhook_delivery` row — accepted-and-fired, accepted-but-filtered,
-- or rejected (bad/absent signature, disabled, unknown event). This is the
-- inspection surface the `ainb hangar autopilot deliveries <id>` CLU verb reads
-- (the e38.18 delivery-inspection sub-gap). It carries NO secret and NO request
-- body — only the outcome, the matched/seen event name, the HTTP status the
-- ingress returned, and the run it fired (when it fired one). A delivery that
-- fired links to its `autopilot_run` via `run_id`; a rejected/filtered delivery
-- has `run_id IS NULL`.
--
-- The `outcome` CHECK keeps the discriminant honest. There is deliberately NO
-- foreign key on `run_id` (PRAGMA foreign_keys is off in this crate, mirroring
-- every other actor-ref / link column); the `autopilot_id` FK to `autopilot` is
-- the engine-enforced parent link.

CREATE TABLE autopilot_webhook_delivery (
    id            TEXT PRIMARY KEY,
    autopilot_id  TEXT NOT NULL REFERENCES autopilot(id),
    received_at   INTEGER NOT NULL,
    outcome       TEXT NOT NULL
                  CHECK (outcome IN ('fired', 'filtered', 'rejected')),
    event         TEXT,
    http_status   INTEGER NOT NULL,
    run_id        TEXT,
    detail        TEXT
);

-- Delivery-history reads (`list_deliveries`) filter by `autopilot_id` and order
-- by `received_at DESC` (latest-first). A composite index serves both, mirroring
-- `idx_autopilot_run_autopilot_started` (migration 0009).
CREATE INDEX idx_autopilot_webhook_delivery_autopilot_received
    ON autopilot_webhook_delivery(autopilot_id, received_at DESC);
