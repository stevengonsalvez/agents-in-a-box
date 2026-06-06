#!/usr/bin/env bash
# swarm-create v2 finalize: notify-only, never kill tmux, never auto-merge
#
# Usage:
#   bash finalize.sh <team-id>
#
# Steps:
#   1. Run team.json.commands.verify (capture exit + last 200 lines)
#   2. If worktree isolation: dry-run merge each agent branch (no commit, abort after)
#   3. Write shared/finalize-report.md (human-readable) + shared/finalize-report.json (machine)
#   4. If team.json.ci.auto_pr == true AND verify passed: gh pr create --draft
#   5. NOTIFY leader.jsonl: { type: "finalize_done", report_path: ..., verify: pass|fail }
#
# Never:
#   - tmux kill-session
#   - actually merge (only dry-run)
#   - mark .finalized (that's the watchdog's job)

set -u

TEAM_ID="${1:?Usage: finalize.sh <team-id>}"
# Match swarm-lib.sh default: $PWD/.claude/swarm
SWARM_BASE_DIR="${SWARM_BASE_DIR:-${PWD}/.claude/swarm}"
TEAM_DIR="${SWARM_BASE_DIR}/${TEAM_ID}"
TEAM_JSON="${TEAM_DIR}/team.json"
SHARED_DIR="${TEAM_DIR}/shared"
REPORT_MD="${SHARED_DIR}/finalize-report.md"
REPORT_JSON="${SHARED_DIR}/finalize-report.json"
LEADER_INBOX="${TEAM_DIR}/inbox/leader.jsonl"

if [[ ! -f "$TEAM_JSON" ]]; then
  echo "FATAL: team.json not found at $TEAM_JSON" >&2
  exit 1
fi

mkdir -p "$SHARED_DIR"

# ---------- read team config ----------
EPIC_ID="$(jq -r '.epic_id // ""' "$TEAM_JSON")"
ISOLATION="$(jq -r '.isolation // "shared"' "$TEAM_JSON")"
VERIFY_CMD="$(jq -r '.commands.verify // ":"' "$TEAM_JSON")"
AUTO_PR="$(jq -r '.ci.auto_pr // false' "$TEAM_JSON")"
PR_TEMPLATE="$(jq -r '.ci.pr_template_path // ""' "$TEAM_JSON")"
WORKTREE_BASE="$(jq -r '.config.worktree_base_branch // "main"' "$TEAM_JSON")"

# Find the actual worktree path. Try team.json first, fall back to cwd.
WORKTREE="$(jq -r '.worktree_path // ""' "$TEAM_JSON")"
[[ -z "$WORKTREE" ]] && WORKTREE="$(pwd)"

TS_START="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "[finalize] starting at $TS_START"
echo "[finalize] team=$TEAM_ID epic=$EPIC_ID isolation=$ISOLATION"
echo "[finalize] worktree=$WORKTREE"
echo "[finalize] verify_cmd=$VERIFY_CMD"

# ---------- step 1: run verify ----------
VERIFY_LOG="${SHARED_DIR}/verify.log"
VERIFY_EXIT=0
VERIFY_DURATION_S=0
VERIFY_STATUS="not_run"
if [[ "$VERIFY_CMD" != ":" && -n "$VERIFY_CMD" ]]; then
  echo "[finalize] running verify: $VERIFY_CMD"
  T0="$(date +%s)"
  ( cd "$WORKTREE" && eval "$VERIFY_CMD" ) > "$VERIFY_LOG" 2>&1
  VERIFY_EXIT=$?
  T1="$(date +%s)"
  VERIFY_DURATION_S=$((T1 - T0))
  if [[ $VERIFY_EXIT -eq 0 ]]; then
    VERIFY_STATUS="pass"
    echo "[finalize] verify PASS (${VERIFY_DURATION_S}s)"
  else
    VERIFY_STATUS="fail"
    echo "[finalize] verify FAIL exit=$VERIFY_EXIT (${VERIFY_DURATION_S}s)"
  fi
else
  echo "[finalize] no verify_cmd configured — skipping"
fi
VERIFY_TAIL="$(tail -200 "$VERIFY_LOG" 2>/dev/null || echo '(no verify log)')"

