"""Aggregate the JSONL metrics file written by :mod:`reflect_kb.metrics`.

Pure stdlib — no pandas, no numpy. The metrics file is bounded to ~10 MB by
the writer's rotation, so a single linear scan is fine for any realistic
fleet size.

Returns a stable :class:`StatsReport` shape that the CLI renders as table or
JSON.
"""

from __future__ import annotations

import json
from collections import Counter
from dataclasses import asdict, dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Iterable, Optional


@dataclass
class WindowStats:
    """One time-window slice of the report (e.g. all-time, last 7 days)."""

    label: str
    total_events: int
    recall_events: int
    recall_with_hits: int
    hit_rate: float            # recall_with_hits / max(recall_events, 1)
    p50_latency_ms: Optional[float]
    p95_latency_ms: Optional[float]
    top_tags: list[tuple[str, int]] = field(default_factory=list)


@dataclass
class StatsReport:
    """Top-level aggregator output."""

    metrics_path: str
    generated_at: str
    all_time: WindowStats
    last_7d: WindowStats

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)


def _percentile(sorted_values: list[float], pct: float) -> Optional[float]:
    """Linear-interpolation percentile. Returns None for empty input."""
    if not sorted_values:
        return None
    if len(sorted_values) == 1:
        return float(sorted_values[0])
    k = (len(sorted_values) - 1) * (pct / 100.0)
    f = int(k)
    c = min(f + 1, len(sorted_values) - 1)
    if f == c:
        return float(sorted_values[f])
    return float(sorted_values[f] + (sorted_values[c] - sorted_values[f]) * (k - f))


def _parse_ts(raw: Any) -> Optional[datetime]:
    if not isinstance(raw, str):
        return None
    try:
        # ``write_metric`` always emits a tz-aware ISO string.
        return datetime.fromisoformat(raw)
    except ValueError:
        return None


def _iter_records(path: Path) -> Iterable[dict[str, Any]]:
    """Yield each parsed JSON object. Skip blank lines and broken records.

    The writer is best-effort, so partial records are possible if a process
    was killed mid-write. We swallow rather than fail.
    """
    if not path.exists():
        return
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                obj = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(obj, dict):
                yield obj


def _bucket(records: list[dict[str, Any]], label: str) -> WindowStats:
    """Aggregate one bucket of records into a :class:`WindowStats`."""
    recall_events = 0
    recall_with_hits = 0
    latencies: list[float] = []
    tag_counts: Counter[str] = Counter()

    for r in records:
        op = r.get("op")
        if op == "recall":
            recall_events += 1
            hits = r.get("hits")
            if isinstance(hits, int) and hits > 0:
                recall_with_hits += 1
            lat = r.get("latency_ms")
            if isinstance(lat, (int, float)):
                latencies.append(float(lat))
            for tag in r.get("tags") or []:
                if isinstance(tag, str) and tag:
                    tag_counts[tag] += 1

    latencies.sort()
    return WindowStats(
        label=label,
        total_events=len(records),
        recall_events=recall_events,
        recall_with_hits=recall_with_hits,
        hit_rate=(recall_with_hits / recall_events) if recall_events else 0.0,
        p50_latency_ms=_percentile(latencies, 50),
        p95_latency_ms=_percentile(latencies, 95),
        top_tags=tag_counts.most_common(10),
    )


def aggregate(
    metrics_path: Path,
    *,
    now: Optional[datetime] = None,
    window_days: int = 7,
) -> StatsReport:
    """Single linear pass over the metrics file. Returns a :class:`StatsReport`.

    Parameters
    ----------
    metrics_path:
        Path to the JSONL file. Missing file → empty stats (no error), so the
        CLI can be run on a fresh box without surfacing a stack trace.
    now:
        Override clock for tests. Defaults to UTC now.
    window_days:
        Size of the rolling window (default 7). Anything older than
        ``now - window_days`` only contributes to the all-time bucket.
    """
    current = now or datetime.now(timezone.utc)
    threshold = current - timedelta(days=window_days)

    all_records: list[dict[str, Any]] = []
    recent_records: list[dict[str, Any]] = []

    for rec in _iter_records(metrics_path):
        all_records.append(rec)
        ts = _parse_ts(rec.get("ts"))
        if ts is not None and ts >= threshold:
            recent_records.append(rec)

    return StatsReport(
        metrics_path=str(metrics_path),
        generated_at=current.isoformat(),
        all_time=_bucket(all_records, label="all-time"),
        last_7d=_bucket(recent_records, label=f"last-{window_days}d"),
    )
