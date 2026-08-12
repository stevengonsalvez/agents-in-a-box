-- A crashed client cannot hold the global fresh-launch reservation forever.
ALTER TABLE interactive_codex_thread
    ADD COLUMN reserved_at INTEGER NOT NULL DEFAULT 0;
