# Agent Mappings Reference

Maps learning categories to target agent files for updating.

## Agent Directory Structure

```
~/.claude/agents/
├── web-search-researcher.md    # Root-level agents (not in subdirectory)
├── universal/
│   ├── backend-developer.md
│   ├── frontend-developer.md
│   └── superstar-engineer.md
├── engineering/
│   ├── api-architect.md
│   ├── architecture-reviewer.md
│   ├── code-reviewer.md
│   ├── security-agent.md
│   ├── test-writer-fixer.md
│   └── ...
├── design/
│   └── ui-designer.md
├── orchestrators/
│   ├── tech-lead-orchestrator.md
│   └── ...
└── meta/
    └── agentmaker.md
```

## Category to Agent Mapping

### Code Style

| Learning Type | Primary Agent | Secondary Agents |
|--------------|---------------|------------------|
| Naming conventions | `code-reviewer` | `backend-developer`, `frontend-developer` |
| Formatting rules | `code-reviewer` | Project's `.editorconfig` or linter config |
| TypeScript/JavaScript style | `frontend-developer` | `code-reviewer` |
| Python style | `backend-developer` | `code-reviewer` |
| Go style | `backend-developer` | `code-reviewer` |
| Rust style | `backend-developer` | `code-reviewer` |

**Section in Agent File:**
```markdown
## Code Style
## Style Guidelines
## Heuristics > Style
```

### Architecture

| Learning Type | Primary Agent | Secondary Agents |
|--------------|---------------|------------------|
| Design patterns | `solution-architect` | `architecture-reviewer` |
| API design | `api-architect` | `backend-developer` |
| Database patterns | `backend-developer` | `migration` |
| Frontend patterns | `frontend-developer` | `architecture-reviewer` |
| System structure | `solution-architect` | `tech-lead-orchestrator` |

**Section in Agent File:**
```markdown
## Architecture Patterns
## Design Patterns
## Heuristics > Architecture
```

### Process

| Learning Type | Primary Agent | Global Config |
|--------------|---------------|---------------|
| Git workflow | `CLAUDE.md` | Commit hygiene section |
| CI/CD | `devops-automator` | Pipeline config |
| Code review | `code-reviewer` | Review checklist |
| Testing workflow | `test-writer-fixer` | Testing philosophy |
| Deployment | `release-manager` | Release process |

**Section in Agent File:**
```markdown
## Workflow
## Process Guidelines
## Heuristics > Process
```

### Domain

| Learning Type | Primary Target | Notes |
|--------------|----------------|-------|
| Business rules | Project `.claude/agents/` | Domain-specific agent |
| Terminology | `CLAUDE.md` | Glossary section |
| Constraints | Project-specific agent | Validation rules |
| User requirements | `CLAUDE.md` | User preferences section |

**Section in Agent File:**
```markdown
## Domain Rules
## Business Logic
## Terminology
```

### Tools

| Learning Type | Primary Target | Notes |
|--------------|----------------|-------|
| CLI preferences | `CLAUDE.md` | Tool usage section |
| Editor config | `.editorconfig` | Format settings |
| Docker usage | `devops-automator` | Container settings |
| Git settings | `CLAUDE.md` | Commit hygiene |

**Section in Agent File:**
```markdown
## Tool Usage
## CLI Preferences
## Environment Setup
```

### Security

| Learning Type | Primary Agent | Secondary Agents |
|--------------|---------------|------------------|
| Input validation | `security-agent` | `code-reviewer` |
| Authentication | `security-agent` | `api-architect` |
| Authorization | `security-agent` | `backend-developer` |
| Encryption | `security-agent` | `backend-developer` |
| OWASP rules | `security-agent` | `code-reviewer` |

**Section in Agent File:**
```markdown
## Security Heuristics
## Review Heuristics > Security
## Validation Rules
```

## Section Identification

When adding a learning to an agent file, identify the correct section:

### Pattern Matching for Sections

```regex
## Heuristics
## Style
## Guidelines
## Patterns
## Rules
## Best Practices
## Review Checklist
```

### Section Priority

1. Most specific matching section (e.g., "Security" for security rules)
2. Generic "Heuristics" section
3. "Guidelines" or "Rules" section
4. Create new section if none match

### Addition Format

Always add as a bullet point under the appropriate section:

```markdown
## Heuristics

* Existing rule 1
* Existing rule 2
* **New learning from reflection**
```

Use bold for newly added rules to distinguish them.

