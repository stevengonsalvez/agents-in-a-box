#!/bin/bash

# Swarm Orchestration Library
# File-based inter-agent messaging for tmux-persistent multi-agent teams
# Integrates with Beads for task coordination and spawn-agent-lib for agent spawning

set -euo pipefail

# ============================================================================
# Configuration & Paths
# ============================================================================

SWARM_BASE_DIR="${PWD}/.claude/swarm"
SPAWN_AGENT_LIB="${HOME}/.claude/utils/spawn-agent-lib.sh"

# Source spawn-agent-lib if available
if [[ -f "$SPAWN_AGENT_LIB" ]]; then
    source "$SPAWN_AGENT_LIB"
fi

# Check if already running inside a Claude Code session
# This prevents nested session errors when spawning agents
is_inside_claude_session() {
    [ -n "${CLAUDECODE:-}" ]
}

# Ensure jq is available
if ! command -v jq &> /dev/null; then
    echo "Error: jq is required but not installed. Install with: brew install jq"
    exit 1
fi

# ============================================================================
# Team Management Functions
# ============================================================================

# swarm_create_team <epic_id> [max_agents] [isolation_mode]
# Creates a new swarm team directory structure
# isolation_mode: "shared" (default) or "worktree"
# Returns: team_id
# Generate a descriptive team_id like `swarm-<branch-slug>-<rand4>`
# so panes are recognisable when many swarms run in parallel.
# Falls back to legacy `swarm-<epoch>` when branch can't be read.
swarm_generate_team_id() {
    local branch
    branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "")
    if [[ -z "$branch" || "$branch" == "HEAD" ]]; then
        echo "swarm-$(date +%s)"
        return
    fi
    # Strip common prefixes
    branch="${branch#feat/}"
    branch="${branch#fix/}"
    branch="${branch#chore/}"
    branch="${branch#docs/}"
    branch="${branch#test/}"
    branch="${branch#refactor/}"
    # Lowercase, replace `/_.` with `-`, drop other non-alnum
    branch=$(echo "$branch" | tr '[:upper:]' '[:lower:]' | tr '/_.' '---' | sed 's/[^a-z0-9-]//g')
    # Collapse repeated `-` and trim leading/trailing
    branch=$(echo "$branch" | sed -E 's/-+/-/g; s/^-+//; s/-+$//')
    # Truncate to 18 chars so the full session name stays under ~32
    branch="${branch:0:18}"
    branch="${branch%-}"
    # 4-char hex random (RANDOM is 15-bit, mod-distrib OK here)
    local rand
    rand=$(printf "%04x" $((RANDOM % 65536)))
    if [[ -z "$branch" ]]; then
        echo "swarm-$(date +%s)"
    else
        echo "swarm-${branch}-${rand}"
    fi
}

swarm_create_team() {
    local epic_id="$1"
    local max_agents="${2:-4}"
    local isolation_mode="${3:-shared}"  # "shared" or "worktree"

    # Descriptive ID: swarm-<branch>-<rand>. Falls back to swarm-<epoch>
    # when git context can't be read. See swarm_generate_team_id().
    local team_id
    team_id=$(swarm_generate_team_id)
    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local base_branch
    base_branch=$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo "main")

    # Create directory structure
    mkdir -p "${team_dir}/inbox"
    mkdir -p "${team_dir}/shared"

    if [[ "$isolation_mode" == "worktree" ]]; then
        mkdir -p "${team_dir}/worktrees"
    fi

    # Create leader inbox
    touch "${team_dir}/inbox/leader.jsonl"

    # Create team.json
    local team_json
    team_json=$(jq -n \
        --arg tid "$team_id" \
        --arg ts "$(date -Iseconds)" \
        --arg epic "$epic_id" \
        --arg iso "$isolation_mode" \
        --arg branch "$base_branch" \
        --argjson max "$max_agents" \
        '{
            team_id: $tid,
            created_at: $ts,
            epic_id: $epic,
            beads_swarm_id: null,
            leader: {
                session: ($tid + "-leader"),
                status: "pending",
                role: "orchestrator"
            },
            members: [],
            config: {
                max_members: $max,
                inbox_poll_seconds: 5,
                idle_timeout_minutes: 15,
                isolation_mode: $iso,
                base_branch: $branch
            }
        }')

    echo "$team_json" > "${team_dir}/team.json"

    echo "$team_id"
}

# swarm_get_team <team_id>
# Returns team.json contents
swarm_get_team() {
    local team_id="$1"
    local team_file="${SWARM_BASE_DIR}/${team_id}/team.json"

    if [[ ! -f "$team_file" ]]; then
        echo "Error: Team $team_id not found" >&2
        return 1
    fi

    cat "$team_file"
}

# swarm_update_team <team_id> <json_update>
# Merges update into team.json
swarm_update_team() {
    local team_id="$1"
    local update="$2"
    local team_file="${SWARM_BASE_DIR}/${team_id}/team.json"

    if [[ ! -f "$team_file" ]]; then
        echo "Error: Team $team_id not found" >&2
        return 1
    fi

    local updated
    updated=$(jq --argjson upd "$update" '. + $upd' "$team_file")
    echo "$updated" > "$team_file"
}

# swarm_list_teams
# Lists all active team IDs
swarm_list_teams() {
    if [[ ! -d "$SWARM_BASE_DIR" ]]; then
        return 0
    fi

    for team_dir in "${SWARM_BASE_DIR}"/swarm-*; do
        if [[ -d "$team_dir" ]]; then
            basename "$team_dir"
        fi
    done
}

