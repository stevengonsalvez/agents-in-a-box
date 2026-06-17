#!/usr/bin/env python3
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""
graphml_repair.py — validate + self-heal the GraphRAG graphml (W5).

The 2026-05-31 incident's rabbit hole started because the KB graphml was
corrupt — ``not well-formed (invalid token): line 38163, column 2`` — caused by
a DOUBLED trailing close-tag block (``</graph></graphml>`` appended twice). The
drain agent discovered this mid-reflect and spent ~200 turns investigating it.

Corruption like this must self-heal as a cheap batch step, never escalate into
a reflect agent loop. This script:
  * --check   : exit 0 if the graphml parses, 1 if corrupt
  * --repair  : back up, attempt repair (truncate after the first valid
                </graphml>), re-validate; exit 0 on success/no-op, 1 if still
                broken after repair (so the caller can fall back to a full
                rebuild rather than feed a broken file forward)

Auto-discovers the graphml under common KB locations when no path is given.

CLI:
    graphml_repair.py [--check|--repair] [PATH] [--quiet]
"""

from __future__ import annotations

import sys
from pathlib import Path
from typing import Optional

# Prefer defusedxml (hardened against XXE / billion-laughs entity expansion).
# The graphml is a self-generated local KB file so the risk is low, but parsing
# with the safe variant when available costs nothing. Fall back to stdlib for
# portability (the hook context is dependency-light); validity-checking catches
# all parse errors regardless of which parser is used.
try:  # pragma: no cover - import preference
    import defusedxml.ElementTree as ET  # type: ignore
except Exception:  # pragma: no cover
    import xml.etree.ElementTree as ET  # type: ignore

# defusedxml's hardened parser is read-only (no Element factory / write path), so
# the --maintain mode — which mutates the tree and re-serialises it — uses the
# stdlib ElementTree explicitly. Parsing the file with the safe variant above
# still gates the maintain pass: we only mutate a graphml that already validates.
import xml.etree.ElementTree as _STD_ET  # noqa: E402

# nano-graphrag namespace; ET.write would otherwise emit ``ns0:`` prefixes.
_GRAPHML_NS = "http://graphml.graphdrawing.org/xmlns"

_CANDIDATE_GLOBS = [
    "~/.learnings/**/graph_chunk_entity_relation.graphml",
    "~/.claude/global-learnings/**/graph_chunk_entity_relation.graphml",
    "~/.reflect/**/*.graphml",
]


def discover() -> Optional[Path]:
    for pat in _CANDIDATE_GLOBS:
        base = Path(pat).expanduser()
        # split the glob: expanduser only handles ~, glob the rest
        root = Path(str(base).split("**")[0]) if "**" in str(base) else base.parent
        try:
            for hit in Path(root).glob("**/" + base.name):
                if hit.is_file():
                    return hit
        except OSError:
            continue
    return None


def is_valid(path: Path) -> bool:
    # Broad catch: stdlib raises ParseError, defusedxml may raise its own
    # EntitiesForbidden / DTDForbidden — any of these means "not a clean parse".
    try:
        ET.parse(path)
        return True
    except Exception:
        return False


def repair_text(text: str) -> Optional[str]:
    """Return repaired graphml text, or None if no safe repair is known.

    The known corruption is content AFTER the first ``</graphml>`` (a doubled
    close-tag block). Truncating to the first close tag drops the junk while
    keeping the (valid) graph above it.
    """
    close = "</graphml>"
    idx = text.find(close)
    if idx == -1:
        return None  # no close tag at all — not the doubled-tag case
    end = idx + len(close)
    if text[end:].strip() == "":
        return None  # nothing trailing — already clean (caller treats as no-op)
    return text[:end] + "\n"


def repair(path: Path, *, quiet: bool = False) -> bool:
    """Validate; if broken, back up and try the truncate repair. Returns True if
    the file ends up valid (or was already valid)."""
    def say(msg: str):
        if not quiet:
            print(msg)

    if is_valid(path):
        say(f"graphml OK: {path}")
        return True

    say(f"graphml CORRUPT: {path} — attempting repair")
    try:
        original = path.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        say(f"  cannot read: {exc}")
        return False

    fixed = repair_text(original)
    if fixed is None:
        say("  no known safe repair (not the doubled-close-tag pattern); "
            "recommend a full rebuild (reflect reindex --force)")
        return False

    backup = path.with_suffix(path.suffix + ".corrupt.bak")
    try:
        backup.write_text(original, encoding="utf-8")
        path.write_text(fixed, encoding="utf-8")
    except OSError as exc:
        say(f"  write failed: {exc}")
        return False

    if is_valid(path):
        say(f"  REPAIRED (backed up to {backup.name}); dropped "
            f"{len(original) - len(fixed)} trailing bytes")
        return True

    # Repair didn't take — restore the original so we don't ship a worse file.
    try:
        path.write_text(original, encoding="utf-8")
    except OSError:
        pass
    say("  repair did not validate; restored original. Full rebuild needed.")
    return False


# ── Maintenance pass (graph repair after deletes) ─────────────────────────────
# Port C3 (Hindsight graph_maintenance.py, 3-pass cleanup after deletes). As
# learnings are deleted/superseded their chunk ids drop out of the GraphRAG
# graphml's node/edge ``source_id`` lists, leaving cruft behind:
#   * orphan entities — nodes with no remaining learning refs (empty source_id)
#     and no edges; pure dead weight that pollutes entity counts and retrieval.
#   * stale cooccurrence edges — edges pointing at a node that no longer exists
#     (a dangling endpoint left after its entity was pruned).
#   * nodes that lost their neighbours — entities that still carry learning refs
#     but became isolated once a stale edge was pruned; their cooccurrence signal
#     needs topping back up so they stay reachable in graph-walk recall.
#
# The pass is a pure structural rewrite of the local graphml — no LLM, no
# embedding model — so it is cheap enough to run as a batch step inside the
# drain hook (once per N drains). It is idempotent: a clean graph is returned
# unchanged.
#
# Source: hindsight graph_maintenance.py:1-34
# https://github.com/vectorize-io/hindsight/blob/c255d35/hindsight-api-slim/hindsight_api/engine/graph/graph_maintenance.py#L1-L34

_GRAPH_FIELD_SEP = "<SEP>"  # nano-graphrag joins source_id chunk ids with this


def _node_key_for(root, attr_name: str) -> Optional[str]:
    """Return the ``<key>`` id whose attr.name matches for node data, or None."""
    for el in root.iter():
        if el.tag.rsplit("}", 1)[-1] == "key" and el.get("for") == "node" \
                and el.get("attr.name") == attr_name:
            return el.get("id")
    return None


def _node_data(node, key_id: Optional[str]) -> str:
    """Read a node's <data key=...> text for the given key id ('' if absent)."""
    if key_id is None:
        return ""
    for data in node:
        if data.tag.rsplit("}", 1)[-1] == "data" and data.get("key") == key_id:
            return (data.text or "")
    return ""


