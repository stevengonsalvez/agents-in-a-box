---
name: brainstorm
description: Turn an idea into an approved design spec. Brainstorm is the orchestrator - it scaffolds a topic-stub with subject-type detection and ASCII preview hypotheses, then DELEGATES the iterative Q&A to /interview. After /interview returns a spec, brainstorm self-reviews, gates on user approval, and hands off to /plan. No HTML, no browser - terminal + ASCII previews only.
user-invocable: true
---

Brainstorm idea: `$ARGUMENTS`. Output is an approved spec doc, not code. **Brainstorm is the orchestrator; `/interview` runs the actual Q&A.**

<HARD-GATE>
Do NOT write code, scaffold a project, run install/build commands, or invoke any implementation skill (`/plan`, `/implement`, `/plan-tdd`, framework codegen) until a written spec exists in `.agents/specs/` AND the user has explicitly approved it via the user-review gate (Step 10). Once both conditions are satisfied, Step 12 (`/plan` handoff) is the intended terminal action. Applies to every project regardless of perceived simplicity.
</HARD-GATE>

## Anti-pattern: "too simple to need a design"

Every brainstorm produces a spec. Todo list, single-function util, config change - all of them. "Simple" is where unexamined assumptions bite hardest. Spec can be 10 lines for trivial work, but it must exist and be approved.

<!-- recall:begin -->

## Step 1: Prior-art check (MANDATORY)

Recall prior learnings from the global knowledge base before brainstorming so we don't re-decide something already captured:

```bash
uv run "{{HOME_TOOL_DIR}}/skills/recall/scripts/recall.py" \
  "<QUERY>" \
  --limit 5 --format markdown
```

**Query construction**: topic + constraint hints (e.g. `"caching strategy mobile offline"`).

**What to do with results:**
- If a returned learning names a constraint, anti-pattern, or prior decision directly relevant - surface it to the user BEFORE proceeding.
- If nothing relevant returns - proceed silently.
- Never block on recall failure. Empty output / non-zero exit = "no prior art found", not error.

<!-- recall:end -->

## Process flow

```
┌──────────────┐   ┌─────────────┐   ┌──────────────┐   ┌──────────────┐
│ Recall +     │──▶│ Detect      │──▶│ Scaffold     │──▶│ INVOKE       │
│ Project scan │   │ subject type│   │ topic-stub   │   │ /interview   │
└──────────────┘   └─────────────┘   └──────────────┘   └──────┬───────┘
                                                               │
                            /interview runs the Q&A,           │
                            uses ASCII previews from stub,     │
                            writes <stub>-spec.md              │
                                                               ▼
┌──────────────┐   ┌─────────────┐   ┌──────────────┐   ┌──────────────┐
│ /plan        │◀──│ User review │◀──│ Self-review  │◀──│ Read back    │
│ handoff      │   │ gate        │   │ spec         │   │ spec from    │
│              │   │             │   │              │   │ /interview   │
└──────────────┘   └─────────────┘   └──────────────┘   └──────────────┘
```

Terminal state = `/plan` invocation. Never invoke `/implement` or framework codegen direct from brainstorm.

## Division of labour

| Skill         | Role                                                            |
|---------------|-----------------------------------------------------------------|
| `/brainstorm` | Recall, project context, subject-type detection, stub scaffolding with ASCII previews, self-review, user-review gate, `/plan` handoff |
| `/interview`  | The actual iterative AskUserQuestion-based Q&A loop, populating the spec file |

Brainstorm is the orchestration layer. It writes one rich stub file, hands off, reads the result. The interview skill is the Q&A engine.

## Checklist (track via TaskCreate)

1. **Recall** prior learnings - Step 1 below
2. **Scan project context** - files, docs, recent commits, existing patterns
3. **Decompose if too large** - flag and split before refining details
4. **Detect subject type** - drives which ASCII preview library slice to embed
5. **Pre-stub clarifier (optional, ≤ 1 question)** - only if subject type / scope is ambiguous
6. **Scaffold topic-stub.md** - `.agents/specs/YYYY-MM-DD-<topic>-stub.md` with hypotheses, ASCII slice, instructions, output template
7. **Invoke `/interview <stub-path>`** via Skill tool - it runs the Q&A and writes `<stub>-spec.md`
8. **Read back spec** - at `.agents/specs/YYYY-MM-DD-<topic>-stub-spec.md`
9. **Self-review spec** - placeholder / contradiction / ambiguity / scope / YAGNI sweep
10. **User-review gate** - explicit approval before handoff
11. **Rename + commit** - move final spec to `.agents/specs/YYYY-MM-DD-<topic>.md`
12. **Handoff** - invoke `/plan` skill with spec path

## Step 2: Project context

