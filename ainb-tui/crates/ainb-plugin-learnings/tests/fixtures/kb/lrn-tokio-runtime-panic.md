---
title: "Owning a Tokio Runtime panics on drop inside tokio::main"
category: "reference"
type: LEARNING
scope: "project"
confidence: 1.0
key_insight: "A struct owning a `tokio::runtime::Runtime` panics on drop when dropped from inside a `#[tokio::main]` async context."
tags: ['rust', 'tokio', 'runtime', 'drop']
provenance:
  source_tool: "claude"
  source_path: "/Users/test/.claude/memory/reference_tokio_runtime_drop_trap.md"
  content_hash: "ab12cd34ef567890"
  ingested_at: "2026-05-12T09:10:00.000000+00:00"
---

## Problem

Owning a `TokioRuntime` panics on drop from inside `#[tokio::main]`.

## Solution

Wrap it in `Option<TokioRuntime>` plus an explicit `shutdown()` and a safe
`Drop` fallback.
