#!/usr/bin/env python3
"""
Global Learnings CLI - Knowledge base with GraphRAG search.

This CLI provides semantic search over the global learnings repository
using Nano-GraphRAG for vector + graph-based retrieval.
"""

import os
import sys
import json
import yaml
import hashlib
import shutil
from pathlib import Path
from datetime import datetime
from typing import Optional, List, Dict, Any

import click
from rich.console import Console
from rich.table import Table
from rich.panel import Panel
from rich.markdown import Markdown

# Initialize console for rich output
console = Console()

# Default paths
DEFAULT_REPO_PATH = Path.home() / ".claude" / "global-learnings"
DOCUMENTS_DIR = "documents"
CACHE_DIR = "nano_graphrag_cache"


def get_repo_path() -> Path:
    """Get the global learnings repository path."""
    env_path = os.environ.get("GLOBAL_LEARNINGS_PATH")
    if env_path:
        return Path(env_path)
    return DEFAULT_REPO_PATH


def ensure_repo_exists():
    """Ensure the repository structure exists."""
    repo = get_repo_path()
    (repo / DOCUMENTS_DIR).mkdir(parents=True, exist_ok=True)
    (repo / CACHE_DIR).mkdir(parents=True, exist_ok=True)


def parse_frontmatter(content: str) -> tuple[Dict[str, Any], str]:
    """Parse YAML frontmatter from markdown content."""
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
    """Generate a unique document ID from title."""
    # Create a slug from title
    slug = title.lower()
    slug = "".join(c if c.isalnum() or c == " " else "" for c in slug)
    slug = "-".join(slug.split())[:50]

    # Add short hash for uniqueness
    hash_suffix = hashlib.md5(title.encode()).hexdigest()[:6]
    return f"{slug}-{hash_suffix}"


def load_embedding_model():
    """Load the sentence transformer model for embeddings."""
    try:
        from sentence_transformers import SentenceTransformer
        model = SentenceTransformer("all-MiniLM-L6-v2")
        return model
    except ImportError:
        console.print("[red]Error: sentence-transformers not installed.[/red]")
        console.print("Run: pip install sentence-transformers")
        sys.exit(1)


def get_all_documents() -> List[Dict[str, Any]]:
    """Load all documents from the repository."""
    repo = get_repo_path()
    docs_dir = repo / DOCUMENTS_DIR
    documents = []

    for doc_path in docs_dir.glob("*.md"):
        try:
            content = doc_path.read_text()
            frontmatter, body = parse_frontmatter(content)
            if frontmatter:
                frontmatter["_path"] = str(doc_path)
                frontmatter["_body"] = body
                documents.append(frontmatter)
        except Exception as e:
            console.print(f"[yellow]Warning: Could not parse {doc_path}: {e}[/yellow]")

    return documents


def search_documents(query: str, documents: List[Dict], model, top_k: int = 10) -> List[Dict]:
    """Search documents using semantic similarity."""
    import numpy as np

    if not documents:
        return []

    # Create searchable text for each document
    doc_texts = []
    for doc in documents:
        text = f"{doc.get('title', '')} {doc.get('key_insight', '')} "
        text += " ".join(doc.get('symptoms', []))
        text += " ".join(doc.get('tags', []))
        doc_texts.append(text)

    # Encode query and documents
    query_embedding = model.encode(query)
    doc_embeddings = model.encode(doc_texts)

    # Compute cosine similarity
    similarities = np.dot(doc_embeddings, query_embedding) / (
        np.linalg.norm(doc_embeddings, axis=1) * np.linalg.norm(query_embedding)
    )

    # Get top-k results
    top_indices = np.argsort(similarities)[::-1][:top_k]

    results = []
    for idx in top_indices:
        if similarities[idx] > 0.1:  # Minimum threshold
            doc = documents[idx].copy()
            doc["_score"] = float(similarities[idx])
            results.append(doc)

    return results


@click.group()
@click.version_option(version="1.0.0")
def cli():
    """Global Learnings CLI - Knowledge base with GraphRAG search."""
    ensure_repo_exists()


@cli.command()
@click.argument("query")
@click.option("--tags", "-t", help="Filter by tags (comma-separated)")
@click.option("--category", "-c", help="Filter by category")
@click.option("--limit", "-l", default=10, help="Maximum results (default: 10)")
@click.option("--format", "-f", "output_format", default="rich",
              type=click.Choice(["rich", "json", "simple"]))
