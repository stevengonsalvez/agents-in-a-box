#!/usr/bin/env python3
"""
Hook runner with profile gating.

Wraps any existing hook script, checking profile/disabled status before executing.

Usage:
    uv run run_with_profile.py <hook_id> <script_path> [profiles] [-- extra_args...]

Examples:
    uv run run_with_profile.py cost-tracker cost_tracker.py minimal,standard,strict
    uv run run_with_profile.py ts-check ts_check.py standard,strict -- --verbose

In settings.json:
    "command": "uv run ~/.claude/hooks/run_with_profile.py cost-tracker cost_tracker.py minimal,standard,strict"
"""

import os
import sys
import subprocess


# Add parent dir to path for imports
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from utils.hook_profile import is_hook_enabled


def main():
    args = sys.argv[1:]

    if len(args) < 2:
        # No hook ID or script — pass through stdin to stdout
        sys.stdout.write(sys.stdin.read())
        sys.exit(0)

    hook_id = args[0]
    script_path = args[1]
    profiles_csv = args[2] if len(args) > 2 and not args[2].startswith("--") else None

    # Parse extra args after --
    extra_args = []
    if "--" in args:
        dash_idx = args.index("--")
        extra_args = args[dash_idx + 1:]
    elif len(args) > 3:
        extra_args = [a for a in args[3:] if a != "--"]

    # Parse profiles
    profiles = None
    if profiles_csv:
        profiles = [p.strip() for p in profiles_csv.split(",") if p.strip()]

    if not is_hook_enabled(hook_id, profiles=profiles):
        # Hook is disabled — pass through stdin unchanged
        sys.stdout.write(sys.stdin.read())
        sys.exit(0)

    # Resolve script path relative to hooks directory
    hooks_dir = os.path.dirname(os.path.abspath(__file__))
    if not os.path.isabs(script_path):
        script_path = os.path.join(hooks_dir, script_path)

    if not os.path.exists(script_path):
        sys.stderr.write(f"[Hook] Script not found for {hook_id}: {script_path}\n")
        sys.stdout.write(sys.stdin.read())
        sys.exit(0)

    # Read stdin to pass to child
    stdin_data = sys.stdin.read()

    # Execute the actual hook script
    cmd = ["uv", "run", script_path] + extra_args
    result = subprocess.run(
        cmd,
        input=stdin_data,
        capture_output=True,
        text=True,
        cwd=os.getcwd(),
        env=os.environ.copy(),
    )

    if result.stdout:
        sys.stdout.write(result.stdout)
    if result.stderr:
        sys.stderr.write(result.stderr)

    sys.exit(result.returncode)


if __name__ == "__main__":
    main()
