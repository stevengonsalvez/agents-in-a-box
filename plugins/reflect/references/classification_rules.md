# Classification Rules

Rules for classifying signals by type, scope, and learning type.

## Signal Type: Behavioral vs Knowledge

| Signal | Type | Target |
|--------|------|--------|
| Behavioral correction ("always do X") | Behavioral | Agent file |
| Explicit preference ("use tabs not spaces") | Behavioral | Agent file or config |
| Solved problem with root cause | Knowledge | Learning note |
| Architecture/design decision | Knowledge | Learning note |
| Discovered pattern or anti-pattern | Knowledge | Learning note |
| Project-specific gotcha | Knowledge | Learning note + `.agents/MEMORY.md` |
| Recurring bug with reusable fix | Knowledge | Learning note (skill-worthy check) |
| Domain knowledge / business rule | Knowledge | Learning note + `.agents/MEMORY.md` |

## Knowledge Learning Type

| Signal Pattern | Learning Type | Examples |
|----------------|---------------|----------|
| "Fixed by", "root cause was", "resolved" | bug-fix | Debugging sessions that reached resolution |
| "Never do X", "always do Y", "wrong approach" | correction | Anti-patterns discovered through failure |
| "Pattern is", "best practice", "rule of thumb" | pattern | Reusable techniques worth preserving |
| "Chose X over Y because" | decision | Architecture or design decisions with rationale |
| "Avoid X", "anti-pattern", "gotcha" | anti-pattern | Pitfalls to document for future avoidance |
| "That worked", "it's fixed", "finally got it" | bug-fix | Trigger phrases indicating problem resolution |
| "The issue was", "the fix was" | bug-fix | Explicit root cause identification |

## Scope Auto-Detection

| Indicator | Scope | Example |
|-----------|-------|---------|
| References specific repo/service names | `scope: project:{name}` | "In the auth-service, always..." |
| References framework/language only | `scope: domain:{tech}` | "In React, prefer..." |
| Generic technique | `scope: universal` | "Never mutate state in..." |

## Behavioral Classification -> Agent Targets

| Category | Target Files |
|----------|--------------|
| Code Style | `code-reviewer`, `backend-developer`, `frontend-developer` |
| Architecture | `solution-architect`, `api-architect`, `architecture-reviewer` |
| Process | agent config file, orchestrator agents |
| Domain | Domain-specific agents, agent config file |
| Tools | agent config file, relevant specialists |
| New Skill | `.claude/skills/{name}/SKILL.md` |

See [agent_mappings.md](agent_mappings.md) for detailed mapping rules.

## Learning Type Details

| Learning Type | Description | Scope Detection |
|---------------|-------------|-----------------|
| `pattern` | Reusable technique or approach | Framework/language references -> `domain:{tech}` |
| `correction` | Wrong approach identified | Generic technique -> `universal` |
| `bug-fix` | Problem diagnosed and solved | Specific repo/service -> `project:{name}` |
| `decision` | Chose X over Y with rationale | Architecture context -> `project:{name}` or `domain:{tech}` |
| `anti-pattern` | What NOT to do | Generic -> `universal`, specific -> scoped |

## Skill-Worthy Check

Some learnings should become new skills rather than notes.

**Criteria (must pass all):**
- [ ] Reusable: Will help with future tasks
- [ ] Non-trivial: Requires discovery, not just docs
- [ ] Specific: Can describe exact trigger conditions
- [ ] Verified: Solution actually worked
- [ ] No duplication: Doesn't exist already

**Indicators:**
- Non-obvious debugging (>10 min investigation)
- Misleading error (root cause different from message)
- Workaround discovered through experimentation
- Configuration insight (differs from documented)
- Reusable pattern (helps in similar situations)

See [skill_template.md](skill_template.md) for skill creation guidelines.