Before drafting the stub:
- `git log --oneline -20` for recent direction
- `ls` repo root + relevant subdirs
- Read existing `docs/`, `README.md`, `AGENTS.md`, `CLAUDE.md`
- Note existing patterns to follow

## Step 3: Decompose check

Before refining details, assess scope. If the request names multiple independent subsystems ("platform with chat + file storage + billing + analytics"), STOP and decompose:

```
┌─ Original idea ────────────────────┐
│ "Build platform with X, Y, Z"      │
└─────────────────┬──────────────────┘
                  │
        ┌─────────┼─────────┐
        ▼         ▼         ▼
    ┌───────┐ ┌───────┐ ┌───────┐
    │ Sub-X │ │ Sub-Y │ │ Sub-Z │
    └───────┘ └───────┘ └───────┘
   spec→plan  spec→plan  spec→plan
   (each runs its own brainstorm cycle)
```

Pick the first sub-project. Brainstorm THAT. Other sub-projects each get their own `/brainstorm` cycle later.

## Step 4: Detect subject type

Infer from `$ARGUMENTS` + project context. If ambiguous, ask ONE AskUserQuestion (Step 5). Often a single brainstorm spans 2-3 types (e.g. API + data model + CLI) - include the ASCII library slice for each.

| Subject     | Preview shape                                  |
|-------------|------------------------------------------------|
| UI (web/mobile) | ASCII wireframe (header / sections / buttons) |
| TUI         | ASCII screen mock (panes / status bar / keys) |
| API         | endpoint + request/response JSON snippet      |
| Data model  | entity box + relationship arrows              |
| Architecture | component box + dataflow arrows              |
| CLI         | command syntax + sample stdout               |
| Config      | YAML/JSON snippet                            |
| State machine | state node + transition arrows              |
| Pure concept | (no preview - text description only)         |

## Step 5: Pre-stub clarifier (optional, ≤ 1 question)

Only if subject type or core scope is unclear. Use AskUserQuestion. Examples:
- "What kind of artifact is this?" with options { Web UI, Mobile UI, TUI, API, CLI, library, mixed }
- "Hard constraint?" with options { has deadline, no constraints, needs perf target }

If you have enough context from `$ARGUMENTS` + project scan, **skip this step** and go straight to stub scaffolding. The point is to give `/interview` enough to run with, not to do its job for it.

## Step 6: Scaffold the topic-stub

**Read the ASCII library first**: `{{HOME_TOOL_DIR}}/skills/brainstorm/assets/ascii-library.md`. It has 8 subject-keyed sections (UI / TUI / API / Data model / Architecture / CLI / Config / State machine). Copy the matching slice(s) into the stub's `## ASCII preview library` section.

