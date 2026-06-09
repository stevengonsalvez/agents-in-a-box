---
name: standup
description: Read-only branch-scoped situation report for Stevie. Surfaces beads in-flight, what's been done since last invocation, what's pending, active swarms/coding-agents/subagents in this worktree, and — most importantly — what specifically needs Stevie's input (decisions, PR merges, AskUserQuestion threads, blocked beads). Output is progressive tables, one shape per stage, address the user as Stevie. Trigger on /standup, "give me a standup", "what's the state of this branch", "what do you need from me". Read-only — never claims beads, never merges, never posts anywhere.
---

# Standup — Stevie's branch-scoped situation report

A standup is not a status dump. It is shaped so Stevie can act in 30 seconds: know the state of the branch, and know exactly what is waiting on **him**. Read-only — it reports, it never touches.

Always address the reader as **Stevie**. Output in **caveman mode** by default — drop articles, filler, hedging — unless he's already in normal mode in this conversation.

## What Stevie needs from a standup

Six facts drive every rule below. Every rule traces back to one of these.

1. **He scans the left edge; he does not read prose.** Eyes drop down the first column looking for trouble. Anything important that isn't a ball or a cell in column one is invisible.
2. **The headline is "what needs ME."** Shipped/pending/next are context. The Inputs section is the lede — it must surface even when every other section is empty.
3. **A session running with unknown scope is a hidden blocker.** An agent burning tokens on something Stevie can't name is a risk, not a status line. The "working on" column is load-bearing.
4. **He returns to worktrees cold.** Cross-session working memory is zero. State must be externalized to disk, and every report framed as "since the last standup."
5. **Read-only is the trust contract.** A standup he can run on autopilot is one that never mutates. The moment it might `bd close` or `gh merge`, he has to babysit it — and then it's useless.
6. **Speed is the product.** A standup that costs 30s of LLM churn is a standup he won't run. Under 10s, no fan-out, no API beyond `gh`/`bd`.

## Output rules

Each rule names the fact it serves. Bad/Good shown so the shape is unambiguous.

### 1. Lead with counts, climax on Inputs (Fact 2)

First shape is the one-row Counts table. Last shape is the Inputs table. Everything between is context the eye can skip.

Bad: bury "PR #2535 ready to merge" as row 7 of the beads table.
Good: Counts row up top, then context tables, then `### Inputs needed from Stevie 🔴` as the final, aggressively-surfaced section.

### 2. One unified beads table — never sub-tables (Fact 1)

All four states (shipped / pending / next / blocked) render in ONE table. Splitting into in-progress/ready/blocked sub-tables forces re-scanning — Stevie hates that.

Bad:
```
### In progress
| id | title |
### Ready
| id | title |
### Blocked
| id | title |
```

Good:
```
### Beads
| id | title | status | PR |
|---|---|---|---|
| ag-xx | … | 🟢 shipped | #2535 → merged |
| ag-yy | … | 🔵 pending | #2540 → open |
| ag-zz | … | 🟡 next (P1) | — |
| ag-ww | … | 🔴 blocked (by ag-vv) | — |
```
Sort: shipped → pending → next → blocked.

### 3. Status ball on the left edge of every status/priority cell (Fact 1)

The ball is the scan target. Plain-text status loses the affordance the moment a table passes ~10 rows (common when many beads are blocked).

Bad: `| ag-ww | … | blocked | — |`
Good: `| ag-ww | … | 🔴 blocked (by ag-vv) | — |`

Enum: 🟢 shipped · 🔵 pending · 🟡 next (P\<n>) · 🔴 blocked (by …). Priority column: 🔴 high · 🟡 medium · 🔵 low.

### 4. Options → horizontal-rule separators + one line each (Fact 1)

When a section enumerates options/alternatives/files-affected, separate each with `---` and give each ONE line of rationale. Never bullet-soup.

Bad:
```
Options: we could add a policy (no FE change but leaves RPC open), or we could
strip the policy and add an RPC (proper fix but needs a frontend migration and
native rebuild), or we could…
```

Good:
```
**Option A — add hotfix policy**
Unblocks users now, leaves RPC follow-up open.

---

**Option B — strip policy + RPC migration**
Proper fix; needs frontend change + native rebuild.
```

### 5. Structural comparison → ASCII diagram, not prose (Fact 1)

When the thing being explained is structural (data flow, RLS chain, deploy sequence, who-calls-what), draw it. ASCII beats prose every time. Box-drawing chars used sparingly — art for its own sake is noise.

Bad: "The client hits the edge function, which validates with Zod and then dispatches to the RPC layer, which writes to the audit log…"

Good:
```
client ──▶ /functions/v1/foo ──▶ alex.* (RPC) ──▶ public.alex_audit_log
                                    │
                                    └─ Zod validate ──▶ tool dispatcher
```

