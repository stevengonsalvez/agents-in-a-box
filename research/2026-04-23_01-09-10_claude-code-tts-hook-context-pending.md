# Research: Claude Code TTS Hook Context and Pending-State Announcements

**Date**: 2026-04-23T01:09:10+01:00  
**Repository**: stevengonsalvez_agents-in-a-box_fix_post-hook  
**Branch**: `fix/post-hook`  
**Commit**: `33c4499c72076f1ba83737d9e5cab980c614df68`  
**Research Type**: Comprehensive (Codebase + Official Docs)

## Research Question

How can the existing Claude Code TTS hook setup announce which session/worktree needs attention, and can it also tell us what is still pending or whether the agent is stopped and waiting for the next task?

## Executive Summary

Yes, the current hook setup can announce a useful human label immediately, because Claude Code already passes `cwd` to the relevant hooks and subagent hooks also include `agent_type` and `agent_id`. The missing part is not raw metadata, it is that the current scripts ignore it and speak generic phrases.

Pending work is not exposed directly by Claude Code on `Stop`, `SubagentStop`, or `Notification`. The practical path is to infer it from the latest `TodoWrite` snapshot in `transcript_path` or `agent_transcript_path`, using the same normalization this repo already uses in `ainb-tui`.

## Key Findings

- The current `Notification` hook intentionally suppresses TTS for the exact idle message `"Claude is waiting for your input"`, which is the most important case for this request.
- `Stop` and `SubagentStop` already receive enough metadata to speak a project/worktree label, but the current scripts ignore it.
- Official Claude Code docs expose `cwd`, `message`, `title`, `notification_type`, `last_assistant_message`, `agent_type`, and `agent_transcript_path`, but do not expose a ready-made pending-task list for these hook events.
- This repo already contains transcript parsing logic for `TodoWrite` that counts `pending`, `in_progress`, and `done`. That logic can be reused in hook code.
- If Agent Teams are in use, `TeammateIdle` and `TaskCompleted` are cleaner event hooks for "waiting" and "ready for next task" than generic `Stop` or `Notification`.

## Prior Learnings

### Relevant Past Solutions

| Learning | Key Insight | Confidence |
|----------|-------------|------------|
| `docs/solutions/debugging-sessions/claude-code-model-spiral-thinking-leak.md` | Keep hooks lightweight and avoid noisy stdout/context injection from hook scripts. | high |
| `docs/solutions/build-errors/nested-claude-session-error.md` | Do not invoke `claude` from hooks or detached subprocesses without guarding nested-session state. | high |

No direct prior learning about TTS pending announcements was found.

## Detailed Findings

### 1. Current Hook Wiring

- `Notification` is wired in [toolkit/claude-code-4.5/settings.json](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/claude-code-4.5/settings.json:28) and runs `uv run ~/.claude/hooks/notification.py --notify`.
- `Stop` is wired in [toolkit/claude-code-4.5/settings.json](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/claude-code-4.5/settings.json:70) and runs `uv run ~/.claude/hooks/stop.py --chat`.
- `SubagentStop` is wired in [toolkit/claude-code-4.5/settings.json](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/claude-code-4.5/settings.json:92) and runs `uv run ~/.claude/hooks/subagent_stop.py`.
- `SessionStart` is wired in [toolkit/claude-code-4.5/settings.json](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/claude-code-4.5/settings.json:124) but does not currently run with `--announce`.

### 2. What the Current Scripts Actually Announce

#### `stop.py`

- The TTS message is generic and comes from either a small LLM helper or a random fallback string such as `"Work complete!"` or `"Ready for next task!"` in [toolkit/packages/utilities/hooks/stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/stop.py:37).
- The hook reads `session_id` and `stop_hook_active`, logs the input payload, and can export the transcript to `logs/chat.json`, but it does not use any of that in the spoken message: [stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/stop.py:132), [stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/stop.py:169), [stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/stop.py:195).

#### `subagent_stop.py`

- The spoken message is always exactly `"Subagent Complete"` in [toolkit/packages/utilities/hooks/subagent_stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/subagent_stop.py:53).
- Like `stop.py`, it logs input data but does not use agent metadata in speech: [subagent_stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/subagent_stop.py:79).

#### `notification.py`

- The script can optionally include `ENGINEER_NAME`, but only 30% of the time: [toolkit/packages/utilities/hooks/notification.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/notification.py:65).
- It explicitly suppresses TTS when `message == "Claude is waiting for your input"` in [notification.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/notification.py:123). That is the main behavior currently blocking the requested feature.

### 3. Official Claude Code Hook Capabilities

The official Claude Code hooks reference confirms:

- All relevant hooks receive common fields including `session_id`, `transcript_path`, and `cwd`.
- `Notification` additionally receives `message`, optional `title`, and `notification_type` such as `permission_prompt`, `idle_prompt`, `auth_success`, and `elicitation_dialog`.
- `SubagentStop` additionally receives `agent_id`, `agent_type`, `agent_transcript_path`, and `last_assistant_message`.
- `Stop` additionally receives `stop_hook_active` and `last_assistant_message`.

