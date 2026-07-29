---
id: lrn-hangar-daemon-provider-contract-drift-a1f9c2
title: "Hangar daemon: fail-closed provider-contract-drift discriminator, keep field parsing permissive"
key_insight: "Make the outcome discriminator (subtype/is_error) a strict allowlist that fails closed into a distinct ProviderContractDrift reason on unrecognized terminal events, while keeping incidental field parsing permissive (serde default, never deny_unknown_fields) — conflating the two false-fails genuine successes."
scope: project
confidence: 0.9
learning_type: architecture-decision
source_episodes:
  - agent-a0d83711a2733e53f (deep-reasoner design memo, ainb-tui/ainb-hangar-daemon)
superseded_by: null
provenance:
  source_tool: reflect
  project: ainb-tui
category: architecture-decisions
problem: "Future claude/codex CLI terminal-event shape drift could either silently mark a failed/no-op task 'done' (exit-0 fallback) or over-eagerly fail a genuine success on a harmless new field."
root_cause: "classify_claude_result (runner.rs:499) is a DENYLIST (only error_*/is_error fail; everything else = Success), so an unrecognized non-error subtype silently resolves to Success instead of a distinct drift state."
fix: "Flip the terminal-outcome discriminator (subtype/is_error) to an ALLOWLIST that fails closed into a new FailureReason::ProviderContractDrift (NoRetry) on any unrecognized-but-terminal outcome, while keeping StreamLine field parsing permissive (#[serde(default)], no deny_unknown_fields)."
rule: "When hardening a state-machine discriminator against upstream schema drift, make the discriminator (enum/tag field) strict-allowlist and fail-closed-with-a-distinct-reason, but keep incidental field parsing permissive (serde default) — conflating 'strict on shape' with 'strict on fields' produces false-failures on harmless additions."
entities:
  - "ainb-hangar-daemon"
  - "runner.rs"
  - "classify_claude_result"
  - "finalize_outcome"
  - "FailureReason::ProviderContractDrift"
  - "StreamLine"
causal_relations:
  - {source: "denylist default arm in classify_claude_result", target: "silent-done on unrecognized subtype", type: "causes"}
  - {source: "allowlist discriminator + ProviderContractDrift arm", target: "silent-done on unrecognized subtype", type: "prevents"}
  - {source: "deny_unknown_fields on StreamLine", target: "false-fail on harmless new field", type: "causes"}
forget_after: null
---

## Problem

PR #410 made the Hangar daemon finalize tasks on the provider's (claude/codex
CLI) structured terminal event rather than exit code. The open question: what
happens when a *future* claude/codex CLI renames its terminal event or adds a
new non-error subtype the parser doesn't recognize?

Two independent designers (adversarial peer-review pair) converged on the
same "danger" framing but the memo corrected the initial premise: one of the
two failure modes described in the prompt was already handled by the merged
code, so the actual fix surface was narrower than assumed. Read the code
before proposing, don't just design against the prompt's stated danger.

## Root Cause

`finalize_outcome` (runner.rs:1452) already fails closed for the "no
terminal event at all" case (exit-0 + no terminal → `Failed{AgentError}`).
The real silent-done hole was `classify_claude_result` (runner.rs:499): a
**denylist** — only `error_*` subtypes or `is_error` map to failure,
everything else (including a hypothetical new success-flavored or
refusal/abort subtype) falls through to `Success`. Same pattern in codex's
`turn.completed => Success` being unconditional.

## Solution

Flip the *discriminator* to an allowlist, keep *field parsing* permissive:

1. `classify_claude_result`: keep `error_max_turns => IterationLimit` and
   `error*`/`is_error => AgentError` arms first (ordering matters — `is_error`
   must not swallow `IterationLimit`'s FreshRetry), add
   `Some("success") | None => Success`, then a final catch-all
   `Some(_) => Failure(ProviderContractDrift)`.
2. New `FailureReason::ProviderContractDrift` (NoRetry — deterministic drift
   shouldn't burn retry budget) in `service/fail.rs` (FAILURE_REASONS list +
   serde-drift + count-guard tests) and `service/retry.rs`.
3. `StreamLine` stays `#[serde(default)]` — never add `deny_unknown_fields`.
   That's the exact anti-pattern: it false-fails a genuine success the moment
   the provider adds one harmless field.
4. Capture CLI version as `tracing` metadata only on the drift event — never
   branch `finalize_outcome` on `--version` output (parsing that rots).
5. The existing live canary test already flips done→failed on any drift
   (built-in early warning) — gate it behind a scheduled `live` feature (not
   per-PR), assert presence not exact counts, and assert the failure reason
   to distinguish "provider contract drifted" from "agent genuinely failed."

## Anti-Pattern

Using `deny_unknown_fields` (or any field-level strictness) as the
fail-closed mechanism. It conflates "unrecognized outcome tag" (safe to fail
closed) with "unrecognized field" (should be ignored) — the latter fails
real, successful work the instant the provider ships an unrelated schema
addition.

Also rejected: reusing the generic `AgentError` reason for drift (kills
operator observability — can't tell "CLI shape changed" from "agent actually
failed") and making drift a `FreshRetry` disposition (deterministic drift
just re-fails identically, wasting retry budget).

## Context

Repo: `ainb-tui` (Rust). File: `crates/ainb-hangar-daemon/src/runner.rs`.
Pinned provider versions at the time: claude 2.1.211 / codex 0.144.0 (const
docs at runner.rs:~1470). `FailureReason` lives in
`crates/ainb-hangar-store/src/service/fail.rs`; retry disposition in
`service/retry.rs`.