# ---------- step 2: dry-run worktree merges (worktree mode only) ----------
WORKTREE_REPORT=""
if [[ "$ISOLATION" == "worktree" ]]; then
  echo "[finalize] dry-run merging agent worktrees against $WORKTREE_BASE"
  WORKTREE_REPORT="### Worktree dry-run merge results\n\n"

  # List agent worktrees from team.json or git worktree list
  AGENT_BRANCHES=$(jq -r '.members[]?.branch // empty' "$TEAM_JSON" 2>/dev/null)
  if [[ -z "$AGENT_BRANCHES" ]]; then
    # Fallback — look for branches matching team-id-agent-*
    AGENT_BRANCHES=$(git -C "$WORKTREE" branch -a 2>/dev/null | \
      grep -oE "${TEAM_ID}-agent-[0-9]+" | sort -u)
  fi

  for branch in $AGENT_BRANCHES; do
    echo "[finalize] dry-run merge: $branch"
    # Use a sub-shell so we don't pollute working tree
    (
      cd "$WORKTREE"
      git fetch --all --quiet 2>/dev/null || true
      # Reset any pending dry-run state from prior iteration
      git merge --abort 2>/dev/null || true
      if git merge --no-commit --no-ff "$branch" >/dev/null 2>&1; then
        echo "CLEAN:$branch"
        git merge --abort 2>/dev/null
      else
        CONFLICTS=$(git diff --name-only --diff-filter=U 2>/dev/null | wc -l | tr -d ' ')
        echo "CONFLICTS=$CONFLICTS:$branch"
        git merge --abort 2>/dev/null
      fi
    ) | while read -r line; do
      case "$line" in
        CLEAN:*)        WORKTREE_REPORT+="- ✅ \`${line#CLEAN:}\` — clean dry-run merge\n" ;;
        CONFLICTS=*:*)  WORKTREE_REPORT+="- ⚠️  \`${line#*:}\` — ${line%%:*} files in conflict\n" ;;
      esac
    done
  done
fi

# ---------- step 3: gather commit summary per agent ----------
COMMIT_SUMMARY=""
BASE_COMMIT=$(git -C "$WORKTREE" merge-base HEAD "origin/${WORKTREE_BASE}" 2>/dev/null || echo "")
if [[ -n "$BASE_COMMIT" ]]; then
  TOTAL_COMMITS=$(git -C "$WORKTREE" rev-list --count "${BASE_COMMIT}..HEAD" 2>/dev/null || echo "?")
  COMMIT_SUMMARY="${TOTAL_COMMITS} commits ahead of \`${WORKTREE_BASE}\`"
else
  TOTAL_COMMITS=$(git -C "$WORKTREE" rev-list --count HEAD 2>/dev/null || echo "?")
  COMMIT_SUMMARY="${TOTAL_COMMITS} total commits on branch"
fi

# ---------- step 4: write reports ----------
TS_END="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

cat > "$REPORT_MD" <<EOF
# Swarm Finalize Report — ${TEAM_ID}

