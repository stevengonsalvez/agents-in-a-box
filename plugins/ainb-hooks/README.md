# ainb-hooks

A **Claude Code / Codex CLI plugin** — installed into the host agent CLIs, not into `ainb` itself. Captures actionable session lifecycle events and forwards them to the `ainb-notifyd` daemon, powering the Inbox screen, per-session attention markers, and OS notification banners in `ainb-tui`.

## What it does

```
┌─ Claude / Codex session ─────────────────┐
│  hook fires: Stop / Notification /       │
│  PermissionRequest / agent-turn-complete │
└──────────────────────────────────────────┘
                    │
                    ▼
              notify.sh (this plugin)
                    │
          build normalized Envelope JSON
                    │
              ┌─────▼──────┐
              │ notify.sock │  ← Unix domain socket
              └─────┬──────┘
    socket absent?  │
         ┌──────────┘
         ▼
    lazy-spawn: nohup ainb notifyd
    retry send once
         │  still fails?
         ▼
    notify.fallback.jsonl  (replayed on next daemon start)
                    │
                    ▼
              ainb-notifyd
              ┌─────────────────────┐
              │  notifications.db   │  SQLite WAL
              │  OS banner          │  osascript / notify-send
              │  Inbox TUI screen   │  press b
              └─────────────────────┘
```

> The session list's live `[?]` "waiting" marker is **not** produced by
> this plugin or the daemon — the ainb TUI derives it directly from each
> session's tmux pane. See [Inbox & notifications](../../docs/tui/inbox-notifications.md).

The hook always exits `0`. A delivery failure (socket absent, daemon not started) never blocks the host agent.

## Layout

```
plugins/ainb-hooks/
├── .claude-plugin/
│   └── plugin.json          # Claude Code plugin manifest (name, version, hooks block)
├── codex/
│   └── hooks.json           # Codex ~/.codex/hooks.json merge template
├── hooks/
│   └── notify.sh            # Universal hook script (Claude + Codex)
└── README.md
```

All three artefacts are baked into the `ainb-notifyd` binary via `include_str!` at compile time. Install extracts them to disk; no network access is required.

## Hook registration

Only events that require human attention are registered. Telemetry is explicitly excluded.

**Registered (Claude Code — `plugin.json`):**

| Event | Fires when |
|---|---|
| `Notification` | Agent is awaiting user input (`Notification:idle_prompt`) or surfacing a permission prompt |
| `Stop` | Agent turn / session finishes |

**Registered (Codex CLI — `hooks.json`):**

Codex uses the same two event names: `Notification` and `Stop`.

**Not registered (intentionally):** `SessionStart`, `UserPromptSubmit`, `PostToolUse`, `PreCompact`. `PostToolUse` alone fires dozens of times per agent turn and would bury the inbox signal.

As a second line of defence, `ainb-notifyd` also drops any non-user-facing event on arrival (`osnotify.rs:is_user_facing`), so a stale install never accumulates noise.

### Claude vs Codex wiring

The same `notify.sh` script handles both agents. The difference is how the payload arrives and how the agent is identified.

| | Claude Code | Codex CLI |
|---|---|---|
| Payload delivery | JSON on **stdin** | JSON as **argv[1]** |
| Agent identification | `AINB_AGENT=claude` set in the hook command | `AINB_AGENT=codex` set in the hook command |
| Hook config | `~/.claude/plugins/ainb-hooks/.claude-plugin/plugin.json` | Managed block in `~/.codex/hooks.json` |
| Hook command | `AINB_AGENT=claude ${CLAUDE_PLUGIN_ROOT}/hooks/notify.sh` | `AINB_AGENT=codex /abs/path/to/notify.sh` |

`notify.sh` autodetects the input source when `AINB_AGENT` is not set — argv-delivery implies Codex, stdin-delivery implies Claude. The explicit env var is preferred and always set by the installer.

## Envelope schema

Every event is normalized into a single JSON envelope before delivery to the socket.

```json
{
  "protocol_version": 1,
  "agent": "claude",
  "raw_event": "Notification:idle_prompt",
  "session_id": "7f2a3b...",
  "cwd": "/Users/stevie/d/git/ai-coder-rules",
  "project": "ai-coder-rules",
  "ts": 1717000000000,
  "payload": { "...verbatim original hook JSON..." }
}
```

| Field | Type | Notes |
|---|---|---|
| `protocol_version` | `u8` | Currently `1`. Bumped only on breaking wire changes. |
| `agent` | `string` | `"claude"` or `"codex"` |
| `raw_event` | `string` | Verbatim event name, preserving matcher suffix (e.g. `Notification:idle_prompt`). Not canonicalized across agents — mapping happens in the consumer. |
| `session_id` | `string` | Host agent's session identifier. May be empty in rare edge cases. |
| `cwd` | `string` | Working directory at the moment the hook fired. Used by the TUI to correlate notifications with ainb sessions. |
| `project` | `string` | `basename(cwd)` — pre-computed for display. |
| `ts` | `i64` | Epoch milliseconds. Falls back to `date +%s * 1000` when `%3N` is unavailable (macOS BSD date). |
| `payload` | `object` | Verbatim original JSON from the host agent. Preserved for full forensics in the Inbox detail pane. |

The schema is defined in `ainb-tui/crates/ainb-plugin-notifyd/src/envelope.rs`.

