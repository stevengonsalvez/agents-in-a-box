#!/usr/bin/env python3
"""Generate the deterministic tripwire_keys fixture for the burndown
interactive-keys tripwire (plan §Phase 7).

Layout produced under
``crates/ainb-core/tests/fixtures/tripwire_keys/``:

    FIXTURE_NOW.txt                                  # pinned "now"
    claude/projects/-Users-test-proj-{alpha,beta,gamma}/*.jsonl
    codex/sessions/YYYY/MM/DD/rollout-*.jsonl

Re-run **only** when the fixture format changes — the JSONL output is
committed alongside this generator so CI does not depend on a Python
runtime. Bumping ``SCHEMA_VERSION`` documents intentional drift.

Design (mirrors the documented constants asserted by
``tests/parses_tripwire_keys_fixture.rs``):

* 3 projects (alpha/beta/gamma) with scale factors 1/2/3 so per-project
  totals are byte-diffable.
* Two providers per project: claude (20 sessions) + codex (10 sessions).
* Six time buckets relative to FIXTURE_NOW so the period chip switch
  produces monotonically growing totals: today < 7d < 30d < 90d < ytd
  < all.
* Branch alternates main/feature by session index inside each bucket.
* Only ``input_tokens`` are non-zero per session — keeps the totals
  trivially equal to ``sessions_in_period * tokens_per_session`` so the
  smoke test can assert constants without re-implementing parser math.
* All timestamps are UTC; the smoke test buckets on UTC days so it
  stays timezone-independent.
"""

from __future__ import annotations

import hashlib
import json
import shutil
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path

SCHEMA_VERSION = 1
FIXTURE_NOW_RFC3339 = "2026-05-11T00:00:00Z"

# Per-bucket session counts (per project, per provider). Tuned so the
# cumulative totals satisfy today < 7d < 30d < 90d < ytd < all.
CLAUDE_BUCKETS = {
    "B0_today":     2,   # today
    "B1_week":      4,   # last 7 days, not today  (-6..-1 days)
    "B2_month":     6,   # last 30 days, not week  (-29..-7 days)
    "B3_quarter":   4,   # last 90 days, not month (-89..-30 days)
    "B4_ytd":       2,   # this year, not in 90d   (-... ..-90 days)
    "B5_last_year": 2,   # before Jan 1 of FIXTURE_NOW's year
}
CODEX_BUCKETS = {
    "B0_today":     1,
    "B1_week":      2,
    "B2_month":     3,
    "B3_quarter":   2,
    "B4_ytd":       1,
    "B5_last_year": 1,
}
# Bucket day ranges expressed as (days_before_today_start,
# days_before_today_end) inclusive — relative to FIXTURE_NOW's UTC date.
BUCKET_DAY_RANGES = {
    "B0_today":     (0, 0),
    "B1_week":      (1, 6),
    "B2_month":     (7, 29),
    "B3_quarter":   (30, 89),
    "B4_ytd":       (90, 130),    # FIXTURE_NOW=May 11 → YTD spans 131 days back to Jan 1
    "B5_last_year": (162, 192),   # late Oct/Nov/Dec 2025 — well before Jan 1 2026
}
PROJECT_SCALES = {"alpha": 1, "beta": 2, "gamma": 3}
CLAUDE_BASE_TOKENS = 100
CODEX_BASE_TOKENS = 200
CLAUDE_MODEL = "claude-sonnet-4-5"
CODEX_MODEL = "gpt-5"
BRANCHES = ("main", "feature")


@dataclass
class Plan:
    project: str
    scale: int
    provider: str
    bucket: str
    idx: int          # 0-based index inside (project, provider, bucket)
    ts_utc: datetime  # exact line timestamp
    branch: str
    tokens: int
    session_id: str


def fixture_root(repo_root: Path) -> Path:
    return repo_root / "crates" / "ainb-core" / "tests" / "fixtures" / "tripwire_keys"