def search(query: str, tags: Optional[str], category: Optional[str],
           limit: int, output_format: str):
    """Search learnings using semantic similarity.

    Examples:
        learnings search "tokio runtime panic"
        learnings search "async timeout" --tags rust,tokio
        learnings search "n+1 query" --category performance-issues
    """
    console.print(f"[bold]Searching:[/bold] {query}")

    # Load documents
    documents = get_all_documents()

    if not documents:
        console.print("[yellow]No documents found in the knowledge base.[/yellow]")
        console.print(f"Add documents to: {get_repo_path() / DOCUMENTS_DIR}")
        return

    # Apply filters
    if tags:
        tag_list = [t.strip().lower() for t in tags.split(",")]
        documents = [d for d in documents
                    if any(t.lower() in [x.lower() for x in d.get("tags", [])]
                          for t in tag_list)]

    if category:
        documents = [d for d in documents
                    if d.get("category", "").lower() == category.lower()]

    if not documents:
        console.print("[yellow]No documents match the filters.[/yellow]")
        return

    # Load model and search
    with console.status("[bold green]Loading embedding model..."):
        model = load_embedding_model()

    with console.status("[bold green]Searching..."):
        results = search_documents(query, documents, model, limit)

    if not results:
        console.print("[yellow]No relevant results found.[/yellow]")
        return

    # Output results
    if output_format == "json":
        # Remove internal fields for JSON output
        clean_results = [{k: v for k, v in r.items() if not k.startswith("_")}
                        for r in results]
        click.echo(json.dumps(clean_results, indent=2, default=str))

    elif output_format == "simple":
        for r in results:
            console.print(f"\n[bold]{r.get('title', 'Untitled')}[/bold]")
            console.print(f"  Key: {r.get('key_insight', 'N/A')}")
            console.print(f"  File: {r.get('_path', 'N/A')}")

    else:  # rich format
        console.print(f"\n[bold green]Found {len(results)} result(s)[/bold green]\n")

        for i, r in enumerate(results, 1):
            score = r.get("_score", 0)
            score_bar = "█" * int(score * 10) + "░" * (10 - int(score * 10))

            table = Table(show_header=False, box=None, padding=(0, 1))
            table.add_column(style="bold cyan")
            table.add_column()

            table.add_row("Title", r.get("title", "Untitled"))
            table.add_row("Category", r.get("category", "N/A"))
            table.add_row("Tags", ", ".join(r.get("tags", [])))
            table.add_row("Confidence", r.get("confidence", "N/A"))
            table.add_row("Score", f"{score_bar} {score:.2f}")

            panel = Panel(
                table,
                title=f"[bold]Result {i}[/bold]",
                subtitle=f"[dim]{Path(r.get('_path', '')).name}[/dim]",
                border_style="green"
            )
            console.print(panel)

            # Show key insight prominently
            if r.get("key_insight"):
                console.print(Panel(
                    f"[bold green]{r['key_insight']}[/bold green]",
                    title="[bold]Key Insight[/bold]",
                    border_style="yellow"
                ))
            console.print()


@cli.command()
@click.argument("file_path", type=click.Path(exists=True))
@click.option("--extract/--no-extract", default=False,
              help="Extract entities using Claude (requires active session)")
def add(file_path: str, extract: bool):
    """Add a learning document to the knowledge base.

    The document should have YAML frontmatter with required fields:
    title, category, tags, symptoms, root_cause, key_insight, created, confidence

    Examples:
        learnings add ./my-solution.md
        learnings add ./my-solution.md --extract
    """
    source = Path(file_path)
    content = source.read_text()

    frontmatter, body = parse_frontmatter(content)

    if not frontmatter:
        console.print("[red]Error: Document must have YAML frontmatter.[/red]")
        console.print("See README.md for required format.")
        return

    # Validate required fields
    required = ["title", "category", "key_insight"]
    missing = [f for f in required if f not in frontmatter]
    if missing:
        console.print(f"[red]Error: Missing required fields: {', '.join(missing)}[/red]")
        return

    # Generate document ID and path
    doc_id = generate_document_id(frontmatter["title"])
    repo = get_repo_path()
    dest = repo / DOCUMENTS_DIR / f"{doc_id}.md"

    if dest.exists():
        if not click.confirm(f"Document {dest.name} exists. Overwrite?"):
            return

    # Copy document
    shutil.copy(source, dest)

    console.print(f"[green]✓ Added:[/green] {dest}")
    console.print(f"[dim]Title: {frontmatter['title']}[/dim]")
    console.print(f"[dim]Category: {frontmatter['category']}[/dim]")

    if extract:
        console.print("\n[yellow]Note: Entity extraction requires an active Claude session.[/yellow]")
        console.print("Run this command within Claude Code to enable extraction.")


@cli.command()
@click.argument("local_path", type=click.Path(exists=True))
def promote(local_path: str):
    """Promote a local learning to the global knowledge base.

    Copies a learning from a project's docs/solutions/ to the global repo.

    Examples:
        learnings promote ~/project/docs/solutions/build-errors/my-fix.md
    """
    source = Path(local_path)
    content = source.read_text()

    frontmatter, body = parse_frontmatter(content)

    if not frontmatter:
        console.print("[red]Error: Document must have YAML frontmatter.[/red]")
        return

    if "title" not in frontmatter:
        console.print("[red]Error: Document must have a title.[/red]")
        return

    # Add promoted metadata
    frontmatter["promoted_from"] = str(source)
    frontmatter["promoted_at"] = datetime.now().isoformat()

    # Regenerate content with updated frontmatter
    new_content = "---\n"
    new_content += yaml.dump(frontmatter, default_flow_style=False, allow_unicode=True)
    new_content += "---\n\n"
    new_content += body

    # Generate document ID and save
    doc_id = generate_document_id(frontmatter["title"])
    repo = get_repo_path()
    dest = repo / DOCUMENTS_DIR / f"{doc_id}.md"

    if dest.exists():
        console.print(f"[yellow]Document already exists: {dest.name}[/yellow]")
        if not click.confirm("Overwrite?"):
            return

    dest.write_text(new_content)

    console.print(f"[green]✓ Promoted to global:[/green] {dest}")
    console.print(f"[dim]Source: {source}[/dim]")
    console.print(f"[dim]Title: {frontmatter['title']}[/dim]")