**Resolve every `<placeholder>` from $ARGUMENTS + Step 2 context BEFORE writing the file.** The on-disk stub must contain no angle-bracket placeholders, no `TBD`, no `TODO` (per Stevie's `<paste_ready_artifacts>` rule). A placeholder-laden stub wastes /interview's first round on re-derivation.

Write to `.agents/specs/YYYY-MM-DD-<topic-slug>-stub.md`. The stub IS the contract with `/interview` - it must contain EXACTLY these headings (interview matches them exactly):

- `## Idea`
- `## Project context (inferred)`
- `## Subject type(s)`
- `## Initial hypotheses`
- `## ASCII preview library`
- `## For /interview`
- `## Output Spec Template`

Template body:

````markdown
# Brainstorm topic stub: <title>

> This is a brainstorm stub. The `/interview` skill will read it and run the iterative Q&A. Follow the instructions in the "For /interview" section.

## Idea

<one-paragraph problem statement from $ARGUMENTS + clarifier answer>

## Project context (inferred)

- Stack: <e.g. Rust + tokio>, <repo patterns spotted>
- Existing patterns: <bullet list of relevant docs / code paths>
- Constraints surfaced from recall: <if any>

## Subject type(s)

<UI | TUI | API | data model | architecture | CLI | config | state machine | mixed>

## Initial hypotheses

Three candidate approaches. **`/interview` MUST present these via AskUserQuestion in its first round, using the ASCII preview field shown below for each.**

### A: <name>  (recommended)

<one-line rationale>

```
<ASCII preview drawn from the matching library section>
```

### B: <name>

<tradeoff vs A>

```
<ASCII preview>
```

### C: <name>  (optional - only if there's a real third path)

<tradeoff>

```
<ASCII preview>
```

## ASCII preview library

Inlined slices from `{{HOME_TOOL_DIR}}/skills/brainstorm/assets/ascii-library.md` that match this brainstorm's subject type(s). `/interview` uses these in subsequent AskUserQuestion `preview` fields.

<paste the matching subject-type sections here>

## For /interview

**Read this carefully before starting the interview.**

- **First round**: present the three approach hypotheses above via AskUserQuestion. Set `preview` to the ASCII block under each.
- **Subsequent rounds**: drill into architecture, data model, interface, behavior, errors, testing. **Every option-style question MUST use `AskUserQuestion`** with `preview` populated from the library above when comparing concrete shapes.
- **One question per turn** (or 2-4 if naturally batched). Multi-choice preferred.
- **Topics to cover** (skip if N/A): architecture / data model / interface (UI/API/CLI/TUI) / happy path / 2-3 critical edge cases / error handling / testing strategy / out of scope / open questions.
- **Output**: write the spec to `<this-stub-basename>-spec.md` using the "Output Spec Template" section below — **NOT** the default `/interview` corporate template.

### Format preferences (apply to BOTH chat outputs AND spec file)

Per Stevie's CLAUDE.md `<flow_diagrams>` rule. Boundary:

| Content shape                                | Use                                              |
|----------------------------------------------|--------------------------------------------------|
| Flow / sequence / relationships / state      | ASCII box+arrow diagram (`┌─┐ │ └─┘ ─▶ ▼`)        |
| Tabular DATA (rows × columns of facts)       | markdown pipe table                              |
| Discrete items, no ordering                  | bullet list                                      |
| Picks / open questions                       | `- [ ]` checklist                                |
| Prose / narrative paragraphs                 | AVOID — break it into one of the above           |

Rules:
- **Diagram FIRST, table SECOND** when both apply to the same section.
- Diagram width ≤ 80 chars.
- Caveman inside boxes (short technical terms, never sentences).
- Sequence diagrams (vertical lifelines) ONLY for protocol handshakes / back-and-forth.
- Branching trees ONLY for explicit if/else logic.
- No prose-only sections in the spec. Every section must be diagram + table + bullets.

## Output Spec Template

```markdown
# Spec: <title>

**Generated from:** <stub-path>
**Date:** <YYYY-MM-DD>
**Format:** diagram-first, table-second, no prose paragraphs

## Problem

| Question | Answer            |
|----------|-------------------|
| What?    | <one-liner>       |
| Why?     | <motivator>       |
| Who?     | <primary user>    |

## Users + use cases

| Persona     | Goal                | Primary use case          |
|-------------|---------------------|---------------------------|
| <persona 1> | <what they want>    | <core flow they trigger>  |
| <persona 2> | <...>               | <...>                     |

## Approach

| Option | Summary       | Tradeoff           | Picked? |
|--------|---------------|--------------------|---------|
| A      | <name>        | <vs B and C>       |  ✓      |
| B      | <name>        | <vs A>             |         |
| C      | <name>        | <vs A>             |         |

**Why A:** <one line>

## Architecture

<ASCII box+arrow diagram, ≤ 80 chars wide>

| Component | Purpose      | Owns                |
|-----------|--------------|---------------------|
| <name>    | <one-liner>  | <data / behavior>   |

## Data model

<ASCII entity + relationship diagram>

| Entity   | Fields (key only)        | Relationships    |
|----------|--------------------------|------------------|
| <name>   | id, ...                  | 1:N to <other>   |

## Interface

ASCII previews per applicable subject type (UI wireframe | TUI screen | API endpoints | CLI sample | config schema).

| Surface    | Trigger              | Shape                 |
|------------|----------------------|-----------------------|
| <name>     | <user/system event>  | <method / screen / cmd> |

## Behavior

Happy path (ASCII flow):

[start] ──action──▶ [state-1] ──action──▶ [state-2] ──ok──▶ [done]

Edge cases:

| Scenario          | Trigger              | Expected behavior        |
|-------------------|----------------------|--------------------------|
| <case 1>          | <condition>          | <what happens>           |
| <case 2>          | <condition>          | <what happens>           |
| <case 3>          | <condition>          | <what happens>           |

## Errors

| Failure mode    | User-visible surface       | Recovery              |
|-----------------|----------------------------|-----------------------|
| <e.g. 500>      | <toast / log / silent>     | <retry / fallback>    |
| <network drop>  | <...>                      | <...>                 |

## Testing strategy

| Layer       | Scope                              | Coverage gate       |
|-------------|------------------------------------|---------------------|
| Integration | <flows covered>                    | <must-pass>         |
| E2E         | <user-visible journeys>            | <must-pass>         |
| Unit        | <only where logic is non-trivial>  | <if applicable>     |

## Out of scope

- <explicit item 1>
- <explicit item 2>

## Open questions for /plan

- [ ] <question 1>
- [ ] <question 2>
```
````

After writing the stub, **DO NOT** answer the questions yourself. Hand off to `/interview`.

## Step 7: Invoke /interview

**Filename contract (used in Steps 8-11):** `/interview` writes its output as `<input-basename>-spec.md` next to the input file. So given `<topic>-stub.md` as input, the output is `<topic>-stub-spec.md`. Every path reference in this skill depends on this contract — if `/interview`'s output suffix ever changes, update Steps 8-11.

Invoke:

```
Skill(skill: "interview", args: ".agents/specs/YYYY-MM-DD-<topic>-stub.md")
```

`/interview` will:
- Read the stub
- Detect the embedded `## Output Spec Template` section and use it (NOT the default template)
- Detect `## Initial hypotheses`, `## ASCII preview library`, `## For /interview` sections and honor them
- Run AskUserQuestion rounds — starting with the three approach hypotheses, then drilling into design
- Write the spec to `.agents/specs/YYYY-MM-DD-<topic>-stub-spec.md`

Wait for `/interview` to complete and return control.

## Step 8: Read back the spec

Read `.agents/specs/YYYY-MM-DD-<topic>-stub-spec.md`. Confirm it exists and is non-trivial.

## Step 9: Spec self-review

Fresh-eyes pass with these gates. Fix inline; don't re-loop.

| Gate         | What to look for                                                |
|--------------|-----------------------------------------------------------------|
| Placeholder  | `TBD`, `TODO`, `<...>`, empty sections                          |
| Consistency  | sections contradicting each other; arch ≠ behavior              |
| Ambiguity    | requirements interpretable two ways - pick one, make explicit   |
| Scope        | still one project, or did it bloat? if bloated, decompose again |
| YAGNI        | unrequested features creeping in - strip                        |

Only flag issues that would cause real planning problems. Minor wording, stylistic preferences - leave alone.

## Step 10: User-review gate

> "Spec written to `.agents/specs/YYYY-MM-DD-<topic>-stub-spec.md`. Review it. Changes welcome before we finalize + move to `/plan`."

Wait for explicit approval. If changes requested → fix → re-run self-review → re-ask.

## Step 11: Rename + commit

After approval. The stub and stub-spec files were created by `Write` (untracked by git), so use plain `mv` / `rm`, NOT `git mv` / `git rm` (which require tracked files):

```bash
mv .agents/specs/YYYY-MM-DD-<topic>-stub-spec.md .agents/specs/YYYY-MM-DD-<topic>.md
rm .agents/specs/YYYY-MM-DD-<topic>-stub.md
git add .agents/specs/YYYY-MM-DD-<topic>.md
git commit -m "feat(spec): add <topic> design spec"
```

One atomic commit; only the final spec lands in history (stub was scaffolding).

## Step 12: Handoff to /plan

Invoke `/plan` (via Skill tool) with the final spec path. Do NOT invoke `/implement`, framework codegen, or anything else.

## ASCII Preview Library

The library lives in a sibling file:

```
{{HOME_TOOL_DIR}}/skills/brainstorm/assets/ascii-library.md
```

**Read this file before scaffolding the stub (Step 6).** Embed the matching subject-type slice(s) inline in the stub's `## ASCII preview library` section so `/interview` has the templates available in-context.

Subjects covered: UI / TUI / API / Data model / Architecture / CLI / Config / State machine.

## Key principles

- **Brainstorm orchestrates, /interview runs the Q&A** - don't duplicate Q&A logic
- **The stub is the contract** - put everything /interview needs in there (hypotheses, ASCII library slice, output template, instructions)
- **Heading exact-match** - stub headings (`## Initial hypotheses`, `## ASCII preview library`, `## For /interview`, `## Output Spec Template`) must EXACTLY match what /interview scans for. No em-dash suffixes, no rewording.
- **AskUserQuestion mandatory** for any A/B/C decision (per CLAUDE.md `<option_presentation>`) — applies to both skills
- **ASCII preview per option** when shapes are being compared
- **Multi-choice preferred** over open-ended
- **YAGNI ruthlessly** - strip unrequested features at every gate
- **Spec is the deliverable**, not code
- **Decompose before stub-ing** for large projects

## Anti-patterns to avoid

- Doing the Q&A inside `/brainstorm` instead of delegating → defeats the integration
- Writing a thin stub that doesn't give `/interview` enough → it will ask shallow questions
- Plaintext markdown option tables in stub or interview → AskUserQuestion only
- HTML / browser companions → terminal + ASCII only
- Writing a stub with unresolved `<placeholders>` → wastes /interview's first round (see HARD-GATE + Step 6)
- Auto-running `/plan` without user-review gate → wait for explicit approval (HARD-GATE)
- Renaming stub-spec with `git mv` instead of plain `mv` → fails on untracked files
