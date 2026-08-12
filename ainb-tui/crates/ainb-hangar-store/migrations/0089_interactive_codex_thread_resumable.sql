-- A thread becomes resumable only after its first turn persisted a rollout.
ALTER TABLE interactive_codex_thread
    ADD COLUMN resumable INTEGER NOT NULL DEFAULT 0;
