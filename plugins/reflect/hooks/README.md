# Reflect Hooks Integration

This directory contains hooks for integrating the reflect skill with Claude Code's hook system.

## Available Hooks

The `reflect` plugin wires five Claude Code lifecycle events via `.claude-plugin/plugin.json`. Most hook scripts live in this directory; the two `recall` hooks live under `../skills/recall/hooks/`.

| Event | Script | Purpose |
|-------|--------|---------|
| `SessionStart` | `../skills/recall/hooks/session_start_recall.py` | Hybrid-search the KB and auto-inject the top learnings into the new session |
| `SessionStart` | `reflect-drain-bg.sh` | Background-drain the queued reflections from prior sessions (spawned detached; sole queue consumer as of v4.0.0) |
| `UserPromptSubmit` | `../skills/recall/hooks/user_prompt_submit_recall.py` | Recall against the submitted prompt before the turn runs |
| `PostToolUse` | `posttooluse_minilearning.py` | Arm low-cost mini-learning capture after each tool call |
| `Stop` | `stop_reflect.py` | Gate + enqueue short-session reflection when the agent turn ends |
| `PreCompact` | `precompact_reflect.py --auto --verbose` | Gate + queue the transcript for the bg-drain cascade before context compaction |

This README documents `precompact_reflect.py` in detail below; the other scripts are wired automatically by the plugin and need no manual install.

> **Note (v4.0.0):** the retired `sessionstart_drain_reflections.py` "surfacer"
> is **not** in this list — it is a no-op and not wired in `plugin.json`. The
> background drainer (`reflect-drain-bg.sh`) is the sole consumer of the
> pending-reflections queue.

### Producer/consumer model — these hooks QUEUE, they don't reflect

The producer hooks (`precompact_reflect.py --auto`, `stop_reflect.py`) are shell
commands and **cannot run an LLM**, so they never perform reflection
synchronously. They run a $0 enqueue **gate** (`../scripts/reflect_gate.py`) over
the transcript's dialogue and append it to
`~/.reflect/pending_reflections.jsonl` only if it's worth model spend:

- reflect-on-reflect transcript → **skip** (no net-new learnings)
- no correction/approval/knowledge signal → **skip** (clean / no-signal session)
- already queued or already processed → **skip** (dedup)
- any signal (incl. LOW) → **enqueue**

The queued transcript is later processed by the **background drain cascade**
(`reflect-drain-bg.sh` → `../scripts/reflect_cascade.py`), which gates again,
slices the transcript to its signal-bearing windows (~10x smaller), and runs
`/reflect` on **Sonnet** by default (`REFLECT_DRAIN_MODEL`).

### Circuit-breaker (drain cost controls)

A single unbounded drain once burned ~41.5M tokens in 9.6 min for zero net-new
learnings (2026-05-31 incident). The drain now defends in depth:

| Control | Env var | Default | Effect |
|---|---|---|---|
| Turn cap | `REFLECT_DRAIN_MAX_TURNS` | `8` | Hard mid-run stop (was 25 pre-v4) |
| Wall-clock cap | `REFLECT_DRAIN_TIMEOUT` | `180` | Per-entry `claude -p` timeout, seconds (was 600 pre-v4) |
| Token-budget poison | `REFLECT_DRAIN_TOKEN_MAX` | `2000000` | Post-hoc: a completed-but-expensive run is archived so it can never be retried |
| Cascade gate+slice | `REFLECT_DRAIN_CASCADE` | `1` | Skip/slice before any spend (set `0` to disable) |
| Debounce | `REFLECT_DRAIN_DEBOUNCE_SEC` | `600` | Collapse a burst of session starts to one drain |
| Model | `REFLECT_DRAIN_MODEL` | `sonnet` | Drain model; Opus reserved for escalation + weekly synthesis |
| Neutral cwd | `REFLECT_DRAIN_CWD` | `$HOME` | The `claude -p` cwd (not the triggering repo) |
| Kill switch | `REFLECT_DISABLED` | unset | Set `1` for a hard no-op |

