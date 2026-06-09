---
name: interview
description: |
  Interview the user about a plan file to extract detailed requirements,
  clarify ambiguities, and uncover edge cases. Uses iterative questioning
  to produce a comprehensive specification.
version: 1.0.0
authors:
  - Thariq Shihipar (original concept)
  - Rob Zolkos (gist adaptation)
  - Claude Code Toolkit (enhancement)
argument-hint: <plan-file>
model: opus
license: Apache-2.0

metadata:
  category: planning
  keywords: [interview, requirements, specification, planning, discovery]
  original-source: https://gist.github.com/robzolkos/40b70ed2dd045603149c6b3eed4649ad

compatibility: |
  Works with any LLM tool that supports:
  - File reading
  - Multi-turn questioning (AskUserQuestion or equivalent)
  - File writing

  Tested with: Claude Code, Codex (OpenAI)

allowed-tools:
  - Read
  - Write
  - AskUserQuestion
---

# Interview Skill

Conduct a detailed interview about a plan to extract comprehensive requirements and produce a specification.

## Purpose

Plans often contain assumptions, ambiguities, and unexplored edge cases. This skill systematically interviews the user to:

1. **Clarify ambiguities** - What did you really mean?
2. **Uncover assumptions** - What are you taking for granted?
3. **Explore edge cases** - What happens when X goes wrong?
4. **Validate priorities** - What's truly important vs nice-to-have?
5. **Surface constraints** - What are the real limitations?

## Usage

```
/interview path/to/plan.md
```

Or invoke directly:
```
interview path/to/plan.md
```

## Interview Process

```
┌────────┐   ┌──────────────┐   ┌──────────────┐   ┌────────────┐
│ Read   │──▶│ Detect       │──▶│ AskUser-     │──▶│ Write      │
│ input  │   │ embedded     │   │ Question     │   │ spec.md    │
│ file   │   │ sections     │   │ rounds 1..N  │   │ (template) │
└────────┘   └──────────────┘   └──────┬───────┘   └────────────┘
                                       │
                              previews from input
                              ASCII library section
```

### Step 1: Read and Analyze the Plan

Read the plan file provided as `$ARGUMENTS` (or `$1`):

```
Read the plan file at: $ARGUMENTS
```

Analyze the plan for:
- **Core objectives** - What is this trying to achieve?
- **Technical components** - What systems/technologies are involved?
- **User-facing aspects** - What will users see/interact with?
- **Dependencies** - What does this rely on?
- **Gaps** - What's missing or underspecified?

**Also scan for these embedded sections (used by `/brainstorm` topic stubs):**

- `## For /interview` — explicit instructions from the upstream skill. Follow these verbatim; they typically dictate first-round question shape, ASCII preview usage, and topic coverage.
- `## Initial hypotheses` — pre-populated A/B/C approach options. If present, your **first** AskUserQuestion round MUST present these as the options, with each option's ASCII code-block embedded in the `preview` field.
- `## ASCII preview library` — reusable preview snippets keyed to subject type. Use these in subsequent AskUserQuestion `preview` fields whenever you compare concrete shapes (mockups, schemas, diagrams).
- `## Output Spec Template` — a literal markdown template for the spec file output. If present, **use this template verbatim** for the spec instead of the default in Step 5.

These sections are how `/brainstorm` hands off the design context. Honor them.

### Step 2: Conduct the Interview

**MANDATORY: use a structured user-prompt tool. Do NOT dump questions in plaintext chat.**

Pick the tool in this order — first match wins:

1. **Claude Code**: `AskUserQuestion` (questions array, supports `multiSelect`).
2. **Codex / OpenAI Agents SDK**: `user_prompt` / `prompt_user` / `Prompt` / `ask_user` — whichever the host harness exposes natively. Codex CLI calls this differently across versions; use the version present in your tool list.
3. **Cursor / Windsurf / other coding agents**: their built-in user-question primitive (typically `request_input`, `ask`, or similar).
4. **Generic LLMs / no native tool**: only as last resort, fall back to a clearly-formatted plaintext block (`### Question 1: ...` / `### Question 2: ...`) and ask the user to answer inline.

Why this is forced: free-form plaintext questioning is unreliable across runs — agents skip the structured tool when given an "or equivalent" out, which produces lower-quality interviews and bad follow-up. The tool produces typed answers the agent can branch on; plaintext does not.

#### Format preferences for chat outputs and spec content