def _learning_refs(node, source_id_key: Optional[str]) -> set:
    """Chunk ids (learning refs) a node still carries via its source_id data."""
    raw = _node_data(node, source_id_key).strip().strip('"')
    if not raw:
        return set()
    return {r.strip() for r in raw.split(_GRAPH_FIELD_SEP) if r.strip()}


def maintain_tree(root):
    """Run the 3-pass cleanup on a parsed graphml root, in place.

    Returns a stats dict: orphans_pruned, edges_pruned, nodes_relinked. A graph
    with no orphans / dangling edges / isolated nodes yields all-zero stats and
    is left byte-identically unchanged (idempotent).
    """
    graph = next((g for g in root.iter()
                  if g.tag.rsplit("}", 1)[-1] == "graph"), None)
    if graph is None:
        return {"orphans_pruned": 0, "edges_pruned": 0, "nodes_relinked": 0}

    source_id_key = _node_key_for(root, "source_id")
    nodes = [c for c in graph if c.tag.rsplit("}", 1)[-1] == "node"]
    edges = [c for c in graph if c.tag.rsplit("}", 1)[-1] == "edge"]

    refs = {n.get("id"): _learning_refs(n, source_id_key) for n in nodes}

    def _adj(edge_list):
        adj: dict = {}
        for e in edge_list:
            s, t = e.get("source"), e.get("target")
            adj.setdefault(s, set()).add(t)
            adj.setdefault(t, set()).add(s)
        return adj

    # ── Pass 1: prune orphan entities (no learning refs AND no edges) ─────────
    adj = _adj(edges)
    orphans = [n for n in nodes
               if not refs.get(n.get("id")) and not adj.get(n.get("id"))]
    orphan_ids = {n.get("id") for n in orphans}
    for n in orphans:
        graph.remove(n)
    nodes = [n for n in nodes if n.get("id") not in orphan_ids]
    live_ids = {n.get("id") for n in nodes}

    # ── Pass 2: prune stale cooccurrence edges (dangling endpoint) ───────────
    stale_edges = [e for e in edges
                   if e.get("source") not in live_ids
                   or e.get("target") not in live_ids]
    for e in stale_edges:
        graph.remove(e)
    edges = [e for e in edges if e not in stale_edges]

    # ── Pass 3: relink nodes that lost all neighbours but keep learning refs ──
    # An entity that still references at least one learning yet became isolated
    # after the stale-edge prune is relinked to a node it co-occurs with (shares
    # a learning ref). This tops the cooccurrence graph back up so the entity
    # stays reachable in graph-walk recall instead of stranding.
    adj = _adj(edges)
    relinked = 0
    for n in nodes:
        nid = n.get("id")
        my_refs = refs.get(nid) or set()
        if not my_refs or adj.get(nid):
            continue  # no refs (kept only because it had edges) or not isolated
        partner = next(
            (o.get("id") for o in nodes
             if o.get("id") != nid and (refs.get(o.get("id")) or set()) & my_refs),
            None,
        )
        if partner is None:
            continue
        edge = _STD_ET.SubElement(graph, "edge")
        edge.set("source", nid)
        edge.set("target", partner)
        adj.setdefault(nid, set()).add(partner)
        adj.setdefault(partner, set()).add(nid)
        relinked += 1

    return {
        "orphans_pruned": len(orphans),
        "edges_pruned": len(stale_edges),
        "nodes_relinked": relinked,
    }


