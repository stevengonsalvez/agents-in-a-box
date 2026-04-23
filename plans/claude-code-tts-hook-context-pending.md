# Claude Code TTS Hook Context + Pending Summary Plan

## Overview

Update the existing Claude Code TTS hooks so they announce which worktree/session needs attention and, when possible, summarize pending work from the latest todo snapshot. Keep the first implementation inside the current hook stack: `Notification`, `Stop`, and `SubagentStop`.

## Current State Analysis

- `notification.py` suppresses TTS for the idle prompt, which is the main "I need your input" case.
- `stop.py` and `subagent_stop.py` speak generic phrases and ignore available context like `cwd`, `agent_type`, and transcript paths.
- Claude Code does not provide pending-task counts directly on these hook events.
- The repo already has transcript-tail and todo-normalization patterns we can reuse.

### Key Discoveries

- [toolkit/packages/utilities/hooks/notification.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/notification.py:123) skips the idle TTS case entirely.
- [toolkit/packages/utilities/hooks/statusline.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/statusline.py:130) already derives project and branch labels from `cwd`.
- [ainb-tui/src/agent_parsers/claude_json.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/agent_parsers/claude_json.rs:306) already defines the right `TodoWrite` normalization.
- Official docs confirm `Notification`, `Stop`, and `SubagentStop` expose enough metadata for identification, but not a pending summary.

## Desired End State

After implementation:

- idle notifications speak a useful label like `agents in a box, fix post hook, waiting for input`
- completion hooks speak which worktree finished, and subagent completions also include `agent_type` when available
- hooks append a short todo summary like `2 pending, 1 in progress` when a recent `TodoWrite` snapshot is present
- transcript parsing is bounded to the file tail and does not require new services or long-running work
- all behavior is covered by script-level tests

## What We're NOT Doing

- We are not adding new external APIs, databases, or daemon processes.
- We are not invoking `claude` from hooks.
- We are not scanning full transcripts from the beginning on every hook run.
- We are not adding Agent Teams-specific hook events in this first pass.
- We are not changing the existing TTS backend priority order.

## Implementation Approach

Create one shared helper module for label-building and todo-summary extraction, then rewire `notification.py`, `stop.py`, and `subagent_stop.py` to use deterministic message builders. Reuse the repo’s existing todo status normalization instead of inventing new semantics.

## Phase 1: Shared Hook Context Utilities
<!-- wave: 1 | depends_on: [] | files: [toolkit/packages/utilities/hooks/utils/hook_context.py, toolkit/packages/utilities/hooks/test_hooks.py] -->

### Overview

Add a shared utility layer so all hook scripts compute labels and pending summaries the same way.

### Changes Required

#### 1. Add shared label + transcript helpers
**File**: `toolkit/packages/utilities/hooks/utils/hook_context.py`  
**Changes**:

- Add `build_session_label(cwd: str) -> str`
- Prefer `git root basename + branch` when available
- Fall back to `Path(cwd).name`
- Normalize TTS-unfriendly separators like `/` into speech-friendly text

```python
def build_session_label(cwd: str) -> str: ...
def extract_latest_todo_snapshot(transcript_path: str) -> dict | None: ...
def summarize_todos(snapshot: dict) -> str | None: ...
```

#### 2. Add bounded transcript parsing
**File**: `toolkit/packages/utilities/hooks/utils/hook_context.py`  
**Changes**:

- Read only the tail of the transcript file
- Walk backward through recent JSONL entries
- Find the latest assistant `TodoWrite` snapshot
- Support the `user.tool_result.content` JSON fallback
- Reuse the parser normalization:
  - `done|completed` => done
  - `in_progress|active` => in progress
  - otherwise => pending

#### 3. Add utility tests
**File**: `toolkit/packages/utilities/hooks/test_hooks.py`  
**Changes**:

- Add transcript fixtures inline in tests
- Verify latest-snapshot-wins behavior
- Verify `active` is treated as in-progress
- Verify label fallback behavior outside git

### Success Criteria

#### Automated Verification

- [ ] `uv run pytest toolkit/packages/utilities/hooks/test_hooks.py`
- [ ] Helper returns correct counts for `pending`, `in_progress`, and `done`
- [ ] Helper handles missing transcript files without failing hooks

#### Manual Verification

- [ ] Helper-generated label is short enough to sound natural in TTS
- [ ] Transcript parsing stays fast on a real session transcript

## Phase 2: Wire Main TTS Hooks
<!-- wave: 2 | depends_on: [Phase 1] | files: [toolkit/packages/utilities/hooks/notification.py, toolkit/packages/utilities/hooks/stop.py, toolkit/packages/utilities/hooks/subagent_stop.py] -->

### Overview

Replace generic spoken phrases with deterministic, context-aware messages.

### Changes Required

#### 1. Update `notification.py`
**File**: `toolkit/packages/utilities/hooks/notification.py`  
**Changes**:

