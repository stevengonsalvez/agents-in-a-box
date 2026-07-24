-- 0048: structured acceptance criteria + context references on an issue
-- (multica parity, gap #11 / init.up.sql:66-67 acceptance_criteria + context_refs).
--
-- Two JSON-array TEXT columns, identical persistence to `labels` (migration 0014):
-- each holds an ORDERED list of strings — `acceptance_criteria` a list of criterion
-- lines, `context_refs` a list of linked references (URLs / `owner/repo#123` / free
-- text). Default `'[]'` keeps every existing row unchanged. ADD COLUMN with a
-- constant default is an O(1) catalog change in SQLite (no table rewrite).
ALTER TABLE issue ADD COLUMN acceptance_criteria TEXT NOT NULL DEFAULT '[]';
ALTER TABLE issue ADD COLUMN context_refs        TEXT NOT NULL DEFAULT '[]';
