#!/usr/bin/env bash
# verify-journey.sh — record ONE ainb journey with vhs, extract frames, OCR pre-filter,
# and print a checklist. The real assertion is YOU reading the frames (see references/frame-truth.md).
#
# Usage:
#   verify-journey.sh --name <id> --bin <ainb> --keys "<vhs keys>" --out <dir> \
#                     [--home <dir>] [--tape <file>] [--width 1700] [--height 1000] \
#                     [--expect <regex>]...
#
# --keys: vhs commands; separate distinct commands with TWO+ spaces (or newlines), e.g.
#         'Type "G"  Sleep 2s  Screenshot {OUT}/open-g.png'
#         Tokens {OUT} and {NAME} are expanded. Always Sleep before Screenshot.
set -euo pipefail

NAME="" BIN="" KEYS='Type "G"  Sleep 2s' OUT="" HOME_DIR="" TAPE="" WIDTH=1700 HEIGHT=1000
EXPECTS=()
while [ $# -gt 0 ]; do
  case "$1" in
    --name) NAME="$2"; shift 2;;
    --bin) BIN="$2"; shift 2;;
    --keys) KEYS="$2"; shift 2;;
    --out) OUT="$2"; shift 2;;
    --home) HOME_DIR="$2"; shift 2;;
    --tape) TAPE="$2"; shift 2;;
    --width) WIDTH="$2"; shift 2;;
    --height) HEIGHT="$2"; shift 2;;
    --expect) EXPECTS+=("$2"); shift 2;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done

[ -n "$NAME" ] || { echo "ERR: --name required" >&2; exit 2; }
[ -n "$OUT" ]  || { echo "ERR: --out required"  >&2; exit 2; }
[ -n "$BIN" ] || [ -n "$TAPE" ] || { echo "ERR: --bin or --tape required" >&2; exit 2; }
for t in vhs ffmpeg; do command -v "$t" >/dev/null || { echo "ERR: $t not on PATH" >&2; exit 3; }; done

SK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
mkdir -p "$OUT/tapes" "$OUT/frames"

# Isolated HOME (advisory wizard-seed check).
if [ -z "$HOME_DIR" ]; then HOME_DIR="$(mktemp -d "${TMPDIR:-/tmp}/ainb-home-${NAME}.XXXX")"; fi
if [ ! -e "$HOME_DIR/.agents-in-a-box/onboarding.toml" ] && [ ! -e "$HOME_DIR/.config/ainb/onboarding.toml" ]; then
  echo "WARN: $HOME_DIR has no onboarding seed — first-run wizard may swallow keys." >&2
  echo "      Seed it like tmux-ui-tripwire does (see ../tmux-ui-tripwire/references/seed-data.md)." >&2
fi

# Render tape from template unless one was supplied.
# KEYS is multi-line; BSD awk can't take newlines via -v, so write keys to a file and
# splice them in with getline. Scalars are substituted with sed first.
if [ -z "$TAPE" ]; then
  TAPE="$OUT/tapes/$NAME.tape"
  KEYSF="$OUT/tapes/$NAME.keys"
  KEYS_X="${KEYS//\{OUT\}/$OUT}"; KEYS_X="${KEYS_X//\{NAME\}/$NAME}"
  printf '%s\n' "$KEYS_X" | sed -E 's/  +/\
/g' > "$KEYSF"
  sed -e "s|{{AINB_BIN}}|$BIN|g" -e "s|{{AINB_HOME}}|$HOME_DIR|g" \
      -e "s|{{WIDTH}}|$WIDTH|g"  -e "s|{{HEIGHT}}|$HEIGHT|g" \
      -e "s|{{NAME}}|$NAME|g"    -e "s|{{OUT}}|$OUT|g" "$SK_DIR/journey.tape.tmpl" \
    | awk -v kf="$KEYSF" '/\{\{KEYS\}\}/{while((getline l < kf)>0)print l; next} {print}' > "$TAPE"
fi

echo "▶ vhs $TAPE"
vhs "$TAPE"

# Extract frames from the mp4 (last/settled + scene cuts) as a backup to any Screenshot in the tape.
MP4="$OUT/$NAME.mp4"
if [ -f "$MP4" ]; then
  ffmpeg -nostdin -loglevel error -y -sseof -0.3 -i "$MP4" -frames:v 1 "$OUT/frames/$NAME-last.png" || true
  ffmpeg -nostdin -loglevel error -y -i "$MP4" -vf "select='gt(scene\,0.2)'" -vsync vfr "$OUT/frames/$NAME-scene-%02d.png" || true
fi
# Any tape Screenshots already landed under $OUT; surface them.
shopt -s nullglob
SHOTS=("$OUT/$NAME"*.png "$OUT/frames/$NAME"*.png)

# OCR pre-filter (best-effort; NOT the verdict).
OCR="$OUT/$NAME.ocr.txt"
if command -v tesseract >/dev/null && [ -f "$OUT/frames/$NAME-last.png" ]; then
  tesseract "$OUT/frames/$NAME-last.png" stdout 2>/dev/null > "$OCR" || true
fi

echo
echo "════════ JOURNEY: $NAME ════════"
echo "recording : $OUT/$NAME.gif , $OUT/$NAME.mp4"
echo "frames    : ${SHOTS[*]:-<none — check vhs output>}"
if [ ${#EXPECTS[@]} -gt 0 ]; then
  echo "expect    (OCR pre-filter, not the verdict):"
  for e in "${EXPECTS[@]}"; do
    if [ -f "$OCR" ] && grep -qiE -- "$e" "$OCR" 2>/dev/null; then echo "  ✓ ocr-hit: $e"; else echo "  • READ-FRAME: $e"; fi
  done
fi
cat <<EOF

CHECKLIST — open each frame above (Read the PNG) and assert the journey's outcome:
  - the RIGHT text/content (paths, counts, code, counters) is present and correct
  - the RIGHT color/state (added=green row, removed=red row, brighter word-emphasis,
    chevron ∨/›, which tab is active)
  - the screen MATCHES THE DESIGN (layout/gutter/bars)
  - for exit journeys: the surface is GONE on the final frame
A journey PASSES only when the read confirms the content — never on "non-blank".
EOF