## Conflict Detection

Before adding a new rule, check for conflicts:

### Contradiction Patterns

```regex
# Opposite directives
"never use X" vs "always use X"
"prefer Y" vs "avoid Y"
"use Z" vs "don't use Z"

# Conflicting versions
"use library v1" vs "use library v2"
"Node 18" vs "Node 20"
```

### Resolution Strategy

1. **Newer wins**: If the new learning contradicts an old rule, flag for review
2. **Higher confidence wins**: HIGH > MEDIUM > LOW
3. **More specific wins**: "Never use var in TypeScript" > "Avoid var"
4. **User decision**: When equal, ask user to resolve

## File Paths

### Global Agents (User-level)

```
~/.claude/agents/universal/backend-developer.md
~/.claude/agents/universal/frontend-developer.md
~/.claude/agents/engineering/code-reviewer.md
~/.claude/agents/engineering/api-architect.md
~/.claude/agents/engineering/solution-architect.md
~/.claude/agents/engineering/security-agent.md
~/.claude/agents/engineering/test-writer-fixer.md
~/.claude/agents/design/ui-designer.md
```

### Project Agents (Project-level)

```
.claude/agents/{name}.md
```

### Global Instructions

```
~/.claude/CLAUDE.md
```

### Project Instructions

```
.claude/CLAUDE.md
CLAUDE.md
```

## Project Memory

Project-specific learnings that don't belong in agent files should be written to `.agents/MEMORY.md` in the repo root (git-tracked, 200-line max).

### Project Memory Mapping

| Learning Type | Primary Target | Notes |
|--------------|----------------|-------|
| Project-specific patterns | `.agents/MEMORY.md` | Architecture, conventions |
| Environment gotchas | `.agents/MEMORY.md` | Config, deploy quirks |
| Build/test insights | `.agents/MEMORY.md` | CI, toolchain issues |
| Domain knowledge | `.agents/MEMORY.md` | Business rules, terminology |

### `.agents/MEMORY.md` Sections

- Architecture & Patterns
- Build & Deploy
- Gotchas & Workarounds
- Testing
- Environment & Config
- [Dynamic sections based on project needs]

### Decision Flow -- Agent File vs `.agents/MEMORY.md`

| Signal | Target |
|--------|--------|
| Behavioral ("always do X") | Agent file |
| Project-specific architecture, gotcha, env quirk | `.agents/MEMORY.md` |
| Recurring bug with reusable fix | New skill |
| Domain term / business rule | `.agents/MEMORY.md` |

## Skill Creation vs Agent Update

Decide whether to create a new skill or update an agent:

| Criteria | Create Skill | Update Agent |
|----------|--------------|--------------|
| One-off solution to specific error | Yes | No |
| General preference | No | Yes |
| Debugging workaround | Yes | No |
| Code style rule | No | Yes |
| Configuration trick | Yes | Sometimes |
| Process preference | No | Yes |

## Example Mappings

### Example 1: Code Style

**Signal**: "Never use `var` in TypeScript, always use `const` or `let`"
**Confidence**: HIGH
**Category**: Code Style

**Mapping**:
- Primary: `~/.claude/agents/universal/frontend-developer.md`
- Section: `## Style Guidelines`
- Addition: `* Use \`const\` or \`let\` instead of \`var\` in TypeScript`

### Example 2: Architecture

**Signal**: "Prefer cursor-based pagination for APIs"
**Confidence**: MEDIUM
**Category**: Architecture

**Mapping**:
- Primary: `~/.claude/agents/engineering/api-architect.md`
- Section: `## Design Patterns`
- Addition: `* Prefer cursor-based pagination over offset-based for large datasets`

### Example 3: Security

**Signal**: "Always validate inputs on the server, never trust client validation"
**Confidence**: HIGH
**Category**: Security

**Mapping**:
- Primary: `~/.claude/agents/engineering/security-agent.md`
- Section: `## Validation Rules`
- Addition: `* Always validate inputs server-side; client validation is for UX only`
- Secondary: `~/.claude/agents/engineering/code-reviewer.md`
- Section: `## Review Heuristics > Security`

### Example 4: New Skill

**Signal**: "Fixed React hydration mismatch by ensuring server and client render same content"
**Confidence**: HIGH
**Category**: New Skill

**Mapping**:
- Create: `.claude/skills/react-hydration-mismatch-fix/SKILL.md`
- Include: Error message, symptoms, step-by-step fix, verification
