#!/usr/bin/env python3
"""
TypeScript Check Hook (PostToolUse:Edit)

Runs tsc --noEmit after editing .ts/.tsx files and reports only errors
in the edited file. Non-blocking — passes through on any failure.

Profile: standard,strict
"""

import json
import os
import subprocess
import sys
from pathlib import Path


def find_tsconfig(file_path: str, max_depth: int = 20) -> str | None:
    """Walk up from file to find nearest tsconfig.json."""
    current = Path(file_path).resolve().parent
    root = Path(current.anchor)
    depth = 0

    while current != root and depth < max_depth:
        tsconfig = current / "tsconfig.json"
        if tsconfig.exists():
            return str(current)
        current = current.parent
        depth += 1

    return None


def main():
    raw = sys.stdin.read()

    try:
        data = json.loads(raw) if raw.strip() else {}
        file_path = data.get("tool_input", {}).get("file_path", "")

        # Only check .ts/.tsx files
        if not file_path or not file_path.endswith((".ts", ".tsx")):
            sys.stdout.write(raw)
            sys.exit(0)

        resolved = str(Path(file_path).resolve())
        if not Path(resolved).exists():
            sys.stdout.write(raw)
            sys.exit(0)

        # Find nearest tsconfig.json
        tsconfig_dir = find_tsconfig(resolved)
        if not tsconfig_dir:
            sys.stdout.write(raw)
            sys.exit(0)

        # Run tsc --noEmit
        try:
            result = subprocess.run(
                ["npx", "tsc", "--noEmit", "--pretty", "false"],
                cwd=tsconfig_dir,
                capture_output=True,
                text=True,
                timeout=30,
            )
        except (subprocess.TimeoutExpired, FileNotFoundError):
            sys.stdout.write(raw)
            sys.exit(0)

        # tsc exits non-zero when there are errors
        if result.returncode != 0:
            output = (result.stdout or "") + (result.stderr or "")

            # Filter to only errors in the edited file
            rel_path = os.path.relpath(resolved, tsconfig_dir)
            candidates = {file_path, resolved, rel_path}

            relevant = []
            for line in output.split("\n"):
                if any(c in line for c in candidates):
                    relevant.append(line)

            relevant = relevant[:10]  # Cap at 10 errors

            if relevant:
                basename = os.path.basename(file_path)
                sys.stderr.write(f"[Hook] TypeScript errors in {basename}:\n")
                for line in relevant:
                    sys.stderr.write(f"  {line}\n")
    except Exception:
        pass  # Non-blocking

    sys.stdout.write(raw)
    sys.exit(0)


if __name__ == "__main__":
    main()
