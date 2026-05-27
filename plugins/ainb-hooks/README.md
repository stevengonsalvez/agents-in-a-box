# ainb-hooks

Plugin that emits Claude Code and Codex CLI lifecycle events to the
**ainb notification inbox** via a Unix socket. Powers session-state
badges, the dedicated Inbox screen, and optional OS notifications in
`ainb-tui`.

## How it works

```
┌─ Claude / Codex session ─────┐
│ hook fires (Stop, Notification│
│   :idle_prompt, ...)         │
│ ────────────────────────────▶│ notify.sh
└──────────────────────────────┘    │
                                    ▼
                       ~/.agents-in-a-box/notify.sock
                                    │
                                    ▼
                              ainb-notifyd
                                    │
                       ┌────────────┴───────────────┐
                       ▼                              ▼
            notifications.db (SQLite)       broadcast → ainb-tui
```

If the socket is unreachable (daemon not running), `notify.sh` writes
the envelope to `~/.agents-in-a-box/notify.fallback.jsonl`. Notifyd
replays + clears that file on its next startup. The hook always exits 0
so a delivery failure never blocks the host agent.

## Layout

```
plugins/ainb-hooks/
├── .claude-plugin/
│   └── plugin.json          # Claude Code marketplace manifest
├── codex/
│   └── hooks.json           # Codex ~/.codex/hooks.json merge template
├── hooks/
│   └── notify.sh            # universal hook script (claude + codex)
└── README.md
```

The same `notify.sh` is used by both agents:

- **Claude Code** pipes the hook payload as JSON on stdin.
- **Codex CLI** passes the hook payload as JSON in `argv[1]`.

`notify.sh` autodetects the source and uses the right input. The agent
is identified via `AINB_AGENT={claude,codex}` set in the registering
command line.

## Install

The recommended install path is the `ainb hooks install` CLI (in `ainb-tui`),
which handles plugin manifests, config merges, and notifyd lifecycle:

```bash
ainb hooks install --claude --codex
ainb hooks status
ainb hooks uninstall --all
```

The CLI:

1. Drops `.claude-plugin/plugin.json` at `~/.claude/plugins/ainb-hooks/` (Claude).
2. Merges this directory's `codex/hooks.json` into `~/.codex/hooks.json` as a
   managed block (Codex).
3. Extracts `notify.sh` to `~/.agents-in-a-box/hooks/notify.sh` and rewrites
   the `__AINB_HOOK_SCRIPT__` placeholder in the codex template to that
   absolute path.
4. Records the install method in `~/.agents-in-a-box/install.json` so
   `ainb hooks uninstall` is fully reversible.

## Hook events

Both agents use identical PascalCase event names: `SessionStart`,
`UserPromptSubmit`, `PostToolUse`, `Notification`, `Stop`, `PreCompact`.

The matcher `Notification:idle_prompt` (Claude) and Codex's variants
(`request_user_input`, `wait_for_user`, etc.) all carry the same
semantic meaning ("agent awaiting user input"). `notify.sh` preserves
the raw event name in the `raw_event` field of the envelope so UI
mapping happens in the consumer, not at the wire.

## Envelope shape

```json
{
  "protocol_version": 1,
  "agent": "claude",
  "raw_event": "Notification:idle_prompt",
  "session_id": "7f2a...",
  "cwd": "/Users/stevie/d/git/ai-coder-rules",
  "project": "ai-coder-rules",
  "ts": 1717000000000,
  "payload": { "...full original hook JSON..." }
}
```

`payload` is the verbatim original JSON from the host agent so the
inbox detail view can show full forensics.

## See also

- `crates/ainb-plugin-notifyd/` — the `ainb-notifyd` daemon
- `crates/ainb-core/src/cli/hooks.rs` — the `ainb hooks` CLI
- `crates/ainb-core/src/screens/inbox/` — the Inbox TUI screen
- `.agents/specs/2026-05-27-ainb-hooks-plugin-stub-spec.md` — design spec
