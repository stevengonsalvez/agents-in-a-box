#!/usr/bin/env python3
"""
Global Learnings CLI - Knowledge base with GraphRAG search.

Provides semantic search over the global learnings repository
using nano-graphrag for vector + graph-based retrieval.
"""

import glob
import json
import os
import hashlib
import shutil
import subprocess
import sys
import time
from pathlib import Path
from datetime import datetime
from typing import Optional, List, Dict, Any

import click
import yaml
from rich.console import Console
from rich.table import Table
from rich.panel import Panel

from reflect_kb.metrics import write_metric
from reflect_kb import errors as _err

console = Console(stderr=True)

DEFAULT_REPO_PATH = Path.home() / ".claude" / "global-learnings"
DOCUMENTS_DIR = "documents"
CACHE_DIR = "nano_graphrag_cache"


def get_repo_path() -> Path:
    env_path = os.environ.get("GLOBAL_LEARNINGS_PATH")
    if env_path:
        return Path(env_path)
    return DEFAULT_REPO_PATH


def ensure_repo_exists():
    repo = get_repo_path()
    (repo / DOCUMENTS_DIR).mkdir(parents=True, exist_ok=True)
    (repo / CACHE_DIR).mkdir(parents=True, exist_ok=True)


def parse_frontmatter(content: str) -> tuple[Dict[str, Any], str]:
    if not content.startswith("---"):
        return {}, content

    parts = content.split("---", 2)
    if len(parts) < 3:
        return {}, content

    try:
        frontmatter = yaml.safe_load(parts[1])
        body = parts[2].strip()
        return frontmatter or {}, body
    except yaml.YAMLError:
        return {}, content


def generate_document_id(title: str, body: str = "") -> str:
    """Stable doc_id = slug(title) + sha256(title + body)[:6].

    Previously hashed title-only, which produced identical ids for any two
    docs sharing a slug-able title (capitalisation / punctuation collapses
    them). Under non-interactive subprocess that triggered click.confirm,
    which silently aborted — entire ingests dropped files. Including body
    in the hash makes collisions content-aware: same title + same body =>
    same id (idempotent re-ingest); same title + different body => distinct
    ids.
    """
    slug = title.lower()
    slug = "".join(c if c.isalnum() or c == " " else "" for c in slug)
    slug = "-".join(slug.split())[:50]
    hash_input = (title + "\n" + body).encode("utf-8", errors="replace")
    hash_suffix = hashlib.sha256(hash_input).hexdigest()[:6]
    return f"{slug}-{hash_suffix}"


def get_all_documents() -> List[Dict[str, Any]]:
    repo = get_repo_path()
    docs_dir = repo / DOCUMENTS_DIR
    documents = []

    for doc_path in sorted(docs_dir.glob("*.md")):
        try:
            content = doc_path.read_text()
            frontmatter, body = parse_frontmatter(content)
            if frontmatter:
                frontmatter["_path"] = str(doc_path)
                frontmatter["_body"] = body
                frontmatter["_full_content"] = content
                documents.append(frontmatter)
        except Exception as e:
            console.print(f"[yellow]Warning: Could not parse {doc_path}: {e}[/yellow]")

    return documents


def _get_graph_engine():
    """Create a LearningsGraphEngine instance."""
    from reflect_kb.cli.graph_engine import LearningsGraphEngine, GraphEngineError

    repo = get_repo_path()
    cache_dir = repo / CACHE_DIR
    return LearningsGraphEngine(cache_dir)


@click.group()
@click.version_option(version="0.1.1")
def cli():
    """Global Learnings CLI - Knowledge base with GraphRAG search."""
    ensure_repo_exists()


