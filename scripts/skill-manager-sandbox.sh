#!/usr/bin/env bash
# skill-manager-sandbox.sh — build / tear down a self-contained sandbox
# for manually testing the SkillManager TUI + `ainb skill ...` CLI
# without touching the real ~/.claude / ~/.codex.
#
# Mirrors the Rust fixture at crates/ainb-skill-core/src/fixtures.rs
# (same dir layout, same env vars). Independent — pure POSIX bash, no
# cargo dependency. The Rust fixture is for in-process tests; this
# script is for manual TUI/CLI testing against a stable path.
#
# See docs/skill-manager/sandbox-testing.md for the full walkthrough.
# Bead ai-e7t.

set -euo pipefail

# ── defaults ─────────────────────────────────────────────────────────
DEFAULT_ROOT="${XDG_CACHE_HOME:-$HOME/.cache}/ainb-sandbox"
DEFAULT_TIER="minimal"
SENTINEL_FILE=".ainb-sandbox-marker"

# ── usage ────────────────────────────────────────────────────────────
usage() {
  cat <<EOF
usage: $(basename "$0") <up|down> [options]

Subcommands:
  up    Build the sandbox + write env.sh
  down  Remove the sandbox (refuses without the .ainb-sandbox-marker
        sentinel)

Options for `up`:
  --root <dir>        Sandbox location (default: $DEFAULT_ROOT)
  --tier minimal|full Seeding tier (default: $DEFAULT_TIER)

Options for `down`:
  --root <dir>        Sandbox location to remove (default: $DEFAULT_ROOT)

Refuses --root / and --root \$HOME.

Examples:
  $(basename "$0") up
  $(basename "$0") up --tier full --root /tmp/sb
  source $DEFAULT_ROOT/env.sh && ./target/debug/ainb
  $(basename "$0") down
EOF
}

# ── arg parse ────────────────────────────────────────────────────────
if [ "$#" -eq 0 ]; then
  usage >&2
  exit 2
fi

CMD="$1"; shift
ROOT="$DEFAULT_ROOT"
TIER="$DEFAULT_TIER"

while [ "$#" -gt 0 ]; do
  case "$1" in
    --root) ROOT="$2"; shift 2 ;;
    --tier) TIER="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "unknown flag: $1" >&2; usage >&2; exit 2 ;;
  esac
done

# ── safety belts (BEFORE path normalisation, so `--root /` is caught
#    literally instead of expanding to `///`) ─────────────────────────
refuse_dangerous_root() {
  # Compare both the raw input and the normalised form against the
  # forbidden values so neither `--root /` nor `--root //` slips by.
  local normalised="${ROOT%/}"
  [ -z "$normalised" ] && normalised="/"
  case "$normalised" in
    /)        echo "refusing --root / (would clobber the filesystem)" >&2; exit 1 ;;
    "$HOME")  echo "refusing --root \$HOME ($HOME)" >&2; exit 1 ;;
  esac
  case "$ROOT" in
    /)        echo "refusing --root /" >&2; exit 1 ;;
    "$HOME"|"$HOME/")
                echo "refusing --root \$HOME ($HOME)" >&2; exit 1 ;;
  esac
}

# Resolve root to an absolute path AFTER the safety check, so the
# safety check sees the literal user input.
if [ -n "$ROOT" ] && [ "$ROOT" != "/" ]; then
  ROOT="$(cd "$(dirname "$ROOT")" 2>/dev/null && pwd)/$(basename "$ROOT")" \
    || true
fi

require_git() {
  if ! command -v git >/dev/null 2>&1; then
    echo "error: git not on PATH" >&2
    exit 1
  fi
}

# ── seeding helpers ──────────────────────────────────────────────────
seed_dir_skill() {
  # $1 = tool_home, $2 = skill_name, $3 = body text
  local dir="$1/skills/$2"
  mkdir -p "$dir"
  {
    echo "---"
    echo "name: $2"
    echo "kind: skill"
    echo "---"
    echo "$3"
  } > "$dir/SKILL.md"
}

