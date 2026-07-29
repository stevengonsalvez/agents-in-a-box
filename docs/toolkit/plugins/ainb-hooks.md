---
title: "ainb-hooks"
description: "Claude Code / Codex / Copilot plugin that emits session lifecycle events to the ainb notification inbox over a Unix socket."
---

`ainb-hooks` (v0.2.0) is a **host-agent hook plugin**. Claude Code and Codex CLI events enter Hangar's durable provider log through `ainb-notifyd`; only attention events enter the notification inbox. It is not an `ainb` TUI subprocess plugin.

## How it works

![ainb-hooks — how it works](../../assets/diagrams/ainb-hooks.svg)

The plugin's `.claude-plugin/plugin.json` registers every documented Claude lifecycle hook with `AINB_AGENT=claude ${CLAUDE_PLUGIN_ROOT}/hooks/notify.sh`. Codex registers every documented CLI hook via `codex/hooks.json` with `AINB_AGENT=codex`; Copilot remains `copilot/hooks.json` with `AINB_AGENT=copilot`. Each hook has a 5-second timeout so a slow delivery never stalls the agent.

Every observed Claude and Codex event is appended to Hangar's durable provider log, including tool and subagent activity. `ainb-notifyd` still drops non-user-facing events, so telemetry never becomes inbox noise.

`hooks/notify.sh` is the universal normalizer. Claude Code pipes the hook payload as JSON on stdin; Codex passes it as `argv[1]`. The script autodetects the source, extracts the event name, session id, and cwd (via `jq`, falling back to `grep` in minimal environments), and wraps the verbatim original payload in a normalized envelope: `{protocol_version, agent, raw_event, session_id, cwd, project, ts, payload}`. The `raw_event` field preserves the original event name (e.g. `Notification:idle_prompt`) so semantic mapping happens in the consumer, not on the wire.

Delivery targets `~/.agents-in-a-box/notify.sock` via `nc` (or `socat`). If the socket is absent, the script attempts a best-effort lazy spawn of `ainb notifyd` (guarded by an atomic lock directory) and retries once. If that still fails, it appends the envelope to `~/.agents-in-a-box/notify.fallback.jsonl`, which `ainb-notifyd` replays and clears on its next startup. The hook always exits 0, so a delivery failure never blocks the host agent.

Downstream, `ainb-notifyd` persists envelopes to a SQLite `notifications.db` and broadcasts them to `ainb-tui` for live inbox/badge updates.

## What it provides

This plugin ships only hooks plus the shared `notify.sh` script — no skills, commands, or agents.

### Hooks

Claude and Codex retain every documented hook in Hangar. Inbox handling remains attention-only:

| Agent | Event | What fires | Why it's actionable |
| --- | --- | --- | --- |
| Claude Code | all 30 documented hooks | full lifecycle and workload record | durable projection source |
| Codex CLI | all 11 documented CLI hooks | full lifecycle and workload record | durable projection source |
| GitHub Copilot CLI | `notification` | Agent notifications and permission prompts | the agent is blocked on you |
| GitHub Copilot CLI | `agentStop` | Agent turn / session stops | the agent finished — come back |

Telemetry stays out of the inbox because the daemon drops non-user-facing events on arrival.

### Components

| File | Purpose |
| --- | --- |
| `.claude-plugin/plugin.json` | Claude Code plugin manifest (`hooks` block, name, version) |
| `codex/hooks.json` | Codex `~/.codex/hooks.json` merge template (`__AINB_HOOK_SCRIPT__` placeholder) |
| `copilot/hooks.json` | Copilot `~/.copilot/hooks/ainb.json` drop-in template |
| `hooks/notify.sh` | Universal hook script — normalizes the payload and delivers it to the socket |

## Install

`ainb-hooks` is published in the `agents-in-a-box` plugin marketplace, but you don't install it by hand — the `ainb-notifyd` binary's installer wires it for the chosen agents and manages the notifyd lifecycle:

```bash
ainb-notifyd install --claude --codex --copilot
ainb-notifyd status
ainb-notifyd uninstall --all
```

For **Claude**, the installer shells out to the `claude` plugin CLI — ensuring the `agents-in-a-box` marketplace is known, then `claude plugin install ainb-hooks@agents-in-a-box` (idempotent; "already installed" counts as success) — so Claude resolves and runs the plugin's bundled `notify.sh`. For **Codex**, it merges `codex/hooks.json` into `~/.codex/hooks.json` as a managed block, extracts `notify.sh` to `~/.agents-in-a-box/hooks/notify.sh`, and rewrites the `__AINB_HOOK_SCRIPT__` placeholder to that absolute path. For **Copilot**, it writes `copilot/hooks.json` as a standalone drop-in at `~/.copilot/hooks/ainb.json`. The install method is recorded in `~/.agents-in-a-box/install.json` so uninstall is fully reversible (`claude plugin uninstall` for Claude, managed-block removal for Codex, drop-in deletion for Copilot).

> The plugin's README recommends a higher-level `ainb hooks install` wrapper; that `ainb hooks` CLI is planned but not yet on `main`, so use `ainb-notifyd install` (above) today.

## Using it

- Once installed, every registered Claude and Codex event is forwarded to Hangar with no user action. Only actionable events surface in the inbox.
- `Notification:idle_prompt` and `PermissionRequest` events surface in `ainb-tui` as attention markers so you can see which sessions need you.
- Events show up as session-state badges and in the dedicated Inbox screen in `ainb-tui`; the inbox detail view exposes the full original hook JSON (carried in `payload`) for forensics.
- Run `ainb-notifyd status` to confirm the plugin is wired for each agent; if `ainb-notifyd` is not running, the next hook fire lazily spawns it (or buffers to the fallback file).

## Source

`plugins/ainb-hooks/` — a thin hook plugin: a manifest plus one `notify.sh` normalizer shared by Claude Code and Codex. Diagram generated via /fireworks-tech-graph.