@cli.command()
@click.argument("query")
@click.option(
    "--mode", "-m", default="naive",
    type=click.Choice(["naive", "local", "global"]),
    help="Search mode: naive (vector), local (graph neighborhood), global (communities)",
)
@click.option("--tags", "-t", help="Filter by tags (comma-separated, appended to query)")
@click.option("--category", "-c", help="Filter by category (appended to query)")
@click.option("--limit", "-l", default=10, help="Maximum results (default: 10)")
@click.option(
    "--format", "-f", "output_format", default="rich",
    type=click.Choice(["rich", "json", "simple"]),
)
def search(query: str, mode: str, tags: Optional[str], category: Optional[str],
           limit: int, output_format: str):
    """Search learnings using GraphRAG.

    Modes:
      naive  - Vector similarity only (fast, good for exact symptom matching)
      local  - Entity neighborhood search (finds related concepts via graph)
      global - Community-based search (broad patterns across all learnings)

    Examples:
        learnings search "tokio runtime panic"
        learnings search "async timeout" --mode local
        learnings search "n+1 query" --tags rust,performance
    """
    # Build enriched query with filters
    search_query = query
    if tags:
        search_query += f" tags: {tags}"
    if category:
        search_query += f" category: {category}"

    start = time.monotonic()
    try:
        engine = _get_graph_engine()
        context = engine.search(search_query, mode=mode, only_context=True)
    except Exception as e:
        write_metric(
            "search",
            query=query,
            mode=mode,
            error=str(e),
            hits=0,
            latency_ms=int((time.monotonic() - start) * 1000),
        )
        if output_format == "json":
            click.echo(json.dumps({
                "query": query, "mode": mode, "error": str(e), "results": []
            }))
        else:
            console.print(f"[red]Search error: {e}[/red]")
            console.print("[dim]Try running 'learnings reindex' to rebuild the graph.[/dim]")
        return

    if not context or context.strip() == "":
        if output_format == "json":
            click.echo(json.dumps({
                "query": query, "mode": mode, "results": [],
                "message": "No results found",
            }))
        else:
            console.print("[yellow]No relevant results found.[/yellow]")
        return

    if output_format == "json":
        click.echo(json.dumps({
            "query": query,
            "mode": mode,
            "context": context,
        }, indent=2, default=str))

    elif output_format == "simple":
        click.echo(context)

    else:
        console.print(f"\n[bold green]Results for:[/bold green] {query}")
        console.print(f"[dim]Mode: {mode}[/dim]\n")
        console.print(Panel(
            context,
            title="[bold]GraphRAG Context[/bold]",
            border_style="green",
        ))

    write_metric(
        "search",
        query=query,
        mode=mode,
        hits=len(context) if context else 0,
        latency_ms=int((time.monotonic() - start) * 1000),
    )


@cli.command("rerank")
@click.argument("query")
@click.option(
    "--batch-size", default=20, show_default=True,
    help="Cross-encoder prediction batch size",
)
@click.option(
    "--model", "model_name", default=None,
    help="Override the cross-encoder model (default: ms-marco-MiniLM-L-6-v2)",
)
def rerank(query: str, batch_size: int, model_name: Optional[str]):
    """Score (query, candidate) pairs with a local cross-encoder (R2).

    Reads JSON from stdin:  {"candidates": [{"id": "...", "text": "..."}]}
    Writes JSON to stdout:  {"available": true, "model": "...",
                             "scores": {"<id>": <raw_logit>, ...}}

    The model auto-downloads on first use and is cached under
    ~/.reflect/models/ thereafter. On the slim build (no
    sentence-transformers) or any scoring failure this emits
    {"available": false, ...} and exits 0 — callers degrade silently.
    """
    from reflect_kb.recall.cross_encoder import (
        cross_encoder_available,
        get_reranker,
    )

    start = time.monotonic()
    raw = sys.stdin.read()
    try:
        payload = json.loads(raw) if raw.strip() else None
    except json.JSONDecodeError:
        payload = None
    candidates = payload.get("candidates") if isinstance(payload, dict) else None
    if not isinstance(candidates, list):
        click.echo(json.dumps({"available": False, "error": "invalid payload"}))
        return

    if not cross_encoder_available():
        click.echo(json.dumps({
            "available": False,
            "error": "sentence-transformers not installed (slim build)",
        }))
        return

    ids: List[str] = []
    texts: List[str] = []
    for cand in candidates:
        if isinstance(cand, dict) and "id" in cand and isinstance(cand.get("text"), str):
            ids.append(str(cand["id"]))
            texts.append(cand["text"])

    try:
        reranker = get_reranker(model_name=model_name, batch_size=batch_size)
        scores = reranker.score(query, texts)
    except Exception as e:  # model download/load/predict failure — degrade
        write_metric(
            "rerank",
            query=query,
            candidates=len(ids),
            error=str(e),
            latency_ms=int((time.monotonic() - start) * 1000),
        )
        click.echo(json.dumps({"available": False, "error": str(e)}))
        return

    click.echo(json.dumps({
        "available": True,
        "model": reranker.model_name,
        "scores": dict(zip(ids, scores)),
    }))
    write_metric(
        "rerank",
        query=query,
        candidates=len(ids),
        latency_ms=int((time.monotonic() - start) * 1000),
    )


