---
name: goal
description: |
  Produce a fully-filled autonomous-run mega-prompt at .agents/goals/<slug>.md
  by interviewing the user for the missing context. The output file is the
  "set the goal · walk away · come back to shipped work" prompt — drop it
  into a fresh Claude Code / Codex session to run end-to-end without hand-
  holding. Use when Stevie says "/goal", "/goal <outcome>", "set a goal",
  "build me a goal prompt", or similar. The skill does NOT execute the
  goal — it only produces the artifact.
argument-hint: "[the final outcome — one line, optional]"
---

# /goal — autonomous-run mega-prompt builder

## What it does

`/goal [outcome line]` interviews Stevie for the operating context an
autonomous coding agent needs, then writes a filled mega-prompt to
`.agents/goals/<slug>.md`. Stevie runs the goal by pasting the file
contents into a fresh session — this skill never executes the run.

The mega-prompt template lives at `assets/mega-prompt.md.tmpl`. The stub
plan handed to `/interview` lives at `assets/stub-plan.md.tmpl`.

## Trigger

- `/goal` — no args, interview asks for the outcome line too
- `/goal <outcome>` — outcome line pre-fills `{{OUTCOME}}`, interview
  gathers the rest
- Natural language: "set a goal", "make a goal prompt"

## Flow

### 1. Capture the outcome line

If `$ARGUMENTS` is non-empty, treat it as the final-outcome line and
slugify it. Else, use `AskUserQuestion` with a single open question:
"What does done look like — in one line?"

The outcome line is the most important field. Push back on vague answers
("ship the app") and demand specificity ("deploy v1 of the dashboard at
dashboard.example.com with login + 3 charts working"). One sentence,
present-tense, observable.

### 2. Generate slug + paths

- Slug: kebab-case the outcome line, strip stopwords, truncate to 50
  chars. Example: "deploy v1 of the dashboard with login" →
  `deploy-v1-dashboard-with-login`.
- Plan path: `.agents/goals/<slug>.plan.md`
- Spec path: `.agents/goals/<slug>.plan-spec.md` (what `/interview` writes)
- Goal path: `.agents/goals/<slug>.md` (the final mega-prompt)

Create `.agents/goals/` if missing. If `<slug>.md` already exists,
append a numeric suffix (`<slug>-2.md`).

### 3. Write the stub plan

Read `assets/stub-plan.md.tmpl`, substitute `{{OUTCOME}}`, and write to
the plan path. The stub is deliberately prescriptive — it tells
`/interview` exactly which fields to extract so the resulting spec maps
back to the mega-prompt without guesswork.

### 4. Delegate to /interview

Invoke the `/interview` skill against `<slug>.plan.md`. `/interview`
will use its mandatory structured-user-prompt tool (`AskUserQuestion` on
Claude Code) and produce the spec at `<slug>.plan-spec.md`.

Per `/interview`'s contract, it runs iteratively until "all major areas
are covered". Trust it.

### 5. Map spec → mega-prompt fields, fill gaps

Read `<slug>.plan-spec.md`. Extract:

| Mega-prompt field | Likely spec source |
|---|---|
| `{{PROJECT}}` | Executive Summary · Objectives → Primary Goals |
| `{{STACK}}` | Technical Requirements → Components / Architecture |
| `{{CURRENT_STATE}}` | Implementation Notes · context paragraphs |
| `{{WORKING_DIR}}` | Constraints section · or not present |
| `{{CONSTRAINTS}}` | Constraints & Dependencies → Technical / Timeline |
| `{{AUDIENCE}}` | User Experience intro · or not present |
| `{{SUCCESS_1..3}}` | Objectives → Success Metrics |

Fields not reliably in `/interview`'s spec template:
**working dir**, **audience**, **current state**. If those are missing
or thin in the spec, run one `AskUserQuestion` round with only the
unfilled fields. Don't re-ask anything the spec already answers.

If the spec produced more than three success metrics, ask Stevie which
three are the hard-gates for "done". If it produced fewer than three,
ask for the remainder — three is the minimum the template needs.

### 6. Render the mega-prompt

Read `assets/mega-prompt.md.tmpl`. Substitute every `{{FIELD}}` token
with the gathered value. Multi-line values: keep them single-line where
the template uses a single line (Context block) — join with `; ` if
needed. The Operating Rules / Quality Bar / Final Deliverable blocks
are verbatim — never edit them.

Write to `<slug>.md`.

### 7. Report back

Tell Stevie:

- where the goal file landed (absolute path)
- where the spec landed (in case he wants to revise context without
  re-interviewing)
- one-line instruction to run it: paste the contents of `<slug>.md`
  into a fresh Claude Code or Codex session
- the slug (so he can refer to it later)

Caveman mode by default. Address as **Stevie**.

## Non-goals

- **Do not execute the goal.** This skill only produces the artifact.
- **Do not commit `.agents/goals/`.** Leave that decision to Stevie —
  the directory may or may not be gitignored per repo.
- **Do not edit the Operating Rules / Quality Bar / Final Deliverable
  blocks** in the template. They're intentionally non-negotiable across
  every run.

## Edge cases

- **No `/interview` skill available** (e.g. a host without it deployed):
  fall back to running the question set directly via `AskUserQuestion`
  using the field list from `assets/stub-plan.md.tmpl`. Skip the spec
  artifact in this mode and render straight to the mega-prompt.
- **Outcome line is multi-sentence**: ask Stevie to compress to one
  line before slugging. The mega-prompt's first line is the contract.
- **Existing goal with same slug**: append `-2`, `-3`, …
- **Working dir is "this repo"**: capture the absolute path of `$PWD`
  at goal-generation time, not the literal string "this repo".
