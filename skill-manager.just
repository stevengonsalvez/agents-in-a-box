# Skill-manager dev workflow — sandbox lifecycle, TUI / CLI launchers,
# scoped test commands. See docs/skill-manager/sandbox-testing.md for
# the full walkthrough. Bead ai-e7t.

set shell := ["bash", "-cu"]

# Sandbox root. Override with AINB_SANDBOX_ROOT=/some/path if you
# want a non-default location (the bash script's --root flag stays
# the source of truth; this just mirrors its default).
sandbox_root := env_var_or_default("AINB_SANDBOX_ROOT", env_var("HOME") + "/.cache/ainb-sandbox")

# AINB-specific env exported globally. Cargo / git / shell don't
# read these, so leaking them across every recipe is harmless and
# saves per-recipe boilerplate.
export AINB_HOME            := sandbox_root + "/.agents-in-a-box"
export AINB_TOOL_HOME_CLAUDE := sandbox_root + "/.claude"
export AINB_TOOL_HOME_CODEX  := sandbox_root + "/.codex"
export GIT_TERMINAL_PROMPT   := "0"
export GIT_ASKPASS           := "/bin/true"

# Default recipe — list everything available with --list output.
default:
    @just --list --justfile {{justfile()}}

# ── build ─────────────────────────────────────────────────────────

# Compile the `ainb` binary (debug profile).
build:
    cd ainb-tui && cargo build

# ── sandbox lifecycle ─────────────────────────────────────────────

# Build the sandbox. Default tier = minimal.
up tier="minimal":
    scripts/skill-manager-sandbox.sh up --tier {{tier}} --root {{sandbox_root}}

# Teardown the sandbox.
down:
    scripts/skill-manager-sandbox.sh down --root {{sandbox_root}}

# Down + up — convenience for the common dev loop.
restart tier="minimal": down (up tier)

# Show what's currently in the sandbox.
inspect:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -d "{{sandbox_root}}" ]; then
      echo "no sandbox at {{sandbox_root}} — try: just skill-manager up"
      exit 0
    fi
    echo "── root: {{sandbox_root}}"
    echo "── .claude/skills/"
    ls "{{sandbox_root}}/.claude/skills/" 2>/dev/null || echo "(empty)"
    echo "── .codex/skills/"
    ls "{{sandbox_root}}/.codex/skills/" 2>/dev/null || echo "(empty)"
    echo "── plugins/cache/ (SKILL.md count)"
    find "{{sandbox_root}}/.claude/plugins/cache" -name SKILL.md 2>/dev/null | wc -l | tr -d ' '
    echo "── bare remote (sandbox-remote.git)"
    git -C "{{sandbox_root}}/sandbox-remote.git" log --oneline 2>/dev/null | head -5 || echo "(no bare remote)"
    if [ -f "{{sandbox_root}}/.agents-in-a-box/manifest.yaml" ]; then
      echo "── manifest.yaml exists ($(wc -l < {{sandbox_root}}/.agents-in-a-box/manifest.yaml) lines)"
    else
      echo "── manifest.yaml absent (empty-state — discovery banner pops on [m])"
    fi

# Print the env-export lines (handy when you'd rather not use just).
env:
    @echo "export HOME={{sandbox_root}}"
    @echo "export AINB_HOME={{sandbox_root}}/.agents-in-a-box"
    @echo "export AINB_TOOL_HOME_CLAUDE={{sandbox_root}}/.claude"
    @echo "export AINB_TOOL_HOME_CODEX={{sandbox_root}}/.codex"
    @echo "export GIT_TERMINAL_PROMPT=0"
    @echo "export GIT_ASKPASS=/bin/true"

# ── launch ────────────────────────────────────────────────────────

# Launch the TUI against the sandbox.
tui: build
    # Build under the real $HOME (rustup needs ~/.rustup/, cargo needs
    # ~/.cargo/) then exec the built binary with HOME redirected to
    # the sandbox. Doing `HOME=sandbox cargo run` breaks rustup's
    # toolchain resolution.
    HOME={{sandbox_root}} ainb-tui/target/debug/ainb

# Convenience: full bring-up — sandbox + launch TUI in one go.
run tier="minimal": (up tier) tui

# Drive the CLI against the sandbox (passes args through to ainb).
cli *args: build
    # Examples:
    #   just skill-manager cli skill check
    #   just skill-manager cli skill sync --to-repo --yes
    #   just skill-manager cli source list
    HOME={{sandbox_root}} ainb-tui/target/debug/ainb {{args}}

# ── tests ─────────────────────────────────────────────────────────

# Scoped acceptance: skill-core + cli (matches the PR #178 verify bar).
test:
    cd ainb-tui && cargo test -p ainb-skill-core -p ainb-cli

# Just the sandbox fixture smoke (fast).
test-fixture:
    cd ainb-tui && cargo test -p ainb-skill-core --test sandbox_fixture_smoke

# Both sandbox tripwires — TestBackend + live tmux.
test-tripwires:
    cd ainb-tui && cargo test -p ainb \
      --test tripwire_core_skill_manager_sandbox_loads \
      --test tripwire_core_skill_manager_sandbox_e2e