### 6. Decisions → /interview, not raw AskUserQuestion (Fact 2)

When the Inputs section surfaces a decision Stevie must make (a row flagged `needs your call`), invoke `/interview` to pace the dialogue — standup decisions are usually multi-faceted (which option, which scope, when). A single `AskUserQuestion` is fine ONLY for a clean binary pick.

Bad: fire one AskUserQuestion with "Merge PR #2535? yes/no" when the real decision is option-A-vs-B with scope and timing attached.
Good: surface the row, render the option block (rule 4) + diagram (rule 5), then call `/interview`.

### 7. Caveman prose, exact terms, tables stay tables (Fact 6)

Surrounding prose obeys caveman mode: drop articles, filler, hedging, pleasantries. Keep technical terms exact. Do NOT caveman the column headers or cell content — tables stay readable.

Bad: "I have gone ahead and checked all of the beads for you, and it looks like there might possibly be a few that could be blocked."
Good: "22 blocked. Top 5 below."

### 8. Emojis only carry status semantics (Fact 1)

Emoji allowed ONLY where it encodes state: 🟢/🔵/🟡/🔴/⚫ status balls, 🔴 on the Inputs header, ✅/✘/⏳ in CI rollups. No decorative emoji anywhere else — it dilutes the scan signal.

Bad: `### 🚀 Beads update! ✨`
Good: `### Beads`

### 9. Cap every list (Fact 1)

Ranked-and-capped beats long-and-flat. Blocked beads cap at 5 + `_(+ N more 🔴 — see bd blocked)_`. Inputs cap at 10. Next cap at 5. If a list would run longer, it gets capped and the overflow noted, never dumped.

## Section order (fixed — 4 tables + optional decision block)

1. **Summary** — 1-row counts table: shipped / next / pending / blocked / inputs, each prefixed with its status ball.
2. **Beads** — the ONE unified table (rule 2). Columns: `id | title | status | PR`. PR column: `#NNNN → merged|open|draft` for shipped, `—` otherwise.
3. **PRs raised in this worktree** — one table: `# | title | state | CI | labels | when`. State ball: 🟢 merged · 🟡 open · ⚫ draft · 🔴 closed-unmerged.
4. **Active swarms / coding-agents / subagents** — one table: `name | source | working on | last activity | status`. Status ball: 🟢 running · 🟡 idle (>5min) · 🔴 stuck. "working on" shows the actual bead/task, never just the session name (Fact 3).
5. **Inputs needed from Stevie 🔴** — THE headline section (Fact 2). Surface aggressively, build even when everything else is empty.

Close with: `Anything else, Stevie?`

## How to gather the data (execution sequence)

Run in order. Skip a section gracefully if its data source is missing — never fail the whole standup because `bd`/`gh`/`tmux` isn't present.

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

### 1. Establish "since" cutoff (Fact 4)

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

Render per **output rules 2 + 3 + 9**: one table, columns `id | title | status | PR`, balls on the left edge, blocked capped at 5 + overflow note. PR column populates `#NNNN → merged|open|draft` for shipped beads, `—` for everything else.

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

`pr-signals` is **NOT** re-run — labels are the truth. If `comments_fixed` + all-green CI, flag this row in the Inputs section.

### 5. Active swarms / coding-agents / subagents (this worktree only) (Fact 3)

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
| swarm-1778…-agent-1 | tmux | shotclubhouse-ag-z0nc | 2m ago | 🟢 running |
| Task agent | TaskList | "validate RLS probe" | 30s ago | 🟢 running |

The **`working on` column is load-bearing** (Fact 3). If empty for a row, mark it `?` and surface in the Inputs section as a "session running with unknown scope" input. If the whole section is empty, render `_(none active)_` rather than skipping.

### 6. Inputs needed from Stevie 🔴 (Fact 2)

