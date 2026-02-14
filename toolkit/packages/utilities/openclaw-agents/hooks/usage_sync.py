#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#     "python-dotenv",
# ]
# ///
"""
usage_sync.py — Claude Code hook for real-time session usage tracking.

Fires on Stop + SubagentStop events. Parses the session JSONL transcript,
aggregates token usage and cost, identifies the agent, then:
  1. Writes a local backup to ~/.claude/usage/sessions.jsonl
  2. POSTs the data to Convex HTTP endpoint for real-time dashboard display
"""

import argparse
import json
import os
import sys
import urllib.request
import urllib.error
from datetime import datetime
from pathlib import Path

try:
    from dotenv import load_dotenv
    load_dotenv()
except ImportError:
    pass

# ─── Model Pricing (per million tokens, USD) ────────────────────────────────

MODEL_PRICING = {
    "claude-opus-4-6": {
        "input": 15.0,
        "output": 75.0,
        "cache_read": 1.50,
        "cache_write": 18.75,
    },
    "claude-opus-4-5-20250514": {
        "input": 15.0,
        "output": 75.0,
        "cache_read": 1.50,
        "cache_write": 18.75,
    },
    "claude-sonnet-4-5-20250929": {
        "input": 3.0,
        "output": 15.0,
        "cache_read": 0.30,
        "cache_write": 3.75,
    },
    "claude-haiku-4-5-20251001": {
        "input": 0.80,
        "output": 4.0,
        "cache_read": 0.08,
        "cache_write": 1.0,
    },
}

# Fallback for unknown models (use sonnet pricing as middle ground)
DEFAULT_PRICING = MODEL_PRICING["claude-sonnet-4-5-20250929"]

# ─── Agent Identification ────────────────────────────────────────────────────

AGENT_PATH_PATTERNS = [
    ("popashot-agent", "popashot"),
    ("popabot", "main"),
    ("heimdall", "heimdall"),
]

AGENT_SLUG_PATTERNS = [
    ("build-", "popashot"),
    ("research-", "popashot"),
    ("fix-pr-", "popashot"),
    ("e2e-", "popashot"),
    ("heimdall-", "heimdall"),
    ("popa-", "main"),
]


def identify_agent(transcript_path: str, slug: str | None = None) -> str:
    """Map a session to an agent ID based on project path and session slug."""
    # Check slug first (more specific)
    if slug:
        for prefix, agent_id in AGENT_SLUG_PATTERNS:
            if slug.startswith(prefix):
                return agent_id

    # Check project path
    for pattern, agent_id in AGENT_PATH_PATTERNS:
        if pattern in transcript_path:
            return agent_id

    return "unknown"


def extract_project_path(transcript_path: str) -> str:
    """Extract the encoded project path from transcript file path."""
    # Path looks like: ~/.claude/projects/{encoded-path}/{session}.jsonl
    parts = Path(transcript_path).parts
    try:
        projects_idx = parts.index("projects")
        if projects_idx + 1 < len(parts):
            return parts[projects_idx + 1]
    except ValueError:
        pass
    return "unknown"


# ─── JSONL Parsing ───────────────────────────────────────────────────────────

def parse_session_jsonl(transcript_path: str) -> dict | None:
    """Parse a session JSONL file and aggregate usage data."""
    if not os.path.exists(transcript_path):
        return None

    tokens_in = 0
    tokens_out = 0
    cache_read = 0
    cache_write = 0
    assistant_count = 0
    user_count = 0
    tool_use_count = 0
    model = "unknown"
    slug = None
    first_ts = None
    last_ts = None

    try:
        with open(transcript_path, "r") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue

                try:
                    entry = json.loads(line)
                except json.JSONDecodeError:
                    continue

                entry_type = entry.get("type")
                timestamp = entry.get("timestamp")

                # Track timestamps for duration
                if timestamp:
                    if first_ts is None:
                        first_ts = timestamp
                    last_ts = timestamp

                # Track slug (appears in later entries)
                if entry.get("slug") and not slug:
                    slug = entry["slug"]

                if entry_type == "assistant":
                    msg = entry.get("message", {})
                    usage = msg.get("usage", {})

                    if usage:
                        assistant_count += 1
                        tokens_in += usage.get("input_tokens", 0)
                        tokens_out += usage.get("output_tokens", 0)
                        cache_read += usage.get("cache_read_input_tokens", 0)
                        cache_write += usage.get("cache_creation_input_tokens", 0)

                    # Track model (prefer real model names, skip synthetic/unknown)
                    msg_model = msg.get("model")
                    if msg_model and not msg_model.startswith("<") and msg_model != "unknown":
                        model = msg_model

                    # Count tool uses from content blocks
                    content = msg.get("content", [])
                    if isinstance(content, list):
                        for block in content:
                            if isinstance(block, dict) and block.get("type") == "tool_use":
                                tool_use_count += 1

                elif entry_type == "user":
                    user_count += 1

    except Exception:
        return None

    # Skip sessions with no usage data
    if assistant_count == 0:
        return None

    # Calculate duration
    duration_ms = 0
    started_at = 0
    ended_at = 0
    if first_ts and last_ts:
        try:
            start_dt = datetime.fromisoformat(first_ts.replace("Z", "+00:00"))
            end_dt = datetime.fromisoformat(last_ts.replace("Z", "+00:00"))
            started_at = int(start_dt.timestamp() * 1000)
            ended_at = int(end_dt.timestamp() * 1000)
            duration_ms = ended_at - started_at
        except (ValueError, TypeError):
            ended_at = int(datetime.now().timestamp() * 1000)
            started_at = ended_at

    # Calculate cost
    pricing = MODEL_PRICING.get(model, DEFAULT_PRICING)
    cost_breakdown = {
        "input": (tokens_in / 1_000_000) * pricing["input"],
        "output": (tokens_out / 1_000_000) * pricing["output"],
        "cacheRead": (cache_read / 1_000_000) * pricing["cache_read"],
        "cacheWrite": (cache_write / 1_000_000) * pricing["cache_write"],
    }
    cost_total = sum(cost_breakdown.values())

    return {
        "tokensIn": tokens_in,
        "tokensOut": tokens_out,
        "cacheRead": cache_read,
        "cacheWrite": cache_write,
        "costTotal": round(cost_total, 6),
        "costBreakdown": {k: round(v, 6) for k, v in cost_breakdown.items()},
        "messageCount": assistant_count,
        "userMessageCount": user_count,
        "toolUseCount": tool_use_count,
        "model": model,
        "slug": slug,
        "durationMs": duration_ms,
        "startedAt": started_at,
        "endedAt": ended_at,
    }


