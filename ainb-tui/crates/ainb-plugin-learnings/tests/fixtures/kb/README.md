# learnings plugin — mini-KB test fixtures

A self-contained, committed knowledge base so the P4 data-layer tests never
touch the real `~/.learnings`. Every shape here was captured from a real
reflect KB (`~/.learnings`) on 2026-06-04 and trimmed to a few records, so the
parsers are tested against representative real-world data.

## Learning records (`*.md` + optional `*.entities.yaml`)

Frontmatter shape mirrors `~/.learnings/documents/learnings/*.md`:
`title / category / type / scope / confidence / key_insight / tags /
provenance{source_tool, source_path, content_hash, ingested_at, project?}`.
The body is the `## Problem` / `## Solution` markdown after the frontmatter.

| file | scope | confidence | category | source_tool | sidecar | note |
|------|-------|-----------|----------|-------------|---------|------|
| `lrn-audit-after-rebase.md` | universal | 0.85 | process | claude | yes | has a `solves` relationship |
| `lrn-tokio-runtime-panic.md` | project | 1.0 | reference | claude | yes | distinct scope/category for filter tests |
| `lrn-claude-plugin-autowire.md` | universal | 0.8 | reference | codex | yes | distinct source_tool + a `project` provenance |
| `lrn-no-sidecar.md` | universal | 0.7 | noteworthy | claude | **no** | exercises the empty-entities path (no `.entities.yaml`) |

`NOTES.txt` is a non-markdown file present so `scan_learnings_dir` is asserted
to ignore anything that is not a `*.md`.

`*.entities.yaml` shape mirrors the real sidecar: `document_id / extracted_at /
entities[]{name,type,description} / relationships[]{source,target,type,
description,strength}`.

## Graph (`graph_chunk_entity_relation.graphml`)

GraphML in the nano_graphrag shape: quote-wrapped node ids
(`&quot;NAME&quot;`), node `data` keys `d0`=entity_type / `d1`=description /
`d2`=source_id / `d3`=clusters, edge `data` keys `d4`=weight / `d5`=description
/ `d6`=source_id / `d7`=order.

**Divergence from the live KB (documented on purpose):** the real
nano_graphrag graphml does **not** carry a typed `rel_type` on edges — the edge
type only lives in the `.entities.yaml` sidecars; the graphml `d5` is a
free-text description. To give `parse_graphml` a deterministic typed-edge
source (the P4 RED `test_parse_graphml` asserts a `solves` edge), this fixture
adds an extra edge `data` key `d_rel` carrying the relationship type. The third
edge deliberately omits `d_rel` so the parser is exercised on the
untyped-edge fallback (`relates_to`), which is what every real-KB edge hits.

## Community reports (`kv_store_community_reports.json`)

Object keyed by community id, mirroring
`~/.learnings/nano_graphrag_cache/kv_store_community_reports.json`: each value
has `report_string / report_json{title,summary,findings,rating,
rating_explanation} / level / title / edges / nodes / chunk_ids / occurrence /
sub_communities`. Two communities.

## qmd query sample (`qmd_query_sample.json`)

**Captured from real `qmd query "audit after rebase" --json -n 3`** on
2026-06-04 (qmd v on this machine, index `~/.cache/qmd/index.sqlite`), then
trimmed. The shape is a JSON array of
`{docid, score, file, title, snippet}`. The data layer's qmd shell is wrapped
behind a trait so `test_qmd_query_parse` parses this committed sample without
shelling out; a single `#[ignore]`d test shells the live `qmd` as a smoke.
