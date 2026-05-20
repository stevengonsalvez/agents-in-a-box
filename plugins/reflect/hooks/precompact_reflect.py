#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "pyyaml",
# ]
# ///
"""
PreCompact Reflect Hook

Integrates with Claude Code's PreCompact hook to trigger reflection
before context compaction. Can run in background mode to avoid blocking.

Usage in settings.json:
{
  "hooks": {
    "PreCompact": [
      {
        "hooks": [
          {
            "type": "command",
            "command": "uv run /path/to/precompact_reflect.py --remind"
          }
        ]
      }
    ]
  }
}

Modes:
  --remind    : Add reminder to run /reflect (non-blocking)
  --auto      : Trigger automatic reflection if enabled (blocking, generates output)
  --log-only  : Just log the event (non-blocking)
"""

import argparse
import json
import os
import sys
import traceback
from datetime import datetime
from pathlib import Path

try:
    import yaml
except ImportError:
    yaml = None


# Shared silent-fail mechanics — breadcrumb writer, secret scrubber,
# forensics log. See plugins/reflect/scripts/silent_fail.py.
_HOOK_NAME = "precompact_reflect"
_PLUGIN_ROOT = Path(__file__).resolve().parents[1]  # hooks/<this> → plugins/reflect/
sys.path.insert(0, str(_PLUGIN_ROOT / "scripts"))
try:
    from silent_fail import write_last_event, forensics_log  # noqa: E402
except ImportError:
    def write_last_event(**kwargs):  # type: ignore[no-redef]
        pass
    def forensics_log(*args, **kwargs):  # type: ignore[no-redef]
        pass


def get_state_dir() -> Path:
    """Get the reflect state directory."""
    custom_dir = os.environ.get('REFLECT_STATE_DIR')
    if custom_dir:
        return Path(custom_dir).expanduser()

    return Path.home() / '.reflect'


def is_auto_reflect_enabled() -> bool:
    """
    Auto-reflect is ON by default when the plugin is installed.
    Override with REFLECT_AUTO_REFLECT=0 to disable.

    Legacy reflect-state.yaml is no longer consulted (deprecated in v3
    migration on 2026-05-09; values would always return False because the
    migration stub does not carry the auto_reflect key).
    """
    val = os.environ.get('REFLECT_AUTO_REFLECT', '').strip().lower()
    if val in ('0', 'false', 'no', 'off'):
        return False
    return True


def log_precompact_event(input_data: dict, mode: str):
    """Log the PreCompact event for debugging.

    Lands at ``$REFLECT_STATE_DIR/logs/reflect_precompact.log`` (defaults
    to ``~/.reflect/logs/``) — harness-agnostic, so codex-fired
    invocations log to the same place as claude-fired ones. Previously
    this hardcoded ``~/.claude/logs/`` which dropped codex logs into a
    Claude-flavoured directory the user wouldn't think to look in.
    """
    session_id = str(input_data.get('session_id', 'unknown'))[:8]
    trigger = input_data.get('trigger', 'unknown')
    forensics_log(_HOOK_NAME, f"session={session_id} trigger={trigger} mode={mode}")


def generate_reminder_context(trigger: str) -> dict:
    """Generate context reminder for reflection."""
    auto_enabled = is_auto_reflect_enabled()

    if trigger == 'auto':
        message = (
            "Context compaction triggered. "
            "Consider running `/reflect` to capture learnings from this session before compaction."
        )
    else:
        message = (
            "Manual compaction requested. "
            "Run `/reflect` first if you want to preserve learnings from this session."
        )

    if auto_enabled:
        message += "\n\nNote: Auto-reflect is enabled. Running reflection analysis..."

    return {
        "hookSpecificOutput": {
            "hookEventName": "PreCompact",
            "additionalContext": message
        }
    }


