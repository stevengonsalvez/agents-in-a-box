---
name: claude-langfuse
description: >
  Claude Code observability skill: analyze session traces stored in Langfuse,
  extract learnings from corrections, identify success patterns, and propose
  agent/skill improvements based on historical data. Powers self-improvement
  through trace analysis of Claude Code sessions.
version: 1.0.0
allowed_tools:
  - Bash
  - Read
  - Write
  - Edit
  - Grep
  - Glob
  - Task
  - WebFetch
---

# Claude Langfuse Observability Skill

Analyze Claude Code session traces stored in Langfuse to extract learnings, identify patterns, and drive continuous improvement.

## Sub-Commands

| Command | Description |
|---------|-------------|
| `/claude-langfuse` | Show help and available sub-commands |
| `/claude-langfuse:status` | Current session status and recent traces |
| `/claude-langfuse:reflect` | Analyze recent sessions for learnings and corrections |
| `/claude-langfuse:insights [trace_id]` | Deep analysis of a specific session |
| `/claude-langfuse:patterns` | Identify recurring patterns across sessions |

## Usage

### Status Check
```
/claude-langfuse:status
```
Shows:
- Current session trace ID and observation count
- Last 5 sessions with quick stats
- Tool usage breakdown

### Reflect on Sessions
```
/claude-langfuse:reflect
/claude-langfuse:reflect --sessions 10
/claude-langfuse:reflect --since 2024-01-01
```
Analyzes traces to find:
- **High confidence signals**: Explicit corrections ("never", "always", "don't", "must")
- **Medium confidence signals**: Patterns that worked well, positive feedback
- **Low confidence signals**: Observations and preferences to review later

### Deep Insights
```
/claude-langfuse:insights <trace_id>
```
Provides detailed analysis of a specific session including:
- Full timeline of tool usage
- User prompt analysis
- Error patterns
- Success patterns

## Implementation

When this skill is invoked, execute the appropriate sub-command:

### For `/claude-langfuse` or `/langfuse:status`:

1. Query Langfuse API for recent traces
2. Display current session info
3. Show summary statistics

```bash
source ~/.secrets && python3 {{HOME_TOOL_DIR}}/skills/claude-langfuse/utils/status.py
```

### For `/claude-langfuse:reflect`:

1. Fetch recent session traces from Langfuse
2. Extract user prompts and tool outputs
3. Scan for correction signals (high/medium/low confidence)
4. Match learnings to relevant agent files
5. Propose updates with diff format
6. Present for user approval

```bash
source ~/.secrets && python3 {{HOME_TOOL_DIR}}/skills/claude-langfuse/utils/reflect.py $ARGUMENTS
```

### For `/claude-langfuse:insights <trace_id>`:

```bash
source ~/.secrets && python3 {{HOME_TOOL_DIR}}/skills/claude-langfuse/utils/insights.py $ARGUMENTS
```

## Signal Detection Patterns

### High Confidence (Explicit Corrections)
- "never do X", "don't ever Y"
- "always check Z", "must verify"
- "stop doing X", "wrong approach"
- Repeated corrections for same issue

### Medium Confidence (Success Patterns)
- "perfect", "exactly what I wanted"
- "good approach", "keep doing this"
- Approved solutions that can be templated

### Low Confidence (Observations)
- Preferences mentioned in passing
- One-time edge cases
- Context-specific decisions

## Learning Categories

| Category | Examples | Target Files |
|----------|----------|--------------|
| Code Style | Formatting, naming conventions | agents/code-reviewer.md |
| Architecture | Design patterns, boundaries | agents/solution-architect.md |
| Process | Workflow, review practices | CLAUDE.md |
| Tools | Preferred utilities, commands | agents/superstar-engineer.md |
| Domain | Project-specific knowledge | Project CLAUDE.md |

## Output Format

### Reflect Output
```
═══════════════════════════════════════════════════════════════
  LANGFUSE REFLECT - Session Analysis
═══════════════════════════════════════════════════════════════

Sessions Analyzed: 5
Time Range: 2024-01-08 to 2024-01-10

┌─────────────────────────────────────────────────────────────┐
│ HIGH CONFIDENCE SIGNALS (3 found)                           │
├─────────────────────────────────────────────────────────────┤
│ [1] "Never guess file paths - always verify with ls first"  │
│     Session: abc123... @ 2024-01-09                         │
│     Target: agents/superstar-engineer.md                    │
│     Proposed: Add to working rules section                  │
├─────────────────────────────────────────────────────────────┤
│ [2] "Always use ast-grep for code searches"                 │
│     Session: def456... @ 2024-01-10                         │
│     Target: CLAUDE.md                                       │
│     Proposed: Already exists - reinforce                    │
└─────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────┐
│ MEDIUM CONFIDENCE SIGNALS (2 found)                         │
├─────────────────────────────────────────────────────────────┤
│ [1] User approved parallel agent pattern                    │
│     Session: ghi789... @ 2024-01-10                         │
│     Pattern: Launch 3+ agents for independent tasks         │
└─────────────────────────────────────────────────────────────┘

Apply these learnings? [Y/n/modify]:
```

## Integration with Hooks

The Langfuse hooks (`session_start`, `pre_tool_use`, `post_tool_use`, `stop`) automatically capture:
- Session metadata (project, branch, user)
- All tool invocations with inputs/outputs
- User prompts
- Timing information

This skill reads that data to power reflection and insights.

## Configuration

Requires Langfuse credentials in `~/.secrets`:
```bash
export LANGFUSE_PUBLIC_KEY="pk-lf-..."
export LANGFUSE_SECRET_KEY="sk-lf-..."
export LANGFUSE_HOST="https://cloud.langfuse.com"  # optional
```
