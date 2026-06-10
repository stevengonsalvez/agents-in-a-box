---
# O1: consolidated observation (Hindsight memory_units fact_type=observation
# shape). Observations are the AGGREGATE layer over raw corrections:
# persona/convention-shaped statements ("this team prefers X", "this codebase
# generally does Y") that accumulate evidence over time. They are distinct
# from learnings (`type: learning` — one specific correction/rule/fix each)
# and from skills (workflow-shaped: how to do X).
#
# The live source of truth is the `observations` row in reflect.db:
# UPDATE passes append source_correction_ids there and bump proof_count by
# the number of NEW corrections cited, snapshotting the prior form into
# `observation_history` first — the values below mirror the row at write
# time and are NOT rewritten per query (the S9 immutable-frontmatter rule).
type: observation
id: obs-{{SLUG}}-{{HASH6}}
created: {{ISO_TIMESTAMP}}
updated: {{ISO_TIMESTAMP}}
scope: {{SCOPE}}                       # project | global
status: active                         # active | retired (non-destructive)
category: "{{CATEGORY}}"
statement: "{{STATEMENT}}"             # the convention/persona claim, ONE sentence
tags: [{{TAGS}}]
provenance:
  proof_count: {{PROOF_COUNT}}         # corrections aggregated; UPDATE increments
  source_correction_ids: [{{CORRECTION_IDS}}]  # learning ids; UPDATE appends, never duplicates
  detected_at: {{ISO_TIMESTAMP}}
---

## Observation

{{OBSERVATION_STATEMENT}}

## Evidence

{{EVIDENCE_SUMMARY}}

## Exceptions

{{KNOWN_EXCEPTIONS}}