def run_reflection_analysis(input_data: dict) -> dict:
    """
    Queue the current session's transcript for reflection on the *next* session start.

    Hook scripts can't run an LLM, so they can't do real signal detection. Instead we
    append the transcript path + metadata to a JSONL queue. A SessionStart drain hook
    surfaces queued entries to the next agent via additionalContext, and that agent
    (which IS an LLM) runs the actual /reflect analysis on each transcript.
    """
    transcript_path = input_data.get('transcript_path', '')

    if not transcript_path:
        return {
            "hookSpecificOutput": {
                "hookEventName": "PreCompact",
                "additionalContext": "Auto-reflect: no transcript_path in event, skipping queue."
            }
        }

    queue_dir = get_state_dir()
    queue_dir.mkdir(parents=True, exist_ok=True)
    queue_file = queue_dir / 'pending_reflections.jsonl'

    entry = {
        "ts": datetime.now().isoformat(),
        "session_id": input_data.get('session_id', 'unknown'),
        "transcript_path": transcript_path,
        "trigger": input_data.get('trigger', 'unknown'),
        "cwd": input_data.get('cwd', os.getcwd()),
    }

    with open(queue_file, 'a') as f:
        f.write(json.dumps(entry) + '\n')

    # Count pending entries (cheap: re-open file)
    with open(queue_file) as f:
        pending_count = sum(1 for line in f if line.strip())

    return {
        "hookSpecificOutput": {
            "hookEventName": "PreCompact",
            "additionalContext": (
                f"Auto-reflect: transcript queued for analysis at next session start "
                f"({pending_count} pending). Real signal detection runs in the next agent."
            )
        }
    }


def _main_body():
    parser = argparse.ArgumentParser(description='PreCompact Reflect Hook')
    parser.add_argument('--remind', action='store_true',
                       help='Add reminder to run /reflect')
    parser.add_argument('--auto', action='store_true',
                       help='Trigger automatic reflection if enabled')
    parser.add_argument('--log-only', action='store_true',
                       help='Just log the event')
    parser.add_argument('--verbose', action='store_true',
                       help='Print verbose output')

    args = parser.parse_args()

    # Read input from stdin
    try:
        input_data = json.loads(sys.stdin.read())
    except json.JSONDecodeError:
        input_data = {}

    trigger = input_data.get('trigger', 'unknown')

    # Determine mode
    if args.log_only:
        mode = 'log-only'
    elif args.auto:
        mode = 'auto'
    else:
        mode = 'remind'

    # Log the event
    log_precompact_event(input_data, mode)

    # Handle based on mode
    if mode == 'log-only':
        if args.verbose:
            print(f"Logged PreCompact event (trigger={trigger})")
        sys.exit(0)

    elif mode == 'auto' and is_auto_reflect_enabled():
        # Run automatic reflection
        output = run_reflection_analysis(input_data)
        print(json.dumps(output))
        sys.exit(0)

    elif mode == 'remind':
        # Just add a reminder
        output = generate_reminder_context(trigger)
        print(json.dumps(output))
        sys.exit(0)

    else:
        # Auto mode but not enabled, just remind
        if args.verbose:
            print("Auto-reflect not enabled, adding reminder")
        output = generate_reminder_context(trigger)
        print(json.dumps(output))
        sys.exit(0)


def main():
    """Top-level entry. Any uncaught exception writes a breadcrumb to
    ~/.reflect/last-event.json and exits 0 silently so PreCompact never
    surfaces a traceback into the user's session.

    SystemExit is re-raised so intentional clean exits (sys.exit(0) from
    the body) propagate unchanged. We don't catch argparse's SystemExit(2)
    on bad args either — that's a config bug worth surfacing.
    """
    try:
        _main_body()
    except SystemExit:
        raise
    except BaseException as exc:  # noqa: BLE001 — deliberately broadest catch
        detail = str(exc) or traceback.format_exc(limit=2)
        write_last_event(
            hook_name=_HOOK_NAME,
            event="error",
            kind=type(exc).__name__,
            detail=detail,
        )
        forensics_log(_HOOK_NAME, f"{type(exc).__name__}: {detail}")
        sys.exit(0)


if __name__ == '__main__':
    main()
