#!/usr/bin/env bash
# SG2: git post-commit capture (agentmemory post-commit.ts shape).
#
# Installed by `/reflect setup` into the repo's .git/hooks/post-commit (or
# chained from an existing one). On every commit it captures the new SHA,
# branch, subject, and changed files, links the SHA to the active reflect
# session, and appends a line to $REFLECT_STATE_DIR/commits.jsonl. A
# merge-conflict resolution is flagged as a high-confidence learning trigger;
# a `git revert` demotes the reverted commit's session learnings.
#
# Silent-fail by contract: a post-commit hook must NEVER fail a developer's
# commit. Every path exits 0; all real work is delegated to reflect_db.py's
# `record-commit` subcommand, which is itself best-effort.
#
# Session linkage: the active session id is read from $CLAUDE_SESSION_ID (set
# by the agent harness) and falls back to the REFLECT_SESSION_ID override.
set -u

# Resolve the plugin scripts dir relative to this hook file when possible, so a
# chained copy in .git/hooks still finds reflect_db.py. REFLECT_SCRIPTS_DIR
# overrides for non-standard installs / tests.
_self="${BASH_SOURCE[0]}"
_hook_dir="$(cd "$(dirname "$_self")" 2>/dev/null && pwd)"
SCRIPTS_DIR="${REFLECT_SCRIPTS_DIR:-${_hook_dir%/hooks}/scripts}"
REFLECT_DB="${SCRIPTS_DIR}/reflect_db.py"

[ -f "$REFLECT_DB" ] || exit 0

# --- capture commit facts -------------------------------------------------
SHA="$(git rev-parse HEAD 2>/dev/null)" || exit 0
[ -n "$SHA" ] || exit 0
BRANCH="$(git rev-parse --abbrev-ref HEAD 2>/dev/null)"
MESSAGE="$(git log -1 --format=%s "$SHA" 2>/dev/null)"
BODY="$(git log -1 --format=%B "$SHA" 2>/dev/null)"
FILES="$(git diff-tree --no-commit-id --name-only -r "$SHA" 2>/dev/null)"
SESSION_ID="${CLAUDE_SESSION_ID:-${REFLECT_SESSION_ID:-}}"

# --- merge-conflict resolution detection ----------------------------------
# A merge commit (2+ parents) whose body mentions conflict resolution is the
# high-confidence learning trigger from the source. Detect 2+ parents and the
# git-generated "Conflicts:" stanza a `git commit` after `git merge` carries.
CONFLICT_FLAG=""
PARENT_COUNT="$(git rev-list --parents -n 1 "$SHA" 2>/dev/null | wc -w)"
if [ "${PARENT_COUNT:-0}" -ge 3 ]; then
  case "$BODY" in
    *Conflicts:*|*"conflict"*) CONFLICT_FLAG="--conflict-resolved" ;;
  esac
fi

# --- delegate to the python capture path ----------------------------------
# record-commit handles commit_links insertion, commits.jsonl append, and
# revert-driven learning demotion (it parses "This reverts commit <sha>." out
# of the message body itself). Pass the full body as --message so the revert
# parser sees the reverts-commit line.
PY_BIN="${REFLECT_PYTHON:-python3}"
"$PY_BIN" "$REFLECT_DB" record-commit \
  --sha "$SHA" \
  --session "$SESSION_ID" \
  --branch "$BRANCH" \
  --message "$BODY" \
  --files "$FILES" \
  $CONFLICT_FLAG >/dev/null 2>&1

exit 0
