---
name: swarm-create
description: Create a new self-sufficient swarm team from a Beads epic with N worker agents + a watchdog daemon that auto-recovers stuck panes and notify-only finalizes when the epic is done. Cross-provider (Claude/Codex/Copilot).
user-invocable: true
---

# /swarm-create

Create a tmux-persistent multi-agent swarm team for parallel task execution,
backed by a cross-provider bash watchdog that prevents agents from getting
stuck indefinitely and writes a notify-only finalize report when the epic
completes.

> **v2 changes from v1**: ships a watchdog tmux session that detects stuck
> panes, sends Enter / "continue" / leader-help nudges, and runs `finalize.sh`
> when the epic is done. Never auto-kills tmux. Never auto-merges. Cross-
> provider via explicit `--provider` flag.

## Usage

```bash
/swarm-create --epic <epic-id> --agents <count> --provider <claude|codex|copilot> \
  [--isolation <shared|worktree>] [--tick-min <N>] [--auto-pr] [--use-loop] \
  [--verify-cmd <cmd>] [--no-watchdog] [--dry-run]
```

## Arguments

| Argument | Required | Default | Description |
|----------|----------|---------|-------------|
| `--epic` | Yes | - | Beads epic ID to create swarm from |
| `--agents` | No | 2 | Number of worker agents (max 4) |
| `--provider` | **Yes** | - | Agent runtime: `claude`, `codex`, `copilot`, or `generic`. Determines spinner detection. Ask the user if not given. |
| `--isolation` | No | (ask user) | `shared` or `worktree` |
| `--tick-min` | No | 5 | Watchdog tick interval in minutes (1-60) |
| `--auto-pr` | No | off | If set, on epic-done with green verify, watchdog opens a **draft** PR via `gh`. Default off. |
| `--use-loop` | No | off | Claude-only: also arm a leader `/loop` for redundant fast-tick. Bash watchdog still runs. |
| `--verify-cmd` | No | auto-detect | Override the auto-detected verify command. Auto-detect: Cargo.toml→`cargo test --workspace --no-fail-fast`, package.json→`npm test`, pyproject.toml→`pytest`, go.mod→`go test ./...`, Makefile→`make test`, etc. |
| `--no-watchdog` | No | off | Escape hatch — skip watchdog entirely. Use v1 (no auto-recovery) behavior. |
| `--dry-run` | No | off | Preview without executing |

## Isolation Modes

Same as v1: `shared` (faster, no merge step) vs `worktree` (full isolation,
needs explicit merge step at the end). The watchdog respects both — for
worktree mode, finalize runs a **dry-run merge** per agent branch and reports
conflicts without actually merging.

**You MUST ask the user which isolation mode they want if not specified.**

## Process

When you receive this command:

1. **Parse Arguments**
   ```bash
   EPIC_ID=""           # --epic
   AGENT_COUNT=2        # --agents
   PROVIDER=""          # --provider (will ask if missing)
   ISOLATION_MODE=""    # --isolation (will ask if missing)
   TICK_MIN=5           # --tick-min
   AUTO_PR=false        # --auto-pr
   USE_LOOP=false       # --use-loop
   VERIFY_CMD=""        # --verify-cmd (will auto-detect if missing)
   USE_WATCHDOG=true    # negated by --no-watchdog
   DRY_RUN=false        # --dry-run
   ```

2. **Ask for provider if not specified**

   Use `AskUserQuestion`:

   ```
   question: "Which agent runtime will the swarm panes run?"
   header: "Provider"
   options:
     - label: "claude"
       description: "Claude Code TUI (default — spinner: ✻/✳/⏺/✿ + verbs)"
     - label: "codex"
       description: "OpenAI Codex CLI (braille spinner — placeholder, may need calibration)"
     - label: "copilot"
       description: "GitHub Copilot CLI (placeholder)"
     - label: "generic"
       description: "Unknown / mixed — pane-hash heuristic only"
   ```

3. **Ask for isolation mode if not specified** (same as v1)

4. **Auto-detect verify command**
   ```bash
   if [[ -z "$VERIFY_CMD" ]]; then
     VERIFY_CMD=$(bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh detect-verify-cmd "$PWD")
     echo "Auto-detected verify cmd: $VERIFY_CMD"
   fi
   ```

   Show the user what was detected and let them confirm/override.

5. **Validate Epic** (if not dry-run)
   ```bash
   bd show "$EPIC_ID" --json || echo "Epic not found"
   ```

