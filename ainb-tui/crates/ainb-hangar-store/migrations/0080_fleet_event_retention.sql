-- Hangar v1 schema, migration 0080: fleet_event retention.
--
-- WHY. Measured on a real profile: fleet_event reached 1.1M rows / 847 MB, of
-- which 724 MB was hook payloads across 87k rows, under NO retention policy at
-- all. 0075 pruned one specific churn bug one time and said so; this is the
-- first ongoing policy for the table.
--
-- The corpus is RATE-driven, not age-driven — 713 MB of that 714 MB was under
-- seven days old (~102 MB/day) — so a seven-day cutoff would have held the
-- problem steady rather than solved it. The daemon therefore evicts hook
-- payloads at 48h and carries a byte ceiling as a co-equal control.
--
-- payload_evicted_at is the TOMBSTONE. `payload` stays `TEXT NOT NULL` and an
-- evicted row is rewritten to '{}', never to NULL: a NULL there would reach a
-- consumer as a driver error, and '{}' alone could not be told apart from an
-- event that genuinely carried no body. The stamp is what makes "we deleted
-- this" legible after the fact.
ALTER TABLE fleet_event ADD COLUMN payload_evicted_at INTEGER;

-- Drives the eviction sweep: seek to one event_type, range-scan observed_at,
-- and skip rows already evicted via the partial predicate so the sweep's cost
-- falls to nothing once it has caught up.
CREATE INDEX idx_fleet_event_retention_sweep
    ON fleet_event(event_type, observed_at) WHERE payload_evicted_at IS NULL;

-- Drives the row-delete guard. The 30-day DELETE must not remove the event
-- backing a session's CURRENT request, which is expressed as
-- `NOT EXISTS (SELECT 1 FROM fleet_session s WHERE s.current_request_fingerprint = ...)`.
-- Measured with EXPLAIN QUERY PLAN on a 1,500-session / 200k-event fixture.
-- Without this index SQLite does not scan per row; it falls back to
-- `SEARCH s USING AUTOMATIC COVERING INDEX`, rebuilding a transient index over
-- the whole roster on EVERY batch statement, inside the single SQLite writer.
-- With it the plan is `SEARCH s USING COVERING INDEX
-- idx_fleet_session_current_request`, and the roster build disappears. Partial,
-- because a roster is mostly idle and only non-NULL fingerprints can match.
CREATE INDEX idx_fleet_session_current_request
    ON fleet_session(current_request_fingerprint)
    WHERE current_request_fingerprint IS NOT NULL;