# ============================================================================
# Agent Spawning Functions
# ============================================================================

# swarm_spawn_leader <team_id> [prompt]
# Spawns the team leader in tmux
swarm_spawn_leader() {
    local team_id="$1"
    local custom_prompt="${2:-}"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local session_name="${team_id}-leader"

    # Check if session already exists
    if tmux has-session -t "$session_name" 2>/dev/null; then
        echo "Leader session already exists: $session_name" >&2
        return 1
    fi

    # Build leader task prompt
    local leader_prompt="You are the TEAM LEADER for swarm team: ${team_id}

## Your Responsibilities:
1. Monitor your inbox at: ${team_dir}/inbox/leader.jsonl
2. Assign ready work from Beads to idle agents
3. Coordinate inter-agent handoffs
4. Track overall progress

## Key Commands:
- Check ready work: bd ready --unassigned --json
- Your team members' inboxes are at: ${team_dir}/inbox/
- Send messages using: swarm_send_message() or /swarm-inbox

## Team Configuration:
- Team file: ${team_dir}/team.json
- Shared context: ${team_dir}/shared/

## Message Protocol:
Poll your inbox every 5 seconds. Message types:
- 'status': Progress update from agent
- 'handoff': Agent passing work to another
- 'help': Agent requesting assistance (NEW: watchdog also writes 'help' when an
         agent has been stuck for >2 cycles — agent name + stuck_snippet in payload)
- 'complete': Agent finished task (verify locally before bd-close + dispatch next!)
- 'finalize_done': Watchdog reports epic-done finalize completed (read
         report_md path; this is INFORMATIONAL — do not auto-merge)

## v2 — Watchdog / Auto-Tick Awareness:
A watchdog daemon runs in tmux session '${team_id}-watchdog' and ticks every
few minutes. It:
- Captures all member panes (yours + workers) and detects stuck/active/capped
- On worker stuck >2 cycles: writes a 'help' message to YOUR inbox
- On epic-done: runs finalize.sh which writes a report and writes
  'finalize_done' to your inbox — DOES NOT MERGE OR KILL TMUX
You should:
1. Treat watchdog 'help' messages with priority — investigate the stuck worker
   (capture-pane, send guidance via inbox, or reassign bd to a free worker)
2. On 'finalize_done': summarize report to user, suggest next steps (merge cmd,
   PR cmd, /swarm-shutdown) — never auto-execute those

Start by reading your team.json and checking for ready work."

    if [[ -n "$custom_prompt" ]]; then
        leader_prompt="${leader_prompt}

## Additional Instructions:
${custom_prompt}"
    fi

    # Spawn using spawn-agent-lib if available, otherwise direct tmux
    # Note: We unset CLAUDECODE in the spawned session so claude doesn't think it's nested
    if type spawn_agent_tmux &>/dev/null; then
        spawn_agent_tmux "$session_name" "$PWD" "$leader_prompt"
    else
        tmux new-session -d -s "$session_name" -c "$PWD"
        tmux send-keys -t "$session_name" "env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT claude --dangerously-skip-permissions" C-m
        sleep 5
        tmux send-keys -t "$session_name" -l "$leader_prompt"
        tmux send-keys -t "$session_name" C-m
        sleep 2
        # Verify prompt was submitted, retry Enter if stuck
        local _pane=$(tmux capture-pane -t "$session_name" -p 2>/dev/null || echo "")
        if echo "$_pane" | grep -qE "bypass permissions|⏵⏵" && ! echo "$_pane" | grep -qE "Thought for|Forming|Creating|⏳|✽|∴|Reading"; then
            sleep 1
            tmux send-keys -t "$session_name" C-m
        fi
    fi

    # Update team.json
    swarm_update_team "$team_id" '{"leader": {"session": "'"$session_name"'", "status": "active", "role": "orchestrator"}}'

    echo "$session_name"
}