# ─── Output ──────────────────────────────────────────────────────────────────

def write_local_backup(usage_data: dict) -> None:
    """Append usage record to local backup file."""
    backup_dir = Path.home() / ".claude" / "usage"
    backup_dir.mkdir(parents=True, exist_ok=True)
    backup_file = backup_dir / "sessions.jsonl"

    try:
        with open(backup_file, "a") as f:
            f.write(json.dumps(usage_data) + "\n")
    except Exception:
        pass


def push_to_convex(usage_data: dict) -> bool:
    """POST usage data to Convex HTTP endpoint. Returns True on success."""
    convex_url = os.getenv("CONVEX_URL") or os.getenv("NEXT_PUBLIC_CONVEX_URL")
    if not convex_url:
        # Try reading from mission-control .env.local
        env_paths = [
            Path.home() / "d" / "popashot-agent" / "mission-control" / ".env.local",
            Path.home() / "d" / "popashot-agent" / "mission-control" / ".env",
        ]
        for env_path in env_paths:
            if env_path.exists():
                try:
                    for line in env_path.read_text().splitlines():
                        line = line.strip()
                        if line.startswith("CONVEX_URL=") or line.startswith("NEXT_PUBLIC_CONVEX_URL="):
                            val = line.split("=", 1)[1].strip().strip("'\"")
                            if val:
                                convex_url = val
                                break
                except Exception:
                    pass
            if convex_url:
                break

    if not convex_url:
        return False

    # Convex site URL → HTTP endpoint
    # e.g., https://upbeat-walrus-100.convex.cloud → https://upbeat-walrus-100.convex.site/api/usage
    site_url = convex_url.replace(".convex.cloud", ".convex.site")
    endpoint = f"{site_url}/api/usage"

    try:
        data = json.dumps(usage_data).encode("utf-8")
        req = urllib.request.Request(
            endpoint,
            data=data,
            headers={"Content-Type": "application/json"},
            method="POST",
        )
        resp = urllib.request.urlopen(req, timeout=5)
        return resp.status == 200
    except (urllib.error.URLError, urllib.error.HTTPError, TimeoutError, OSError):
        return False


# ─── Main ────────────────────────────────────────────────────────────────────

def main():
    try:
        parser = argparse.ArgumentParser()
        parser.add_argument("--subagent", action="store_true", help="Mark as subagent session")
        args = parser.parse_args()

        input_data = json.load(sys.stdin)

        session_id = input_data.get("session_id", "")
        transcript_path = input_data.get("transcript_path", "")

        if not transcript_path or not os.path.exists(transcript_path):
            sys.exit(0)

        # Parse the JSONL
        parsed = parse_session_jsonl(transcript_path)
        if not parsed:
            sys.exit(0)

        # Build the full usage record
        project_path = extract_project_path(transcript_path)
        agent_id = identify_agent(transcript_path, parsed.get("slug"))

        usage_data = {
            "sessionId": session_id,
            "agentId": agent_id,
            "projectPath": project_path,
            "model": parsed["model"],
            "tokensIn": parsed["tokensIn"],
            "tokensOut": parsed["tokensOut"],
            "cacheRead": parsed["cacheRead"],
            "cacheWrite": parsed["cacheWrite"],
            "costTotal": parsed["costTotal"],
            "costBreakdown": parsed["costBreakdown"],
            "messageCount": parsed["messageCount"],
            "userMessageCount": parsed["userMessageCount"],
            "toolUseCount": parsed["toolUseCount"],
            "durationMs": parsed["durationMs"],
            "startedAt": parsed["startedAt"],
            "endedAt": parsed["endedAt"],
            "isSubagent": args.subagent,
            "slug": parsed.get("slug"),
        }

        # Write local backup (always)
        write_local_backup(usage_data)

        # Push to Convex (best-effort)
        push_to_convex(usage_data)

        sys.exit(0)

    except json.JSONDecodeError:
        sys.exit(0)
    except Exception:
        sys.exit(0)


if __name__ == "__main__":
    main()
