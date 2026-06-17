#!/usr/bin/env bash
# SG2 installer: wire reflect's git post-commit capture into a repo.
#
# `post_commit.sh` is the SG2 capture hook, but a git hook lives in a repo's
# .git/hooks and is NOT installed by `claude plugin install` (that only wires
# the Claude Code session hooks in plugin.json). This script installs it
# per-repo, idempotently, and chains any existing post-commit hook so we never
# clobber a developer's own hook.
#
# Usage:
#   install_post_commit.sh [REPO_DIR]      # install into REPO_DIR (default: cwd repo)
#   install_post_commit.sh --uninstall [REPO_DIR]
#
# The installed .git/hooks/post-commit is a thin wrapper that:
#   1. runs the prior hook's content if we chained one (saved as
#      post-commit.reflect-chained), then
#   2. execs this plugin's hooks/post_commit.sh with REFLECT_SCRIPTS_DIR pinned
#      to an absolute path, so capture works regardless of how the hook was
#      reached. Silent-fail: a capture error never fails the developer's commit.
set -euo pipefail

_self="${BASH_SOURCE[0]}"
HOOK_DIR="$(cd "$(dirname "$_self")" && pwd)"
PLUGIN_ROOT="${HOOK_DIR%/hooks}"
SCRIPTS_DIR="${PLUGIN_ROOT}/scripts"
REAL_HOOK="${HOOK_DIR}/post_commit.sh"

MARKER="# >>> reflect SG2 post-commit (managed) >>>"
END_MARKER="# <<< reflect SG2 post-commit (managed) <<<"

UNINSTALL=0
REPO=""
for arg in "$@"; do
  case "$arg" in
    --uninstall) UNINSTALL=1 ;;
    -h|--help) sed -n '2,17p' "$_self"; exit 0 ;;
    *) REPO="$arg" ;;
  esac
done

# Resolve the target repo's hooks dir (respect core.hooksPath).
cd "${REPO:-.}"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || { echo "error: not a git repo: ${REPO:-$PWD}" >&2; exit 1; }
GIT_DIR="$(git rev-parse --git-dir)"
HOOKS_PATH="$(git config --get core.hooksPath || true)"
HOOKS_DIR="${HOOKS_PATH:-$GIT_DIR/hooks}"
mkdir -p "$HOOKS_DIR"
TARGET="$HOOKS_DIR/post-commit"
CHAINED="$HOOKS_DIR/post-commit.reflect-chained"

if [ "$UNINSTALL" = "1" ]; then
  if [ -f "$TARGET" ] && grep -qF "$MARKER" "$TARGET" 2>/dev/null; then
    if [ -f "$CHAINED" ]; then
      mv "$CHAINED" "$TARGET"; echo "reflect SG2: uninstalled, restored prior post-commit hook"
    else
      rm -f "$TARGET"; echo "reflect SG2: uninstalled post-commit hook"
    fi
  else
    echo "reflect SG2: nothing to uninstall (no managed hook in $HOOKS_DIR)"
  fi
  exit 0
fi

[ -f "$REAL_HOOK" ] || { echo "error: reflect post_commit.sh not found at $REAL_HOOK" >&2; exit 1; }

# Idempotent: if our managed wrapper is already there, done.
if [ -f "$TARGET" ] && grep -qF "$MARKER" "$TARGET" 2>/dev/null; then
  echo "reflect SG2: already installed in $HOOKS_DIR"
  exit 0
fi

# Chain a pre-existing (non-managed) hook so we don't clobber it.
CHAIN_CALL=""
if [ -e "$TARGET" ]; then
  mv "$TARGET" "$CHAINED"
  chmod +x "$CHAINED" 2>/dev/null || true
  CHAIN_CALL='[ -x "$(dirname "$0")/post-commit.reflect-chained" ] && "$(dirname "$0")/post-commit.reflect-chained" "$@"'
  echo "reflect SG2: chained existing post-commit hook -> post-commit.reflect-chained"
fi

cat > "$TARGET" <<EOF
#!/usr/bin/env bash
$MARKER
# Managed by reflect install_post_commit.sh — edits will be overwritten on
# re-install. To remove: $HOOK_DIR/install_post_commit.sh --uninstall
$CHAIN_CALL
REFLECT_SCRIPTS_DIR="$SCRIPTS_DIR" "$REAL_HOOK" "\$@" || true
$END_MARKER
EOF
chmod +x "$TARGET"
echo "reflect SG2: installed post-commit capture into $HOOKS_DIR"
echo "  commits in this repo now append to \$REFLECT_STATE_DIR/commits.jsonl and link to the active session."
