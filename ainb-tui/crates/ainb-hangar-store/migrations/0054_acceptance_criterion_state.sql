-- 0054: promote issue.acceptance_criteria from a flat JSON array of STRINGS
-- (migration 0048) to a JSON array of structured criterion OBJECTS
--   {"id": "ac-<16 hex>", "text": "...", "checked": false}
-- (multica parity gap #11-rest: `issue.acceptance_criteria` is a JSONB
-- structured list — a criterion must be individually addressable AND
-- individually completable).
--
-- No schema change: the column stays TEXT NOT NULL DEFAULT '[]'. This is a
-- one-time DATA normalisation so the on-disk shape is uniform. The Rust decoder
-- (`ainb_hangar_core::acceptance::criteria_from_json`) accepts BOTH shapes
-- regardless, so this migration is a tidy-up, not a correctness dependency — a
-- row it skips still reads correctly and normalises on its next write.
--
-- Guards: only rows that are valid JSON, non-empty, and whose FIRST element is
-- a text scalar (i.e. genuinely legacy 0048 rows) are rewritten. Rows already
-- holding objects, and the '[]' default, are left untouched — so re-running is
-- a no-op and a mixed database converges.
UPDATE issue
SET acceptance_criteria = (
      SELECT json_group_array(
               json_object(
                 'id',      'ac-' || lower(hex(randomblob(8))),
                 'text',    je.value,
                 'checked', json('false')
               )
             )
      FROM json_each(issue.acceptance_criteria) AS je
    )
WHERE json_valid(acceptance_criteria)
  AND json_array_length(acceptance_criteria) > 0
  AND json_type(acceptance_criteria, '$[0]') = 'text';
