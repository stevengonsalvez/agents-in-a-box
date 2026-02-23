---
name: session-info
description: Display current session information and context
user-invocable: true
---

# Session Info Command

Display current session context including git status, recent work, and development context.

## What to Display

Immediately show:

1. **Current Session State**
   - Session ID
   - Current branch
   - Working directory status

2. **Git Status**
   - Current branch
   - Uncommitted changes count
   - Recent commits (last 3)

3. **Context Files** (if they exist)
   - `{{HOME_TOOL_DIR}}/CLAUDE.md` summary
   - `{{HOME_TOOL_DIR}}/TODO.md` items
   - `TODO.md` items

4. **Recent GitHub Issues** (if `gh` available)
   - Last 5 open issues

## Format

Present as a clean, readable summary banner:

```
+============================================================+
|                    SESSION INFORMATION                      |
+============================================================+

Location: /path/to/project
Branch: main
Changes: 3 uncommitted files

Recent Work:
- feat: add session start hook improvements
- fix: combine git status and context output
- docs: update CLAUDE.md with comment directives

Active TODOs:
[ ] Fix session start visibility
[ ] Test hook output format
[x] Update hook logic

Recent Issues:
#123 - Session start hook not displaying
#122 - Improve git status formatting
```

Be concise but informative.
