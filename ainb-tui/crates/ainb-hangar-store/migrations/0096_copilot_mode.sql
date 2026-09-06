-- The copilot's guardrail dial, per channel.
--
-- `help` reads and never writes, `guarded` parks every write on a confirm card,
-- `yolo` fires the confirm-class tools immediately. This is the DAEMON-SIDE
-- copilot guardrail and nothing else: the ACP adapter's own `permission_mode`
-- stays pinned at `session/new` and re-asserted after `session/load`, because
-- an ambient `bypassPermissions` disables the whole permission surface and a
-- settable one would be a remote off-switch for it.
--
-- Defaults to `guarded`, which is also what an existing channel migrates to:
-- the safe direction, and the mode the dial resets to at every daemon start.
ALTER TABLE fleet_channel
    ADD COLUMN copilot_mode TEXT NOT NULL DEFAULT 'guarded'
    CHECK (copilot_mode IN ('help', 'guarded', 'yolo'));