An atomic `mkdir` lock (`~/.reflect/drain.lock.d/`) replaces the old PID file so
concurrent session-start spawns can't each slip past the daily cap. Spend is
auditable via `reflect cost`. See [`../CHANGELOG.md`](../CHANGELOG.md).

### precompact_reflect.py

Integrates with the `PreCompact` hook event to queue reflection before context compaction.

**Modes:**

| Mode | Flag | Behavior |
|------|------|----------|
| Remind | `--remind` | No-op on disk (the drainer surfaces queued entries on its own) |
| Auto | `--auto` | Runs the enqueue gate and queues the transcript for the bg-drain cascade (does NOT reflect synchronously) |
| Log Only | `--log-only` | Just logs the event without any output |

The plugin wires `--auto --verbose`. PreCompact is a pure side-effect hook: it
emits no `additionalContext` (the transcript is about to be compacted away, and
Codex's schema rejects PreCompact output), so the queue is the only effect.

## Installation

### Option 1: Add to Existing PreCompact Hook Chain

If you already have a PreCompact hook (like running `/handover`), chain the reflect hook:

```json
{
  "hooks": {
    "PreCompact": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "uv run /path/to/plugins/reflect/hooks/precompact_reflect.py --remind"
          }
        ]
      }
    ]
  }
}
```

### Option 2: Combined Hook Command

Combine with your existing hook using shell chaining:

```json
{
  "hooks": {
    "PreCompact": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "uv run ~/.claude/hooks/pre_compact.py --backup && uv run /path/to/precompact_reflect.py --remind"
          }
        ]
      }
    ]
  }
}
```

### Option 3: Copy to ~/.claude/hooks/

Copy the script to your hooks directory for easier access:

```bash
cp precompact_reflect.py $HOME/.claude/hooks/
chmod +x $HOME/.claude/hooks/precompact_reflect.py
```

Then configure:

```json
{
  "hooks": {
    "PreCompact": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "uv run ~/.claude/hooks/precompact_reflect.py --auto"
          }
        ]
      }
    ]
  }
}
```

## Auto-reflection toggle

Auto-reflection is **ON by default** when the plugin is installed. The legacy
`/reflect on` / `auto_reflect: true` state file is no longer consulted
(deprecated in the v3 migration). To disable, set the env var:

```bash
export REFLECT_AUTO_REFLECT=0   # 0/false/no/off disables; anything else = on
```

When PreCompact triggers with `--auto`:

1. If auto-reflect is enabled (default): the enqueue gate runs and the
   transcript is queued for the bg-drain cascade. No synchronous reflection,
   no output file.
2. If `REFLECT_AUTO_REFLECT=0`: the hook is a no-op on disk (the drainer is
   not fed for this session).

## Hook Input/Output

### Input (via stdin)

The hook receives JSON input from Claude Code:

```json
{
  "session_id": "abc123...",
  "transcript_path": "/path/to/transcript.jsonl",
  "cwd": "/current/working/directory",
  "trigger": "auto|manual",
  "custom_instructions": "..."
}
```

### Output (via stdout)

The hook returns JSON that Claude processes:

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreCompact",
    "additionalContext": "Message to add to Claude's context"
  }
}
```

## Logs

The hook logs events to `$REFLECT_STATE_DIR/logs/reflect_precompact.log`
(defaults to `~/.reflect/logs/`) — harness-agnostic, so codex-fired
invocations log to the same place as claude-fired ones. The drain logs to
`~/.reflect/drain.log` (rotates at 10 MB).

## Related Files

- `../scripts/state_manager.py` - State management
- `../scripts/output_generator.py` - Reflection output generation
- `../scripts/signal_detector.py` - Signal detection
- `../skills/reflect/SKILL.md` - Main skill documentation