When invoked from `/brainstorm`, the input stub contains a "Format preferences" section that dictates output shape. Honor it. The default convention (matching Stevie's CLAUDE.md `<flow_diagrams>` rule):

| Content shape                                | Use                                              |
|----------------------------------------------|--------------------------------------------------|
| Flow / sequence / relationships / state      | ASCII box+arrow diagram (`┌─┐ │ └─┘ ─▶ ▼`)        |
| Tabular DATA (rows × columns of facts)       | markdown pipe table                              |
| Discrete items, no ordering                  | bullet list                                      |
| Picks / open questions                       | `- [ ]` checklist                                |
| Prose / narrative paragraphs                 | AVOID — break into one of the above              |

Rules:
- **Diagram FIRST, table SECOND** when both apply.
- Diagram width ≤ 80 chars. Caveman inside boxes.
- No prose-only sections in the produced spec. Every section = diagram + table + bullets.
- AskUserQuestion `preview` fields should follow the same convention.

Interview categories:

#### Technical Implementation
- Architecture choices and trade-offs
- Performance requirements and constraints
- Security considerations
- Error handling strategies
- Integration points

#### UI & UX (if applicable)
- User flows and journeys
- Edge cases in user interaction
- Accessibility requirements
- Responsive/mobile considerations

#### Business & Product
- Success metrics
- Priority of features
- MVP vs future scope
- Stakeholder concerns

#### Concerns & Risks
- Known risks and mitigations
- Dependencies on external systems/teams
- Timeline constraints
- Technical debt considerations

#### Trade-offs
- What would you sacrifice for speed?
- What's non-negotiable?
- Build vs buy decisions
- Complexity vs maintainability

### Step 3: Question Guidelines

**DO ask questions that are:**
- Specific and actionable
- Non-obvious (not answerable by reading the plan)
- Exploratory of edge cases
- Challenging to assumptions
- Focused on trade-offs and priorities

**DON'T ask questions that are:**
- Already answered in the plan
- Yes/no without follow-up
- Too abstract to be actionable
- Trivial or obvious

### Step 4: Iterative Deep-Dive

Continue the interview iteratively:

1. **MANDATORY: Always batch 2-4 questions per AskUserQuestion call.** NEVER send a single question per round unless it is the final yes/no approval gate. Filling the question budget reduces round-trips and respects the user's time. Use `multiSelect: true` when the user can pick multiple valid answers.
2. Analyze responses for follow-up opportunities
3. Go deeper on areas of uncertainty or complexity
4. Continue until all major areas are covered

**Completion Criteria:**
- All core technical decisions clarified
- Edge cases identified and addressed
- Priorities clearly established
- Constraints documented
- No remaining ambiguities in scope

### Step 5: Generate Specification

Once the interview is complete, write a comprehensive specification to the same directory as the plan:

**Output file:** `{plan-file-basename}-spec.md`

Example: `project-plan.md` → `project-plan-spec.md`

#### Template selection

**If the input file contained an `## Output Spec Template` section** (typically present when invoked by `/brainstorm`):
- Use that template verbatim for the output spec
- Fill its placeholders from the interview answers
- Do NOT mix in sections from the default template below

**Otherwise**, use the default Specification Template below.

#### Default Specification Template

```markdown
# Specification: {Project/Feature Name}

**Generated from:** {plan file path}
**Interview date:** {current date}
**Version:** 1.0

## Executive Summary

{2-3 sentence summary of what this specification covers}

## Objectives

### Primary Goals
- {Goal 1}
- {Goal 2}

### Success Metrics
- {Metric 1}
- {Metric 2}

## Scope

### In Scope
- {Item 1}
- {Item 2}

### Out of Scope
- {Item 1}
- {Item 2}

### Future Considerations
- {Item 1}

## Technical Requirements

### Architecture
{Architecture decisions and rationale}

### Components
| Component | Purpose | Technology |
|-----------|---------|------------|
| {name} | {purpose} | {tech} |

### Integrations
- {System 1}: {How it integrates}

### Performance Requirements
- {Requirement 1}

### Security Requirements
- {Requirement 1}

## User Experience (if applicable)

### User Flows
1. {Flow name}: {Description}

### Edge Cases
| Scenario | Expected Behavior |
|----------|-------------------|
| {scenario} | {behavior} |

## Constraints & Dependencies

### Technical Constraints
- {Constraint 1}

### External Dependencies
- {Dependency 1}

### Timeline Constraints
- {Constraint 1}

## Risks & Mitigations

| Risk | Impact | Likelihood | Mitigation |
|------|--------|------------|------------|
| {risk} | High/Med/Low | High/Med/Low | {mitigation} |

## Decisions Made

### Key Trade-offs
- **Decision:** {What was decided}
- **Alternatives considered:** {What else was considered}
- **Rationale:** {Why this choice}

### Deferred Decisions
- {Decision 1}: {Why deferred}

## Implementation Notes

### Priority Order
1. {Highest priority item}
2. {Second priority}

### Technical Debt Accepted
- {Item 1}: {Justification}

## Open Questions

- [ ] {Any remaining questions}

---

*This specification was generated through systematic interview of the plan author.*
```

## Example Session

**Plan excerpt:**
> "Build a user authentication system with OAuth support"

**Interview questions:**
1. "Which OAuth providers need to be supported? (Google, GitHub, Apple, etc.) Are there any that are must-haves vs nice-to-haves?"
2. "What should happen if a user tries to link an OAuth account that's already connected to a different user? Should accounts be mergeable?"
3. "Are there specific session timeout requirements? Should sessions persist across browser restarts?"
4. "What level of audit logging is required for authentication events? Compliance requirements?"

## Portability Notes

The skill enforces a **structured user-prompt tool** (see Step 2 for the priority order). Detailed per-host notes:

### Claude Code
Use `AskUserQuestion` with a `questions` array. Set `multiSelect: true` when the user can pick multiple options. 2–4 questions per round.

### Codex (OpenAI Agents SDK / Codex CLI)
Use whichever native user-prompt primitive the harness exposes. The exact name has shifted across Codex CLI versions (`user_prompt`, `prompt_user`, `ask_user`, `Prompt`). Check the available tool list at runtime and pick the one whose schema matches "ask the user a question and wait for an answer". Do **not** simulate it via plaintext output — Codex agents have a real one.

### Cursor / Windsurf / Aider / other coding agents
Each agent has its own user-input primitive (`request_input`, `ask`, etc.). Use whichever is available; do not fall through to plaintext if a structured one exists.

### Generic LLMs (no native questioning tool)
Last-resort fallback only. Format questions as a numbered list with clear "Question N:" headers and explicit "please answer inline before continuing" instructions. Do not interleave questions with other content.

## Tips for Best Results

1. **Provide a detailed plan** - The more context in the plan, the better the questions
2. **Answer thoroughly** - Detailed answers lead to better follow-up questions
3. **Flag uncertainty** - Say "I'm not sure" and the interview will explore that area
4. **Mention constraints early** - Timeline, budget, team size affect many decisions
5. **Be honest about scope** - Clearly distinguish MVP from future phases