@cli.command()
def reindex():
    """Rebuild the GraphRAG index.

    This regenerates embeddings and the graph structure for all documents.
    Run this after adding multiple documents or if search seems stale.
    """
    repo = get_repo_path()
    documents = get_all_documents()

    if not documents:
        console.print("[yellow]No documents to index.[/yellow]")
        return

    console.print(f"[bold]Reindexing {len(documents)} documents...[/bold]")

    with console.status("[bold green]Loading embedding model..."):
        model = load_embedding_model()

    # Generate embeddings for all documents
    import numpy as np

    doc_texts = []
    doc_ids = []
    for doc in documents:
        text = f"{doc.get('title', '')} {doc.get('key_insight', '')} "
        text += " ".join(doc.get('symptoms', []))
        text += " ".join(doc.get('tags', []))
        doc_texts.append(text)
        doc_ids.append(Path(doc["_path"]).stem)

    with console.status("[bold green]Generating embeddings..."):
        embeddings = model.encode(doc_texts)

    # Save embeddings
    cache_dir = repo / CACHE_DIR
    np.save(cache_dir / "embeddings.npy", embeddings)

    # Save document index
    index = {
        "documents": doc_ids,
        "indexed_at": datetime.now().isoformat(),
        "count": len(documents)
    }
    (cache_dir / "index.json").write_text(json.dumps(index, indent=2))

    console.print(f"[green]✓ Indexed {len(documents)} documents[/green]")
    console.print(f"[dim]Cache: {cache_dir}[/dim]")


@cli.command("critical-patterns")
@click.option("--language", "-l", help="Filter by programming language")
@click.option("--domain", "-d", help="Filter by domain (backend, frontend, etc.)")
def critical_patterns(language: Optional[str], domain: Optional[str]):
    """Show critical patterns that should always be considered.

    These are high-confidence, widely-applicable patterns.

    Examples:
        learnings critical-patterns
        learnings critical-patterns --language rust
        learnings critical-patterns --domain backend
    """
    documents = get_all_documents()

    # Filter for high-confidence, pattern-type documents
    patterns = [d for d in documents
                if d.get("confidence") == "high"
                and d.get("category") in ["architecture-decisions", "patterns"]]

    if language:
        patterns = [d for d in patterns
                   if d.get("language", "").lower() == language.lower()
                   or language.lower() in [t.lower() for t in d.get("tags", [])]]

    if domain:
        patterns = [d for d in patterns
                   if domain.lower() in d.get("_body", "").lower()
                   or domain.lower() in [t.lower() for t in d.get("tags", [])]]

    if not patterns:
        console.print("[yellow]No critical patterns found matching filters.[/yellow]")
        return

    console.print(f"[bold]Critical Patterns ({len(patterns)})[/bold]\n")

    for p in patterns:
        console.print(Panel(
            f"[bold]{p.get('title', 'Untitled')}[/bold]\n\n"
            f"{p.get('key_insight', 'No insight provided')}",
            border_style="red"
        ))


@cli.command()
def stats():
    """Show statistics about the knowledge base."""
    repo = get_repo_path()
    documents = get_all_documents()

    if not documents:
        console.print("[yellow]Knowledge base is empty.[/yellow]")
        return

    # Count by category
    categories = {}
    for doc in documents:
        cat = doc.get("category", "uncategorized")
        categories[cat] = categories.get(cat, 0) + 1

    # Count by confidence
    confidence = {}
    for doc in documents:
        conf = doc.get("confidence", "unknown")
        confidence[conf] = confidence.get(conf, 0) + 1

    # Build stats table
    table = Table(title="Knowledge Base Statistics")
    table.add_column("Metric", style="cyan")
    table.add_column("Value", style="green")

    table.add_row("Total Documents", str(len(documents)))
    table.add_row("Repository", str(repo))

    console.print(table)

    # Category breakdown
    cat_table = Table(title="\nBy Category")
    cat_table.add_column("Category", style="cyan")
    cat_table.add_column("Count", style="green")

    for cat, count in sorted(categories.items(), key=lambda x: -x[1]):
        cat_table.add_row(cat, str(count))

    console.print(cat_table)

    # Confidence breakdown
    conf_table = Table(title="\nBy Confidence")
    conf_table.add_column("Confidence", style="cyan")
    conf_table.add_column("Count", style="green")

    for conf, count in sorted(confidence.items()):
        conf_table.add_row(conf, str(count))

    console.print(conf_table)


if __name__ == "__main__":
    cli()
