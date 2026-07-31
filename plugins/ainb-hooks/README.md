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
│   ├── notify.sh            # universal hook script (claude + codex + copilot)
│   └── stall_guard.py       # Stop-hook stall guard (claude + codex)
└── README.md
```

The same `notify.sh` is used by all three agents:

- **Claude Code** pipes the hook payload as JSON on stdin.
- **Codex CLI** passes the hook payload as JSON in `argv[1]`.
- **GitHub Copilot CLI** pipes the hook payload as JSON on stdin.

`notify.sh` autodetects the source and uses the right input. The agent
is identified via `AINB_AGENT={claude,codex,copilot}` set in the registering
command line.

## Stall guard (Claude + Codex)

`hooks/stall_guard.py` runs as a second `Stop` hook, alongside `notify.sh`. It
refuses turn-end when a session is about to park on work that is still in
flight with nothing armed to wake it, which is how an ATC-managed session goes
quiet with a CI job half-finished.

```
┌──────────────┐  no    ┌────────────────────┐  no   ┌──────────────┐
│ live task /  │───────▶│ in-flight evidence │──────▶│ allow stop   │
│ Monitor?     │        │ in the final state │       └──────────────┘
└──────┬───────┘        └─────────┬──────────┘
       │ yes                      │ yes
       ▼                          ▼
┌──────────────┐          ┌──────────────────┐
│ allow stop   │          │ block + tell it  │
│ (wake armed) │          │ to arm a wake    │
└──────────────┘          └──────────────────┘
```

Design notes, each one paid for by a false positive found in a
120-transcript replay:

- Scans **assistant text and `tool_result` bodies only**. Matching whole
  transcript entries drags in system prompts, skill listings, `TaskUpdate`
  todo statuses and task-notification bodies (13% false-positive rate).
- Only the turn's **final** state counts. A turn that polled a queued job and
  then watched it go green is finished, not stalled.
- "Armed" means **still live**. A background watcher that already completed,
  or was launched over 30 minutes ago and never reported, is not a wake. Todo
  tools (`TaskCreate`/`TaskUpdate`) are never treated as arming.
- `stop_hook_active` short-circuits to allow, so the guard nudges once and can
  never wedge a session.

It cannot catch a watcher that was armed and later died silently; nothing at
turn-end can see that. That case belongs to the idle-session path
(`TeammateIdle` / notifyd), not here.

### Both agents, one script

Codex ships the same Stop contract as Claude: `stop.command.output` accepts
`{"decision": "block", "reason": …}`, and the binary rejects a block without a
non-empty reason. The differences are all at the edges, so they are absorbed in
one place each:

| | Claude Code | Codex CLI |
|---|---|---|
| payload delivery | stdin | `argv[1]` |
| transcript | `transcript_path` JSONL | rollout JSONL, `response_item` lines (nullable path) |
| closing text | last assistant entry | `last_assistant_message`, required on input |
| advice given | background Bash, Monitor, ScheduleWakeup | foreground poll loop (no background primitive) |

`read_payload()` accepts either delivery, `adapt()` reshapes a Codex rollout
into the Claude transcript shape, and everything downstream is shared. Codex is
wired through `codex/hooks.json` via the `__AINB_STALL_GUARD__` placeholder,
which `ainb-notifyd install --codex` substitutes after extracting the script to
`~/.agents-in-a-box/hooks/stall_guard.py` next to `notify.sh`.

Copilot is not wired: its hook format has no Stop-with-decision contract.

Run the self-check after editing it:

```bash
python3 plugins/ainb-hooks/hooks/stall_guard.py --self-test   # 24 cases
```

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
4. Extracts `notify.sh` to `~/.agents-in-a-box/hooks/notify.sh` and
   `stall_guard.py` beside it, then rewrites the `__AINB_HOOK_SCRIPT__` and
   `__AINB_STALL_GUARD__` placeholders in each agent's template to those
   absolute paths. Claude needs neither substitution: the marketplace install
   puts both scripts in the plugin directory, which `${CLAUDE_PLUGIN_ROOT}`
   already resolves.
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
