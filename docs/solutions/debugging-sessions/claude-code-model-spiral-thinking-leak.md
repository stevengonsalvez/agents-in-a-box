---
title: "Claude Code model spiral — extended thinking leak and system prompt overload"
category: debugging-sessions
tags: [claude-code, extended-thinking, system-prompt, hooks, model-spiral, opus, sonnet]
symptoms:
  - "Model enters deliberation loop, generating visible internal reasoning as output"
  - "Dotted lines (. . . . .) appearing between text blocks in Claude Code response"
  - "Model says 'I was given internal processing/analysis content that I mistook for needing to respond to'"
  - "Response never terminates — keeps generating 'walks away from keyboard', 'silence', 'truly final'"
  - "Cost spikes ($100+) on a single turn with no useful output"
root_cause: "Two interacting issues: (1) Extended thinking tokens leaking into the visible conversation stream, causing the model to see its own deliberation as context it must respond to, creating a feedback loop. (2) Massive system prompt (25KB+ global CLAUDE.md + project CLAUDE.md + 70 skill descriptions + 15+ hooks injecting content) overwhelming the model on ambiguous questions."
key_insight: "Reduce hook density (especially 'entire' hooks on every event), trim global CLAUDE.md to essentials, and ensure startup scripts log to files not stdout — verbose echo statements in SessionStart hooks become system prompt content"
created: "2026-03-16"
confidence: high
language: n/a
framework: claude-code
---

## Problem

Claude Code sessions spiral into infinite deliberation loops where the model generates
visible internal reasoning, can't settle on a response, and produces $100+ in API costs
on a single turn. The output contains lines like "walks away from keyboard", "comes back",
"silence", "Response delivered", "truly final" interspersed with dotted lines representing
leaked thinking tokens.

## Root Cause

Two compounding factors:

### 1. Extended Thinking Content Leaking
Thinking block tokens render partially in the output stream. The model sees its own
deliberation as conversation context, enters a feedback loop, and can't break out.
The model explicitly recognizes this: "I was given internal processing/analysis content
that I mistook for needing to respond to."

### 2. System Prompt Overload
The combined system prompt payload is enormous:
- Global `~/.claude/CLAUDE.md`: 25KB+ (~7K tokens)
- Project `.claude/CLAUDE.md`: 12KB (~3K tokens)
- 70 global skill descriptions in system-reminder
- 11 project-specific skills
- 32 project-specific agents
- Auto-memory files from multiple sources
- SessionStart hook output (git status + tmux sessions + orphan warnings)

With 15+ hooks (global + project) running per interaction, every user message triggers
multiple scripts that can inject additional context.

### 3. Noisy Startup Scripts
Project-level `claude-install.sh` runs on every SessionStart with verbose debug output:
```bash
exec 2>&1  # Redirects stderr to stdout
echo "========== HOOK DEBUG START =========="
echo "Date: $(date)"
echo "Script: $0"
# ... 40+ lines of echo statements
```
This output becomes part of the system prompt.

## Solution

### Immediate
1. Check for CLI updates: `claude update` (thinking leak may be version-specific)
2. Kill the spiraling session and start fresh

### Structural
1. **Reduce hook density**: Disable project-level `entire` hooks on `UserPromptSubmit`,
   `Stop`, and `PostToolUse` if not essential — each injects content per turn
2. **Trim global CLAUDE.md**: Move rarely-needed sections (image manipulation, background
   server execution, session management) to on-demand skills
3. **Fix startup scripts**: Replace `echo` statements with file logging:
   ```bash
   # Instead of: echo "Debug info" (goes to system prompt)
   echo "Debug info" >> /tmp/claude-install-debug.log
   ```
4. **Reduce skill count**: 70 skill descriptions in the system-reminder is heavy —
   consider grouping or lazy-loading

## Context

This primarily affects sessions with large project-level configurations (many hooks,
skills, agents) combined with Opus or Sonnet models using extended thinking. Simple
projects with minimal hooks don't exhibit the behavior.

The `entire` integration adds hooks on PreToolUse, PostToolUse, Stop, UserPromptSubmit,
SessionStart, and SessionEnd — 6 hook points that all inject content into the
conversation context.
