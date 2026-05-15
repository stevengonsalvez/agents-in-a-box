#!/usr/bin/env bash
# Seed an isolated $HOME with the onboarding-complete stamp + a
# synthetic Claude session, then exec ainb tui. The home dir is
# driven by $AINB_SCREENSHOT_HOME — vhs sets it via Env in the tape
# file so screenshots are reproducible across runs and never touch
# the user's real ~/.

set -euo pipefail

HOME_DIR="${AINB_SCREENSHOT_HOME:-/tmp/ainb-screenshot-home}"
CFG_DIR="$HOME_DIR/.agents-in-a-box/config"
PROJECTS_DIR="$HOME_DIR/.claude/projects/-screenshot-fixture-project"
VERSION="1.1.0"

mkdir -p "$CFG_DIR" "$PROJECTS_DIR"

cat > "$CFG_DIR/onboarding.toml" <<EOF
completed = true
completed_at = "2026-05-11T00:00:00+00:00"
version = "$VERSION"
skipped_dependencies = []
git_directories = []
EOF

cat > "$PROJECTS_DIR/screenshot-session-1.jsonl" <<'EOF'
{"type":"assistant","timestamp":"2026-05-14T09:15:00.000Z","sessionId":"screenshot-1","cwd":"/Users/stevengonsalvez/projects/ainb","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":12500,"output_tokens":3400,"cache_creation_input_tokens":2000,"cache_read_input_tokens":18000}}}
{"type":"assistant","timestamp":"2026-05-14T10:42:00.000Z","sessionId":"screenshot-1","cwd":"/Users/stevengonsalvez/projects/ainb","message":{"model":"claude-opus-4","usage":{"input_tokens":4500,"output_tokens":2100,"cache_creation_input_tokens":0,"cache_read_input_tokens":8000}}}
{"type":"assistant","timestamp":"2026-05-14T14:08:00.000Z","sessionId":"screenshot-2","cwd":"/Users/stevengonsalvez/projects/ainb","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":8200,"output_tokens":1800,"cache_creation_input_tokens":0,"cache_read_input_tokens":12000}}}
{"type":"assistant","timestamp":"2026-05-15T08:30:00.000Z","sessionId":"screenshot-3","cwd":"/Users/stevengonsalvez/projects/ainb","message":{"model":"claude-sonnet-4-5","usage":{"input_tokens":15000,"output_tokens":4100,"cache_creation_input_tokens":3500,"cache_read_input_tokens":22000}}}
EOF

# Same isolated home for the plugin runtime's data dir + log dir.
HOME="$HOME_DIR" exec ./target/debug/ainb tui
