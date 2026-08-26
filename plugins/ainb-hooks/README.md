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
refuses turn-end on two shapes of stall: a session about to park on work still
in flight with nothing armed to wake it, and a session handing a decision back
to the human as prose, which no fleet surface can see.

```
   Stop
     │
     ▼
┌─────────────────┐  spent  ┌────────────┐
│ block budget    │────────▶│ allow stop │
│ (the terminator)│         └────────────┘
└────────┬────────┘
         ▼
┌─────────────────┐  yes    ┌──────────────────────────┐
│ closing text    │────────▶│ block: ask it through    │
│ asks the human? │         │ AskUserQuestion instead  │
└────────┬────────┘         └──────────────────────────┘
         │ no
         ▼
┌──────────────┐  yes   ┌────────────┐
│ live task /  │───────▶│ allow stop │
│ Monitor?     │        │ (armed)    │
└──────┬───────┘        └────────────┘
       │ no
       ▼
┌────────────────────┐  yes   ┌──────────────────┐
│ in-flight evidence │───────▶│ block: arm a wake│
│ in the final state │        └──────────────────┘
└────────────────────┘
```

**Why the ask mode exists.** `Stop` with no background work records
`AttentionState::None`, and only an `AskUserQuestion` event records
`attention=Ask`. A question written as prose is therefore indistinguishable
from a finished turn: nothing reaches `ainb fleet needs`, the TUI or the macOS
app, and the session parks until somebody looks at the pane.

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
- Closing text is matched against a **prose surface**, with fenced code,
  markdown table rows, blockquotes, quoted strings and `[fact]`/`[inference]`
  evidence bullets stripped first. The `<turn_end_block>` state table CLAUDE.md
  mandates ends turns with cells like `| repo | no files changed | your call |`,
  which made the guard block on its own required output format. Replayed over
  400 real turn-ends this cut blocks from 54 to 16.
- An open picker exempts the **ask** check only, never the wait checks. The
  picker's answer arrives as a `tool_result`, not a real user message, so the
  turn never rolls over; exempting the whole turn silently disabled CI stall
  detection for the rest of it.
- `stop_hook_active` is **not** a short-circuit. Measured: the harness re-fires
  `Stop` and honours a block every time, so exiting on the flag capped
  enforcement at one nudge per stall. A per-session budget under
  `$AINB_HOME/stall/` replaces it: 3 consecutive blocks, 20 per session, and no
  re-block while the tool-call count and closing text are both unchanged.
  Because that budget is the ONLY terminator, a state file that cannot be
  written fails open rather than blocking forever. `AINB_STALL_GUARD=off`
  disables the hook outright.
- A watcher still live at turn-end is **not** treated as a leak. Turn-end is
  not session-end, and every candidate signal for "the turn claims to be
  finished" matched narration about watching (`catches merged/closed`) in real
  transcripts. That check belongs on `SessionEnd`, not here.

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
| payload delivery | stdin | stdin (Stop; probed live as `argc=0`) |
| transcript | `transcript_path` JSONL | rollout JSONL, `response_item` lines (nullable path) |
| closing text | last assistant entry | `last_assistant_message`, required on input |
| advice given | background Bash, Monitor, ScheduleWakeup | foreground poll loop (no background primitive) |

`read_payload()` accepts either delivery, `adapt()` reshapes a Codex rollout
into the Claude transcript shape, and everything downstream is shared. Codex is
wired through `codex/hooks.json` via the `__AINB_STALL_GUARD__` placeholder,
which `ainb-notifyd install --codex` substitutes after extracting the script to
`~/.agents-in-a-box/hooks/stall_guard.py` next to `notify.sh`.

Copilot is not wired: its hook format has no Stop-with-decision contract.

### Codex hook trust (required, or the guard never runs)

Codex will not run a hook it has not trusted. Every hook is pinned in
`~/.codex/config.toml` under `[hooks.state]`, keyed by
`<hooks.json path>:<event>:<group index>:<hook index>`, with a `trusted_hash`:

```toml
[hooks.state."/Users/you/.codex/hooks.json:stop:3:0"]
trusted_hash = "sha256:…"
```

An untrusted or changed entry is **skipped silently** — no error, no log line,
no hook output. Writing `hooks.json` therefore installs the hook without
enabling it, and everything looks fine until you notice turns are not being
blocked. This was diagnosed by wiring a probe wrapper in place of the guard and
watching it never get invoked while three sibling `Stop` hooks ran.

Trust covers the hook entry itself, so editing an entry (a bare `timeout`
change included) invalidates it and needs one more approval. Codex caps the
`SessionEnd` hook at 3s and prints a `clamping SessionEnd hook timeout to 3s`
error item at the top of every session for anything higher, which is why the
template pins that one event to 3.

After `ainb-notifyd install --codex`, start `codex` once and approve the
startup hooks review (`tui/src/startup_hooks_review.rs`, shown when "hooks are
new or changed"). That writes the `trusted_hash` entries.

For non-interactive automation, `codex exec --dangerously-bypass-hook-trust`
runs enabled hooks without persisted trust. That is how the end-to-end
verification below was done; it is not a substitute for trusting the hook.

Verified end to end on Codex with trust bypassed: a turn closing with "The CI
checks are still running on main." produced `{"decision":"block",…}` and the
turn was re-run. Note that Codex's `transcript_path` is nullable, so the budget
fingerprint folds in the closing text rather than relying on a tool-call count
that would otherwise be pinned at zero for the whole session.

Run the self-check after editing it:

```bash
python3 plugins/ainb-hooks/hooks/stall_guard.py --self-test   # 52 cases
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
