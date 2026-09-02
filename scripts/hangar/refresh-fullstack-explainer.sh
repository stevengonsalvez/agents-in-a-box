#!/usr/bin/env bash
# Rebuild + republish the prove-fullstack status explainer (explain-to-me skill
# template + theme + here.now domain publish). Run at the end of every loop
# iteration so https://explainers.stevengonsalvez.com/prove-hangar/ tracks
# REPORT.md and the branch. Re-publishing to the same --path repoints the mount;
# the domain URL is stable.
set -euo pipefail
WT="$(cd "$(dirname "$0")/../.." && pwd)"
SKILL="$HOME/.claude/skills/explain-to-me"
OUT="$WT/explainers/prove-hangar-fullstack.html"
python3 "$WT/scripts/hangar/build-fullstack-explainer.py" "$OUT"
python3 "$SKILL/scripts/inject_theme.py" "$OUT" >/dev/null
python3 "$SKILL/scripts/publish_explainer.py" "$OUT" \
  --path prove-hangar --title "prove-hangar: live proving run" \
  --desc "Hangar builds a real app through the TUI: legs, green recordings, defect ledger, fix commits (PR #815), refreshed every loop" \
  --category ainb | tail -1
