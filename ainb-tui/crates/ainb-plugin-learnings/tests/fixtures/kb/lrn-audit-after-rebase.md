---
title: "Audit after large rebase before continuing prior plan"
category: "process"
type: LEARNING
scope: "universal"
confidence: 0.85
key_insight: "When `git pull --rebase` reveals many commits landed (>30), STOP the in-flight plan and audit."
tags: ['process', 'git', 'rebase', 'workflow']
provenance:
  source_tool: "claude"
  source_path: "/Users/test/.claude/memory/feedback_audit_after_rebase.md"
  content_hash: "147209f82dae87c6"
  ingested_at: "2026-05-07T14:52:33.890366+00:00"
---

## Problem

Long-lived plans go stale fast. When a `git fetch` reveals many commits landed
since the plan started, blindly continuing risks redundant or conflicting work.

## Solution

After every `git fetch`, gate continuation on commit count: if >30, audit before
continuing; skim file paths for overlap; check merged PRs.
