-- Hangar v1 schema, migration 0028: ATC-on-the-daemon + auto-standup (spec P9,
-- architecture §4.7 / §4.8, decisions D12 + D13).
--
-- P9 pulls two mechanisms off the side (launchd/systemd timers, JSON side-files)
-- and onto the daemon store, so the daemon is the single owner of ATC + standup
-- state and every surface reads one plane. Four tables land, all catalog-only
-- (`CREATE TABLE` with no backfill) — O(1) and safe on a populated database, and
-- every pre-existing workspace simply starts empty.
--
-- ## `atc_instance` — the ATC registry (D12)
--
-- `ainb fleet atc setup` used to write `~/.agents-in-a-box/atc/<name>/meta.json` and install a
-- launchd plist / systemd timer to fire the heartbeat. P9 registers the instance
-- HERE and turns the heartbeat into a daemon cron job (reusing the autopilot
-- scheduler's DB-durable tick loop): `next_tick_at` is the cached next-firing
-- instant the heartbeat cron reads, exactly like `autopilot.next_tick_at`
-- (migration 0009). The old `meta.json` / `task-log.md` stay as human-readable
-- audit; the AUTHORITATIVE state is these rows.
--
--   - `name`             the sanitized instance name — the PK (one instance per
--                        name, like the launchd label was unique).
--   - `cwd`              the directory the ATC session drives from (empty when
--                        unset). Carried so the heartbeat cron can build the
--                        `fleet needs` read for the right scope.
--   - `tmux_session`     the ATC session's tmux target, or NULL when not spawned.
--                        The heartbeat cron sends the `[HEARTBEAT]` nudge here via
--                        the one verified send path.
--   - `heartbeat_cron`   the cron expression the daemon fires the heartbeat on
--                        (UTC), validated by the same `autopilot::cron` parser.
--   - `err_retry_cap`    the per-session auto-`continue` cap (D12) — moved off the
--                        advisory JSON into a real column (default 3).
--   - `idle_pause_min`   minutes of a quiet fleet before the heartbeat downgrades
--                        to a cheap idle ping (default 60).
--   - `next_tick_at`     cached next-firing instant (epoch-ms); NULL = not
--                        scheduled (disabled, or a cron with no future match).
--   - `enabled`          whether the heartbeat cron considers this instance
--                        (INTEGER 0/1, the standard idiom).
--   - `last_heartbeat_at` epoch-ms of the last fired heartbeat, or NULL. Powers
--                        the `atc status` age readout without a side-file stat.
--   - `created_at`       epoch-ms the instance was registered.
--
-- ## `atc_retry` — per-session retry / escalation ledger (D12)
--
-- The heartbeat's continue-retry cap + escalated flag + free-text note used to
-- live in `state.json` / `task-log.md`, which the ATC session and the heartbeat
-- process BOTH read-modify-wrote (a lost-update race). P9 makes the store the
-- single writer: one row per (instance, monitored session). `task-log.md` stays
-- as the append-only human audit; THIS is the machine truth the cap enforces.
--
--   - `continue_count`   code-owned count of auto-`continue`s presented for the
--                        session; once it reaches `atc_instance.err_retry_cap`
--                        the heartbeat presents the ERR as escalate-only.
--   - `escalated`        1 once the session has been escalated to a human
--                        (an attention row, kind=escalation, was raised).
--   - `note`             the ATC session's free-text note for the session, or
--                        NULL.
--
-- ## `standup_session` — auto-standup per-session state (D13, Stevie override)
--
-- Auto-`/standup` WRITES into a stagnant, idle-at-prompt session. The guardrails
-- that MUST all hold are enforced against these rows: a per-session opt-out, a
-- per-session cooldown anchored on `last_standup_at`, and an `in_flight` marker
-- (with the daemon-global `max concurrent = 1` cap counted across rows). One row
-- per monitored session; a session with no row has never been standup-touched
-- (default: not opted out, no cooldown, not in flight).
--
--   - `session_id`       the monitored session — the PK.
--   - `opted_out`        1 when the human excluded this session from auto-standup
--                        (a hard guardrail — an opted-out session NEVER fires).
--   - `last_standup_at`  epoch-ms the last auto-standup fired, or NULL — the
--                        cooldown anchor (default 60-min window).
--   - `in_flight`        1 while a fired standup's turn is still running; blocks a
--                        second fire and is counted toward the max-concurrent cap.
--   - `updated_at`       epoch-ms of the last state change.
--
-- ## `daemon_config` — global daemon key/value config
--
-- Auto-standup's GLOBAL toggle + knobs (default ON per Stevie) are host-wide, not
-- per-workspace, so `workspace_config` (migration 0020) is the wrong home. A tiny
-- string kv table holds them (`autostandup.enabled` / `autostandup.stagnant_min`
-- / `autostandup.cooldown_min` / `autostandup.max_concurrent`). A missing key
-- reads back the coded default, so an upgrading daemon behaves identically until
-- a value is written.

CREATE TABLE atc_instance (
    name               TEXT PRIMARY KEY,
    cwd                TEXT NOT NULL DEFAULT '',
    tmux_session       TEXT,
    heartbeat_cron     TEXT NOT NULL,
    err_retry_cap      INTEGER NOT NULL DEFAULT 3,
    idle_pause_min     INTEGER NOT NULL DEFAULT 60,
    next_tick_at       INTEGER,
    enabled            INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    last_heartbeat_at  INTEGER,
    created_at         INTEGER NOT NULL
);

-- The heartbeat cron's hot path: `ORDER BY next_tick_at ASC LIMIT 1` over the
-- enabled instances. Partial (WHERE enabled = 1) so a disabled instance never
-- bloats the index the tick loop scans (mirrors `idx_autopilot_next_tick`).
CREATE INDEX idx_atc_instance_next_tick
    ON atc_instance(next_tick_at)
    WHERE enabled = 1;

CREATE TABLE atc_retry (
    instance_name   TEXT NOT NULL REFERENCES atc_instance(name),
    session_id      TEXT NOT NULL,
    continue_count  INTEGER NOT NULL DEFAULT 0,
    escalated       INTEGER NOT NULL DEFAULT 0 CHECK (escalated IN (0, 1)),
    note            TEXT,
    updated_at      INTEGER NOT NULL,
    PRIMARY KEY (instance_name, session_id)
);

CREATE TABLE standup_session (
    session_id       TEXT PRIMARY KEY,
    opted_out        INTEGER NOT NULL DEFAULT 0 CHECK (opted_out IN (0, 1)),
    last_standup_at  INTEGER,
    in_flight        INTEGER NOT NULL DEFAULT 0 CHECK (in_flight IN (0, 1)),
    updated_at       INTEGER NOT NULL
);

-- The auto-standup watcher counts concurrent in-flight standups across all rows
-- (the max-concurrent guardrail). A partial index over just the in-flight set
-- keeps that COUNT tight regardless of how much standup history accumulates.
CREATE INDEX idx_standup_in_flight
    ON standup_session(session_id)
    WHERE in_flight = 1;

CREATE TABLE daemon_config (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
