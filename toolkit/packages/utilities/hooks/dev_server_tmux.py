#!/usr/bin/env python3
"""
Dev Server Auto-Tmux Hook (PreToolUse:Bash)

Detects bare dev server commands and transforms them to run in tmux
with random ports. Prevents Claude from blocking on foreground servers.

Supported commands:
- npm run dev, npm start, npx next dev, npx vite
- pnpm dev, pnpm run dev
- yarn dev, yarn run dev
- bun run dev
- flask run, python -m flask run
- python app.py, python manage.py runserver
- cargo run (when in a web project)
- go run main.go (when in a web project)
- rails server, rails s
- php artisan serve

Profile: standard,strict
"""

import json
import os
import random
import re
import shutil
import sys
from pathlib import Path


# Patterns that indicate a dev server command
DEV_SERVER_PATTERNS = [
    r'\bnpm run dev\b',
    r'\bnpm start\b',
    r'\bnpx (next|vite|nuxt|remix) dev\b',
    r'\bpnpm( run)? dev\b',
    r'\byarn( run)? dev\b',
    r'\bbun run dev\b',
    r'\bflask run\b',
    r'\bpython -m flask run\b',
    r'\bpython manage\.py runserver\b',
    r'\brails server\b',
    r'\brails s\b',
    r'\bphp artisan serve\b',
    r'\buvicorn\b.*\bapp\b',
    r'\bgunicorn\b',
]

# Compile patterns
DEV_REGEX = re.compile('|'.join(DEV_SERVER_PATTERNS), re.IGNORECASE)


def sanitize_session_name(name: str) -> str:
    """Sanitize for tmux session name (alphanumeric, dash, underscore only)."""
    return re.sub(r'[^a-zA-Z0-9_-]', '_', name) or 'dev'


def main():
    raw = sys.stdin.read()

    try:
        data = json.loads(raw) if raw.strip() else {}
        command = data.get("tool_input", {}).get("command", "")

        if not command or not DEV_REGEX.search(command):
            sys.stdout.write(raw)
            sys.exit(0)

        # Check if already using tmux
        if "tmux" in command:
            sys.stdout.write(raw)
            sys.exit(0)

        # Check if tmux is available
        if not shutil.which("tmux"):
            sys.stderr.write("[Hook] tmux not installed — skipping auto-tmux transform\n")
            sys.stdout.write(raw)
            sys.exit(0)

        # Generate random port and session name
        port = random.randint(3000, 9999)
        project_name = sanitize_session_name(Path.cwd().name)
        timestamp = str(int(__import__('time').time()))
        session_name = f"dev-{project_name}-{timestamp}"

        # Escape single quotes in original command for shell safety
        escaped_cmd = command.replace("'", "'\\''")

        # Build tmux-wrapped command
        transformed = (
            f'SESSION="{session_name}"; '
            f'PORT={port}; '
            f'tmux kill-session -t "$SESSION" 2>/dev/null || true; '
            f"tmux new-session -d -s \"$SESSION\" 'PORT={port} {escaped_cmd} 2>&1 | tee dev-server-{port}.log'; "
            f'echo "[Hook] Dev server started in tmux session: {session_name}"; '
            f'echo "  Port: {port}"; '
            f'echo "  Attach: tmux attach -t {session_name}"; '
            f'echo "  Logs: dev-server-{port}.log"'
        )

        data["tool_input"]["command"] = transformed
        sys.stdout.write(json.dumps(data))
        sys.exit(0)

    except Exception:
        # On any error, pass through unchanged
        sys.stdout.write(raw)
        sys.exit(0)


if __name__ == "__main__":
    main()