6. **Create Beads Swarm**
   ```bash
   bd swarm create "$EPIC_ID" --json
   ```

7. **Initialize Team Directory** (v1 helper)
   ```bash
   source {{HOME_TOOL_DIR}}/utils/swarm-lib.sh
   TEAM_ID=$(swarm_create_team "$EPIC_ID" "$AGENT_COUNT" "$ISOLATION_MODE")
   ```

8. **Initialize v2 team.json schema fields**
   ```bash
   bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh init-v2-team-json \
     "$TEAM_ID" "$PROVIDER" "$VERIFY_CMD" "$TICK_MIN" "$AUTO_PR" "$USE_LOOP" "$PWD"
   ```

9. **Spawn Leader** (v1 helper, now has v2-aware prompt baked in)
   ```bash
   LEADER_SESSION=$(swarm_spawn_leader "$TEAM_ID")
   ```

10. **Spawn Worker Agents** (v1 helper, also v2-aware prompt)
    ```bash
    for i in $(seq 1 $AGENT_COUNT); do
      AGENT_SESSION=$(swarm_spawn_agent "$TEAM_ID" "agent-$i")
      sleep 2
    done
    ```

11. **Spawn Watchdog Daemon** (unless `--no-watchdog`)
    ```bash
    if [[ "$USE_WATCHDOG" == "true" ]]; then
      WATCHDOG_SESSION=$(bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh spawn-watchdog "$TEAM_ID")
      echo "Watchdog spawned: $WATCHDOG_SESSION"
    fi
    ```

12. **Optional: arm leader /loop** (Claude only, `--use-loop`)

    If `$PROVIDER == "claude"` and `$USE_LOOP == "true"`, separately arm a
    `/loop` on the leader for redundant fast-tick. This requires the leader's
    Claude TUI to be receptive — the bash watchdog is the primary tick path.

13. **Verify & Report**

    ```bash
    swarm_get_status "$TEAM_ID" | jq .

    echo ""
    echo "========================================"
    echo "Swarm Created (v2): $TEAM_ID"
    echo "========================================"
    echo "Epic:         $EPIC_ID"
    echo "Provider:     $PROVIDER"
    echo "Isolation:    $ISOLATION_MODE"
    echo "Leader:       ${TEAM_ID}-leader"
    echo "Agents:       $AGENT_COUNT"
    if [[ "$USE_WATCHDOG" == "true" ]]; then
      echo "Watchdog:     ${TEAM_ID}-watchdog (${TICK_MIN}-min ticks)"
    else
      echo "Watchdog:     DISABLED (--no-watchdog)"
    fi
    echo "Verify cmd:   $VERIFY_CMD"
    echo "Auto-PR:      $AUTO_PR"
    echo ""
    echo "Commands:"
    echo "  Attach to leader:    tmux attach -t ${TEAM_ID}-leader"
    if [[ "$USE_WATCHDOG" == "true" ]]; then
      echo "  Attach to watchdog:  tmux attach -t ${TEAM_ID}-watchdog"
      echo "  Watchdog log:        tail -f {{HOME_TOOL_DIR}}/swarm/${TEAM_ID}/watchdog.log"
      echo "  Kill watchdog only:  bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh kill-watchdog ${TEAM_ID}"
    fi
    echo "  Status:              /swarm-status ${TEAM_ID}"
    echo "  Shutdown:            /swarm-shutdown ${TEAM_ID}"
    echo ""
    echo "Watchdog behavior:"
    echo "- Captures all panes every ${TICK_MIN}min"
    echo "- Nudges stuck panes (Enter → 'continue' + Enter → leader-help-inbox)"
    echo "- On epic-done: runs finalize.sh (notify-only, writes report)"
    echo "- NEVER kills tmux. NEVER auto-merges. Human owns those steps."
    echo "========================================"
    ```

## Dry Run Output

```
DRY RUN: Would create v2 swarm from epic: bd-epic-123

Provider:        claude
Isolation:       worktree
Agents:          3
Watchdog:        ON (5-min ticks)
Verify cmd:      cargo test --workspace --no-fail-fast (auto-detected)
Auto-PR:         off
Use-loop:        off

Would create:
  - Team directory: {{HOME_TOOL_DIR}}/swarm/swarm-XXXXXXXXXX/
  - team.json with v2 schema (provider, commands, ci, watchdog, finalize)
  - Inbox files: leader.jsonl, agent-1.jsonl, agent-2.jsonl, agent-3.jsonl
  - Worktrees (worktree mode): agent-1, agent-2, agent-3

Would spawn tmux sessions:
  - swarm-XXXXXXXXXX-leader
  - swarm-XXXXXXXXXX-agent-{1,2,3}
  - swarm-XXXXXXXXXX-watchdog (bash daemon)

Would run:
  bd swarm create bd-epic-123

No changes made.
```