@cli.command()
@click.argument("file_path", type=click.Path(exists=True))
@click.option(
    "--entities", "-e", type=click.Path(exists=True),
    help="Path to .entities.yaml sidecar with pre-extracted entities",
)
@click.option(
    "--force", "-f", is_flag=True, default=False,
    help="Overwrite an existing document with the same generated ID without prompting.",
)
def add(file_path: str, entities: Optional[str], force: bool):
    """Add a learning document to the knowledge base.

    The document should have YAML frontmatter with at least:
    title, category, key_insight

    Examples:
        reflect add ./my-solution.md
        reflect add ./my-solution.md --entities ./my-solution.entities.yaml
        reflect add --force ./my-solution.md   # non-interactive overwrite
    """
    source = Path(file_path)
    content = source.read_text()

    frontmatter, body = parse_frontmatter(content)

    if not frontmatter:
        console.print("[red]Error: Document must have YAML frontmatter.[/red]")
        return

    required = ["title", "category", "key_insight"]
    missing = [f for f in required if f not in frontmatter]
    if missing:
        console.print(f"[red]Error: Missing required fields: {', '.join(missing)}[/red]")
        return

    # Generate document ID (slug + sha256(title+body)[:6]) and copy to repo.
    doc_id = generate_document_id(frontmatter["title"], body)
    repo = get_repo_path()
    dest = repo / DOCUMENTS_DIR / f"{doc_id}.md"

    if dest.exists():
        # If --force is set, overwrite silently. Else require a TTY for the
        # confirm prompt — click.confirm under a non-TTY pipe silently aborts,
        # which used to make ingest pipelines drop files invisibly. Now we
        # fail loudly with an instruction to retry with --force.
        if force:
            pass  # overwrite below
        elif not sys.stdin.isatty():
            console.print(
                f"[red]Error: document {dest.name} already exists and stdin is not a TTY.[/red]\n"
                f"[red]Re-run with --force to overwrite, or update the source title/body so the "
                f"generated id differs.[/red]"
            )
            sys.exit(2)
        else:
            if not click.confirm(f"Document {dest.name} exists. Overwrite?"):
                return

    shutil.copy(source, dest)

    # Load or auto-generate entity sidecar
    entities_formatted = None
    entity_count = 0
    rel_count = 0

    from reflect_kb.cli.entity_store import DocumentEntities, find_sidecar, auto_extract_entities, write_sidecar

    if entities:
        # Explicit sidecar provided — use it as-is
        entities_path = Path(entities)
        doc_entities = DocumentEntities.from_yaml_file(entities_path)
        entities_formatted = doc_entities.to_graphrag_format()
        entity_count = doc_entities.entity_count
        rel_count = doc_entities.relationship_count

        # Save sidecar alongside document
        sidecar_dest = dest.with_suffix(".entities.yaml")
        shutil.copy(entities_path, sidecar_dest)
    else:
        # Auto-generate entities from document content (heuristic, no LLM)
        try:
            doc_entities = auto_extract_entities(content, frontmatter)
            if doc_entities.entity_count > 0:
                entities_formatted = doc_entities.to_graphrag_format()
                entity_count = doc_entities.entity_count
                rel_count = doc_entities.relationship_count

                write_sidecar(dest, doc_entities)
                console.print(f"[dim]Auto-generated sidecar: {entity_count} entities, {rel_count} relationships[/dim]")
            else:
                console.print("[dim]No entities extracted (document too short or generic)[/dim]")
        except Exception as e:
            console.print(f"[yellow]Warning: Auto-extraction failed: {e}[/yellow]")

    # Insert into graph
    try:
        engine = _get_graph_engine()
        with console.status("[bold green]Indexing document..."):
            engine.insert_document(content, entities_formatted=entities_formatted)
        console.print(f"[green]Indexed into graph[/green]")
    except Exception as e:
        console.print(f"[yellow]Warning: Graph indexing failed: {e}[/yellow]")
        console.print("[dim]Document saved. Run 'learnings reindex' to retry.[/dim]")

    # Keep QMD in sync (if installed). `qmd embed` only embeds files QMD
    # already tracks, so we MUST run `qmd update` first to rescan the
    # collection for the newly-added file — otherwise new docs are visible
    # to GraphRAG but silently missing from QMD. Graceful if qmd absent.
    #
    # Both subprocesses are synchronous and can take ~10s-2min on large
    # KBs. Echo progress so a user running `learnings add` isn't staring
    # at a silent terminal.
    if shutil.which("qmd"):
        try:
            import subprocess

            console.print("[dim]QMD: rescanning collection…[/dim]")
            subprocess.run(["qmd", "update"], capture_output=True, timeout=30)
            console.print("[dim]QMD: embedding new docs (up to 2 min on large KBs)…[/dim]")
            subprocess.run(["qmd", "embed"], capture_output=True, timeout=120)
            console.print("[green]QMD index + embeddings updated[/green]")
        except Exception as e:
            console.print(f"[yellow]Warning: QMD sync failed: {e}[/yellow]")

    console.print(f"[green]Added:[/green] {dest}")
    console.print(f"[dim]Title: {frontmatter['title']}[/dim]")
    console.print(f"[dim]Category: {frontmatter['category']}[/dim]")
    if entity_count:
        console.print(f"[dim]Entities: {entity_count}, Relationships: {rel_count}[/dim]")