def now_dt() -> datetime:
    return datetime.fromisoformat(FIXTURE_NOW_RFC3339.replace("Z", "+00:00"))


def stable_uuid(seed: str) -> str:
    """Format a 32-hex blake-style digest as a UUID-ish string. Stable
    across runs since we derive from a known seed. Not RFC 4122 — the
    parsers don't validate UUIDs, they just want a filename token."""
    digest = hashlib.sha256(seed.encode("utf-8")).hexdigest()
    h = digest[:32]
    return f"{h[0:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:32]}"


def build_plans() -> list[Plan]:
    now = now_dt()
    today = now.date()
    plans: list[Plan] = []

    for project, scale in PROJECT_SCALES.items():
        for provider, counts in (("claude", CLAUDE_BUCKETS), ("codex", CODEX_BUCKETS)):
            base = CLAUDE_BASE_TOKENS if provider == "claude" else CODEX_BASE_TOKENS
            tokens = base * scale
            for bucket, count in counts.items():
                day_lo, day_hi = BUCKET_DAY_RANGES[bucket]
                span = day_hi - day_lo
                for idx in range(count):
                    # Spread idx evenly across the bucket's day range so
                    # different sessions land on different days when the
                    # bucket spans more than one day.
                    if span == 0:
                        days_back = day_lo
                    else:
                        days_back = day_lo + (idx * span) // max(count - 1, 1)
                    day = today - timedelta(days=days_back)
                    # Stagger times by idx so byte-stable rollout
                    # filenames don't collide on the same day.
                    hour = 10 + (idx % 6)
                    minute = 5 * (idx % 12)
                    ts = datetime(day.year, day.month, day.day, hour, minute, 0, tzinfo=timezone.utc)
                    branch = BRANCHES[idx % 2]
                    seed = f"{provider}:{project}:{bucket}:{idx}"
                    session_id = stable_uuid(seed)
                    plans.append(Plan(
                        project=project,
                        scale=scale,
                        provider=provider,
                        bucket=bucket,
                        idx=idx,
                        ts_utc=ts,
                        branch=branch,
                        tokens=tokens,
                        session_id=session_id,
                    ))
    return plans


def claude_jsonl_for(plan: Plan) -> str:
    """One user turn + one assistant turn carrying the usage record."""
    user_ts = plan.ts_utc.strftime("%Y-%m-%dT%H:%M:%SZ")
    assistant_ts = (plan.ts_utc + timedelta(seconds=30)).strftime("%Y-%m-%dT%H:%M:%SZ")
    cwd = f"/Users/test/proj-{plan.project}"
    user = {
        "type": "user",
        "timestamp": user_ts,
        "sessionId": plan.session_id,
        "cwd": cwd,
        "gitBranch": plan.branch,
        "message": {"content": [{"type": "text", "text": f"tripwire seed {plan.bucket} {plan.idx}"}]},
    }
    assistant = {
        "type": "assistant",
        "timestamp": assistant_ts,
        "sessionId": plan.session_id,
        "cwd": cwd,
        "gitBranch": plan.branch,
        "message": {
            "model": CLAUDE_MODEL,
            "content": [{"type": "text", "text": "ok"}],
            "usage": {
                "input_tokens": plan.tokens,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "output_tokens": 0,
            },
        },
    }
    return json.dumps(user, separators=(",", ":")) + "\n" + json.dumps(assistant, separators=(",", ":")) + "\n"