seed_marketplace_plugin() {
  # $1 = claude_home, $2 = marketplace, $3 = plugin, $4 = version, $5..n = skill names
  local claude_home="$1" marketplace="$2" plugin="$3" version="$4"
  shift 4
  local plugin_root="$claude_home/plugins/cache/$marketplace/$plugin/$version"
  mkdir -p "$plugin_root/.claude-plugin"
  cat > "$plugin_root/.claude-plugin/plugin.json" <<JSON
{"name":"$plugin","version":"$version","description":"sandbox fixture plugin"}
JSON
  while [ "$#" -gt 0 ]; do
    seed_dir_skill "$plugin_root" "$1" "Plugin-bundled skill seeded by sandbox script."
    shift
  done
}

init_bare_remote_with_seed() {
  # $1 = bare path
  local bare="$1" work
  work="$(mktemp -d)"
  trap 'rm -rf "$work"' EXIT

  git init --bare -b main "$bare" >/dev/null

  git -C "$work" init -q -b main
  git -C "$work" config user.email "sandbox@example.invalid"
  git -C "$work" config user.name  "ainb-sandbox"

  mkdir -p "$work/skills/initial-skill"
  cat > "$work/skills/initial-skill/SKILL.md" <<'YAML'
---
name: initial-skill
kind: skill
---
Seed skill in the bare remote; gives drift + sync tests something to act on.
YAML
  git -C "$work" add . >/dev/null
  GIT_AUTHOR_NAME=ainb-sandbox GIT_AUTHOR_EMAIL=sandbox@example.invalid \
  GIT_COMMITTER_NAME=ainb-sandbox GIT_COMMITTER_EMAIL=sandbox@example.invalid \
    git -C "$work" commit -q -m "seed: initial-skill"
  git -C "$work" remote add origin "$bare"
  git -C "$work" push -q origin main

  rm -rf "$work"
  trap - EXIT
}

seed_minimal() {
  local claude="$ROOT/.claude" codex="$ROOT/.codex"
  mkdir -p "$claude/skills" "$claude/agents" "$claude/commands" "$codex/skills"

  seed_dir_skill "$claude" "commit" \
    "Local hand-authored commit skill — no upstream provenance."
  seed_dir_skill "$claude" "fireworks-tech-graph" \
    "External clone skill — simulates bootstrap cloning from gh:yizhiyanhua-ai/fireworks-tech-graph."

  cat > "$claude/agents/distinguished-engineer.md" <<'YAML'
---
name: distinguished-engineer
---
Senior reviewer agent.
YAML

  seed_dir_skill "$codex" "deploy" "Codex-side deploy skill."

  seed_marketplace_plugin "$claude" "sandbox-marketplace" "discord" "0.1.0" \
    "access" "configure"
}

ALL_TOOLS_NON_PRIMARY=(copilot gemini cursor amazonq cline roo claude-desktop)

real_home_subpath_for() {
  case "$1" in
    claude)         echo ".claude" ;;
    codex)          echo ".codex" ;;
    copilot)        echo ".copilot" ;;
    gemini)         echo ".gemini" ;;
    cursor)         echo ".cursor" ;;
    amazonq)        echo ".aws/amazonq" ;;
    claude-desktop) echo ".config/Claude" ;;
    cline)          echo ".cline" ;;
    roo)            echo ".roo" ;;
    *)              echo ".$1" ;;
  esac
}

