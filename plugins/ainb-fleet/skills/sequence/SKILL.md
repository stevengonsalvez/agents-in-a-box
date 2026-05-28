---
name: ainb-fleet:sequence
description: |
  Ordered multi-step prompts to fleet targets, ack-gated between steps via
  JSONL assistant-turn-end detection. Use for cycles like
  disconnect→reconnect→verify, or any flow where step N+1 requires step N
  to have completed first. The skill BLOCKS until each target's transcript
  shows the next assistant turn finishing OR per-step timeout fires
  (default 300s).
version: "0.1.0"
user-invocable: true
triggers:
  - ainb-fleet:sequence
  - fleet sequence
  - ordered prompts to fleet
  - multi-step ack-gated
allowed-tools:
  - Bash
---

# ainb fleet:sequence

Send N prompts in order. Between steps, wait for every target's next
assistant-turn-end before firing the next step.

## Run

```bash
ainb fleet sequence "step1" "step2" "step3" --all --timeout 300
```

## Args

| arg/flag | purpose |
|---|---|
| `<steps...>` | 1 or more prompts, fired in argv order |
| `--all` | required (v0.1 only supports --all targeting) |
| `--timeout <seconds>` | per-step ack deadline, default 300 |

## How ack-gating works

After each step (except the last):

```
for each target:
  watch  ~/.claude/projects/<cwd-slug>/<sid>.jsonl  via notify
  resolve when next row has:
    type     == "assistant"
    message.stop_reason == "end_turn"
  OR timeout fires
```

This guarantees claude has *finished responding* to step N before step
N+1 lands. The watch runs in parallel across all targets; the next step
fires when all targets either acked or timed out.

## Common flows

**Disconnect → reconnect cycle:**
```bash
ainb fleet sequence \
  "remote-control disconnect" \
  "remote-control" \
  --all
```

**Clear → resume:**
```bash
ainb fleet sequence \
  "/clear" \
  "continue from where you left off" \
  --all
```

**Verify-then-act:**
```bash
ainb fleet sequence \
  "/status" \
  "if status shows clean, proceed; otherwise abort" \
  --all
```

## Caveats

- **--all only in v0.1.** Filter/cwd selectors not yet wired for sequence.
  Workaround: use a wrapper script that builds a filter list then runs
  per-target sequences.
- **Ack detection requires JSONL access.** If `~/.claude/projects/` is
  unreadable (perms / sandboxing), ack always times out → step N+1
  fires after the timeout regardless of actual completion.
- **Default 300s is generous.** For chatty steps (analysis prompts, code
  generation), 300s is realistic. For trivial prompts (`/clear`), pass
  `--timeout 30` to fail fast.
- **Tail wallclock = sum of per-step wallclocks.** A 3-step sequence
  against a slow model can take minutes. Daemon mode keeps moving;
  sequence is intentionally synchronous.

## Verify ack-gating actually happened

Time the run:

```bash
time ainb fleet sequence "say hi" "say bye" --all --timeout 30
```

If elapsed is suspiciously short (< number of round-trips × ~3s), check
the JSONL transcripts to see if turn-ends were actually observed —
fall-through on timeout looks identical from the outside.
