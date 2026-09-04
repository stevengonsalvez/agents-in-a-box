#!/usr/bin/env bash
# The five clauses of move 1 exit criterion (B), each read from ground truth
# rather than from the screen. Printed in the recording's own terminal (still
# `p3-acp-8-ground-truth.png`) and run again OUTSIDE vhs, where its exit code
# is what decides whether the recording is kept.
#
# The two ANSWER lines are an assertion, not a display. The tape presses `3`
# then `2`, which the daemon reads as 1-based indexes into the adapter's option
# list, so the only proof that the operator's glyph reached the option the
# operator saw is that the store recorded that option's id. vhs cannot assert
# the circled glyph itself (its `Wait+Screen` matcher never matches ①②③,
# measured against a control tape that matches the same string in ASCII), so
# the positional check lives here and exits non-zero when it fails.
#
# Usage: p3-acp-proof.sh <task-id> | p3-acp-proof.sh --latest
#
# The task id is REQUIRED. `--latest` (what the tape types, because the tape is
# written before the task exists) takes the newest ACP task in the store, which
# is right during a run and stale any time after it.
set -u
DB="$HOME/.agents-in-a-box/hangar.db"

case "${1:-}" in
  --latest) TASK=$(sqlite3 "$DB" "select id from agent_task_queue where session_id like 'acp:%' order by created_at desc limit 1;") ;;
  "" | -*) echo "usage: $(basename "$0") <task-id> | --latest" >&2; exit 2 ;;
  *) TASK="$1" ;;
esac
[ -n "$TASK" ] || { echo "no acp task found" >&2; exit 2; }

SESSION=$(sqlite3 "$DB" "select session_id from agent_task_queue where id='$TASK';")
ROOT="$HOME/.agents-in-a-box/hangar/workspaces/default/$TASK"

echo "PROOF task=$TASK"
echo "PROOF session=$SESSION"
printf 'PROOF tmux_hangar sessions: '
tmux ls 2>/dev/null | grep -c tmux_hangar
printf 'PROOF provider jsonl in the run logs: '
find "$ROOT/logs" -name '*.jsonl' 2>/dev/null | wc -l
echo "PROOF store rows:"
sqlite3 "$DB" "select '  '||state||'|'||answered_by||'|'||answer from attention where session_id='$SESSION' order by created_at;"
echo "PROOF agent wrote: $(cat "$HOME/.agents-in-a-box/worktrees/$TASK/api/DBPATH.txt" 2>/dev/null)"
echo "PROOF final message: $(sqlite3 "$DB" "select result from agent_task_queue where id='$TASK';")"

answers=$(sqlite3 "$DB" "select group_concat(answer,',') from (select answer from attention where session_id='$SESSION' order by created_at);")
if [ "$answers" = "reject,allow" ]; then
  echo "PROOF glyph->option: 3 delivered reject, 2 delivered allow"
else
  echo "PROOF FAILED glyph->option: expected reject,allow and got '$answers'" >&2
  exit 1
fi
