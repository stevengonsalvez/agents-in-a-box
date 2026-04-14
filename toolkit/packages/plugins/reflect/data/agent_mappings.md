# Agent Mappings Reference

Maps learning categories to target agent files for updating.

## Agent Directory Structure

```
~/.claude/agents/
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

### Architecture

| Learning Type | Primary Agent | Secondary Agents |
|--------------|---------------|------------------|
| Design patterns | `solution-architect` | `architecture-reviewer` |
| API design | `api-architect` | `backend-developer` |
| Database patterns | `backend-developer` | `migration` |
| Frontend patterns | `frontend-developer` | `architecture-reviewer` |
| System structure | `solution-architect` | `tech-lead-orchestrator` |

### Process

| Learning Type | Primary Agent | Global Config |
|--------------|---------------|---------------|
| Git workflow | `CLAUDE.md` | Commit hygiene section |
| CI/CD | `devops-automator` | Pipeline config |
| Code review | `code-reviewer` | Review checklist |
| Testing workflow | `test-writer-fixer` | Testing philosophy |
| Deployment | `release-manager` | Release process |

### Domain

| Learning Type | Primary Target | Notes |
|--------------|----------------|-------|
| Business rules | Project `.claude/agents/` | Domain-specific agent |
| Terminology | `CLAUDE.md` | Glossary section |
| Constraints | Project-specific agent | Validation rules |
| User requirements | `CLAUDE.md` | User preferences section |

### Tools

| Learning Type | Primary Target | Notes |
|--------------|----------------|-------|
| CLI preferences | `CLAUDE.md` | Tool usage section |
| Editor config | `.editorconfig` | Format settings |
| Docker usage | `devops-automator` | Container settings |
| Git settings | `CLAUDE.md` | Commit hygiene |

### Security

| Learning Type | Primary Agent | Secondary Agents |
|--------------|---------------|------------------|
| Input validation | `security-agent` | `code-reviewer` |
| Authentication | `security-agent` | `api-architect` |
| Authorization | `security-agent` | `backend-developer` |
| Encryption | `security-agent` | `backend-developer` |
| OWASP rules | `security-agent` | `code-reviewer` |

## Skill Creation vs Agent Update

| Criteria | Create Skill | Update Agent |
|----------|--------------|--------------|
| One-off solution to specific error | Yes | No |
| General preference | No | Yes |
| Debugging workaround | Yes | No |
| Code style rule | No | Yes |
| Configuration trick | Yes | Sometimes |
| Process preference | No | Yes |
