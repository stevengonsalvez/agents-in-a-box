---
name: standup
description: Read-only branch-scoped situation report for Stevie. Surfaces beads in-flight, what's been done since last invocation, what's pending, active swarms/coding-agents/subagents in this worktree, and — most importantly — what specifically needs Stevie's input (decisions, PR merges, AskUserQuestion threads, blocked beads). Output is progressive tables, one shape per stage, address the user as Stevie. Trigger on /standup, "give me a standup", "what's the state of this branch", "what do you need from me". Read-only — never claims beads, never merges, never posts anywhere.
---

# Standup — Stevie's branch-scoped situation report

## Purpose

One-shot snapshot of the current git worktree/branch. No writes. Tells Stevie:
- what's been done since the last `/standup` on this branch
- what's pending and ready
- what's blocked
- what swarms / coding-agents / subagents are running in this worktree
- what specifically needs *his* input

Always address the reader as **Stevie** (per his global preference). Output in **caveman mode** by default — drop articles, filler, hedging — unless he's already in normal mode in this conversation.

## Trigger

Manual `/standup` only in v1. Future flag (not implemented yet): `/standup --since <date>`.

## Output format

**Primary principle: easy readability.** Stevie scans, doesn't read. Optimise for that.

Rules, in priority order:

1. **Clean tables are the default shape.** Every section is a self-contained table the reader can act on without re-scanning earlier sections. No prose paragraphs. No nested bullet lists when a table fits. Per Stevie's `feedback_incident_report_table_format.md`.
2. **Options / files affected → neat separators + one-line rationale per option.** When a section enumerates options, alternatives, or files affected, separate each with a horizontal rule (`---`) and give each ONE line of rationale (why-it-matters / what-changes). Never bullet-soup. Pattern:
   ```markdown
   **Option A — short label**
   One line: what it does + the trade-off.
   
   ---
   
   **Option B — short label**
   One line: what it does + the trade-off.
   ```
3. **ASCII diagrams preferred for architecture / flow / trade-off explanations.** When a comparison is structural (data flow, RLS chain, deploy sequence, who-calls-what), draw it. ASCII beats prose every time. Pattern:
   ```
   client ──▶ /functions/v1/foo ──▶ alex.* (RPC) ──▶ public.alex_audit_log
                                       │
                                       └─ Zod validate ─▶ tool dispatcher
   ```
   Use box-drawing chars (`──▶ ── │ ┌ ┐ └ ┘ ├ ┤`) sparingly. ASCII art for the sake of it = noise.
4. **Inputs from Stevie at end of report → use `/interview` skill, not raw AskUserQuestion.** When the standup surfaces a decision Stevie needs to make (section 8 row flagged `needs your call`), invoke `/interview` to walk through it conversationally rather than firing a single AskUserQuestion. Reason: standup decisions are usually multi-faceted (which option, which scope, when) and `/interview` paces the dialogue. AskUserQuestion is fine for single binary picks.

Section order is fixed (4 tables only — keep it tight):

1. **Summary** (1-row counts table: shipped / next / pending / blocked / inputs — each prefixed with its status ball)
2. **Beads** — ONE unified table, columns: `id | title | status | PR`. Status enum prefixed with a coloured ball so the eye picks out trouble at a glance:
   - 🟢 `shipped` — closed bead, PR merged
   - 🔵 `pending` — in_progress
   - 🟡 `next (P<n>)` — ready, priority annotated
   - 🔴 `blocked (by …)` — blocker IDs in parentheses
   PR column shows `#NNNN → merged|open|draft` or `—`. Don't split into 3 sub-tables — Stevie hates that.
