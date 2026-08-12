-- Cursor prevents a pending launch claiming a thread that predated it.
ALTER TABLE interactive_codex_thread
    ADD COLUMN event_watermark INTEGER;