THIS is the highest-signal section. Build the table even if every other section is empty.

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
   TRANSCRIPT=$(ls -t ~/.claude/projects/-${PROJECT_DIR}/*.jsonl 2>/dev/null | head -1)
   ```

   Best-effort — if parsing fails, skip silently.

e. **Sessions running with unknown scope** (from step 5): any agent row whose "working on" resolved to `?` is an input — Stevie can't see what it's burning tokens on (Fact 3).

f. **Explicit `// @stevie:` markers in branch diff**:
   ```bash
   git diff $(git merge-base HEAD main)..HEAD -- ':!*.lock' \
     | grep -E "^\+.*(// @stevie:|#shame:blocked|TODO:.*stevie)" \
     | sed 's/^+//'
   ```

**Render** (per output rules 3 + 9):

| source | item | action | priority |
|---|---|---|---|
| PR #2535 | popashot-pr-comments-fixed + CI green | admin-merge ready | 🔴 high |
| Bead -ag-xx | blocked: awaiting decision on caching strategy | needs your call | 🔴 high |
| tmux swarm-1778 | "(Y/n)" prompt unanswered for 12m | reply or kill | 🟡 medium |
| chat | Last assistant message ended with "Want me to ship option 2?" — no reply yet | answer in chat | 🔴 high |

Sort by priority (high first), cap at 10 rows. If a row needs a multi-faceted decision, follow output rules 4 + 5 + 6 (option block → diagram → `/interview`). If empty, render: `Stevie — nothing waiting on you. Carry on.`

### 7. Persist state (Fact 4)

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

### Decision sub-block (when the Inputs section surfaces a >1-option choice)

If the Inputs section surfaces a decision with more than one option, OR a PR with a structural change worth explaining (RLS chain, deploy sequence), append a sub-block AFTER the Inputs table using output rules 4 + 5, then call `/interview`:

```markdown
### Option to decide — <one-line context>

**Option A — ship hotfix policy**
Unblocks users now, leaves RPC follow-up open.

---

**Option B — ship RPC migration**
Proper fix, requires frontend change + native rebuild.

Architecture:

    client ──▶ profiles SELECT (RLS-gated)
                    │
        ┌───────────┴───────────┐
        │                       │
    Option A                Option B
    add policy           strip policy + RPC
    (no FE change)       (FE migration needed)
```

## Pre-render check

Before rendering, delete:

1. Any prose paragraph that a table could carry instead (output rule 2).
2. Any decorative emoji that doesn't encode status (output rule 8).
3. Any hedging adverb in the surrounding prose — "perhaps," "might," "could possibly" (output rule 7).
4. Any list past its cap; replace the tail with an overflow note (output rule 9).

Then verify the one test: **if Stevie reads ONLY the Counts row and the Inputs table, does he know (a) the state of the branch and (b) exactly what is waiting on him?**

If yes, render. If no, the Counts row or the Inputs section is under-built — fix that before anything else.

## When to break the rules

Override the defaults when:

1. **Stevie asks to "explain" or "walk me through" a row.** Drop into full prose for that one item — still no preamble, still caveman, but the body runs as long as the topic needs. Add a header so he can skim back to the table.
2. **A decision is genuinely binary.** Skip `/interview` (output rule 6) and fire a single `AskUserQuestion` — pacing a yes/no wastes his time.
3. **Zero data everywhere.** If beads, PRs, agents, and inputs are all empty, skip the four context tables and render just: `Stevie — branch <X> is quiet. Nothing in flight, nothing waiting on you.`
4. **He's already in normal mode this conversation.** Honour it — don't force caveman back on.

## Error handling

- Missing `bd` → skip beads sections; render note `_(beads not initialised)_`
- Missing `gh` → skip PR sections; render note `_(gh CLI not authenticated)_`
- Missing `tmux` → skip swarm section; render note `_(tmux unavailable)_`
- `jq` is required — if missing, fail with `Stevie — install jq, /standup needs it`
- TaskList tool unavailable in current session → skip todo + agent enumeration silently
- Transcript parse failure for "decision points" → skip that sub-source silently

## Hard rules

- READ-ONLY (Fact 5). Never `bd update`, `bd close`, `gh pr merge`, `gh pr comment`, `git commit`, etc.
- NEVER post to Slack / Discord / GH comments — output is stdout only
- NEVER re-run `/pr-signals` — labels are the source of truth
- NEVER claim a bead or change assignee
- The state file is the only write — under `.agents/standup/` (tool-neutral location, same convention as `.agents/MEMORY.md` and `.agents/pr-signals/`). Add `.agents/standup/` to project `.gitignore` if you don't want per-branch state committed.

## Cost (Fact 6)

`/standup` should run in **<10 seconds** end-to-end. No LLM-heavy operations. No `/pr-signals` fan-out. No external API calls beyond `gh` and `bd`.

## Future flags (NOT in v1)

- `/standup --since <ISO-date>` — override the cutoff
- `/standup --no-state` — don't read or write the state file
- `/standup --json` — emit machine-readable output
- Cron / scheduled mode → wire into `/schedule`

## When to use

- Stevie types `/standup`, "give me a standup", "what's the state of this branch"
- Stevie asks "what do you need from me?" — run /standup, especially the Inputs section
- Stevie returns to a worktree after time away — proactive trigger
- Beginning of a working session in an unfamiliar worktree

## When NOT to use

- For cross-branch / cross-project surveys — different tool
- For active task tracking within a single conversation — TodoWrite owns that
- For PR-level deep dive — use `/pr-signals` or open the PR
- For incident triage — use `/incident-investigate`