@cli.command()
@click.option("--force", is_flag=True, help="Clear cache and rebuild from scratch")
def reindex(force: bool):
    """Rebuild the GraphRAG index from all documents.

    Reads all documents and their entity sidecars, then rebuilds the
    graph in a single batch. Use --force to clear the cache first.

    Examples:
        learnings reindex
        learnings reindex --force
    """
    repo = get_repo_path()
    documents = get_all_documents()

    if not documents:
        console.print("[yellow]No documents to index.[/yellow]")
        return

    engine = _get_graph_engine()

    if force:
        console.print("[bold]Clearing graph cache...[/bold]")
        engine.clear_cache()

    console.print(f"[bold]Reindexing {len(documents)} documents...[/bold]")

    from reflect_kb.cli.entity_store import DocumentEntities, find_sidecar, auto_extract_entities, write_sidecar

    # Auto-generate missing sidecars before batch indexing
    generated_count = 0
    for doc in documents:
        doc_path = Path(doc["_path"])
        if not find_sidecar(doc_path):
            try:
                fm = {k: v for k, v in doc.items() if not k.startswith("_")}
                doc_entities = auto_extract_entities(doc["_full_content"], fm)
                if doc_entities.entity_count > 0:
                    write_sidecar(doc_path, doc_entities)
                    generated_count += 1
            except Exception as e:
                title = doc.get("title", doc.get("name", doc_path.name))
                msg = f"Auto-extract failed for {title}: {e}"
                console.print(f"  [yellow]Warning: {msg}[/yellow]")
                _err.append(
                    severity="warn", source="parse",
                    kind="autoextract_" + type(e).__name__.lower(),
                    message=msg,
                    context={"path": str(doc_path), "title": title},
                )

    if generated_count:
        console.print(f"[green]Auto-generated {generated_count} missing sidecars[/green]")

    # Build batch: list of (text, entities_formatted) tuples.
    # Batching avoids nano-graphrag state issues with sequential inserts
    # (community_reports dropped, early return skipping KV persistence).
    batch = []
    entity_total = 0
    rel_total = 0

    for doc in documents:
        doc_path = Path(doc["_path"])
        title = doc.get("title", doc.get("name", doc_path.name))

        entities_formatted = None
        sidecar_path = find_sidecar(doc_path)

        if sidecar_path:
            try:
                doc_entities = DocumentEntities.from_yaml_file(sidecar_path)
                entities_formatted = doc_entities.to_graphrag_format()
                entity_total += doc_entities.entity_count
                rel_total += doc_entities.relationship_count
                console.print(f"  [dim]{title} - {doc_entities.entity_count} entities[/dim]")
            except Exception as e:
                msg = f"Bad sidecar for {title}: {e}"
                console.print(f"  [yellow]Warning: {msg}[/yellow]")
                _err.append(
                    severity="warn", source="parse",
                    kind="sidecar_" + type(e).__name__.lower(),
                    message=msg,
                    context={"path": str(sidecar_path), "title": title},
                )
        else:
            console.print(f"  [dim]{title} - no sidecar (placeholder entities)[/dim]")

        batch.append((doc["_full_content"], entities_formatted))

    try:
        with console.status("[bold green]Indexing batch..."):
            engine.insert_documents_batch(batch)
        console.print(f"\n[green]Indexed {len(batch)} documents[/green]")
    except Exception as e:
        console.print(f"\n[red]Batch indexing error: {e}[/red]")
        console.print("[dim]Try running 'learnings reindex --force' to rebuild from scratch.[/dim]")
        return

    if entity_total:
        console.print(f"[dim]Entities: {entity_total}, Relationships: {rel_total}[/dim]")


