-- Expiring, token-fenced ATC scheduler claims let a new scheduler recover a
-- heartbeat abandoned by a crashed process without allowing two live schedulers
-- to complete the same tick.
ALTER TABLE atc_instance ADD COLUMN scheduler_claim_token TEXT;
ALTER TABLE atc_instance ADD COLUMN scheduler_claimed_at INTEGER;
