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
import time
import traceback
from datetime import datetime
from pathlib import Path

try:
    import yaml
except ImportError:
    yaml = None


# --- Silent-fail event sink ----------------------------------------------
#
# PreCompact must never raise into the user's session. On any uncaught
# exception we still exit 0 (silently), but we leave a breadcrumb at
# ~/.reflect/last-event.json so the status line can render a "⚠ reflect
# failed" fragment without polluting stderr.

LAST_EVENT_PATH = Path(
    os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect"))
) / "last-event.json"


def _write_last_event(event: str, kind: str, detail: str) -> None:
    """Best-effort error breadcrumb for the status line. Never raises."""
    try:
        LAST_EVENT_PATH.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "event": event,
            "hook": "precompact_reflect",
            "kind": kind,
            "detail": detail[:500],
            "ts": time.time(),
        }
        tmp = LAST_EVENT_PATH.with_suffix(".json.tmp")
        tmp.write_text(json.dumps(payload), encoding="utf-8")
        tmp.replace(LAST_EVENT_PATH)
    except Exception:
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
    """Log the PreCompact event for debugging."""
    log_dir = Path.home() / '.claude' / 'logs'
    log_dir.mkdir(parents=True, exist_ok=True)

    log_file = log_dir / 'reflect_precompact.log'

    timestamp = datetime.now().isoformat()
    session_id = input_data.get('session_id', 'unknown')[:8]
    trigger = input_data.get('trigger', 'unknown')

    log_entry = f"[{timestamp}] session={session_id} trigger={trigger} mode={mode}\n"

    with open(log_file, 'a') as f:
        f.write(log_entry)


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
        _write_last_event(
            event="error",
            kind=type(exc).__name__,
            detail=str(exc) or traceback.format_exc(limit=2),
        )
        sys.exit(0)


if __name__ == '__main__':
    main()
