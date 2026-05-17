"""``reflect metrics`` subgroup — aggregate the JSONL log.

The aggregator lives in :mod:`reflect_kb.metrics_stats` (no click deps);
this file is purely presentation + CLI plumbing.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Optional

import click
from rich.console import Console
from rich.table import Table

from reflect_kb import metrics_stats
from reflect_kb.metrics import METRICS_PATH

console = Console()


def _render_window_table(window: metrics_stats.WindowStats) -> Table:
    table = Table(title=window.label, show_header=True, header_style="bold cyan")
    table.add_column("metric")
    table.add_column("value", justify="right")

    table.add_row("total events", str(window.total_events))
    table.add_row("recall events", str(window.recall_events))
    table.add_row("recall with hits", str(window.recall_with_hits))
    table.add_row("hit rate", f"{window.hit_rate * 100:.1f}%")
    table.add_row(
        "p50 latency (ms)",
        "—" if window.p50_latency_ms is None else f"{window.p50_latency_ms:.1f}",
    )
    table.add_row(
        "p95 latency (ms)",
        "—" if window.p95_latency_ms is None else f"{window.p95_latency_ms:.1f}",
    )
    if window.top_tags:
        for tag, count in window.top_tags:
            table.add_row(f"  tag: {tag}", str(count))
    return table


@click.group(name="metrics")
def metrics_group() -> None:
    """Aggregate the recall-metrics JSONL log."""


@metrics_group.command("stats")
@click.option(
    "--metrics-path",
    type=click.Path(dir_okay=False, path_type=Path),
    default=None,
    help=f"Override metrics file (default: {METRICS_PATH}).",
)
@click.option(
    "--format",
    "fmt",
    type=click.Choice(["table", "json"]),
    default="table",
    show_default=True,
)
@click.option("--window-days", type=int, default=7, show_default=True)
def stats(metrics_path: Optional[Path], fmt: str, window_days: int) -> None:
    """Aggregate metrics: total events, hit rate, top tags, p50/p95 latency."""
    path = metrics_path or METRICS_PATH
    report = metrics_stats.aggregate(path, window_days=window_days)

    if fmt == "json":
        # Use stdout directly so the output is machine-parseable; Rich Console
        # styles colour codes that JSON consumers don't want.
        click.echo(json.dumps(report.to_dict(), indent=2, sort_keys=True))
        return

    console.print(f"[dim]metrics file: {report.metrics_path}[/dim]")
    console.print(_render_window_table(report.last_7d))
    console.print(_render_window_table(report.all_time))