3. **PRs raised in this worktree** (one table — # / title / state / CI / labels / when). State ball: 🟢 merged · 🟡 open · ⚫ draft · 🔴 closed-unmerged.
4. **Active swarms / coding-agents / subagents** (one table — name / source / working on / last activity / status). Status ball: 🟢 running · 🟡 idle (>5min no activity) · 🔴 stuck/errored. The "working on" column shows the actual bead or task they're driving, not just the session name.
5. **Inputs needed from Stevie** 🔴 (THE headline section — surface aggressively). Priority column also coloured: 🔴 high · 🟡 medium · 🔵 low.

**Why coloured balls:** in tables that grow past ~10 rows (especially the Beads union table when many things are blocked), Stevie scans the left edge of the status column to find the red. Plain text status loses that affordance.

Close the report with: `Anything else, Stevie?`

## Execution sequence

Run these checks in order. Skip a section gracefully if its data source is missing — never fail the whole standup because beads or gh isn't installed.

### 0. Guardrails

```bash
# Must be in a git worktree
if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  echo "Stevie — not in a git repo. /standup needs a worktree."; exit 1
fi

BRANCH=$(git branch --show-current)
if [ -z "$BRANCH" ]; then
  echo "Stevie — detached HEAD. Switch to a branch and re-run."; exit 1
fi

WORKTREE_PATH=$(git rev-parse --show-toplevel)
SAFE_BRANCH=$(echo "$BRANCH" | sed 's|[/:]|_|g')

# State + config live under .agents/standup/ — tool-neutral location shared
# across Claude / Codex / Copilot / Gemini / any future agent. Same pattern
# as .agents/MEMORY.md, .agents/pr-signals/, .agents/scratch/. Never write
# to .claude/ (Claude-only) or .codex/ (Codex-only).
STATE_DIR="$WORKTREE_PATH/.agents/standup"
STATE_FILE="$STATE_DIR/${SAFE_BRANCH}.json"
mkdir -p "$STATE_DIR"
```

### 1. Establish "since" cutoff

```bash
# If state file exists, use last_run_at. Otherwise fall back to merge-base with main.
if [ -f "$STATE_FILE" ]; then
  SINCE=$(jq -r '.last_run_at' "$STATE_FILE")
else
  MERGE_BASE=$(git merge-base HEAD main 2>/dev/null || git merge-base HEAD master 2>/dev/null)
  SINCE=$(git show -s --format=%cI "$MERGE_BASE" 2>/dev/null || date -u -v-1d '+%Y-%m-%dT%H:%M:%SZ')
fi
NOW=$(date -u '+%Y-%m-%dT%H:%M:%SZ')
```

Compute elapsed nicely: minutes if <60, hours+minutes otherwise. Use `date -d` / `gdate` if available; fall back to manual subtraction.

### 2. Load project label config (optional)

Config path order (first match wins, all tool-neutral):
1. `$WORKTREE_PATH/.agents/standup/config.json` (preferred — same dir as state)
2. `$WORKTREE_PATH/.agents/standup-config.json` (legacy fallback)

If neither exists, default to SHOT semantics:

```jsonc
{
  "label_map": {
    "review_requested":  "popashot-please-review",
    "reviewed":          "popashot-reviewed",
    "review_done":       "popashot-review-done",
    "comments_fixed":    "popashot-pr-comments-fixed",
    "e2e_passed":        "popashot-e2e-passed"
  }
}
```

Stevie-action signal = `comments_fixed` label present + all required CI green = ready to admin-merge.

### 3. Beads — ONE unified table

Skip if `[ ! -d .beads ]`.

```bash
# Pull all four states, then union into a single rendering.
SINCE=...  # from step 1

# shipped = closed beads since cutoff that mention branch/issue
SHIPPED=$(bd list --status=closed --json 2>/dev/null \
  | jq --arg s "$SINCE" --arg b "$BRANCH" '[.[]
      | select(.closed_at >= $s)
      | select((.title // "" | contains($b)) or
               (.description // "" | contains($b)) or
               ((.notes // "") | contains($b)))
      | {id, title, status: "shipped", pr: (.notes // "" | capture("#(?<n>[0-9]+)").n // "—")}]')

# pending = currently in_progress on this branch
PENDING=$(bd list --status=in_progress --json 2>/dev/null \
  | jq --arg b "$BRANCH" '[.[]
      | select((.title // "" | contains($b)) or
               (.description // "" | contains($b)) or
               ((.notes // "") | contains($b)))
      | {id, title, status: "pending", pr: "—"}]')

# next = ready (top 5 — these surface even if not branch-scoped, since branch may be done)
NEXT=$(bd ready --json 2>/dev/null | jq '.[0:5] | map({id, title, status: ("next (P" + (.priority|tostring) + ")"), pr: "—"})')

# blocked = all blocked beads, with blocker IDs in status column
BLOCKED=$(bd blocked --json 2>/dev/null | jq '[.[] | {
  id, title,
  status: ("blocked (by " + ((.blocked_by // []) | join(", ")) + ")"),
  pr: "—"
}]')

# Combine — render one table.
echo "$SHIPPED $PENDING $NEXT $BLOCKED" | jq -s 'add'
```

**Render ONE table, columns: `id | title | status | PR`.** Sort: shipped first, then pending, then next, then blocked. Cap blocked at 5 rows + `_(+ N more 🔴 — see bd blocked)_`. The PR column populates `#NNNN → merged|open|draft` for shipped beads, `—` for everything else.

**Status column always prefixed with a coloured ball** (🟢 shipped / 🔵 pending / 🟡 next / 🔴 blocked) — load-bearing for visual scanning when blocked count is high.

DO NOT split into in-progress/ready/blocked sub-tables — Stevie's preference is one unified view.

### 4. PRs raised in this worktree

For each open PR with head branch matching current branch:

```bash
gh pr view <num> --json number,title,isDraft,statusCheckRollup,labels,reviews,createdAt --jq '{
  number, title, isDraft,
  ci: ([.statusCheckRollup[] | .conclusion] | group_by(.) | map({(.[0]): length}) | add),
  labels: [.labels[].name],
  age_hours: (((now - (.createdAt | fromdateiso8601)) / 3600) | floor)
}'
```

Render:

| # | title | draft | CI | labels | age |
|---|---|---|---|---|---|
| 2535 | fix(rls)... | no | ✅ 12 / ✘ 0 / ⏳ 0 | popashot-pr-comments-fixed, popashot-e2e-passed | 6h |

`pr-signals` is **NOT** re-run — labels are the truth. If `comments_fixed` + all-green CI, flag this row in section 7.

### 5. Active swarms / coding-agents / subagents (this worktree only)

```bash
# tmux sessions whose CWD == current worktree
if command -v tmux >/dev/null 2>&1 && tmux ls >/dev/null 2>&1; then
  tmux list-sessions -F '#{session_name}|#{session_path}|#{session_activity}' 2>/dev/null \
    | awk -F'|' -v wt="$WORKTREE_PATH" '$2 == wt' \
    | while IFS='|' read -r name path activity; do
        case "$name" in
          swarm-*|agent-*|expect-*|dev-*|coding-agent-*)
            # Extract WHAT they're working on, not just last line.
            # Heuristics for the "working on" column (in priority order):
            #   1. Bead ID pattern: grep -oE 'shotclubhouse-[a-z0-9.]+' on last 200 lines
            #   2. Issue/PR ref: grep -oE '#[0-9]+' on last 50 lines
            #   3. File path being edited: last "Edit|Write" tool call target
            #   4. Fall back to last non-empty user-visible line
            pane=$(tmux capture-pane -t "$name" -p -S -200 2>/dev/null)
            working_on=$(echo "$pane" | grep -oE 'shotclubhouse-[a-z0-9.]+' | tail -1)
            [ -z "$working_on" ] && working_on=$(echo "$pane" | grep -oE '#[0-9]+' | tail -1)
            [ -z "$working_on" ] && working_on=$(echo "$pane" | tail -5 | grep -v '^$' | tail -1 | cut -c1-60)
            echo "$name|tmux|$working_on|$activity|running"
            ;;
        esac
      done
fi

# .agents/scratch/swarm-status.sh if present (preferred — agent-authored "what I'm doing")
[ -x .agents/scratch/swarm-status.sh ] && .agents/scratch/swarm-status.sh 2>/dev/null
```

Plus call the **TaskList** tool to enumerate in-flight Task agents in *this* conversation. The agent's `description` (4-word task summary passed at spawn) goes in the "working on" column.

Render ONE table:

| name | source | working on | last activity | status |
|---|---|---|---|---|
| swarm-1778…-agent-1 | tmux | shotclubhouse-ag-z0nc | 2m ago | running |
| Task agent | TaskList | "validate RLS probe" | 30s ago | running |

The **`working on` column is load-bearing** — Stevie wants to see the actual bead/issue/file/task each agent is driving, not just the session name. If the column is empty for a row, mark it `?` and surface in section 7 as a "session running with unknown scope" input.

If empty, render `_(none active)_` rather than skipping the section.

### 6. Inputs needed from Stevie 🔴

THIS is the highest-signal section. Build the table even if other sections are empty.

**Sources:**

a. **Blocked beads with decision-shaped notes**:
   ```bash
   bd blocked --json 2>/dev/null | jq '[.[] | select(
     (.notes // "" | test("(?i)awaiting|decision|stevie|review|approval|input|choose|pick|confirm"))
   )]'
   ```

b. **PRs flagged Stevie-action**:
   - Label state matches: `comments_fixed` present + all required CI = `success`
   - OR `please_review` present AND latest claude-review verdict = `CHANGES REQUESTED`
   ```bash
   gh pr view <num> --json comments,statusCheckRollup,labels --jq '
     {needs_merge: (.labels | map(.name) | contains(["popashot-pr-comments-fixed"])
                   and ([.statusCheckRollup[] | .conclusion] | all(. == "SUCCESS" or . == "SKIPPED" or . == "NEUTRAL"))),
      claude_changes_req: (.comments | map(select(.author.login == "claude")) | last | .body | contains("CHANGES REQUESTED"))
     }'
   ```

c. **AskUserQuestion threads in spawned tmux sessions**:
   For each agent tmux session in this worktree, scan the last 50 lines of pane capture for the question prompt pattern:
   ```bash
   tmux capture-pane -t "$session" -p -S -50 \
     | grep -E "^[?❯>] |Press|please choose|please select|y/n|\(Y/n\)" \
     | tail -3
   ```

d. **Decision points raised in CURRENT conversation that haven't been answered**:
   Read the conversation transcript via the session JSONL. Look for the most recent assistant message that ends with a question to the user (heuristics: ends with `?`, contains `Want me to`, `Should I`, `Confirm`, `(A) ... (B) ...` choice patterns), and check whether it's been followed by a user reply. If not, surface it.

   Find the transcript:
   ```bash
   PROJECT_DIR=$(echo "$WORKTREE_PATH" | sed 's|/|-|g; s|^-||')
   TRANSCRIPT=$(ls -t {{HOME_TOOL_DIR}}/projects/-${PROJECT_DIR}/*.jsonl 2>/dev/null | head -1)
   ```

   Best-effort — if parsing fails, skip silently.

e. **Explicit `// @stevie:` markers in branch diff**:
   ```bash
   git diff $(git merge-base HEAD main)..HEAD -- ':!*.lock' \
     | grep -E "^\+.*(// @stevie:|#shame:blocked|TODO:.*stevie)" \
     | sed 's/^+//'
   ```

**Render:**

| source | item | action | priority |
|---|---|---|---|
| PR #2535 | popashot-pr-comments-fixed + CI green | admin-merge ready | high |
| Bead -ag-xx | blocked: awaiting decision on caching strategy | needs your call | high |
| tmux swarm-1778 | "(Y/n)" prompt unanswered for 12m | reply or kill | medium |
| chat | Last assistant message ended with "Want me to ship option 2?" — no reply yet | answer in chat | high |

Sort by priority (high first), cap at 10 rows. If empty, render: `Stevie — nothing waiting on you. Carry on.`

### 7. Persist state

After successful render:

```bash
jq -n --arg b "$BRANCH" --arg w "$WORKTREE_PATH" --arg t "$NOW" --arg s "$BRIEF_SUMMARY" '{
  branch: $b, worktree_path: $w, last_run_at: $t, last_summary: $s
}' > "$STATE_FILE"
```

Where `$BRIEF_SUMMARY` is one line like `shipped=1 next=5 pending=0 blocked=22 inputs=1`.

## Output skeleton

```markdown
**Stevie's standup — <BRANCH>**

| field | value |
|---|---|
| Branch | <BRANCH> |
| Worktree | <WORKTREE_PATH> |
| Counts | 🟢 <S> shipped / 🟡 <N> next / 🔵 <P> pending / 🔴 <B> blocked / inputs <I> |

### Beads
| id | title | status | PR |
|---|---|---|---|
| ag-xx | ... | 🟢 shipped | #2535 → merged |
| ag-yy | ... | 🔵 pending | #2540 → open |
| ag-zz | ... | 🟡 next (P1) | — |
| ag-ww | ... | 🔴 blocked (by ag-vv) | — |
*(cap blocked at 5 + `_(+ N more 🔴 — see bd blocked)_`)*

### PRs raised in this worktree
| # | title | state | CI | labels | when |
|---|---|---|---|---|---|
| 2535 | fix(rls)... | 🟢 merged | ✅ | popashot-pr-comments-fixed | 13h ago |
*(or `_(none open or merged this cycle)_`)*

### Active swarms / coding-agents / subagents
| name | source | working on | last activity | status |
|---|---|---|---|---|
| swarm-1778… | tmux | shotclubhouse-ag-z0nc | 2m ago | 🟢 running |
*(or `_(none active)_`)*

### Inputs needed from Stevie 🔴
| source | item | action | priority |
|---|---|---|---|
| ... | ... | ... | 🔴 high |
*(or `Stevie — nothing waiting on you. Carry on.`)*

Anything else, Stevie?
```

### When a section needs the options/diagram pattern

If section 8 surfaces a decision with >1 option, OR section 6 surfaces a PR with structural change worth explaining (RLS chain, deploy sequence), append a sub-block AFTER the table using the patterns above:

```markdown
### Option to decide — <one-line context>

**Option A — ship hotfix policy**
One line: unblocks users now, leaves RPC follow-up open.

---

**Option B — ship RPC migration**
One line: proper fix, requires frontend change + native rebuild.

Architecture:

    client ──▶ profiles SELECT (RLS-gated)
                    │
        ┌───────────┴───────────┐
        │                       │
    Option A                Option B
    add policy           strip policy + RPC
    (no FE change)       (FE migration needed)
```

Then call `/interview` to walk Stevie through the pick.

## Error handling

- Missing `bd` → skip beads sections; render note `_(beads not initialised)_`
- Missing `gh` → skip PR sections; render note `_(gh CLI not authenticated)_`
- Missing `tmux` → skip swarm section; render note `_(tmux unavailable)_`
- `jq` is required — if missing, fail with `Stevie — install jq, /standup needs it`
- TaskList tool unavailable in current session → skip todo + agent enumeration silently
- Transcript parse failure for "decision points" → skip that sub-source silently

## Cost

`/standup` should run in **<10 seconds** end-to-end. No LLM-heavy operations. No `/pr-signals` fan-out. No external API calls beyond `gh` and `bd`.

## Future flags (NOT in v1)

- `/standup --since <ISO-date>` — override the cutoff
- `/standup --no-state` — don't read or write the state file
- `/standup --json` — emit machine-readable output
- Cron / scheduled mode → wire into `/schedule`

## Hard rules

- READ-ONLY. Never `bd update`, `bd close`, `gh pr merge`, `gh pr comment`, `git commit`, etc.
- NEVER post to Slack / Discord / GH comments — output is stdout only
- NEVER re-run `/pr-signals` — labels are the source of truth
- NEVER claim a bead or change assignee
- The state file is the only write — under `.agents/standup/` (tool-neutral location, same convention as `.agents/MEMORY.md` and `.agents/pr-signals/`). Add `.agents/standup/` to project `.gitignore` if you don't want per-branch state committed.
- Emojis allowed ONLY for status semantics: 🟢/🔵/🟡/🔴/⚫ status balls in status & priority columns, `🔴` on the Inputs section header, ✅/✘/⏳ in CI rollup. No decorative emoji elsewhere.

## Caveman default

Output text obeys Stevie's caveman mode: drop articles, filler, hedging, pleasantries. Keep technical terms exact. Tables stay as tables — don't caveman the column headers, just the prose surrounding them.

## When to use

- Stevie types `/standup`, "give me a standup", "what's the state of this branch"
- Stevie asks "what do you need from me?" — run /standup, especially section 7
- Stevie returns to a worktree after time away — proactive trigger
- Beginning of a working session in an unfamiliar worktree

## When NOT to use

- For cross-branch / cross-project surveys — different tool
- For active task tracking within a single conversation — TodoWrite owns that
- For PR-level deep dive — use `/pr-signals` or open the PR
- For incident triage — use `/incident-investigate`
