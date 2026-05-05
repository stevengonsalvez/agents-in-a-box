#!/usr/bin/env bash
# Update all external skills/plugins tracked in toolkit/external-dependencies.yaml.
# Pulls latest from each upstream source so ~/.claude/skills/ matches manifest.
#
# Categories handled:
#   - npx-skills        (vercel-labs/agent-browser, pbakaus/impeccable, etc.)
#   - claude-plugins    (beads, debug-bridge, caveman, codex, etc.)
#   - agent-skills      (git-cloned: fireworks-tech-graph, ui-ux-pro-max, etc.)
#   - nanoclaw-skills   (auto-synced from stevengonsalvez/nanoclaw fork via native-runner)
#   - external-packages (uv tool installs: reflect-kb)
#
# Usage:
#   ./update-externals.sh           # update everything
#   ./update-externals.sh npx       # only npx-skills
#   ./update-externals.sh plugins   # only claude-plugins
#   ./update-externals.sh agent     # only agent-skills (git clones)
#   ./update-externals.sh nanoclaw  # pull nanoclaw fork
#   ./update-externals.sh packages  # external-packages (uv tool)
#   ./update-externals.sh --dry-run # show commands without executing

set -euo pipefail

SCOPE="${1:-all}"
DRY_RUN=0
[ "${1:-}" = "--dry-run" ] && { DRY_RUN=1; SCOPE="all"; }
[ "${2:-}" = "--dry-run" ] && DRY_RUN=1

run() {
  if [ "$DRY_RUN" = "1" ]; then
    echo "  DRY: $*"
  else
    echo "  $ $*"
    eval "$@" || echo "    (failed — continuing)"
  fi
}

section() { echo; echo "=== $* ==="; }

# ----- npx-skills (run install commands from manifest) -----
update_npx() {
  section "npx-skills (re-installs latest)"
  run "npx skills add vercel-labs/agent-browser --yes"
  run "npx skills add vercel-labs/agent-skills --yes"
  run "npx skills add pbakaus/impeccable --yes"
  run "npx add-skill here-now"
  run "npx add-skill find-skills"
  run "npx add-skill summarize"
  run "npx add-skill browserbase-skills"
  run "npx add-skill gws-skills"
}

# ----- claude-plugins -----
update_plugins() {
  section "claude-plugins (marketplace add + plugin install)"
  # beads, debug-bridge, ralph-loop, code-review, playground, dev-browser,
  # skill-creator, open-prose, codex, caveman
  run "claude plugin marketplace add stevengonsalvez/beads-marketplace"
  run "claude plugin install beads@beads-marketplace"
  run "claude plugin marketplace add stevengonsalvez/agent-bridge-marketplace"
  run "claude plugin install debug-bridge@agent-bridge-marketplace"
  run "claude plugin marketplace add stevengonsalvez/claude-plugins-official"
  run "claude plugin install ralph-loop@claude-plugins-official"
  run "claude plugin install code-review@claude-plugins-official"
  run "claude plugin install skill-creator@claude-plugins-official"
  run "claude plugin install discord@claude-plugins-official"
  run "claude plugin marketplace add stevengonsalvez/dev-browser-marketplace"
  run "claude plugin install dev-browser@dev-browser-marketplace"
  run "claude plugin marketplace add stevengonsalvez/prose"
  run "claude plugin install open-prose@prose"
  run "claude plugin marketplace add openai/codex-plugin-cc"
  run "claude plugin install codex"
  run "claude plugin marketplace add JuliusBrussee/caveman"
  run "claude plugin install caveman@caveman"
}

# ----- agent-skills (git clone targets) -----
update_agent_skills() {
  section "agent-skills (git clone refresh)"
  local AS_DIR="$HOME/.claude/skills"
  declare -A repos=(
    ["ui-ux-pro-max"]="https://github.com/nextlevelbuilder/ui-ux-pro-max-skill"
    ["notebooklm"]="https://github.com/PleasePrompto/notebooklm-skill"
    ["fireworks-tech-graph"]="https://github.com/yizhiyanhua-ai/fireworks-tech-graph"
  )
  for name in "${!repos[@]}"; do
    local url="${repos[$name]}"
    local dir="$AS_DIR/$name"
    if [ -d "$dir/.git" ]; then
      run "git -C '$dir' pull --ff-only"
    else
      run "git clone --depth 1 '$url' '$dir'"
    fi
  done
  # scrapling-official has its own install path
  run "scrapling install --force"
}

# ----- nanoclaw-skills (fork pull, then run any agent in native mode) -----
update_nanoclaw() {
  section "nanoclaw-skills (fork pull)"
  local FORK="$HOME/d/git/nanoclaw"
  if [ ! -d "$FORK/.git" ]; then
    run "gh repo clone stevengonsalvez/nanoclaw '$FORK'"
  else
    run "git -C '$FORK' fetch origin && git -C '$FORK' pull --ff-only origin main"
  fi
  echo "  Note: skills auto-sync to ~/.claude/skills/ on next agent spawn (native-runner cpSync)"
  # mcporter ships in nanoclaw skills but the underlying CLI is npm — pull latest CLI here too
  run "npm install -g mcporter@latest"
}

# ----- reflect plugin (deploy from packaged plugin source) -----
update_reflect() {
  section "reflect plugin (claude_adapter install --force)"
  local TOOLKIT_ROOT
  TOOLKIT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
  local ADAPTER="$TOOLKIT_ROOT/packages/plugins/reflect/adapters/claude/claude_adapter.py"
  if [ ! -f "$ADAPTER" ]; then
    echo "  (skip: $ADAPTER not found)"
    return
  fi
  run "python3 '$ADAPTER' install --force"
  echo "  Sub-skills deployed: reflect, reflect:consolidate, reflect:ingest, recall, reflect-status"
}

# ----- external-packages (uv tool) -----
update_packages() {
  section "external-packages (uv tool)"
  run "uv tool install --force --upgrade 'git+https://github.com/stevengonsalvez/reflect-kb.git[graph]'"
  # graphify (PyPI: graphifyy, double-y) — knowledge-graph builder used by /graphify skill
  run "uv tool install --force --upgrade graphifyy"
}

case "$SCOPE" in
  all)       update_npx; update_plugins; update_agent_skills; update_nanoclaw; update_reflect; update_packages ;;
  npx)       update_npx ;;
  plugins)   update_plugins ;;
  agent)     update_agent_skills ;;
  nanoclaw)  update_nanoclaw ;;
  reflect)   update_reflect ;;
  packages)  update_packages ;;
  *)         echo "Usage: $0 [all|npx|plugins|agent|nanoclaw|reflect|packages] [--dry-run]"; exit 1 ;;
esac

section "Done"
