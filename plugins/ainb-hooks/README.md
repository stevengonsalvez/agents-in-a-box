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

## ATC plumbing (event-driven orchestration)

The same `notify.sh` doubles as the shell shim for the **event-driven ATC
plumbing**. It is dormant by default and activates only when the hook was
installed under ATC management — i.e. the command carries `AINB_MANAGED=atc`
(set by the lifecycle-hook block that `ainb fleet atc setup` merges into
`~/.claude/settings.json`). A plain notifyd-only install never sets it, so the
plumbing block is skipped and leaf sessions pay nothing.

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

> The lifecycle hooks are installed into `~/.claude/settings.json` by
> `ainb fleet atc setup` via a read-preserve-modify-write merge that keeps the
> reflect plugin's and notifyd's hooks intact — they are **not** part of this
> marketplace manifest (which stays notifyd-only: `Notification` + `Stop`).

## See also

- `crates/ainb-plugin-notifyd/` — the `ainb-notifyd` daemon
- `crates/ainb-core/src/cli/hooks.rs` — the `ainb hooks` CLI
- `crates/ainb-core/src/screens/inbox/` — the Inbox TUI screen
- `.agents/specs/2026-05-27-ainb-hooks-plugin-stub-spec.md` — design spec