# swarm_spawn_agent <team_id> <agent_name> [prompt]
# Spawns a worker agent in tmux
# Automatically uses worktree if team is in worktree isolation mode
swarm_spawn_agent() {
    local team_id="$1"
    local agent_name="$2"
    local custom_prompt="${3:-}"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local session_name="${team_id}-${agent_name}"
    local inbox_file="${team_dir}/inbox/${agent_name}.jsonl"

    # Check if session already exists
    if tmux has-session -t "$session_name" 2>/dev/null; then
        echo "Agent session already exists: $session_name" >&2
        return 1
    fi

    # Check max members
    local max_members
    max_members=$(jq -r '.config.max_members' "${team_dir}/team.json")
    local current_count
    current_count=$(jq -r '.members | length' "${team_dir}/team.json")

    if [[ "$current_count" -ge "$max_members" ]]; then
        echo "Error: Team at max capacity ($max_members members)" >&2
        return 1
    fi

    # Check isolation mode
    local isolation_mode
    isolation_mode=$(jq -r '.config.isolation_mode // "shared"' "${team_dir}/team.json")

    local working_dir="$PWD"
    local worktree_info=""

    if [[ "$isolation_mode" == "worktree" ]]; then
        # Create worktree for this agent
        working_dir=$(swarm_create_worktree "$team_id" "$agent_name")
        worktree_info="
## Git Worktree (Isolated Mode):
- Your working directory: ${working_dir}
- Your branch: ${team_id}-${agent_name}
- IMPORTANT: Commit your changes before reporting task complete!
- Commit command: git add -A && git commit -m \"[${agent_name}] your message\"
- Your changes will be merged to the main branch when swarm completes"
    fi

    # Create agent inbox
    touch "$inbox_file"

    # Build agent task prompt
    local agent_prompt="You are WORKER AGENT '${agent_name}' for swarm team: ${team_id}

## Your Responsibilities:
1. Monitor your inbox at: ${inbox_file}
2. Execute tasks assigned by the leader
3. Report progress and completion back to leader
4. Hand off work to other agents when appropriate

## Key Commands:
- Check your assigned work: bd ready --assignee ${agent_name} --json
- Claim a task: bd update <task-id> --assignee ${agent_name} --status in_progress
- Complete a task: bd close <task-id> --reason \"Done\"
- Send to leader: /swarm-inbox --to leader --msg \"your message\"

## Team Configuration:
- Team file: ${team_dir}/team.json
- Your inbox: ${inbox_file}
- Leader inbox: ${team_dir}/inbox/leader.jsonl
- Isolation mode: ${isolation_mode}
${worktree_info}

## Message Protocol:
Poll your inbox every 5 seconds. Send 'status' updates to leader:
- When starting a task
- At 25%, 50%, 75% progress
- When complete or blocked

## v2 — Watchdog / Auto-Tick Awareness:
This swarm runs a watchdog daemon in tmux session '${team_id}-watchdog' that
ticks every few minutes. On each tick, the watchdog inspects your tmux pane
and may send you 'continue' or Enter if it detects you've stalled with text in
the prompt. When you receive such a nudge:
1. Re-check your inbox at ${inbox_file} for new task messages
2. Run \`bd ready --assignee ${agent_name} --json\` to find work currently
   assigned to you
3. If you have an in-progress bd, RESUME WORK on it (don't wait for re-prompt)
4. If nothing is assigned, write a 'idle' status to leader inbox and stay
   responsive — don't write large multi-line prompt text and sit on it (that's
   what triggers nudges)

Start by checking your inbox for task assignments."

    if [[ -n "$custom_prompt" ]]; then
        agent_prompt="${agent_prompt}

## Additional Instructions:
${custom_prompt}"
    fi

    # Spawn agent in appropriate directory
    # Note: We unset CLAUDECODE in the spawned session so claude doesn't think it's nested
    if type spawn_agent_tmux &>/dev/null; then
        spawn_agent_tmux "$session_name" "$working_dir" "$agent_prompt"
    else
        tmux new-session -d -s "$session_name" -c "$working_dir"
        tmux send-keys -t "$session_name" "env -u CLAUDECODE -u CLAUDE_CODE_ENTRYPOINT claude --dangerously-skip-permissions" C-m
        sleep 5
        tmux send-keys -t "$session_name" -l "$agent_prompt"
        tmux send-keys -t "$session_name" C-m
        sleep 2
        # Verify prompt was submitted, retry Enter if stuck
        local _pane=$(tmux capture-pane -t "$session_name" -p 2>/dev/null || echo "")
        if echo "$_pane" | grep -qE "bypass permissions|⏵⏵" && ! echo "$_pane" | grep -qE "Thought for|Forming|Creating|⏳|✽|∴|Reading"; then
            sleep 1
            tmux send-keys -t "$session_name" C-m
        fi
    fi

    # Add to team members (include worktree path if applicable)
    local new_member
    if [[ "$isolation_mode" == "worktree" ]]; then
        new_member=$(jq -n \
            --arg name "$agent_name" \
            --arg session "$session_name" \
            --arg worktree "$working_dir" \
            --arg branch "${team_id}-${agent_name}" \
            '{name: $name, session: $session, status: "active", current_task: null, worktree: $worktree, branch: $branch}')
    else
        new_member=$(jq -n \
            --arg name "$agent_name" \
            --arg session "$session_name" \
            '{name: $name, session: $session, status: "active", current_task: null}')
    fi

    local updated_team
    updated_team=$(jq --argjson member "$new_member" '.members += [$member]' "${team_dir}/team.json")
    echo "$updated_team" > "${team_dir}/team.json"

    echo "$session_name"
}

# ============================================================================
# Inter-Agent Messaging Functions
# ============================================================================

# swarm_send_message <team_id> <from> <to> <type> <payload>
# Appends a message to an agent's inbox
swarm_send_message() {
    local team_id="$1"
    local from="$2"
    local to="$3"
    local msg_type="$4"
    local payload="$5"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local inbox_file="${team_dir}/inbox/${to}.jsonl"

    if [[ ! -f "$inbox_file" ]]; then
        echo "Error: Inbox for $to not found" >&2
        return 1
    fi

    # Create message
    local message
    message=$(jq -n \
        --arg ts "$(date -Iseconds)" \
        --arg from "$from" \
        --arg to "$to" \
        --arg type "$msg_type" \
        --argjson payload "$payload" \
        '{ts: $ts, from: $from, to: $to, type: $type, payload: $payload}')

    # Append to inbox
    echo "$message" >> "$inbox_file"

    echo "Message sent to $to"
}

# swarm_broadcast <team_id> <from> <type> <payload>
# Sends a message to all team members including leader
swarm_broadcast() {
    local team_id="$1"
    local from="$2"
    local msg_type="$3"
    local payload="$4"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"

    # Send to leader
    swarm_send_message "$team_id" "$from" "leader" "$msg_type" "$payload"

    # Send to all members
    local members
    members=$(jq -r '.members[].name' "${team_dir}/team.json")

    for member in $members; do
        if [[ "$member" != "$from" ]]; then
            swarm_send_message "$team_id" "$from" "$member" "$msg_type" "$payload"
        fi
    done
}

# swarm_read_inbox <team_id> <agent_name> [--unread] [--last N]
# Reads messages from an agent's inbox
swarm_read_inbox() {
    local team_id="$1"
    local agent_name="$2"
    shift 2

    local unread_only=false
    local last_n=""

    while [[ $# -gt 0 ]]; do
        case $1 in
            --unread)
                unread_only=true
                shift
                ;;
            --last)
                last_n="$2"
                shift 2
                ;;
            *)
                shift
                ;;
        esac
    done

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local inbox_file="${team_dir}/inbox/${agent_name}.jsonl"

    if [[ ! -f "$inbox_file" ]]; then
        echo "Error: Inbox for $agent_name not found" >&2
        return 1
    fi

    if [[ -n "$last_n" ]]; then
        tail -n "$last_n" "$inbox_file" | jq -s '.'
    else
        jq -s '.' "$inbox_file"
    fi
}

# swarm_clear_inbox <team_id> <agent_name>
# Clears an agent's inbox (archives to .bak)
swarm_clear_inbox() {
    local team_id="$1"
    local agent_name="$2"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local inbox_file="${team_dir}/inbox/${agent_name}.jsonl"

    if [[ -f "$inbox_file" ]]; then
        mv "$inbox_file" "${inbox_file}.$(date +%s).bak"
        touch "$inbox_file"
        echo "Inbox cleared and archived"
    fi
}

# ============================================================================
# Status & Monitoring Functions
# ============================================================================

# swarm_get_status <team_id>
# Returns comprehensive team status
swarm_get_status() {
    local team_id="$1"
    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local team_file="${team_dir}/team.json"

    if [[ ! -f "$team_file" ]]; then
        echo "Error: Team $team_id not found" >&2
        return 1
    fi

    local team_json
    team_json=$(cat "$team_file")

    # Check tmux session status
    local leader_session
    leader_session=$(echo "$team_json" | jq -r '.leader.session')
    local leader_alive="false"
    if tmux has-session -t "$leader_session" 2>/dev/null; then
        leader_alive="true"
    fi

    # Check each member's session
    local members_status="[]"
    while IFS= read -r member; do
        local name session
        name=$(echo "$member" | jq -r '.name')
        session=$(echo "$member" | jq -r '.session')
        local alive="false"
        if tmux has-session -t "$session" 2>/dev/null; then
            alive="true"
        fi

        # Count unread messages
        local inbox_file="${team_dir}/inbox/${name}.jsonl"
        local unread=0
        if [[ -f "$inbox_file" ]]; then
            unread=$(wc -l < "$inbox_file" | tr -d ' ')
        fi

        local member_status
        member_status=$(echo "$member" | jq \
            --argjson alive "$alive" \
            --argjson unread "$unread" \
            '. + {tmux_alive: $alive, unread_messages: $unread}')

        members_status=$(echo "$members_status" | jq --argjson m "$member_status" '. += [$m]')
    done < <(echo "$team_json" | jq -c '.members[]')

    # Build status object
    local status
    status=$(jq -n \
        --argjson team "$team_json" \
        --argjson leader_alive "$leader_alive" \
        --argjson members "$members_status" \
        '{
            team_id: $team.team_id,
            epic_id: $team.epic_id,
            created_at: $team.created_at,
            leader: ($team.leader + {tmux_alive: $leader_alive}),
            members: $members,
            config: $team.config
        }')

    echo "$status"
}

# swarm_update_member_status <team_id> <agent_name> <status> [task_id]
# Updates a member's status in team.json
swarm_update_member_status() {
    local team_id="$1"
    local agent_name="$2"
    local status="$3"
    local task_id="${4:-null}"

    local team_file="${SWARM_BASE_DIR}/${team_id}/team.json"

    local updated
    if [[ "$task_id" == "null" ]]; then
        updated=$(jq \
            --arg name "$agent_name" \
            --arg st "$status" \
            '(.members[] | select(.name == $name)) |= (. + {status: $st, current_task: null})' \
            "$team_file")
    else
        updated=$(jq \
            --arg name "$agent_name" \
            --arg st "$status" \
            --arg task "$task_id" \
            '(.members[] | select(.name == $name)) |= (. + {status: $st, current_task: $task})' \
            "$team_file")
    fi

    echo "$updated" > "$team_file"
}

# ============================================================================
# Git Worktree Functions (Optional Isolation Mode)
# ============================================================================

# swarm_create_worktree <team_id> <agent_name>
# Creates a git worktree for an agent (isolated working directory)
swarm_create_worktree() {
    local team_id="$1"
    local agent_name="$2"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local base_branch
    base_branch=$(git rev-parse --abbrev-ref HEAD)
    local worktree_branch="${team_id}-${agent_name}"
    local worktree_path="${team_dir}/worktrees/${agent_name}"

    # Create branch for this agent
    git branch "$worktree_branch" "$base_branch" 2>/dev/null || true

    # Create worktree
    mkdir -p "${team_dir}/worktrees"
    git worktree add "$worktree_path" "$worktree_branch"

    echo "$worktree_path"
}

# swarm_remove_worktree <team_id> <agent_name>
# Removes an agent's worktree
swarm_remove_worktree() {
    local team_id="$1"
    local agent_name="$2"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local worktree_path="${team_dir}/worktrees/${agent_name}"
    local worktree_branch="${team_id}-${agent_name}"

    if [[ -d "$worktree_path" ]]; then
        git worktree remove "$worktree_path" --force 2>/dev/null || true
        git branch -D "$worktree_branch" 2>/dev/null || true
        echo "Removed worktree: $worktree_path"
    fi
}

# swarm_list_worktrees <team_id>
# Lists all worktrees for a team
swarm_list_worktrees() {
    local team_id="$1"
    git worktree list | grep "$team_id" || echo "No worktrees found for $team_id"
}

# swarm_merge_worktrees <team_id> [--no-delete]
# Merges all agent worktrees back to the base branch
swarm_merge_worktrees() {
    local team_id="$1"
    local no_delete="${2:-}"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local team_file="${team_dir}/team.json"

    if [[ ! -f "$team_file" ]]; then
        echo "Error: Team $team_id not found" >&2
        return 1
    fi

    # Check if worktree mode is enabled
    local isolation_mode
    isolation_mode=$(jq -r '.config.isolation_mode // "shared"' "$team_file")

    if [[ "$isolation_mode" != "worktree" ]]; then
        echo "Team $team_id is not using worktree isolation mode"
        return 0
    fi

    local base_branch
    base_branch=$(jq -r '.config.base_branch' "$team_file")

    echo "Merging worktrees back to: $base_branch"

    # Get list of agents
    local agents
    agents=$(jq -r '.members[].name' "$team_file")

    local merge_failed=false
    local merged_count=0

    for agent in $agents; do
        local worktree_branch="${team_id}-${agent}"
        local worktree_path="${team_dir}/worktrees/${agent}"

        if [[ ! -d "$worktree_path" ]]; then
            echo "⏭️  Skipping $agent (no worktree)"
            continue
        fi

        # Check if there are commits to merge
        local commits_ahead
        commits_ahead=$(git rev-list --count "${base_branch}..${worktree_branch}" 2>/dev/null || echo "0")

        if [[ "$commits_ahead" == "0" ]]; then
            echo "⏭️  Skipping $agent (no new commits)"
            continue
        fi

        echo "🔀 Merging $agent ($commits_ahead commits)..."

        # Try to merge
        if git merge --no-edit "$worktree_branch" 2>/dev/null; then
            echo "   ✅ Merged $agent successfully"
            ((merged_count++))
        else
            echo "   ❌ Merge conflict for $agent - manual resolution needed"
            git merge --abort 2>/dev/null || true
            merge_failed=true
        fi
    done

    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "Merge Summary: $merged_count agents merged"

    if [[ "$merge_failed" == "true" ]]; then
        echo "⚠️  Some merges had conflicts - resolve manually"
        return 1
    fi

    # Cleanup worktrees if not --no-delete
    if [[ "$no_delete" != "--no-delete" ]]; then
        echo ""
        echo "Cleaning up worktrees..."
        for agent in $agents; do
            swarm_remove_worktree "$team_id" "$agent"
        done
    fi

    echo "✅ All worktrees merged successfully"
}

# swarm_agent_commit <team_id> <agent_name> <message>
# Commits changes in an agent's worktree
swarm_agent_commit() {
    local team_id="$1"
    local agent_name="$2"
    local message="$3"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local team_file="${team_dir}/team.json"
    local worktree_path="${team_dir}/worktrees/${agent_name}"

    # Check isolation mode
    local isolation_mode
    isolation_mode=$(jq -r '.config.isolation_mode // "shared"' "$team_file")

    if [[ "$isolation_mode" == "worktree" ]] && [[ -d "$worktree_path" ]]; then
        # Commit in worktree
        git -C "$worktree_path" add -A
        git -C "$worktree_path" commit -m "[$agent_name] $message" || echo "Nothing to commit"
    else
        # Commit in main repo (shared mode)
        git add -A
        git commit -m "[$agent_name] $message" || echo "Nothing to commit"
    fi
}

# ============================================================================
# Shutdown & Cleanup Functions
# ============================================================================

# swarm_kill_session_processes <session_name>
# Kills all child processes spawned by a tmux session
# This prevents orphaned processes (like tsc, npm, node) after session is killed
swarm_kill_session_processes() {
    local session_name="$1"

    if ! tmux has-session -t "$session_name" 2>/dev/null; then
        return 0
    fi

    # Get the pane PIDs for this session
    local pane_pids
    pane_pids=$(tmux list-panes -t "$session_name" -F '#{pane_pid}' 2>/dev/null || true)

    for pid in $pane_pids; do
        if [[ -n "$pid" ]]; then
            # Kill all child processes of the pane
            # Using pkill with parent PID to get all descendants
            pkill -9 -P "$pid" 2>/dev/null || true

            # Also get grandchildren (processes spawned by children)
            local children
            children=$(pgrep -P "$pid" 2>/dev/null || true)
            for child in $children; do
                pkill -9 -P "$child" 2>/dev/null || true
            done
        fi
    done
}

# swarm_cleanup_orphaned_processes
# Cleans up common orphaned development processes
# Call this after shutdown as a safety net
swarm_cleanup_orphaned_processes() {
    local patterns=("tsc --noEmit" "tsc -p" "vite" "esbuild" "webpack")
    local killed_count=0

    for pattern in "${patterns[@]}"; do
        local pids
        pids=$(pgrep -f "$pattern" 2>/dev/null || true)
        if [[ -n "$pids" ]]; then
            # Check if these are orphaned (no parent terminal)
            for pid in $pids; do
                local ppid
                ppid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ' || true)
                # If parent is init (1) or this shell, it's orphaned
                if [[ "$ppid" == "1" ]] || [[ -z "$ppid" ]]; then
                    kill -9 "$pid" 2>/dev/null && ((killed_count++)) || true
                fi
            done
        fi
    done

    if [[ "$killed_count" -gt 0 ]]; then
        echo "Cleaned up $killed_count orphaned development processes"
    fi
}

# swarm_shutdown <team_id> [--force]
# Gracefully shuts down a swarm team
swarm_shutdown() {
    local team_id="$1"
    local force="${2:-}"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local team_file="${team_dir}/team.json"

    if [[ ! -f "$team_file" ]]; then
        echo "Error: Team $team_id not found" >&2
        return 1
    fi

    if [[ "$force" != "--force" ]]; then
        # Send shutdown broadcast
        swarm_broadcast "$team_id" "system" "shutdown" '{"reason": "graceful shutdown requested"}'

        echo "Shutdown message broadcast. Waiting 10 seconds for agents to checkpoint..."
        sleep 10
    fi

    # Kill leader session and its child processes
    local leader_session
    leader_session=$(jq -r '.leader.session' "$team_file")
    if tmux has-session -t "$leader_session" 2>/dev/null; then
        echo "Killing child processes for leader: $leader_session"
        swarm_kill_session_processes "$leader_session"
        tmux kill-session -t "$leader_session" 2>/dev/null || true
        echo "Killed leader session: $leader_session"
    fi

    # Kill member sessions and their child processes
    while IFS= read -r session; do
        if tmux has-session -t "$session" 2>/dev/null; then
            echo "Killing child processes for agent: $session"
            swarm_kill_session_processes "$session"
            tmux kill-session -t "$session" 2>/dev/null || true
            echo "Killed agent session: $session"
        fi
    done < <(jq -r '.members[].session' "$team_file")

    # Final cleanup: kill any orphaned development processes
    echo "Cleaning up orphaned processes..."
    swarm_cleanup_orphaned_processes

    # Update team status
    swarm_update_team "$team_id" '{"status": "shutdown", "shutdown_at": "'"$(date -Iseconds)"'"}'

    echo "Swarm $team_id shut down (including child processes)"
}

# swarm_archive <team_id>
# Archives a team (moves to .archive/)
swarm_archive() {
    local team_id="$1"
    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local archive_dir="${SWARM_BASE_DIR}/.archive"

    if [[ ! -d "$team_dir" ]]; then
        echo "Error: Team $team_id not found" >&2
        return 1
    fi

    # Ensure shutdown first
    swarm_shutdown "$team_id" --force 2>/dev/null || true

    # Move to archive
    mkdir -p "$archive_dir"
    mv "$team_dir" "${archive_dir}/${team_id}-$(date +%Y%m%d%H%M%S)"

    echo "Team $team_id archived"
}

# ============================================================================
# v2 — Watchdog / Provider / Verify-cmd / Attach helpers
# ============================================================================

# swarm_detect_verify_cmd <worktree-path>
# Returns a verify command string based on what's present in the worktree.
swarm_detect_verify_cmd() {
    local worktree="${1:-.}"
    if [[ -f "${worktree}/Cargo.toml" ]]; then
        echo "cargo test --workspace --no-fail-fast"
    elif [[ -f "${worktree}/package.json" ]]; then
        if jq -e '.scripts.test' "${worktree}/package.json" >/dev/null 2>&1; then
            echo "npm test"
        else
            echo ":"
        fi
    elif [[ -f "${worktree}/pyproject.toml" || -f "${worktree}/pytest.ini" || -f "${worktree}/setup.cfg" ]]; then
        echo "pytest"
    elif [[ -f "${worktree}/go.mod" ]]; then
        echo "go test ./..."
    elif [[ -f "${worktree}/mix.exs" ]]; then
        echo "mix test"
    elif [[ -f "${worktree}/Makefile" ]] && grep -qE '^test:' "${worktree}/Makefile" 2>/dev/null; then
        echo "make test"
    elif [[ -f "${worktree}/Gemfile" ]] && grep -q rspec "${worktree}/Gemfile" 2>/dev/null; then
        echo "bundle exec rspec"
    else
        echo ":"  # no-op
    fi
}

# swarm_set_team_field <team-id> <jq-path> <value>
# Atomically merge a JSON field into team.json (string value).
swarm_set_team_field() {
    local team_id="$1"
    local jq_path="$2"
    local value="$3"
    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local team_json="${team_dir}/team.json"
    local tmp; tmp="$(mktemp)"
    jq --arg v "$value" "${jq_path} = \$v" "$team_json" > "$tmp" && mv "$tmp" "$team_json" || rm -f "$tmp"
}

# swarm_set_team_field_raw <team-id> <jq-path> <raw-json-value>
# Same as above but value is treated as raw JSON (numbers, booleans, objects).
swarm_set_team_field_raw() {
    local team_id="$1"
    local jq_path="$2"
    local raw="$3"
    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local team_json="${team_dir}/team.json"
    local tmp; tmp="$(mktemp)"
    jq --argjson v "$raw" "${jq_path} = \$v" "$team_json" > "$tmp" && mv "$tmp" "$team_json" || rm -f "$tmp"
}

# swarm_init_v2_team_json <team-id> <provider> <verify-cmd> <tick-min> <auto-pr> <use-loop> <worktree-path>
# Adds the v2 schema fields to an existing team.json (idempotent — safe to re-run).
swarm_init_v2_team_json() {
    local team_id="$1"
    local provider="${2:-claude}"
    local verify_cmd="${3:-}"
    local tick_min="${4:-5}"
    local auto_pr="${5:-false}"
    local use_loop="${6:-false}"
    local worktree="${7:-$(pwd)}"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local team_json="${team_dir}/team.json"
    local watchdog_session="${team_id}-watchdog"

    [[ -z "$verify_cmd" ]] && verify_cmd="$(swarm_detect_verify_cmd "$worktree")"

    local tmp; tmp="$(mktemp)"
    jq \
        --arg provider "$provider" \
        --arg verify "$verify_cmd" \
        --argjson tick "$tick_min" \
        --argjson autopr "$auto_pr" \
        --argjson useloop "$use_loop" \
        --arg session "$watchdog_session" \
        --arg pid_file "${team_dir}/watchdog.pid" \
        --arg log_file "${team_dir}/watchdog.log" \
        --arg report_path "${team_dir}/shared/finalize-report.md" \
        --arg worktree "$worktree" \
        '
        .provider = $provider
        | .commands = (.commands // {}) | .commands.verify = $verify
        | .ci = (.ci // {})
        | .ci.auto_pr = $autopr
        | .ci.pr_template_path = (.ci.pr_template_path // null)
        | .watchdog = (.watchdog // {})
        | .watchdog.enabled = true
        | .watchdog.tick_min = $tick
        | .watchdog.session = $session
        | .watchdog.pid_file = $pid_file
        | .watchdog.log_file = $log_file
        | .watchdog.use_loop = $useloop
        | .finalize = (.finalize // {})
        | .finalize.policy = "notify-only"
        | .finalize.report_path = $report_path
        | .worktree_path = $worktree
        ' "$team_json" > "$tmp" && mv "$tmp" "$team_json" || { rm -f "$tmp"; return 1; }
}

# swarm_spawn_watchdog <team-id>
# Spawn the watchdog daemon as a dedicated tmux session.
# Idempotent — refuses if session already exists.
swarm_spawn_watchdog() {
    local team_id="$1"
    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    local session_name="${team_id}-watchdog"
    local watchdog_sh="${HOME}/.claude/skills/swarm-create/watchdog.sh"

    if [[ ! -f "$watchdog_sh" ]]; then
        echo "Error: watchdog.sh not found at $watchdog_sh" >&2
        return 1
    fi
    if [[ ! -x "$watchdog_sh" ]]; then
        chmod +x "$watchdog_sh"
    fi
    if tmux has-session -t "$session_name" 2>/dev/null; then
        echo "Watchdog session already exists: $session_name" >&2
        return 1
    fi

    # Spawn dedicated tmux session running watchdog.sh
    tmux new-session -d -s "$session_name" -c "$PWD" \
        "bash '${watchdog_sh}' '${team_id}'; echo '[watchdog exited]'; exec bash"

    # Wait briefly for pid file
    local i=0
    while [[ ! -f "${team_dir}/watchdog.pid" && $i -lt 10 ]]; do
        sleep 1; i=$((i+1))
    done

    echo "$session_name"
}

# swarm_kill_watchdog <team-id>
# Stop the watchdog (kill tmux session named <team-id>-watchdog only).
# Does NOT touch leader/agent sessions.
swarm_kill_watchdog() {
    local team_id="$1"
    local session_name="${team_id}-watchdog"
    if tmux has-session -t "$session_name" 2>/dev/null; then
        tmux kill-session -t "$session_name"
        echo "Watchdog session killed: $session_name"
    else
        echo "No watchdog session for $team_id"
    fi
    rm -f "${SWARM_BASE_DIR}/${team_id}/watchdog.pid"
}

# swarm_attach_watchdog <team-id> <provider> [verify-cmd] [tick-min]
# Retrofit: take an existing v1 swarm, upgrade team.json to v2, spawn watchdog.
swarm_attach_watchdog() {
    local team_id="$1"
    local provider="${2:-claude}"
    local verify_cmd="${3:-}"
    local tick_min="${4:-5}"

    local team_dir="${SWARM_BASE_DIR}/${team_id}"
    if [[ ! -d "$team_dir" || ! -f "${team_dir}/team.json" ]]; then
        echo "Error: team $team_id not found" >&2
        return 1
    fi

    # Upgrade team.json in place
    swarm_init_v2_team_json "$team_id" "$provider" "$verify_cmd" "$tick_min" "false" "false" "$(pwd)"
    echo "team.json upgraded to v2 schema for $team_id"

    # Spawn watchdog
    swarm_spawn_watchdog "$team_id"
}

# ============================================================================
# CLI Interface
# ============================================================================

case "${1:-}" in
    create-team)
        # Usage: create-team <epic_id> [max_agents] [isolation_mode]
        swarm_create_team "$2" "${3:-4}" "${4:-shared}"
        ;;
    get-team)
        swarm_get_team "$2"
        ;;
    list-teams)
        swarm_list_teams
        ;;
    spawn-leader)
        swarm_spawn_leader "$2" "${3:-}"
        ;;
    spawn-agent)
        swarm_spawn_agent "$2" "$3" "${4:-}"
        ;;
    send)
        swarm_send_message "$2" "$3" "$4" "$5" "$6"
        ;;
    broadcast)
        swarm_broadcast "$2" "$3" "$4" "$5"
        ;;
    read-inbox)
        swarm_read_inbox "$2" "$3" "${@:4}"
        ;;
    clear-inbox)
        swarm_clear_inbox "$2" "$3"
        ;;
    status)
        swarm_get_status "$2"
        ;;
    update-member)
        swarm_update_member_status "$2" "$3" "$4" "${5:-null}"
        ;;
    shutdown)
        swarm_shutdown "$2" "${3:-}"
        ;;
    cleanup-orphans)
        swarm_cleanup_orphaned_processes
        ;;
    archive)
        swarm_archive "$2"
        ;;
    merge-worktrees)
        swarm_merge_worktrees "$2" "${3:-}"
        ;;
    list-worktrees)
        swarm_list_worktrees "$2"
        ;;
    agent-commit)
        swarm_agent_commit "$2" "$3" "$4"
        ;;
    detect-verify-cmd)
        # Usage: detect-verify-cmd [worktree-path]
        swarm_detect_verify_cmd "${2:-$(pwd)}"
        ;;
    init-v2-team-json)
        # Usage: init-v2-team-json <team-id> <provider> [verify-cmd] [tick-min] [auto-pr] [use-loop] [worktree-path]
        swarm_init_v2_team_json "$2" "$3" "${4:-}" "${5:-5}" "${6:-false}" "${7:-false}" "${8:-$(pwd)}"
        ;;
    spawn-watchdog)
        # Usage: spawn-watchdog <team-id>
        swarm_spawn_watchdog "$2"
        ;;
    kill-watchdog)
        # Usage: kill-watchdog <team-id>
        swarm_kill_watchdog "$2"
        ;;
    attach-watchdog)
        # Usage: attach-watchdog <team-id> <provider> [verify-cmd] [tick-min]
        swarm_attach_watchdog "$2" "$3" "${4:-}" "${5:-5}"
        ;;
    *)
        cat <<EOF
Swarm Orchestration Library

Usage: swarm-lib.sh <command> [args...]

Team Management:
  create-team <epic_id> [max_agents] [isolation_mode]
                                         Create a new swarm team
                                         isolation_mode: "shared" (default) or "worktree"
  get-team <team_id>                     Get team configuration
  list-teams                             List all active teams

Agent Spawning:
  spawn-leader <team_id> [prompt]        Spawn team leader in tmux
  spawn-agent <team_id> <name> [prompt]  Spawn worker agent in tmux
                                         (auto-creates worktree if team uses worktree mode)

Messaging:
  send <team_id> <from> <to> <type> <payload_json>    Send message to agent
  broadcast <team_id> <from> <type> <payload_json>    Broadcast to all agents
  read-inbox <team_id> <agent> [--last N]             Read agent's inbox
  clear-inbox <team_id> <agent>                       Clear agent's inbox

Git Worktree (for worktree isolation mode):
  list-worktrees <team_id>               List all agent worktrees
  merge-worktrees <team_id> [--no-delete]  Merge all worktrees to base branch
  agent-commit <team_id> <agent> <msg>   Commit changes in agent's worktree

Status & Control:
  status <team_id>                       Get comprehensive team status
  update-member <team_id> <name> <status> [task_id]   Update member status
  shutdown <team_id> [--force]           Gracefully shutdown team (kills child processes)
  cleanup-orphans                        Kill orphaned dev processes (tsc, vite, etc.)
  archive <team_id>                      Archive team to .archive/

v2 Watchdog (self-sufficient swarm):
  detect-verify-cmd [path]               Auto-detect verify command from worktree files
  init-v2-team-json <team_id> <provider> [verify-cmd] [tick-min] [auto-pr] [use-loop] [worktree-path]
                                         Initialize v2 schema fields on team.json
  spawn-watchdog <team_id>               Spawn the watchdog daemon as a dedicated tmux session
  kill-watchdog <team_id>                Stop watchdog (only kills <team>-watchdog session)
  attach-watchdog <team_id> <provider> [verify-cmd] [tick-min]
                                         Retrofit existing v1 swarm: upgrade team.json + spawn watchdog

Isolation Modes:
  shared    - All agents work in same directory (default)
              Faster, no merge step, but risk of file conflicts
  worktree  - Each agent gets own git worktree
              Isolated, agents commit independently, merge at end

Examples:
  # Shared mode (default) - agents work on independent files
  swarm-lib.sh create-team bd-epic-123 3 shared
  swarm-lib.sh spawn-agent swarm-1234567890 agent-1

  # Worktree mode - full git isolation per agent
  swarm-lib.sh create-team bd-epic-123 3 worktree
  swarm-lib.sh spawn-agent swarm-1234567890 agent-1
  swarm-lib.sh merge-worktrees swarm-1234567890

  # Messaging
  swarm-lib.sh send swarm-1234567890 leader agent-1 task '{"task_id": "bd-001"}'
  swarm-lib.sh status swarm-1234567890
  swarm-lib.sh shutdown swarm-1234567890
EOF
        exit 1
        ;;
esac