Source: Claude Code hooks reference, [https://code.claude.com/docs/en/hooks](https://code.claude.com/docs/en/hooks)

Important implication: there is enough metadata to identify the worktree/session in speech, but there is no built-in pending-task summary on these events.

### 4. Best Available Session/Worktree Label

The simplest reliable label is derived from `cwd`:

- `Path(cwd).name` gives the current worktree or folder name.
- The repo’s statusline already computes project and branch labels from `cwd` and git state in [toolkit/packages/utilities/hooks/statusline.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/statusline.py:118), [statusline.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/statusline.py:130), [statusline.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/statusline.py:147), [statusline.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/statusline.py:157).

If a tmux/dev-session metadata file exists, `session_start.py` already knows how to derive richer labels like `session`, `project_name`, `branch`, and `dev_port`; that can be used later, but it is not required for the first implementation.

### 5. Can We Infer Pending Work?

Yes, but only indirectly from the transcript JSONL.

This repo already normalizes `TodoWrite` snapshots in `ainb-tui`:

- `TodoWrite` items are parsed from top-level `todos`.
- Item text is read from `text`, then `task`, then `content`.
- Status normalization is:
  - `done|completed` => done
  - `in_progress|active` => in progress
  - everything else => pending

References:

- [ainb-tui/src/agent_parsers/claude_json.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/agent_parsers/claude_json.rs:306)
- [ainb-tui/src/agent_parsers/claude_json.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/agent_parsers/claude_json.rs:321)
- [ainb-tui/src/agent_parsers/claude_json.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/agent_parsers/claude_json.rs:650)
- [ainb-tui/src/docker/log_streaming.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/docker/log_streaming.rs:596)

The safest rule is: **latest `TodoWrite` snapshot wins**.

That matches both:

- the parser behavior in `ainb-tui`, and
- the post-tool-use hook which hashes the whole todo list snapshot rather than treating it as a patch: [toolkit/packages/utilities/hooks/post_tool_use.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/post_tool_use.py:94).

### 6. Which Hook Should Announce "Waiting"?

For the current hook stack:

- `Notification` with `notification_type == "idle_prompt"` is the correct built-in signal for "Claude is waiting for your input".
- `Stop` tells you Claude finished a turn, but not necessarily that the session is now waiting for human input.

If Agent Teams are in use, official and repo docs suggest even better events:

- `TeammateIdle` for "this teammate is idle and ready"
- `TaskCompleted` for "this task finished"

Those are better than generic `Stop`, but they are team-specific and not required for the first implementation.

### 7. Constraints and Risks

- Keep the hook lightweight. A full transcript scan on every notification is unnecessary and risks noisy or slow hooks. The repo already contains examples of reading only the tail of a transcript for performance in [statusline.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/statusline.py:39) and [action-summary.js](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/action-summary.js:10).
- Do not invoke `claude` from these hooks. The repo has a documented nested-session failure mode when hooks spawn Claude: [docs/solutions/build-errors/nested-claude-session-error.md](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/docs/solutions/build-errors/nested-claude-session-error.md:59).
- Avoid noisy stdout or extra context injection from hook scripts. The repo documents hook-density/system-prompt risks in [docs/solutions/debugging-sessions/claude-code-model-spiral-thinking-leak.md](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/docs/solutions/debugging-sessions/claude-code-model-spiral-thinking-leak.md:67).

## Code References

- [toolkit/packages/utilities/hooks/stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/stop.py:132) - main completion TTS path
- [toolkit/packages/utilities/hooks/subagent_stop.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/subagent_stop.py:53) - fixed subagent completion phrase
- [toolkit/packages/utilities/hooks/notification.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/notification.py:123) - current idle-message suppression
- [toolkit/packages/utilities/hooks/statusline.py](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/toolkit/packages/utilities/hooks/statusline.py:106) - existing project/branch label logic
- [ainb-tui/src/agent_parsers/claude_json.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/agent_parsers/claude_json.rs:306) - todo snapshot parsing
- [ainb-tui/src/docker/log_streaming.rs](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/ainb-tui/src/docker/log_streaming.rs:596) - todo count rendering

## Recommendations

1. Change `notification.py` so `idle_prompt` speaks a useful label instead of being suppressed.
2. Add a shared helper for:
   - session/worktree label generation from `cwd` and git state
   - extracting the latest todo snapshot from `transcript_path` or `agent_transcript_path`
   - formatting short TTS-safe summaries such as `2 pending, 1 in progress`
3. Update `stop.py` and `subagent_stop.py` to use that shared helper and include:
   - project/worktree label
   - subagent type for subagent stops
   - pending summary when available
4. Keep Agent Teams hooks (`TeammateIdle`, `TaskCompleted`) as a follow-up, not part of the first pass.

## Decision

The best first implementation is:

- `Notification` handles "needs your input" and "permission needed"
- `Stop` handles "main turn finished"
- `SubagentStop` handles "subagent finished"
- all three use one shared, transcript-aware helper
- the spoken label is `project + branch` when git is available, otherwise `Path(cwd).name`

That gives clear, low-risk signal without adding new external dependencies or new hook classes.

## Sources

- Claude Code hooks reference: [https://code.claude.com/docs/en/hooks](https://code.claude.com/docs/en/hooks)
- Repo research note on Agent Teams: [research/2026-02-17_10-30-00_claude-code-agent-teams.md](/Users/stevengonsalvez/.agents-in-a-box/worktrees/stevengonsalvez_agents-in-a-box_fix_post-hook/research/2026-02-17_10-30-00_claude-code-agent-teams.md:1)