- Remove the special-case suppression for `"Claude is waiting for your input"`
- Branch on `notification_type`
- Use `build_session_label(cwd)`
- For `idle_prompt`, include pending summary when available
- For `permission_prompt`, include a short version of the permission message

```python
if notification_type == "idle_prompt":
    message = f"{label} is waiting for input"
elif notification_type == "permission_prompt":
    message = f"{label} needs permission"
```

#### 2. Update `stop.py`
**File**: `toolkit/packages/utilities/hooks/stop.py`  
**Changes**:

- Replace fully generic completion text with a deterministic builder
- Include session label in every spoken message
- Use `transcript_path` to append todo summary when available
- Keep current silent-failure behavior and Langfuse handling

#### 3. Update `subagent_stop.py`
**File**: `toolkit/packages/utilities/hooks/subagent_stop.py`  
**Changes**:

- Use `agent_type` in the spoken message when present
- Prefer `agent_transcript_path` over main `transcript_path` for todo summary
- Speak `{agent_type} complete in {label}` or equivalent compact phrasing

### Success Criteria

#### Automated Verification

- [ ] `uv run pytest toolkit/packages/utilities/hooks/test_hooks.py`
- [ ] Notification tests cover `idle_prompt` and `permission_prompt`
- [ ] Stop/subagent tests verify message text includes label and counts when present

#### Manual Verification

- [ ] Idle notification now speaks instead of staying silent
- [ ] Main completion announces the correct worktree
- [ ] Subagent completion includes meaningful identity instead of just `Subagent Complete`
- [ ] Spoken messages remain short and understandable

### Checkpoint

- **`[CHECKPOINT:human-verify]`**: Confirm the spoken phrasing is useful before adding any extra hook events
  - What was built: context-aware TTS for idle, main stop, and subagent stop
  - How to verify:
    1. Trigger an idle notification and confirm it names the right worktree
    2. Complete a main-agent turn and confirm it speaks the label and any pending summary
    3. Complete a subagent turn and confirm it speaks the agent type and worktree
  - Resume: Type `approved` or describe the phrasing to adjust

## Phase 3: Test Hardening and Edge Cases
<!-- wave: 3 | depends_on: [Phase 2] | files: [toolkit/packages/utilities/hooks/test_hooks.py] -->

### Overview

Lock down edge cases so the hooks stay safe and quiet under imperfect input.

### Changes Required

#### 1. Add malformed-input coverage
**File**: `toolkit/packages/utilities/hooks/test_hooks.py`  
**Changes**:

- Missing `cwd`
- Missing or unreadable transcript path
- Transcript without any `TodoWrite`
- Transcript with only completed tasks
- Notification events with unexpected `notification_type`

#### 2. Add phrasing regression coverage
**File**: `toolkit/packages/utilities/hooks/test_hooks.py`  
**Changes**:

- Ensure messages do not include raw session IDs
- Ensure branch separators and underscores are normalized for speech
- Ensure counts are omitted when no todo snapshot exists

### Success Criteria

#### Automated Verification

- [ ] `uv run pytest toolkit/packages/utilities/hooks/test_hooks.py`
- [ ] All new edge-case fixtures pass
- [ ] Hook scripts still exit `0` on malformed input

#### Manual Verification

- [ ] Hooks remain silent on parsing failures rather than breaking Claude Code flow
- [ ] Messages stay concise and non-repetitive across repeated events

## Testing Strategy

### Script-Level Tests

- Feed synthetic hook JSON into each script with `subprocess.run`
- Use temporary transcript files containing realistic JSONL fragments
- Assert message-building logic without requiring live TTS playback

### Integration Checks

- Trigger one real `idle_prompt`
- Trigger one real main `Stop`
- Trigger one real `SubagentStop`
- Confirm logs still write to `logs/*.json`

### Manual Testing Steps

1. Start a Claude Code session in this worktree and wait for an idle notification.
2. Confirm the spoken message names this worktree and does not stay silent.
3. Create a todo list with pending and in-progress items, then finish a turn.
4. Confirm the completion message includes the correct counts.
5. Run a subagent and confirm its completion message includes the agent type.

## Performance Considerations

- Only read the tail of transcript files.
- Do not shell out more than needed for label generation.
- Avoid any LLM call for pending-summary construction.

## Migration Notes

- No settings change is required for the first pass.
- Existing hook registration remains valid.
- Existing TTS backend selection remains unchanged.

## Follow-Up, Explicitly Deferred

- Add `TeammateIdle` and `TaskCompleted` hooks for Agent Teams sessions.
- Add tmux-session-name speech if the current label is not sufficient in practice.

## References

- Research: [research/2026-04-23_01-09-10_claude-code-tts-hook-context-pending.md](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/research/2026-04-23_01-09-10_claude-code-tts-hook-context-pending.md:1)
- Existing hook config: [toolkit/claude-code-4.5/settings.json](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/claude-code-4.5/settings.json:27)
- Existing todo normalization: [ainb-tui/src/agent_parsers/claude_json.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/agent_parsers/claude_json.rs:306)
