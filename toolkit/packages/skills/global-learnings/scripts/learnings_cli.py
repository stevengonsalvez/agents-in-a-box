#!/usr/bin/env python3
"""
Global Learnings CLI - Knowledge base with GraphRAG search.

Provides semantic search over the global learnings repository
using nano-graphrag for vector + graph-based retrieval.
"""

import json
import os
import hashlib
import shutil
import sys
from pathlib import Path
from datetime import datetime
from typing import Optional, List, Dict, Any

import click
import yaml
from rich.console import Console
from rich.table import Table
from rich.panel import Panel

console = Console(stderr=True)

DEFAULT_REPO_PATH = Path.home() / ".learnings"
DOCUMENTS_DIR = "documents"
CACHE_DIR = "nano_graphrag_cache"


def get_repo_path() -> Path:
    env_path = os.environ.get("LEARNINGS_HOME")
    if env_path:
        return Path(env_path)
    return DEFAULT_REPO_PATH


def ensure_repo_exists():
    repo = get_repo_path()
    (repo / DOCUMENTS_DIR / "learnings").mkdir(parents=True, exist_ok=True)
    (repo / DOCUMENTS_DIR / "episodes").mkdir(parents=True, exist_ok=True)
    (repo / DOCUMENTS_DIR / "clusters").mkdir(parents=True, exist_ok=True)
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


def generate_document_id(title: str) -> str:
    slug = title.lower()
    slug = "".join(c if c.isalnum() or c == " " else "" for c in slug)
    slug = "-".join(slug.split())[:50]
    hash_suffix = hashlib.md5(title.encode()).hexdigest()[:6]
    return f"{slug}-{hash_suffix}"


def get_all_documents() -> List[Dict[str, Any]]:
    repo = get_repo_path()
    docs_dir = repo / DOCUMENTS_DIR
    documents = []

    for doc_path in sorted(docs_dir.rglob("*.md")):
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
    from graph_engine import LearningsGraphEngine, GraphEngineError

    repo = get_repo_path()
    cache_dir = repo / CACHE_DIR
    return LearningsGraphEngine(cache_dir)


@click.group()
@click.version_option(version="3.0.0")
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

    try:
        engine = _get_graph_engine()
        context = engine.search(search_query, mode=mode, only_context=True)
    except Exception as e:
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