**Epic**: \`${EPIC_ID}\`
**Started**: ${TS_START}
**Finished**: ${TS_END}
**Isolation**: \`${ISOLATION}\`
**Verify**: **${VERIFY_STATUS}** (exit ${VERIFY_EXIT}, ${VERIFY_DURATION_S}s)

## Status: $([ "$VERIFY_STATUS" = "pass" ] && echo "🟢 READY FOR HUMAN REVIEW" || echo "🔴 VERIFY FAILED — DO NOT MERGE")

---

## Verify command

\`\`\`
${VERIFY_CMD}
\`\`\`

### Verify output (last 200 lines)

\`\`\`
${VERIFY_TAIL}
\`\`\`

---

## Branch state

${COMMIT_SUMMARY}

### Recent commits

\`\`\`
$(git -C "$WORKTREE" log --oneline -20 2>/dev/null || echo "(no git log)")
\`\`\`

EOF

if [[ -n "$WORKTREE_REPORT" ]]; then
  printf '%s\n' "$WORKTREE_REPORT" >> "$REPORT_MD"
fi

cat >> "$REPORT_MD" <<EOF

---

## Beads epic state

\`\`\`
$(BEADS_DIR="$(jq -r '.beads_dir // ""' "$TEAM_JSON")" bd list --json 2>/dev/null | \
  jq -r --arg epic "$EPIC_ID" \
    '.[] | select((.parent_epic // "") == $epic or (.depends_on // [] | contains([$epic]))) | "\(.status[:8]) | \(.id) | \(.title)"' || \
  echo "(bd unavailable)")
\`\`\`

---

## Next steps for human

\`\`\`bash
# Inspect agent commits
git log --oneline ${BASE_COMMIT}..HEAD

# If worktree isolation, run actual merge when ready:
bash {{HOME_TOOL_DIR}}/utils/swarm-lib.sh merge-worktrees ${TEAM_ID}

# Push branch + open PR
git push origin HEAD
gh pr create --title "<title>" --body-file ${REPORT_MD}

# Shutdown swarm (tmux kill, archive)
/swarm-shutdown ${TEAM_ID}
\`\`\`

---

*Generated by swarm-create v2 finalize.sh — notify-only, no auto-kill, no auto-merge.*
EOF

# JSON variant for tooling
jq -n \
  --arg team "$TEAM_ID" \
  --arg epic "$EPIC_ID" \
  --arg verify "$VERIFY_STATUS" \
  --argjson exit "$VERIFY_EXIT" \
  --argjson duration "$VERIFY_DURATION_S" \
  --arg isolation "$ISOLATION" \
  --arg commits "$TOTAL_COMMITS" \
  --arg ts_start "$TS_START" \
  --arg ts_end "$TS_END" \
  --arg report_path "$REPORT_MD" \
  '{
    team_id: $team,
    epic_id: $epic,
    verify_status: $verify,
    verify_exit_code: $exit,
    verify_duration_seconds: $duration,
    isolation: $isolation,
    total_commits: $commits,
    started_at: $ts_start,
    finished_at: $ts_end,
    report_md_path: $report_path
  }' > "$REPORT_JSON"

echo "[finalize] reports written:"
echo "  - $REPORT_MD"
echo "  - $REPORT_JSON"

# ---------- step 5: auto-PR (if enabled + verify green) ----------
PR_URL=""
if [[ "$AUTO_PR" == "true" && "$VERIFY_STATUS" == "pass" ]]; then
  if command -v gh >/dev/null 2>&1; then
    echo "[finalize] auto-pr enabled and verify passed — opening draft PR"
    BRANCH_NAME=$(git -C "$WORKTREE" branch --show-current 2>/dev/null || echo "")
    if [[ -n "$BRANCH_NAME" ]]; then
      # Push first (in case branch not yet on origin)
      git -C "$WORKTREE" push -u origin "$BRANCH_NAME" 2>&1 | head -5 || true

      PR_TITLE_SOURCE="$PR_TEMPLATE"
      [[ -z "$PR_TITLE_SOURCE" || ! -f "$PR_TITLE_SOURCE" ]] && PR_TITLE_SOURCE="$REPORT_MD"

      PR_URL=$( ( cd "$WORKTREE" && gh pr create --draft \
        --title "swarm ${TEAM_ID}: epic ${EPIC_ID}" \
        --body-file "$PR_TITLE_SOURCE" 2>&1 ) | grep -oE 'https://github.com[^ ]+' | head -1 || echo "")
      if [[ -n "$PR_URL" ]]; then
        echo "[finalize] PR opened: $PR_URL"
      else
        echo "[finalize] PR creation may have failed — check gh output"
      fi
    else
      echo "[finalize] no current branch — skipping PR"
    fi
  else
    echo "[finalize] gh CLI not available — skipping PR (set auto_pr=false to silence)"
  fi
elif [[ "$AUTO_PR" == "true" ]]; then
  echo "[finalize] auto-pr enabled but verify failed — NOT opening PR"
fi

# ---------- step 6: notify leader inbox ----------
PAYLOAD="{\"ts\":\"${TS_END}\",\"type\":\"finalize_done\",\"from\":\"watchdog\",\"verify\":\"${VERIFY_STATUS}\",\"verify_exit\":${VERIFY_EXIT},\"report_md\":\"${REPORT_MD}\",\"report_json\":\"${REPORT_JSON}\""
[[ -n "$PR_URL" ]] && PAYLOAD+=",\"pr_url\":\"${PR_URL}\""
PAYLOAD+="}"
mkdir -p "$(dirname "$LEADER_INBOX")"
echo "$PAYLOAD" >> "$LEADER_INBOX"

echo "[finalize] notified leader.jsonl"
echo "[finalize] DONE — never killed tmux, never auto-merged."

exit 0
