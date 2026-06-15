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

# Cross-harness stdin readers (snake_case claude/codex, camelCase copilot).
try:
    from hook_input import get_session_id, get_transcript_path, get_cwd  # noqa: E402
except ImportError:
    def get_session_id(data, default=""):  # type: ignore[no-redef]
        for k in ("session_id", "sessionId"):
            if k in data:
                return data[k]
        return default
    def get_transcript_path(data, default=""):  # type: ignore[no-redef]
        for k in ("transcript_path", "transcriptPath"):
            if k in data:
                return data[k]
        return default
    def get_cwd(data, default=""):  # type: ignore[no-redef]
        return data["cwd"] if "cwd" in data else default


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
    session_id = str(get_session_id(input_data, 'unknown'))[:8]
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
    transcript_path = get_transcript_path(input_data)

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
    cost_file = queue_dir / 'drain-cost.jsonl'

    # Enqueue gate + dedup ($0 regex). Skip reflect-on-reflect / clean /
    # no-signal transcripts and anything already queued/processed, so the
    # drainer never spends a model call on them. Fail-open: if the gate
    # module is unavailable we enqueue as before.
    try:
        from reflect_gate import should_enqueue  # noqa: E402
        ok, reason = should_enqueue(transcript_path, queue_file, cost_file)
        if not ok:
            forensics_log(_HOOK_NAME, f"gate skip ({reason}): {transcript_path}")
            return {
                "hookSpecificOutput": {
                    "hookEventName": "PreCompact",
                    "additionalContext": f"Auto-reflect: transcript skipped ({reason}).",
                }
            }
    except Exception:  # noqa: BLE001 — never block enqueue on a gate error
        pass

    entry = {
        "ts": datetime.now().isoformat(),
        "session_id": get_session_id(input_data, 'unknown'),
        "transcript_path": transcript_path,
        "trigger": input_data.get('trigger', 'unknown'),
        "cwd": get_cwd(input_data, os.getcwd()),
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

    # ── stdout protocol ───────────────────────────────────────────────────
    # PreCompact is a pure side-effect hook in both harnesses:
    #   * The transcript is about to be compacted — anything we inject as
    #     additionalContext gets compacted immediately, so it's pointless.
    #   * Codex 0.131 does NOT define ``PreCompactHookSpecificOutputWire``
    #     in its schema. Emitting ``{"hookSpecificOutput":{...}}`` for
    #     PreCompact triggers an "invalid PreCompact hook JSON output"
    #     error in codex (Claude tolerates it, but it's noise).
    #
    # So: regardless of mode, we just enqueue (run_reflection_analysis does
    # the file I/O for its side-effect) and exit 0 with NO stdout output.
    # Verbose chatter goes to stderr only — never pollute the protocol
    # channel.
    if args.verbose:
        print(f"[precompact_reflect] mode={mode} trigger={trigger}", file=sys.stderr)

    if mode == 'log-only':
        sys.exit(0)

    # 'auto' or 'remind' — both enqueue for the next-session drainer.
    # We deliberately ignore the dict return value: the queue is the
    # canonical side-effect; the dict was only useful when we were
    # injecting additionalContext (which we no longer do).
    if mode == 'auto' and is_auto_reflect_enabled():
        run_reflection_analysis(input_data)
    # 'remind' mode and the "auto but disabled" fallback are no-ops on
    # disk now — the drainer surfaces queued entries on its own.

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
