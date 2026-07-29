-- Migration 0069. A configuration mutation must invalidate a due heartbeat that was
-- read before the mutation. The scheduler claims a specific generation, then
-- only that generation may complete its reschedule.
ALTER TABLE atc_instance ADD COLUMN config_generation INTEGER NOT NULL DEFAULT 1;
ALTER TABLE atc_instance ADD COLUMN scheduler_claim_generation INTEGER;
