---
title: "Claude Code plugin auto-wiring via plugin.json hooks block"
category: "reference"
type: LEARNING
scope: "universal"
confidence: 0.8
key_insight: "plugin.json supports a top-level `hooks` block with `${CLAUDE_PLUGIN_ROOT}` substitution — declare hooks in the manifest instead of shipping copy-paste snippets."
tags: ['claude', 'plugins', 'hooks']
provenance:
  source_tool: "codex"
  source_path: "/Users/test/.codex/memory/reference_plugin_autowire.md"
  content_hash: "765276aabbccddee"
  ingested_at: "2026-05-20T18:00:00.000000+00:00"
  project: "agents-in-a-box"
---

## Problem

Plugins shipped hook snippets users had to copy-paste into settings.

## Solution

Declare a top-level `hooks` block in `plugin.json` with
`${CLAUDE_PLUGIN_ROOT}` substitution; the host auto-wires them.
