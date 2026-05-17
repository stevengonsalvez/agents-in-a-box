# Reflect Hooks Integration

This directory contains hooks for integrating the reflect skill with Claude Code's hook system.

## Available Hooks

### precompact_reflect.py

Integrates with the `PreCompact` hook event to trigger reflection before context compaction.

**Modes:**

| Mode | Flag | Behavior |
|------|------|----------|
| Remind | `--remind` | Adds context reminder to run `/reflect` (non-blocking) |
| Auto | `--auto` | Triggers automatic reflection if enabled (creates output file) |
| Log Only | `--log-only` | Just logs the event without any output |

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

## Enabling Auto-Reflection

To enable automatic reflection on context compaction:

```bash
/reflect on
```

This sets `auto_reflect: true` in the state file. When PreCompact triggers:

1. If `--auto` flag is set and auto_reflect is enabled:
   - Creates reflection output file
   - Updates indexes
   - Adds context about the reflection

2. If auto_reflect is disabled:
   - Falls back to remind mode
   - Adds reminder to run `/reflect` manually

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

The hook logs events to `~/.claude/logs/reflect_precompact.log`.

## Related Files

- `../scripts/state_manager.py` - State management
- `../scripts/output_generator.py` - Reflection output generation
- `../scripts/signal_detector.py` - Signal detection
- `../skills/reflect/SKILL.md` - Main skill documentation