def maintain(path: Path, *, quiet: bool = False) -> bool:
    """Validate then run the post-delete maintenance sweep, writing back only if
    the rewrite re-validates. Returns True on success (incl. clean no-op)."""
    def say(msg: str):
        if not quiet:
            print(msg)

    if not is_valid(path):
        say(f"graphml not valid; skipping maintenance (run --repair first): {path}")
        return False

    try:
        # Parse with the stdlib tree (defusedxml has no write path). The file
        # already validated above, so this parse cannot hit untrusted entities
        # the safe parser would have rejected.
        _STD_ET.register_namespace("", _GRAPHML_NS)
        tree = _STD_ET.parse(str(path))
    except (OSError, _STD_ET.ParseError) as exc:
        say(f"  cannot parse for maintenance: {exc}")
        return False

    root = tree.getroot()
    stats = maintain_tree(root)

    if not any(stats.values()):
        say(f"graphml clean (no maintenance needed): {path}")
        return True  # idempotent no-op — leave the file untouched

    backup = path.with_suffix(path.suffix + ".premaint.bak")
    try:
        original = path.read_text(encoding="utf-8", errors="replace")
        backup.write_text(original, encoding="utf-8")
        tree.write(str(path), encoding="utf-8", xml_declaration=True)
    except OSError as exc:
        say(f"  write failed: {exc}")
        return False

    if not is_valid(path):
        # Rewrite didn't validate — restore so we never ship a worse file.
        try:
            path.write_text(original, encoding="utf-8")
        except OSError:
            pass
        say("  maintenance rewrite did not validate; restored original.")
        return False

    say(f"  MAINTAINED (backed up to {backup.name}): "
        f"orphans_pruned={stats['orphans_pruned']} "
        f"edges_pruned={stats['edges_pruned']} "
        f"nodes_relinked={stats['nodes_relinked']}")
    return True


def main() -> None:
    import argparse

    ap = argparse.ArgumentParser(description="Validate/repair the GraphRAG graphml")
    ap.add_argument("path", nargs="?", default=None)
    ap.add_argument("--check", action="store_true", help="validate only (exit 1 if corrupt)")
    ap.add_argument("--repair", action="store_true", help="back up + attempt repair")
    ap.add_argument("--maintain", action="store_true",
                    help="post-delete sweep: prune orphan entities + stale edges, relink")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    target = Path(args.path).expanduser() if args.path else discover()
    if target is None or not target.exists():
        if not args.quiet:
            print("no graphml found", file=sys.stderr)
        sys.exit(0)  # nothing to do is not an error

    if args.check or not (args.repair or args.maintain):
        ok = is_valid(target)
        if not args.quiet:
            print(f"{'OK' if ok else 'CORRUPT'}: {target}")
        sys.exit(0 if ok else 1)

    # --repair first (a corrupt graphml can't be maintained), then --maintain.
    if args.repair and not repair(target, quiet=args.quiet):
        sys.exit(1)
    if args.maintain:
        sys.exit(0 if maintain(target, quiet=args.quiet) else 1)
    sys.exit(0)


if __name__ == "__main__":
    main()
