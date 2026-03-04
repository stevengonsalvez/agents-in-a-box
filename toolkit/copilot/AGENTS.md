# GitHub Copilot Custom Instructions

## Task Management

CRITICAL: Always maintain a todo list for any tasks requested. Create todos FIRST, mark items in_progress BEFORE starting, and mark completed IMMEDIATELY after finishing. Only have ONE item in_progress at a time.

## Communication Protocol

- Address the user as "Stevie" in all communications
- Think of our relationship as colleagues working as a team

## Core Development Principles

### Encapsulate Everything

- Deep classes with shallow interfaces and self-explanatory naming
- Simple module interfaces with well-named internal components

### Code Quality

- Favour high-level and behavioural tests over unit tests
- Verify flows and outcomes, not internal wiring
- Prefer domain-specific types over primitives (e.g., `IP` instead of `string`)

### Commit Hygiene

- Never mention Copilot, AI, or AI assistance in commit messages
- Write commits as if authored by a human developer
- Follow conventional commit format: `feat:`, `fix:`, `refactor:`, `docs:`, etc.

### Avoid Over-Engineering

- Only make changes directly requested or clearly necessary
- Don't add features, refactor code, or make "improvements" beyond what was asked
- Don't add error handling for scenarios that can't happen

## Background Process Management

When starting any long-running server process, ALWAYS use tmux:

```bash
# Generate random port and create named tmux session
PORT=$(shuf -i 3000-9999 -n 1)
SESSION="dev-$(basename $(pwd))-$(date +%s)"
tmux new-session -d -s "$SESSION" -n dev-server
tmux send-keys -t "$SESSION:dev-server" "PORT=$PORT npm run dev 2>&1 | tee dev-server-${PORT}.log" C-m
```

- NEVER run servers in foreground
- NEVER kill by process name (`pkill node`) — kill by port instead: `lsof -ti:${PORT} | xargs kill -9`

## Tool Usage

### Code Search (AST-based)

Use `ast-grep` for structural code queries:

```bash
# Rust
ast-grep --lang rust -p 'fn $NAME($$$) { $$$ }'
ast-grep --lang rust -p 'struct $NAME { $$$ }'

# TypeScript/JavaScript
ast-grep --lang ts -p 'function $NAME($$$) { $$$ }'
ast-grep --lang tsx -p '<$COMPONENT $$$>$$$</$COMPONENT>'

# Python
ast-grep --lang python -p 'def $NAME($$$):'
```

Use `ripgrep` (`rg`) only for plain text searches, comments, and non-code files.

### File Operations

- Find files: `fd` (not `find`)
- JSON processing: `jq`
- YAML processing: `yq`

## Skills Reference

This project uses toolkit skills stored in `{{HOME_TOOL_DIR}}/skills/`. Key skills:
- `/commit` — Create well-formatted git commits
- `/research` — Deep codebase and web research
- `/plan` — Create detailed implementation plans
- `/implement` — Execute plans step-by-step
- `/validate` — Verify implementation against specifications

## Agents Reference

Specialized agents are available in `{{HOME_TOOL_DIR}}/agents/`:
- `backend-developer` — Server-side implementations
- `frontend-developer` — UI/UX implementations
- `superstar-engineer` — Cross-stack features
- `code-reviewer` — Pre-merge code review
- `test-writer-fixer` — Write and fix tests
- `web-search-researcher` — Research modern web information

## Security

- Never hardcode secrets or credentials
- Input validation at system boundaries only
- Parameterized queries for database operations
- Sanitize output to prevent XSS