@cli.command()
@click.argument("file_path", type=click.Path(exists=True))
@click.option(
    "--entities", "-e", type=click.Path(exists=True),
    help="Path to .entities.yaml sidecar with pre-extracted entities",
)
def add(file_path: str, entities: Optional[str]):
    """Add a learning document to the knowledge base.

    The document should have YAML frontmatter with at least:
    title, category, key_insight

    Examples:
        learnings add ./my-solution.md
        learnings add ./my-solution.md --entities ./my-solution.entities.yaml
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

    # Generate document ID and copy to repo
    doc_id = generate_document_id(frontmatter["title"])
    repo = get_repo_path()
    dest = repo / DOCUMENTS_DIR / "learnings" / f"{doc_id}.md"

    if dest.exists():
        if not click.confirm(f"Document {dest.name} exists. Overwrite?"):
            return

    shutil.copy(source, dest)

    # Copy entity sidecar if provided
    entities_formatted = None
    entity_count = 0
    rel_count = 0

    if entities:
        from entity_store import DocumentEntities, find_sidecar

        entities_path = Path(entities)
        doc_entities = DocumentEntities.from_yaml_file(entities_path)
        entities_formatted = doc_entities.to_graphrag_format()
        entity_count = doc_entities.entity_count
        rel_count = doc_entities.relationship_count

        # Save sidecar alongside document
        sidecar_dest = dest.with_suffix(".entities.yaml")
        shutil.copy(entities_path, sidecar_dest)

    # Insert into graph
    try:
        engine = _get_graph_engine()
        with console.status("[bold green]Indexing document..."):
            engine.insert_document(content, entities_formatted=entities_formatted)
        console.print(f"[green]Indexed into graph[/green]")
    except Exception as e:
        console.print(f"[yellow]Warning: Graph indexing failed: {e}[/yellow]")
        console.print("[dim]Document saved. Run 'learnings reindex' to retry.[/dim]")

    # Trigger QMD re-embedding (graceful if qmd not installed)
    if shutil.which("qmd"):
        try:
            import subprocess

            subprocess.run(["qmd", "embed"], capture_output=True)
            console.print("[green]QMD embeddings updated[/green]")
        except Exception as e:
            console.print(f"[yellow]Warning: QMD embed failed: {e}[/yellow]")

    # Auto-commit to git
    try:
        import subprocess

        subprocess.run(["git", "-C", str(repo), "add", "-A"], capture_output=True)
        subprocess.run(
            ["git", "-C", str(repo), "commit", "-m", f"Add learning: {frontmatter['title']}", "--quiet"],
            capture_output=True,
        )
        console.print("[green]Changes committed to git[/green]")
    except Exception as e:
        console.print(f"[yellow]Warning: Git commit failed: {e}[/yellow]")

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

    from entity_store import DocumentEntities, find_sidecar

    # Build batch: list of (text, entities_formatted) tuples.
    # Batching avoids nano-graphrag state issues with sequential inserts
    # (community_reports dropped, early return skipping KV persistence).
    batch = []
    entity_total = 0
    rel_total = 0

    for doc in documents:
        doc_path = Path(doc["_path"])
        title = doc.get("title", doc_path.name)

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
                console.print(f"  [yellow]Warning: Bad sidecar for {title}: {e}[/yellow]")
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


@cli.command()
def init():
    """Initialize the global learnings repository.

    Creates the directory structure at ~/.learnings/ (or LEARNINGS_HOME)
    and initializes a git repository.
    """
    repo = get_repo_path()

    console.print(f"[bold]Initializing global learnings at {repo}[/bold]")

    (repo / DOCUMENTS_DIR / "learnings").mkdir(parents=True, exist_ok=True)
    (repo / DOCUMENTS_DIR / "episodes").mkdir(parents=True, exist_ok=True)
    (repo / DOCUMENTS_DIR / "clusters").mkdir(parents=True, exist_ok=True)
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
    console.print(f"[dim]  learnings/: Learning documents[/dim]")
    console.print(f"[dim]  episodes/:  Session episodes[/dim]")
    console.print(f"[dim]  clusters/:  Clustered patterns[/dim]")
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
    from entity_store import find_sidecar

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


@cli.command()
@click.option("--output", "-o", default=None, help="Output HTML file path (default: graph_viz.html in repo)")
@click.option("--no-open", is_flag=True, help="Don't auto-open in browser")
def visualize(output, no_open):
    """Generate an interactive visualization of the knowledge graph."""
    import networkx as nx

    repo = get_repo_path()
    graph_file = repo / CACHE_DIR / "graph_chunk_entity_relation.graphml"

    if not graph_file.exists():
        console.print("[red]No graph found.[/red] Run 'learnings reindex' first.")
        raise SystemExit(1)

    try:
        from pyvis.network import Network
    except ImportError:
        console.print("[red]pyvis not installed.[/red] Run: pip install pyvis")
        raise SystemExit(1)

    G = nx.read_graphml(str(graph_file))
    console.print(f"Graph loaded: {G.number_of_nodes()} nodes, {G.number_of_edges()} edges")

    if G.number_of_nodes() == 0:
        console.print("[yellow]Graph is empty — nothing to visualize.[/yellow]")
        return

    TYPE_COLORS = {
        "technology": "#4a90d9",
        "pattern": "#50c878",
        "function": "#f5a623",
        "concept": "#9b59b6",
        "tool": "#e74c3c",
    }
    DEFAULT_COLOR = "#888888"

    net = Network(
        height="950px",
        width="100%",
        bgcolor="#1a1a2e",
        font_color="#e0e0e0",
        notebook=False,
        directed=G.is_directed(),
    )

    for node_id, attrs in G.nodes(data=True):
        label = attrs.get("entity_id", str(node_id))[:40]
        description = attrs.get("description", "")[:300]
        entity_type = attrs.get("entity_type", "unknown").lower()
        color = TYPE_COLORS.get(entity_type, DEFAULT_COLOR)
        net.add_node(
            node_id,
            label=label,
            title=f"{label}\nType: {entity_type}\n{description}",
            color=color,
        )

    for u, v, attrs in G.edges(data=True):
        weight = float(attrs.get("weight", 1.0))
        keywords = attrs.get("keywords", attrs.get("relation_type", ""))
        net.add_edge(u, v, title=keywords, width=max(0.5, weight * 0.5))

    net.set_options("""{
      "physics": {
        "forceAtlas2Based": { "gravitationalConstant": -50, "springLength": 150 },
        "solver": "forceAtlas2Based"
      },
      "nodes": { "shape": "dot", "size": 14 },
      "edges": { "smooth": { "type": "continuous" } }
    }""")

    output_path = Path(output) if output else repo / "graph_viz.html"
    net.show(str(output_path), notebook=False)
    console.print(f"[green]Saved to {output_path}[/green]")

    if not no_open:
        import webbrowser
        webbrowser.open(f"file://{output_path.resolve()}")


if __name__ == "__main__":
    cli()
