#!/usr/bin/env bash
# Generate docs/tui/keyboard-shortcuts.md from the effective built-in keymap.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
AINB_TUI_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$AINB_TUI_DIR/.." && pwd)"
OUT="$REPO_ROOT/docs/tui/keyboard-shortcuts.md"

if [[ -n "${AINB_BIN:-}" ]]; then
  if [[ "$AINB_BIN" = /* ]]; then
    BIN="$AINB_BIN"
  elif [[ -x "$AINB_BIN" ]]; then
    BIN="$(cd "$(dirname "$AINB_BIN")" && pwd)/$(basename "$AINB_BIN")"
  else
    BIN="$AINB_TUI_DIR/$AINB_BIN"
  fi
else
  (cd "$AINB_TUI_DIR" && cargo build --release -p ainb >&2)
  BIN="$AINB_TUI_DIR/target/release/ainb"
fi

[[ -x "$BIN" ]] || { echo "[gen-keymap-docs] binary not found: $BIN" >&2; exit 1; }
# The docs site parses every page in docs/ as a content collection entry and
# REQUIRES a `title` in the frontmatter. The binary's `--format md` output is a
# plain markdown body on purpose — other consumers read it — so the frontmatter
# is written here, exactly as gen-cli-reference.sh writes its own.
{
  cat <<'PREAMBLE'
---
title: "Keyboard shortcuts"
description: "Every built-in ainb key binding, grouped by context, generated from the effective keymap."
---

PREAMBLE
  "$BIN" keymap list --format md
} > "$OUT"
echo "[gen-keymap-docs] wrote $OUT" >&2