## Install

### Via `ainb notifyd install` (recommended)

The `ainb notifyd` subcommand (or standalone `ainb-notifyd` binary) handles everything:

```bash
# Install for both agents
ainb notifyd install

# Claude Code only
ainb notifyd install --claude

# Codex CLI only
ainb notifyd install --codex

# Check what's installed
ainb notifyd status

# Remove
ainb notifyd uninstall
ainb notifyd uninstall --claude
ainb notifyd uninstall --codex
```

Install is **idempotent** — running it twice produces the same state. The install record at `~/.agents-in-a-box/install.json` records the plugin version stamped at install time so `ainb-tui` can detect drift after an `ainb` upgrade and offer to re-install.

What the installer does, step by step:

1. Extracts `notify.sh` to `~/.agents-in-a-box/hooks/notify.sh` (chmod 755).
2. **Claude:** creates `~/.claude/plugins/ainb-hooks/.claude-plugin/plugin.json` and symlinks (or copies) `notify.sh` into `~/.claude/plugins/ainb-hooks/hooks/notify.sh`.
3. **Codex:** reads `~/.codex/hooks.json` (if present), strips any prior ainb-managed entries tagged `_ainb_managed: true`, and merges the new block in — user hooks are never touched.
4. Writes `~/.agents-in-a-box/install.json` with the list of installed agents + `plugin_version`.

Uninstall is fully reversible: the Codex managed block is stripped by walking the JSON and removing entries tagged `_ainb_managed`, leaving user-authored hooks intact.

### Manual / standalone install

For environments where the `ainb` binary is not available, you can wire the hook manually.

**Claude Code:**

```bash
# 1. Create plugin dir
mkdir -p ~/.claude/plugins/ainb-hooks/.claude-plugin
mkdir -p ~/.claude/plugins/ainb-hooks/hooks
mkdir -p ~/.agents-in-a-box/hooks

# 2. Copy notify.sh (from this repo's plugins/ainb-hooks/hooks/)
cp plugins/ainb-hooks/hooks/notify.sh ~/.agents-in-a-box/hooks/notify.sh
chmod 755 ~/.agents-in-a-box/hooks/notify.sh
ln -sf ~/.agents-in-a-box/hooks/notify.sh ~/.claude/plugins/ainb-hooks/hooks/notify.sh

# 3. Copy plugin.json
cp plugins/ainb-hooks/.claude-plugin/plugin.json \
   ~/.claude/plugins/ainb-hooks/.claude-plugin/plugin.json
```

**Codex CLI:**

Merge the following block into `~/.codex/hooks.json` under the top-level `"hooks"` key, replacing `__AINB_HOOK_SCRIPT__` with the absolute path to `notify.sh`:

```json
"Notification": [
  {
    "hooks": [
      {
        "type": "command",
        "command": "AINB_AGENT=codex /absolute/path/to/notify.sh",
        "timeout": 5,
        "_ainb_managed": true
      }
    ]
  }
],
"Stop": [
  {
    "hooks": [
      {
        "type": "command",
        "command": "AINB_AGENT=codex /absolute/path/to/notify.sh",
        "timeout": 5,
        "_ainb_managed": true
      }
    ]
  }
]
```

> **Note:** Hooks load at session start. Restart any running Claude or Codex session after installing. Inside Claude, run `/hooks` to confirm `ainb-hooks` is listed.

## Daemon lifecycle

This plugin does **not** start `ainb-notifyd`. The daemon is lazy-spawned by `notify.sh` on the first event:

1. `notify.sh` attempts delivery to `~/.agents-in-a-box/notify.sock`.
2. If the socket is absent, it runs `nohup ainb notifyd &` (guarded by a `flock` to avoid concurrent spawn races) and waits up to 500ms for the socket to appear.
3. If the spawn succeeds, it retries delivery.
4. If the spawn fails (e.g. `ainb` is not on `PATH`), it appends the envelope to `~/.agents-in-a-box/notify.fallback.jsonl`. The daemon replays and clears this file on its next startup.

The daemon can also be started manually: `ainb notifyd run`.

## Smoke-test

Fire the installed hook directly to confirm end-to-end delivery:

```bash
printf '{"hook_event_name":"Stop","session_id":"test","cwd":"'"$PWD"'","payload":{"message":"smoke-test"}}' \
  | AINB_AGENT=claude bash ~/.agents-in-a-box/hooks/notify.sh
```

Then verify:

```bash
# Check daemon status
ainb notifyd status

# Check database
sqlite3 ~/.agents-in-a-box/notifications.db \
  "SELECT agent, raw_event, datetime(ts/1000,'unixepoch','localtime') FROM notifications ORDER BY ts DESC LIMIT 3;"
```

A row should appear and a macOS banner should pop (if Notification Center permission is granted for your terminal).

## See also

- [Inbox & notifications](../../docs/tui/inbox-notifications.md) — comprehensive guide to the daemon, Inbox screen, and all four UI surfaces
- `ainb-tui/crates/ainb-plugin-notifyd/` — daemon source (envelope, store, listener, osnotify, install)
- `ainb-tui/crates/ainb-core/src/components/inbox.rs` — Inbox TUI screen
- `ainb-tui/crates/ainb-core/src/components/session_list.rs` — per-session attention markers