@cli.command("generate-sidecars")
@click.option("--force", is_flag=True, help="Regenerate all sidecars, even existing ones")
def generate_sidecars(force: bool):
    """Generate entity sidecars for documents missing them.

    Uses heuristic extraction (no LLM required) to create .entities.yaml
    sidecar files from document content. This ensures every document
    contributes entities and relationships to the knowledge graph.

    Use --force to regenerate all sidecars, replacing existing ones.

    Examples:
        learnings generate-sidecars
        learnings generate-sidecars --force
    """
    documents = get_all_documents()

    if not documents:
        console.print("[yellow]No documents found.[/yellow]")
        return

    from reflect_kb.cli.entity_store import find_sidecar, auto_extract_entities, write_sidecar

    generated = 0
    skipped = 0
    failed = 0

    for doc in documents:
        doc_path = Path(doc["_path"])
        title = doc.get("title", doc.get("name", doc_path.name))

        existing_sidecar = find_sidecar(doc_path)
        if existing_sidecar and not force:
            skipped += 1
            continue

        try:
            fm = {k: v for k, v in doc.items() if not k.startswith("_")}
            doc_entities = auto_extract_entities(doc["_full_content"], fm)

            if doc_entities.entity_count > 0:
                write_sidecar(doc_path, doc_entities)
                generated += 1
                console.print(
                    f"  [green]{title}[/green] - "
                    f"{doc_entities.entity_count} entities, "
                    f"{doc_entities.relationship_count} relationships"
                )
            else:
                skipped += 1
                console.print(f"  [dim]{title} - no entities extracted[/dim]")
        except Exception as e:
            failed += 1
            console.print(f"  [yellow]{title} - failed: {e}[/yellow]")

    console.print(f"\n[bold]Results:[/bold]")
    console.print(f"  Generated: {generated}")
    console.print(f"  Skipped:   {skipped}")
    if failed:
        console.print(f"  Failed:    {failed}")
    console.print(
        f"\n[dim]Run 'learnings reindex --force' to rebuild the graph with new sidecars.[/dim]"
    )