## Examples

```bash
# Basic v2 (cross-provider, claude)
/swarm-create --epic bd-epic-skill-manager-v1 --agents 3 --provider claude

# Codex agents
/swarm-create --epic bd-epic-X --agents 2 --provider codex

# Aggressive watchdog cadence
/swarm-create --epic bd-epic-X --agents 3 --provider claude --tick-min 3

# Belt-and-suspenders Claude swarm with auto-PR
/swarm-create --epic bd-epic-X --agents 3 --provider claude --use-loop --auto-pr

# Escape hatch: v1 behavior (no auto-recovery)
/swarm-create --epic bd-epic-X --agents 3 --provider claude --no-watchdog
```

## Worktree Mode — Post-Swarm Workflow

When using worktree isolation, after the swarm completes (finalize runs):

```bash
# 1. Watchdog writes finalize-report.md to shared/
cat {{HOME_TOOL_DIR}}/swarm/<team-id>/shared/finalize-report.md

# 2. Inspect conflict report. If clean, do the actual merge:
bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh merge-worktrees <team-id>

# 3. Review the merged changes
git log --oneline -10

# 4. Push and open PR (if not done via --auto-pr)
git push -u origin HEAD
gh pr create --title "..." --body-file {{HOME_TOOL_DIR}}/swarm/<team-id>/shared/finalize-report.md

# 5. Shutdown the swarm (you own this — watchdog never does it)
/swarm-shutdown <team-id>
```

## Integration with Beads

Same as v1:
- **Epic** = parent issue
- **bd ready --unassigned** = ready work pool
- **bd swarm create** = bd-side bookkeeping

## What This Does NOT Do (v2 hard constraints)

- ❌ Never kills tmux sessions automatically — human owns `/swarm-shutdown`
- ❌ Never auto-merges worktree branches — finalize does dry-run only
- ❌ Never marks a PR as ready — auto-PR opens **draft** only
- ❌ Never modifies code in worker panes directly — watchdog only sends Enter / "continue" / writes inbox

## Hard Failure Modes & Mitigations

| Failure | What watchdog does | What human still owns |
|---|---|---|
| Worker pane has stuck text in `❯` prompt | Send Enter → "continue" + Enter → log to leader.jsonl | Reviewing if leader correctly recovers |
| Worker capped (5h limit hit) | Skip nudge, note in log, continue | Wait for cap reset OR run /swarm-attach-watchdog with different provider config |
| Leader pane stuck | Same nudge sequence as workers | Same as above |
| Watchdog tmux session dies | Nothing — daemon is dead | Run `/swarm-attach-watchdog <team-id>` to respawn |
| Epic-done but verify fails | Writes failed verify in report; does NOT touch worktrees or PR | Fix code + re-trigger by clearing `.finalized` marker |
| All workers capped, leader idle | Watchdog logs 'all_capped' to leader.jsonl, keeps ticking | Wait for cap or reassign work |

## Files Created at Swarm Creation

```
{{HOME_TOOL_DIR}}/swarm/<team-id>/
├── team.json                       # v2 schema (provider, commands, ci, watchdog, finalize, ...)
├── inbox/
│   ├── leader.jsonl
│   └── agent-{1..N}.jsonl
├── shared/                         # populated by workers + finalize.sh
├── watchdog.pid                    # written by watchdog at startup
├── watchdog.log                    # tick-by-tick log
├── watchdog.state                  # per-pane stuck counters + hashes
└── .finalized                      # sentinel — set after finalize.sh runs once
```

Tmux sessions:
```
<team-id>-leader        # Claude/Codex/Copilot TUI (orchestrator)
<team-id>-agent-{1..N}  # Worker TUIs
<team-id>-watchdog      # Pure bash daemon — captures + nudges + finalizes
```

## Migration from v1

Existing v1 swarms can be retrofitted in-place — no need to recreate them.

```bash
/swarm-attach-watchdog <team-id> --provider claude
```

This upgrades `team.json` to the v2 schema and spawns the watchdog tmux session
without touching the leader or worker tmux sessions.

See `/swarm-attach-watchdog` skill for details.