def codex_jsonl_for(plan: Plan) -> str:
    meta_ts = plan.ts_utc.strftime("%Y-%m-%dT%H:%M:%SZ")
    token_ts = (plan.ts_utc + timedelta(seconds=30)).strftime("%Y-%m-%dT%H:%M:%SZ")
    cwd = f"/Users/test/proj-{plan.project}"
    meta = {
        "type": "session_meta",
        "timestamp": meta_ts,
        "payload": {
            "session_id": plan.session_id,
            "originator": "codex",
            "model": CODEX_MODEL,
            "cwd": cwd,
        },
    }
    token = {
        "type": "event_msg",
        "timestamp": token_ts,
        "payload": {
            "type": "token_count",
            "info": {
                "last_token_usage": {
                    "input_tokens": plan.tokens,
                    "cached_input_tokens": 0,
                    "output_tokens": 0,
                    "reasoning_output_tokens": 0,
                },
                "total_token_usage": {
                    "input_tokens": plan.tokens,
                    "cached_input_tokens": 0,
                    "output_tokens": 0,
                    "reasoning_output_tokens": 0,
                    "total_tokens": plan.tokens,
                },
            },
        },
    }
    return json.dumps(meta, separators=(",", ":")) + "\n" + json.dumps(token, separators=(",", ":")) + "\n"


def claude_path(root: Path, plan: Plan) -> Path:
    project_dir = root / "claude" / "projects" / f"-Users-test-proj-{plan.project}"
    date_prefix = plan.ts_utc.strftime("%Y-%m-%d")
    return project_dir / f"{date_prefix}-{plan.session_id}.jsonl"


def codex_path(root: Path, plan: Plan) -> Path:
    day_dir = root / "codex" / "sessions" / plan.ts_utc.strftime("%Y") / plan.ts_utc.strftime("%m") / plan.ts_utc.strftime("%d")
    stamp = plan.ts_utc.strftime("%Y-%m-%dT%H-%M-%S")
    return day_dir / f"rollout-{stamp}-{plan.session_id}.jsonl"


def write_fixture(repo_root: Path) -> None:
    root = fixture_root(repo_root)
    if root.exists():
        shutil.rmtree(root)
    root.mkdir(parents=True)

    (root / "FIXTURE_NOW.txt").write_text(FIXTURE_NOW_RFC3339 + "\n")
    (root / "SCHEMA_VERSION.txt").write_text(f"{SCHEMA_VERSION}\n")

    plans = build_plans()
    for plan in plans:
        if plan.provider == "claude":
            path = claude_path(root, plan)
            payload = claude_jsonl_for(plan)
        else:
            path = codex_path(root, plan)
            payload = codex_jsonl_for(plan)
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(payload)

    readme = root / "README.md"
    readme.write_text(README_TEMPLATE.lstrip())


README_TEMPLATE = f"""
# tripwire_keys fixture

Deterministic burndown fixture for the interactive-keys tripwire
(plan §Phase 7). Generated by `scripts/gen_tripwire_keys_fixture.py`.
Re-run **only** when the schema needs to change — the JSONL output is
committed alongside the generator so CI does not need Python.

- `FIXTURE_NOW.txt` is the pinned "now" (`AINB_NOW` env value used by
  tests). The burndown plugin reads `AINB_NOW` in
  `data/usage.rs::date_range_for_period` so period chips bucket
  deterministically against this anchor.
- `SCHEMA_VERSION.txt` bumps when the generator's output shape drifts.

Layout:

```
claude/projects/-Users-test-proj-{{alpha,beta,gamma}}/*.jsonl
codex/sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl
```

Per-project scale: alpha=1, beta=2, gamma=3 — per-project token totals
are byte-diffable. Per-project input-token totals (claude+codex) by
period satisfy `today < 7d < 30d < 90d < ytd < all`.

Asserted values live in
`crates/ainb-core/tests/parses_tripwire_keys_fixture.rs` as constants.
SCHEMA_VERSION = {SCHEMA_VERSION}.
"""


def main() -> None:
    here = Path(__file__).resolve()
    # scripts/ lives at ainb-tui/scripts/. Repo root is the ainb-tui dir.
    repo_root = here.parents[1]
    write_fixture(repo_root)
    plans = build_plans()
    print(f"wrote {len(plans)} sessions to {fixture_root(repo_root)}")


if __name__ == "__main__":
    main()