@cli.command()
def init():
    """Initialize the global learnings repository.

    Creates the directory structure at {{HOME_TOOL_DIR}}/global-learnings/
    and initializes a git repository.
    """
    repo = get_repo_path()

    console.print(f"[bold]Initializing global learnings at {repo}[/bold]")

    (repo / DOCUMENTS_DIR).mkdir(parents=True, exist_ok=True)
    (repo / CACHE_DIR).mkdir(parents=True, exist_ok=True)

    # Initialize git if not already a repo
    if not (repo / ".git").exists():
        import subprocess

        subprocess.run(["git", "init"], cwd=str(repo), capture_output=True)
        console.print("[green]Git repository initialized[/green]")

        # Create .gitignore if missing
        gitignore = repo / ".gitignore"
        if not gitignore.exists():
            gitignore.write_text(
                ".venv/\n__pycache__/\n*.pyc\nnano_graphrag_cache/\n"
            )

        subprocess.run(
            ["git", "add", "."], cwd=str(repo), capture_output=True
        )
        subprocess.run(
            ["git", "commit", "-m", "Initialize global learnings", "--quiet"],
            cwd=str(repo), capture_output=True,
        )
    else:
        console.print("[dim]Git repository already exists[/dim]")

    console.print(f"[green]Ready.[/green]")
    console.print(f"[dim]Documents: {repo / DOCUMENTS_DIR}[/dim]")
    console.print(f"[dim]Graph cache: {repo / CACHE_DIR}[/dim]")


@cli.command("critical-patterns")
@click.option("--language", "-l", help="Filter by programming language")
@click.option("--domain", "-d", help="Filter by domain (backend, frontend, etc.)")
def critical_patterns(language: Optional[str], domain: Optional[str]):
    """Show critical patterns that should always be considered.

    These are high-confidence, widely-applicable patterns.

    Examples:
        learnings critical-patterns
        learnings critical-patterns --language rust
    """
    documents = get_all_documents()

    patterns = [
        d for d in documents
        if d.get("confidence") == "high"
        and d.get("category") in ["architecture-decisions", "patterns"]
    ]

    if language:
        patterns = [
            d for d in patterns
            if d.get("language", "").lower() == language.lower()
            or language.lower() in [t.lower() for t in d.get("tags", [])]
        ]

    if domain:
        patterns = [
            d for d in patterns
            if domain.lower() in d.get("_body", "").lower()
            or domain.lower() in [t.lower() for t in d.get("tags", [])]
        ]

    if not patterns:
        console.print("[yellow]No critical patterns found matching filters.[/yellow]")
        return

    console.print(f"[bold]Critical Patterns ({len(patterns)})[/bold]\n")

    for p in patterns:
        console.print(Panel(
            f"[bold]{p.get('title', 'Untitled')}[/bold]\n\n"
            f"{p.get('key_insight', 'No insight provided')}",
            border_style="red",
        ))


@cli.command()
def stats():
    """Show statistics about the knowledge base."""
    repo = get_repo_path()
    documents = get_all_documents()

    table = Table(title="Knowledge Base Statistics")
    table.add_column("Metric", style="cyan")
    table.add_column("Value", style="green")

    table.add_row("Total Documents", str(len(documents)))
    table.add_row("Repository", str(repo))

    # Graph stats
    try:
        engine = _get_graph_engine()
        graph_stats = engine.get_stats()
        table.add_row("Graph Entities", str(graph_stats.get("entity_count", 0)))
        table.add_row("Graph Relationships", str(graph_stats.get("relationship_count", 0)))
    except Exception:
        table.add_row("Graph Status", "Not initialized")

    # Entity sidecar stats
    from reflect_kb.cli.entity_store import find_sidecar

    docs_with_entities = 0
    for doc in documents:
        if find_sidecar(Path(doc["_path"])):
            docs_with_entities += 1
    table.add_row("Docs with Entities", f"{docs_with_entities}/{len(documents)}")

    console.print(table)

    if not documents:
        console.print("[yellow]Knowledge base is empty.[/yellow]")
        return

    # Category breakdown
    categories: Dict[str, int] = {}
    for doc in documents:
        cat = doc.get("category", "uncategorized")
        categories[cat] = categories.get(cat, 0) + 1

    cat_table = Table(title="\nBy Category")
    cat_table.add_column("Category", style="cyan")
    cat_table.add_column("Count", style="green")

    for cat, count in sorted(categories.items(), key=lambda x: -x[1]):
        cat_table.add_row(cat, str(count))

    console.print(cat_table)

    # Confidence breakdown
    confidence: Dict[str, int] = {}
    for doc in documents:
        conf = doc.get("confidence", "unknown")
        confidence[conf] = confidence.get(conf, 0) + 1

    conf_table = Table(title="\nBy Confidence")
    conf_table.add_column("Confidence", style="cyan")
    conf_table.add_column("Count", style="green")

    for conf, count in sorted(confidence.items()):
        conf_table.add_row(conf, str(count))

    console.print(conf_table)


