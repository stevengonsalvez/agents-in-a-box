# ainb-hooks

Plugin that emits Claude Code, Codex CLI, and GitHub Copilot CLI lifecycle
events to the **ainb notification inbox** via a Unix socket. Powers session-state
badges, the dedicated Inbox screen, and optional OS notifications in
`ainb-tui`.

## How it works

```
┌─ Claude / Codex / Copilot ───┐
│ hook fires (Stop, Notification│
│   / PermissionRequest, ...)  │
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
├── copilot/
│   └── hooks.json           # Copilot ~/.copilot/hooks/ainb.json drop-in (native format)
├── hooks/
│   └── notify.sh            # universal hook script (claude + codex + copilot)
└── README.md
```

The same `notify.sh` is used by all three agents:

- **Claude Code** pipes the hook payload as JSON on stdin.
- **Codex CLI** passes the hook payload as JSON in `argv[1]`.
- **GitHub Copilot CLI** pipes the hook payload as JSON on stdin.

`notify.sh` autodetects the source and uses the right input. The agent
is identified via `AINB_AGENT={claude,codex,copilot}` set in the registering
command line.

## Install

The recommended install path is the `ainb-notifyd` CLI (the daemon doubles as
the hook installer; `ainb notifyd …` is a hidden alias), which handles plugin
manifests, config merges, and notifyd lifecycle:

```bash
ainb-notifyd install --claude --codex --copilot   # or: --all
ainb-notifyd status
ainb-notifyd uninstall --all
```

The CLI:

1. Installs `ainb-hooks@agents-in-a-box` through the Claude plugin marketplace (Claude).
2. Merges this directory's `codex/hooks.json` into `~/.codex/hooks.json` as a
   managed block (Codex).
3. Writes this directory's `copilot/hooks.json` to `~/.copilot/hooks/ainb.json`
   as a standalone drop-in (Copilot loads every `*.json` in `~/.copilot/hooks/`
   and combines them, so ainb owns one file; uninstall deletes just that file).
4. Extracts `notify.sh` to `~/.agents-in-a-box/hooks/notify.sh` and rewrites
   the `__AINB_HOOK_SCRIPT__` placeholder in each agent's template to that
   absolute path.
5. Records the install method in `~/.agents-in-a-box/install.json` so
   `ainb hooks uninstall` is fully reversible.

## Hook events

Claude and Codex both use PascalCase event names. Every documented event is
registered and retained by Hangar. The notification inbox independently keeps
only human-facing events:

| Agent | Hooks registered | Meaning |
| --- | --- | --- |
| Claude Code | all 30 documented hooks | lifecycle, tool, task, worktree, compact, and attention state |
| Codex CLI | all 11 documented CLI hooks | lifecycle, tool, compact, subagent, approval, and session-end state |
| GitHub Copilot CLI | `notification`, `agentStop` | awaiting input / permission prompt, turn finished |

Telemetry reaches the durable provider log, never the operator inbox. This
keeps lifecycle projections accurate without burying attention signal.

The matcher `Notification:idle_prompt` (Claude) and `PermissionRequest`
(Codex) carry different raw names but the same user-facing shape: the agent
needs Stevie. `notify.sh` preserves the raw event name in the `raw_event` field
of the envelope so UI mapping happens in the consumer, not at the wire.

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

## ATC plumbing (event-driven orchestration)

The same `notify.sh` sends Claude and Codex hook payloads into Hangar's durable
provider log. `AINB_MANAGED=atc` adds ATC-only structured control: completion
routing and synchronous Claude answer or permission brokerage.

When active, after delivering the notifyd envelope the script forwards the parsed
event to the Rust side:

```sh
ainb fleet atc hook --event "$AINB_HOOK_EVENT" --session-id "$SID" --cwd "$CWD"
```

`ainb fleet atc hook` then:

1. Writes an atomic per-session **status file** (`~/.agents-in-a-box/status/<id>.json`).
2. On `UserPromptSubmit` (a genuine user turn) resets the session's Stop-drain
   block budget.
3. On `Stop`: commits a **last-wins completion** to the parent's durable inbox
   (resolved from `AINB_PARENT_SESSION` or `parents.json`; unresolvable parents
   dead-letter), then **drains its own inbox** — if it carries child completions
   and the block budget allows, the script relays
   `{"decision":"block","reason":<completions>}` on stdout so Claude Code feeds
   the completions back as the session's next turn.

Empty inbox ⇒ no block, no writes beyond the status file. This durable inbox
replaces reliance on the claude-peers broker's lossy delivery path. Full design,
formats, and the consecutive-block budget are documented in
[`docs/atc-plumbing.md`](../../docs/atc-plumbing.md).

> `ainb fleet atc setup` merges the same Claude lifecycle registrations into
> `~/.claude/settings.json` for managed sessions while preserving other hooks.

## See also

- `crates/ainb-plugin-notifyd/` — the `ainb-notifyd` daemon
- `crates/ainb-core/src/cli/hooks.rs` — the `ainb hooks` CLI
- `crates/ainb-core/src/screens/inbox/` — the Inbox TUI screen
- `.agents/specs/2026-05-27-ainb-hooks-plugin-stub-spec.md` — design spec
