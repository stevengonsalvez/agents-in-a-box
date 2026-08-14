-- Fresh remote Codex clients create their own thread.  The reservation
-- snapshots the source-ledger cursor before tmux starts, so a later claim
-- cannot adopt an older same-directory thread.
ALTER TABLE interactive_codex_thread ADD COLUMN event_watermark INTEGER;
ALTER TABLE interactive_codex_thread ADD COLUMN resumable INTEGER NOT NULL DEFAULT 0;

-- Codex does not attach an Ainb correlation token to `thread/started`.
-- One pending launch per exact working directory makes cursor-plus-cwd
-- matching unambiguous without serializing unrelated projects.
CREATE UNIQUE INDEX interactive_codex_thread_one_pending_cwd
    ON interactive_codex_thread(cwd)
    WHERE thread_id IS NULL;
