---
title: "Record without an entities sidecar"
category: "noteworthy"
type: LEARNING
scope: "universal"
confidence: 0.7
key_insight: "A learning may have no `.entities.yaml` sidecar; the reader must yield empty entities/relationships instead of panicking."
tags: ['fixture', 'edge-case']
provenance:
  source_tool: "claude"
  source_path: "/Users/test/.claude/memory/lrn_no_sidecar.md"
  content_hash: "deadbeefdeadbeef"
  ingested_at: "2026-05-25T12:00:00.000000+00:00"
---

## Problem

Some learnings are ingested before entity extraction runs, so the
`.entities.yaml` sidecar is absent.

## Solution

The fs reader treats a missing sidecar as empty entities and relationships.
