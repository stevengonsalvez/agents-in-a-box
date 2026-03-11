#!/usr/bin/env python3
"""
Cost Tracker Hook (Stop)

Appends lightweight session usage metrics to ~/.claude/metrics/costs.jsonl.
Runs as async Stop hook — non-blocking, fire-and-forget.

Profile: minimal,standard,strict (always runs)
"""

import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path


# Token pricing per 1M tokens (USD)
MODEL_PRICING = {
    "haiku": {"input": 0.80, "output": 4.00},
    "sonnet": {"input": 3.00, "output": 15.00},
    "opus": {"input": 15.00, "output": 75.00},
}


def detect_model_tier(model_name: str) -> str:
    """Map model name to pricing tier."""
    normalized = model_name.lower()
    if "haiku" in normalized:
        return "haiku"
    if "opus" in normalized:
        return "opus"
    return "sonnet"  # default


def estimate_cost(model: str, input_tokens: int, output_tokens: int) -> float:
    """Estimate USD cost for given token counts."""
    tier = detect_model_tier(model)
    rates = MODEL_PRICING.get(tier, MODEL_PRICING["sonnet"])
    cost = (input_tokens / 1_000_000) * rates["input"] + (output_tokens / 1_000_000) * rates["output"]
    return round(cost, 6)


def to_int(value) -> int:
    """Safely convert to int."""
    try:
        n = int(value)
        return n if n >= 0 else 0
    except (TypeError, ValueError):
        return 0


def main():
    raw = sys.stdin.read()

    try:
        data = json.loads(raw) if raw.strip() else {}

        # Extract usage — try multiple field names
        usage = data.get("usage") or data.get("token_usage") or {}
        input_tokens = to_int(usage.get("input_tokens") or usage.get("prompt_tokens") or 0)
        output_tokens = to_int(usage.get("output_tokens") or usage.get("completion_tokens") or 0)

        # Detect model
        model = str(data.get("model") or os.environ.get("CLAUDE_MODEL", "unknown"))
        session_id = os.environ.get("CLAUDE_SESSION_ID", "default")

        # Only write if we have token data
        if input_tokens > 0 or output_tokens > 0:
            metrics_dir = Path.home() / ".claude" / "metrics"
            metrics_dir.mkdir(parents=True, exist_ok=True)

            row = {
                "timestamp": datetime.now(timezone.utc).isoformat(),
                "session_id": session_id,
                "model": model,
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
                "estimated_cost_usd": estimate_cost(model, input_tokens, output_tokens),
            }

            costs_file = metrics_dir / "costs.jsonl"
            with open(costs_file, "a") as f:
                f.write(json.dumps(row) + "\n")
    except Exception:
        pass  # Non-blocking — never fail

    # Pass through stdin unchanged
    sys.stdout.write(raw)


if __name__ == "__main__":
    main()
