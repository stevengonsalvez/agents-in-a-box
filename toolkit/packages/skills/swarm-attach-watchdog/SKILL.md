---
name: swarm-attach-watchdog
description: Retrofit a watchdog daemon onto an existing v1 swarm (no recreation). Upgrades team.json to v2 schema and spawns the watchdog tmux session.
user-invocable: true
---

# /swarm-attach-watchdog

Attach the v2 watchdog daemon to an existing swarm team without recreating it.
Use this to recover an in-flight swarm whose agents keep stalling because there
was no automatic stuck-pane detection in v1.

## Usage

```bash
/swarm-attach-watchdog <team-id> [--provider <claude|codex|copilot>] [--tick-min <N>] [--verify-cmd <cmd>]
```

| Argument | Required | Default | Description |
|----------|----------|---------|-------------|
| `<team-id>` | Yes | - | Existing swarm team id (e.g. `swarm-1778723020`) |
| `--provider` | No | ask | Which agent runtime the panes are running. Sets spinner regex. |
| `--tick-min` | No | 5 | Watchdog tick interval in minutes (min 1, max 60) |
| `--verify-cmd` | No | auto-detected | Override the auto-detected verify command. Auto-detect maps Cargo.toml→`cargo test --workspace --no-fail-fast`, package.json→`npm test`, pyproject.toml→`pytest`, go.mod→`go test ./...`, Makefile→`make test`, etc. |

## Process

When the user runs this command:

1. **Parse args**
   ```bash
   TEAM_ID="$1"
   PROVIDER=""        # from --provider
   TICK_MIN=5         # from --tick-min
   VERIFY_CMD=""      # from --verify-cmd
   ```

2. **Validate team exists**
   ```bash
   TEAM_DIR="${HOME}/.claude/swarm/${TEAM_ID}"
   if [[ ! -d "$TEAM_DIR" || ! -f "${TEAM_DIR}/team.json" ]]; then
     echo "Error: team not found at $TEAM_DIR"
     exit 1
   fi
   ```

3. **Ask for provider if not specified**

   Use `AskUserQuestion`:

   ```
   question: "Which agent runtime are the swarm tmux panes running?"
   header: "Provider"
   options:
     - label: "claude"
       description: "Claude Code TUI (spinner: ✻/✳/⏺/✿ + verb)"
     - label: "codex"
       description: "OpenAI Codex CLI (spinner: braille dots ⠋⠙⠹⠸⠼⠴⠦⠧)"
     - label: "copilot"
       description: "GitHub Copilot CLI (spinner: braille dots — placeholder)"
     - label: "generic"
       description: "Unknown / mixed — falls back to pane-hash heuristic only"
   ```

4. **Auto-detect verify command if not specified**
   ```bash
   if [[ -z "$VERIFY_CMD" ]]; then
     VERIFY_CMD=$(bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh detect-verify-cmd "$PWD")
     echo "Auto-detected verify: $VERIFY_CMD"
   fi
   ```

5. **Check no existing watchdog**
   ```bash
   if tmux has-session -t "${TEAM_ID}-watchdog" 2>/dev/null; then
     echo "Watchdog already running for $TEAM_ID. Use 'tmux attach -t ${TEAM_ID}-watchdog' to inspect."
     exit 0
   fi
   ```

6. **Attach watchdog**
   ```bash
   bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh attach-watchdog \
     "$TEAM_ID" "$PROVIDER" "$VERIFY_CMD" "$TICK_MIN"
   ```

   This:
   - Upgrades `team.json` in place with v2 schema fields (provider, commands.verify, watchdog config, finalize config)
   - Spawns `<team-id>-watchdog` tmux session running `watchdog.sh <team-id>`

7. **Report**
   ```
   ==========================================
   Watchdog attached: swarm-XXXXXXXXXX-watchdog
   ==========================================
   Provider:   claude
   Tick:       5min
   Verify cmd: cargo test --workspace --no-fail-fast

   Commands:
     Attach to watchdog:  tmux attach -t swarm-XXXXXXXXXX-watchdog
     Watchdog log:        tail -f {{HOME_TOOL_DIR}}/swarm/swarm-XXXXXXXXXX/watchdog.log
     Kill watchdog only:  bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh kill-watchdog swarm-XXXXXXXXXX
     Status:              /swarm-status swarm-XXXXXXXXXX

   The watchdog will:
   - Capture leader + agent panes every 5min
   - Send Enter (then "continue" + Enter) to stuck panes
   - Escalate to leader.jsonl after 2 stuck cycles
   - On epic-done: run finalize.sh (notify-only, NO tmux kill, NO auto-merge)

   It will NEVER kill tmux sessions or auto-merge worktrees.
   Human owns merge + PR ready-marking + /swarm-shutdown.
   ==========================================
   ```

## When to Use

- An existing v1 swarm has stalled agents and you don't want to recreate it
- You're upgrading from v1 to v2 without restarting in-flight work
- You want to add watchdog capability to a swarm that was started with `--no-watchdog`

## What This Does NOT Do

- Does not restart leader or worker tmux sessions (they keep their context)
- Does not modify worker prompts (existing workers don't get the v2 awareness — that requires a new spawn)
- Does not kill any tmux session
- Does not merge any worktrees
- Does not open a PR (those happen at finalize time, opt-in)

## Troubleshooting

**Watchdog session dies immediately**
- Check `{{HOME_TOOL_DIR}}/swarm/<team-id>/watchdog.log` for the error
- Common: `team.json` not found, `jq` missing, `tmux` not on PATH

**False-stuck detections**
- Provider regex may not cover all spinner states for the runtime
- Set `--provider generic` to fall back to pure pane-hash heuristic

**Two watchdog sessions accidentally spawned**
- Run `bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh kill-watchdog <team-id>` once, then `/swarm-attach-watchdog <team-id>` again
- The kill-watchdog command only targets `<team-id>-watchdog` — never touches workers