@cli.command('timeline')
@click.option('--explain', 'explain_row', default=None,
              help='Print drill-down for a single row (REC, MEM, ING, DRN, TOK, ERR, COM, AGT) or "all".')
def timeline(explain_row):
    """Show or drill into the reflect timeline dashboard.

    Without --explain, prints a usage hint. With --explain, shells out to
    the agents-in-a-box reflect plugin's reflect_timeline.sh helper to
    render the drill-down. The helper is auto-discovered via
    $CLAUDE_PLUGIN_ROOT, then via the highest-versioned plugin cache dir
    under ~/.claude/plugins/cache/agents-in-a-box/reflect/.
    """
    if not explain_row:
        click.echo("Live dashboard renders on your Claude Code statusline.")
        click.echo(
            "Run `reflect timeline --explain <ROW>` for drill-down. "
            "ROW = REC|MEM|ING|DRN|TOK|ERR|COM|AGT|all"
        )
        return

    helper = None
    plugin_root = os.environ.get('CLAUDE_PLUGIN_ROOT')
    if plugin_root:
        candidate = Path(plugin_root) / 'scripts' / 'reflect_timeline.sh'
        if candidate.is_file():
            helper = str(candidate)

    if not helper:
        pattern = str(
            Path.home() / '.claude' / 'plugins' / 'cache' / 'agents-in-a-box'
            / 'reflect' / '*' / 'scripts' / 'reflect_timeline.sh'
        )
        matches = sorted(glob.glob(pattern))
        if matches:
            helper = matches[-1]

    if not helper:
        click.echo(click.style("error: reflect plugin not found.", fg='red'), err=True)
        click.echo("Install with: `claude plugin install reflect@agents-in-a-box`", err=True)
        raise click.Abort()

    rc = subprocess.call([helper, '--explain', explain_row])
    raise click.exceptions.Exit(rc)


@click.group("errors")
def errors_group():
    """Triage the reflect error sink (~/.reflect/errors.json).

    Exposed on the installed `reflect` binary so callers (statusline badge,
    drain hook, errors-ack skill) get a fast hot-path instead of bare
    `python3 -m reflect_kb.errors`, which only works when reflect_kb is
    importable by *system* python3 (it usually isn't — it lives in the uv
    tool venv). The legacy `python -m reflect_kb.errors` entrypoint keeps
    working for back-compat.
    """


@errors_group.command("count")
def errors_count():
    """Print the number of un-acked errors (drives the statusline badge)."""
    click.echo(_err.count_unacked())


@errors_group.command("ack")
@click.argument("ids", nargs=-1)
def errors_ack(ids):
    """Acknowledge errors by id (all un-acked if none given); prints the count acked."""
    click.echo(_err.ack(list(ids) or None))


@errors_group.command("append")
@click.option("--severity", default="error", type=click.Choice(["error", "warn", "info"]))
@click.option("--source", required=True)
@click.option("--kind", required=True)
@click.option("--message", required=True)
@click.option("--context", default="{}", help="JSON object string")
def errors_append(severity, source, kind, message, context):
    """Append an error record; prints the (deduped) error id."""
    try:
        ctx = json.loads(context)
    except Exception:
        ctx = {}
    click.echo(_err.append(severity, source, kind, message, ctx))


# Register subcommand groups. Import here (after `cli` exists) to keep
# circular-import risk at zero.
from reflect_kb.cli.metrics_cli import metrics_group as _metrics_group  # noqa: E402

cli.add_command(_metrics_group)
cli.add_command(errors_group)


if __name__ == "__main__":
    cli()