seed_full() {
  # Builds on top of Minimal.
  for tool in "${ALL_TOOLS_NON_PRIMARY[@]}"; do
    local sub
    sub="$(real_home_subpath_for "$tool")"
    local home="$ROOT/$sub"
    mkdir -p "$home"
    seed_dir_skill "$home" "${tool}-side-skill" "Skill seeded under the $tool adapter home."
  done

  seed_marketplace_plugin "$ROOT/.claude" "official" "code-review" "0.2.0" "review"
  seed_marketplace_plugin "$ROOT/.claude" "community" "third-party-thing" "unknown" "thing"

  # Pre-seed manifest with a shadowed_by conflict pair.
  cat > "$ROOT/.agents-in-a-box/manifest.yaml" <<YAML
schema_version: 1
sources:
  - name: sandbox
    type: git
    uri: git:file://$ROOT/sandbox-remote.git
    ref: main
    enabled: true
    target_layout:
      - glob: "skills/*/SKILL.md"
        home: ".claude/skills"
        repo: "skills"
units:
  - uri: git:file://$ROOT/sandbox-remote.git@main/skills/initial-skill
    targets: [claude]
  - uri: local:.claude/skills/commit
    targets: [claude]
    shadowed_by: git:file://$ROOT/sandbox-remote.git@main/skills/initial-skill
YAML

  : > "$ROOT/.agents-in-a-box/.skip-banner"

  cat > "$ROOT/external-dependencies.yaml" <<'YAML'
# Sandbox fixture — mirrors toolkit/external-dependencies.yaml schema.
version: "1.0.0"
updated: "2026-06-01"

external-packages:
  - name: reflect-kb
    source: https://github.com/stevengonsalvez/reflect-kb.git
    install: "uv tool install --force '{source}[graph]'"
    cli: reflect
    version-pin: main

bundled-skills:
  - name: coding-agent
    path: packages/skills/coding-agent
    purpose: "Delegate coding tasks to subagents"
  - name: interview
    path: packages/skills/interview
    purpose: "Plan interview + specification generation"

agent-skills:
  - name: fireworks-tech-graph
    repo: https://github.com/yizhiyanhua-ai/fireworks-tech-graph
    applies-to: [claude, codex, copilot, gemini]
    purpose: "SVG technical-diagram generator"
  - name: ui-ux-pro-max
    repo: https://github.com/nextlevelbuilder/ui-ux-pro-max-skill
    version: "2.2.1"
    applies-to: [claude, codex, copilot, gemini]
    purpose: "UI/UX design intelligence"
YAML
}

write_env_sh() {
  cat > "$ROOT/env.sh" <<EOF
# source this file before running ainb against the sandbox.
# generated by skill-manager-sandbox.sh on $(date -u +'%Y-%m-%dT%H:%M:%SZ')

export HOME="$ROOT"
export AINB_HOME="$ROOT/.agents-in-a-box"
export AINB_TOOL_HOME_CLAUDE="$ROOT/.claude"
export AINB_TOOL_HOME_CODEX="$ROOT/.codex"
export GIT_TERMINAL_PROMPT=0
export GIT_ASKPASS=/bin/true
EOF
}

# ── dispatch ─────────────────────────────────────────────────────────
case "$CMD" in
  up)
    refuse_dangerous_root
    require_git

    if [ -e "$ROOT" ]; then
      echo "error: $ROOT already exists; run 'down' first or pass --root <fresh>" >&2
      exit 1
    fi

    case "$TIER" in
      minimal|full) ;;
      *) echo "unknown tier: $TIER" >&2; exit 2 ;;
    esac

    mkdir -p "$ROOT/.agents-in-a-box"
    : > "$ROOT/$SENTINEL_FILE"        # marker before any seeding
    seed_minimal
    init_bare_remote_with_seed "$ROOT/sandbox-remote.git"
    [ "$TIER" = "full" ] && seed_full
    write_env_sh

    cat <<EOF
sandbox ready
  root           = $ROOT
  tier           = $TIER
  bare remote    = $ROOT/sandbox-remote.git
  env file       = $ROOT/env.sh

to use:
  source $ROOT/env.sh
  ./target/debug/ainb       # then press [m] in the TUI

to teardown:
  $(basename "$0") down --root $ROOT
EOF
    ;;

  down)
    refuse_dangerous_root

    if [ ! -e "$ROOT" ]; then
      # Idempotent: down on a missing dir is a no-op.
      exit 0
    fi

    if [ ! -f "$ROOT/$SENTINEL_FILE" ]; then
      echo "error: $ROOT does not contain the $SENTINEL_FILE sentinel — refusing rm -rf" >&2
      echo "       (this dir was not built by skill-manager-sandbox.sh)" >&2
      exit 1
    fi

    rm -rf "$ROOT"
    echo "torn down $ROOT"
    ;;

  *)
    echo "unknown subcommand: $CMD" >&2
    usage >&2
    exit 2
    ;;
esac
