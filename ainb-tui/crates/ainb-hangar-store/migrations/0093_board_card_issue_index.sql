-- Every terminal task transition asks "which card does this issue sit on?"
-- (`STAGES_REMAIN_SQL`, `ADVANCE_SQL`, the pull's position read-back) by
-- `board_card.issue_id`, but the table's only indexes were the `(board_id,
-- issue_id)` primary key and `idx_board_card_column`, so each lookup scanned the
-- whole table. Index the issue column directly.
CREATE INDEX IF NOT EXISTS idx_board_card_issue ON board_card(issue_id);
