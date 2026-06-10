---
type: learning
id: lrn-{{SLUG}}-{{HASH6}}
created: {{ISO_TIMESTAMP}}
updated: {{ISO_TIMESTAMP}}
scope: {{SCOPE}}
confidence: {{CONFIDENCE}}
learning_type: {{LEARNING_TYPE}}
title: "{{TITLE}}"
tags: [{{TAGS}}]
symptoms:
  - "{{SYMPTOM_1}}"
key_insight: "{{KEY_INSIGHT}}"
# S1: structured extraction fields (Hindsight fact_extraction shape).
# Typed distillations of the prose body — recall can return just one field
# (`recall.py --field rule`) instead of the whole note. Be SELECTIVE and
# CONCISE: one strong sentence each; only what stays useful 6 months out.
problem: "{{PROBLEM_ONE_LINER}}"        # what went wrong, 1 sentence
root_cause: "{{ROOT_CAUSE}}"            # the underlying cause, 1 sentence
fix: "{{FIX_ONE_LINER}}"                # what resolved it, 1 sentence
rule: "{{RULE}}"                        # imperative do/don't to follow next time
category: "{{CATEGORY}}"                # e.g. build-errors | debugging-sessions
entities: [{{ENTITIES}}]                # specific named tech/tools/errors involved
causal_relations:                       # cause -> effect chains ([] when none)
  - source: "{{CAUSE_ENTITY}}"
    target: "{{EFFECT_ENTITY}}"
    type: caused_by
links: []
source_episodes: [{{EPISODE_ID}}]
superseded_by: null
# A3: per-row TTL. ISO timestamp after which the hourly forget sweep archives
# this learning; null = permanent. Set ONLY for clearly time-bounded knowledge
# ("avoid X service during the incident", "valid for the current migration /
# sprint / quarter") — durable rules must stay null.
forget_after: null
provenance:
  source_tool: "{{SOURCE_TOOL}}"      # claude | codex | copilot | gemini
  source_path: "{{SOURCE_PATH}}"
  content_hash: "{{CONTENT_HASH}}"
  detected_at: {{ISO_TIMESTAMP}}
  source_memory_ids: [{{EPISODE_ID}}]  # unique; UPDATE appends, never duplicates
  proof_count: 1                       # CREATE starts at 1; UPDATE increments
---

## Problem

{{PROBLEM_DESCRIPTION}}

## Solution

{{SOLUTION_STEPS}}

## Anti-Pattern

{{ANTI_PATTERN}}

## Context

{{ADDITIONAL_CONTEXT}}
