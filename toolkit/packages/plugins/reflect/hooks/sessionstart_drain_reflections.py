#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# ///
"""
SessionStart Drain Reflections Hook

Reads ~/.reflect/pending_reflections.jsonl (populated by precompact_reflect.py
when auto-reflect is enabled) and surfaces pending entries to the new agent
via additionalContext. The new agent — which IS an LLM — can then run real
/reflect analysis on each queued transcript.

This is the second half of a producer/consumer pair:
  - PreCompact (producer): hook script can't run an LLM, so it just appends
    {transcript_path, session_id, ts, ...} to a JSONL queue.
  - SessionStart (this script, the consumer-surfacer): reads the queue and
    asks the next agent to process it.

Output goes to additionalContext so the new agent sees it as part of its
initial context. The agent is responsible for archiving the queue once
processed (instructions included in the surfaced text).

Wire up in ~/.claude/settings.json:
{
  "hooks": {
    "SessionStart": [
      { "hooks": [
        { "type": "command",
          "command": "uv run ~/.claude/skills/reflect/hooks/sessionstart_drain_reflections.py" }
      ] }
    ]
  }
}
"""

import json
import os
import sys
from pathlib import Path


def get_state_dir() -> Path:
    custom = os.environ.get('REFLECT_STATE_DIR')
    return Path(custom).expanduser() if custom else Path.home() / '.reflect'


def main():
    queue_file = get_state_dir() / 'pending_reflections.jsonl'

    # No queue, no work — exit silently so SessionStart isn't noisy.
    if not queue_file.exists() or queue_file.stat().st_size == 0:
        sys.exit(0)

    entries = []
    with open(queue_file) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                entries.append(json.loads(line))
            except json.JSONDecodeError:
                # Skip malformed lines rather than blowing up SessionStart.
                pass

    if not entries:
        sys.exit(0)

    lines = [
        f"## Pending auto-reflections ({len(entries)})",
        "",
        "Previous Claude session(s) ended with a context compaction. Their transcripts "
        "were queued for reflection but the actual learning capture didn't run yet "
        "(hook scripts can't invoke an LLM).",
        "",
        "**Action:** For each transcript below, invoke the `/reflect` skill with the "
        "transcript path. Process them in order, then archive the queue.",
        "",
    ]
    for i, e in enumerate(entries, 1):
        sid = str(e.get('session_id', 'unknown'))[:8]
        cwd = e.get('cwd', '?')
        ts = e.get('ts', '')
        trigger = e.get('trigger', 'unknown')
        tpath = e.get('transcript_path', '')
        lines.append(f"{i}. session=`{sid}` trigger=`{trigger}` cwd=`{cwd}` ts=`{ts}`")
        lines.append(f"   transcript: `{tpath}`")
        lines.append("")

    lines.extend([
        "After processing all entries, archive the queue:",
        "```bash",
        f"mv {queue_file} {queue_file}.processed-$(date +%s)",
        "```",
    ])

    output = {
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": "\n".join(lines),
        }
    }
    print(json.dumps(output))
    sys.exit(0)


if __name__ == '__main__':
    main()
