#!/usr/bin/env python3
"""``reflect issues`` — distill recent transcripts into deduped GitHub issues.

A NEW MODE inside the existing ``/reflect`` ecosystem. It reuses the reflect
queue (``~/.reflect/pending_reflections.jsonl``) and state dir, ports
agent-deck's distill + privacy-sanitize + gh-dedupe logic, and files GitHub
issues via ``gh issue create`` — with a mandatory ``--dry-run`` so a human can
see the exact bodies before anything leaves the machine.

Subcommands:
    reflect issues run [--dry-run] [--repo OWNER/NAME] [--limit N]
                       [--model M] [--map K=V ...]
    reflect issues ledger        # show what's already been filed
"""

from __future__ import annotations

import json
from typing import Optional

import click
from rich.console import Console
from rich.panel import Panel
from rich.table import Table

from reflect_kb.issues import dedupe as dedupe_mod
from reflect_kb.issues import manifest as manifest_mod
from reflect_kb.issues.pipeline import run_issues

console = Console(stderr=True)


def _parse_maps(pairs: tuple[str, ...]) -> dict[str, str]:
    maps: dict[str, str] = {}
    for pair in pairs:
        if "=" not in pair:
            raise click.BadParameter(f"--map expects KEY=VALUE, got: {pair!r}")
        key, _, value = pair.partition("=")
        key = key.strip()
        if key:
            maps[key] = value.strip()
    return maps


@click.group("issues")
def issues_group():
    """Turn recent session transcripts into privacy-sanitized GitHub issues.

    Pipeline: queue → distill (~30x) → analyze (LLM) → sanitize → dedupe → file.
    Always preview with ``--dry-run`` first; it prints the exact issue bodies
    that WOULD be filed without calling ``gh``.
    """


@issues_group.command("run")
@click.option(
    "--dry-run",
    is_flag=True,
    default=False,
    help="Print issue bodies that WOULD be filed; never call gh.",
)
@click.option("--repo", default=None, help="Target repo OWNER/NAME (defaults to cwd's repo).")
@click.option(
    "--limit",
    default=20,
    show_default=True,
    help="Max recent transcripts to pull from the reflect queue.",
)
@click.option(
    "--model", default="sonnet", show_default=True, help="Model passed to the analyzer (claude -p)."
)
@click.option(
    "--map",
    "maps",
    multiple=True,
    metavar="KEY=VALUE",
    help="Extra sanitizer substitution (repeatable), e.g. --map AcmeCorp=<company>.",
)
@click.option(
    "--format", "-f", "output_format", default="rich", type=click.Choice(["rich", "json"])
)
def issues_run(
    dry_run: bool,
    repo: Optional[str],
    limit: int,
    model: str,
    maps: tuple[str, ...],
    output_format: str,
):
    """Distill recent transcripts and file (or preview) GitHub issues.

    Examples:
        reflect issues run --dry-run
        reflect issues run --repo myorg/myrepo --limit 10
        reflect issues run --dry-run --map AcmeCorp=<company>
    """
    parsed_maps = _parse_maps(maps)
    result = run_issues(
        repo=repo,
        limit=limit,
        dry_run=dry_run,
        maps=parsed_maps or None,
        model=model,
    )

    if output_format == "json":
        payload = {
            "dry_run": result.dry_run,
            "transcripts_seen": result.transcripts_seen,
            "transcripts_distilled": result.transcripts_distilled,
            "analyze_reason": result.analyze_reason,
            "candidates": result.candidates,
            "filed": [
                {
                    "title": f.title,
                    "fingerprint": f.fingerprint,
                    "gh_issue_number": f.gh_issue_number,
                    "gh_url": f.gh_url,
                }
                for f in result.filed
            ],
            "skipped": [
                {"title": d.candidate.title, "reason": d.reason, "existing": d.existing_ref}
                for d in result.skipped
            ],
            "previews": result.previews,
            "notes": result.notes,
        }
        click.echo(json.dumps(payload, indent=2))
        return

    # Rich output.
    console.print(
        f"[dim]transcripts: {result.transcripts_seen} seen, "
        f"{result.transcripts_distilled} distilled · "
        f"candidates: {result.candidates} · "
        f"analyze: {result.analyze_reason or 'n/a'}[/dim]"
    )

    if dry_run:
        if result.previews:
            console.print(
                f"\n[bold green]{len(result.previews)} issue(s) WOULD be filed:[/bold green]\n"
            )
            for preview in result.previews:
                console.print(Panel(preview, border_style="green"))
        else:
            console.print("[yellow]No new issues would be filed.[/yellow]")
    else:
        if result.filed:
            console.print(f"\n[bold green]Filed {result.filed_count} issue(s):[/bold green]")
            for f in result.filed:
                ref = f.gh_url or (f"#{f.gh_issue_number}" if f.gh_issue_number else "(filed)")
                console.print(f"  [green]{ref}[/green]  {f.title}")
        else:
            console.print("[yellow]No new issues filed.[/yellow]")

    if result.skipped:
        console.print(f"\n[dim]Skipped {len(result.skipped)} duplicate(s):[/dim]")
        for d in result.skipped:
            console.print(f"  [dim]- {d.candidate.title}  ({d.reason})[/dim]")

    for note in result.notes:
        console.print(f"[dim]· {note}[/dim]")


@issues_group.command("ledger")
@click.option(
    "--format", "-f", "output_format", default="rich", type=click.Choice(["rich", "json"])
)
def issues_ledger(output_format: str):
    """Show issues already filed by ``reflect issues`` (the idempotency ledger)."""
    ledger = dedupe_mod.load_ledger()
    filed = ledger.get("filed_issues", [])

    if output_format == "json":
        click.echo(json.dumps(ledger, indent=2))
        return

    if not filed:
        console.print(
            f"[yellow]No issues filed yet.[/yellow] [dim](ledger: {dedupe_mod.ledger_path()})[/dim]"
        )
        return

    table = Table(title="Filed Issues")
    table.add_column("#", style="cyan")
    table.add_column("Title", style="white")
    table.add_column("Filed", style="dim")
    table.add_column("URL", style="green")
    for entry in filed:
        table.add_row(
            str(entry.get("gh_issue_number") or "-"),
            str(entry.get("title", "")),
            str(entry.get("filed_at", ""))[:19],
            str(entry.get("gh_url") or ""),
        )
    console.print(table)


@issues_group.command("queue")
@click.option("--limit", default=20, show_default=True)
def issues_queue(limit: int):
    """List the recent transcripts the ``run`` command would analyze."""
    refs = manifest_mod.gather_transcripts(limit=limit)
    if not refs:
        console.print(
            f"[yellow]Reflect queue is empty.[/yellow] [dim]({manifest_mod.queue_file()})[/dim]"
        )
        return
    table = Table(title="Reflect Queue (transcripts to analyze)")
    table.add_column("Session", style="cyan")
    table.add_column("Trigger", style="dim")
    table.add_column("Transcript", style="white")
    for ref in refs:
        table.add_row(ref.session_id[:8], ref.trigger, str(ref.transcript_path))
    console.print(table)
