---
title: "Graph hub with many neighbours (map overflow fixture)"
category: "reference"
type: LEARNING
scope: "universal"
confidence: 0.7
key_insight: "A hot entity with many typed relationships exercises the radial map's node cap and the [+N more] overflow node."
tags: ['graph', 'fixture', 'overflow']
provenance:
  source_tool: "claude"
  source_path: "/Users/test/.claude/memory/graph_hub.md"
  content_hash: "abcdef0123456789"
  ingested_at: "2026-06-07T00:00:00.000000+00:00"
---

## Problem

The radial local-graph map caps a centre's neighbours (default 15) and folds the
remainder into a single `[+N more]` node that the `e` key expands. That path
needs a fixture entity with more than the cap's worth of neighbours.

## Solution

`zzz-graph-hub` (named to sort last so it never displaces the alphabetically-first
entity the other Graph tests select) has 18 typed neighbours — over the 15-node
cap — so centring the map on it renders `[+3 more]`.
