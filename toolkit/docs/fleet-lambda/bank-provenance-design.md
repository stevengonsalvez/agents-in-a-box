# BANK Provenance Design

## Goal

Make BANK retrieval traceable end-to-end:
- what was searched
- what was injected
- what later correlated with a correction
- which retrieved hits were useful vs noisy

## Existing wiring

`injection_history.py` already exists as helper state:
- writer: `bank_lookup.py` calls `injection_history.record(...)`
- reader: `correction_detector.py` calls `injection_history.recent(session_id)`

This means join substrate already exists. Missing piece is **durable observability**.

## Required event types

### 1. `lookup`
Emitted when BANK evaluates candidate hits.

Required fields:
- `ts`
- `session_id`
- `agent`
- `query` or `keywords`
- `candidate_hit_ids`
- `selected_hit_ids`
- `score_summary`
- `token_budget`
- `source = bank_lookup`

### 2. `injection`
Emitted when selected hits are actually injected into context.

Required fields:
- `ts`
- `session_id`
- `injection_id`
- `selected_hit_ids`
- `rendered_count`
- `approx_tokens_used`
- `source = bank_lookup`

### 3. `correction_after_injection`
Emitted when downstream correction logic joins a correction with a recent injection.

Required fields:
- `ts`
- `session_id`
- `correction_id`
- `matched_injection_id`
- `matched_hit_ids`
- `confidence`
- `matched_signals`
- `source = correction_detector`

### 4. optional `retrieval_feedback`
Derived signal: useful, noisy, superseded, or neutral.

## Sink decision

Default recommendation:
- append durable JSONL under shared learnings/runtime area for simplicity and diffability
- optional later mirror/index into sqlite for aggregation

Suggested runtime sink names:
- `~/.clan/learnings/bank-lookups.jsonl`
- `~/.clan/learnings/bank-injections.jsonl`
- `~/.clan/learnings/corrections-after-injection.jsonl`

## Operator queries needed

- show recent lookups for session
- show injection trail for session
- show correction joins for session
- show hit usefulness summary over time

## Important shape rule

Do **not** turn `injection_history.py` into a hook just to make it visible.
Keep it as helper plumbing. Emit durable events around it instead.

## Success condition

Given a session id, operator can answer:
1. what BANK looked up
2. what got injected
3. what later triggered correction
4. which hits helped vs polluted context
