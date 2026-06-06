#!/usr/bin/env bash
# publish-explainer.sh — validate the explainer dir, then print the exact /here-now invocation.
# There is no here-now CLI; publishing is the /here-now skill's job. This only gates + reminds.
#
# Usage: publish-explainer.sh --dir <explainer-dir> --slug <stable-slug>
set -euo pipefail

DIR="" SLUG=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dir) DIR="$2"; shift 2;;
    --slug) SLUG="$2"; shift 2;;
    *) echo "unknown arg: $1" >&2; exit 2;;
  esac
done
[ -n "$DIR" ] && [ -n "$SLUG" ] || { echo "ERR: --dir and --slug required" >&2; exit 2; }
[ -d "$DIR" ] || { echo "ERR: $DIR is not a directory" >&2; exit 2; }

# index.html present?
HTML="$(find "$DIR" -maxdepth 1 -iname 'index.html' | head -1)"
[ -n "$HTML" ] || HTML="$(find "$DIR" -maxdepth 1 -iname '*.html' | head -1)"
[ -n "$HTML" ] || { echo "ERR: no .html in $DIR" >&2; exit 4; }

# at least one journey gif present (the explainer must embed recordings)?
GIFS="$(find "$DIR" -maxdepth 2 -iname '*.gif' | wc -l | tr -d ' ')"
[ "$GIFS" -ge 1 ] || echo "WARN: no .gif found under $DIR — journeys section will be empty." >&2

# warn on obvious placeholders left in the html
if rg -qi 'TODO|TBD|PLACEHOLDER|\{\{' "$HTML" 2>/dev/null; then
  echo "WARN: $HTML still contains TODO/TBD/PLACEHOLDER/{{ — fix before publishing." >&2
fi

echo "✓ explainer dir OK: $HTML  (gifs: $GIFS)"
cat <<EOF

PUBLISH (run the /here-now skill — stable slug, overwrites same URL):
  /here-now publish $DIR --slug $SLUG

Then FETCH the live URL and run the G6 HTML-solid checklist
(see ../references/explainer.md): links resolve, GIFs load, no empty sections,
test output is the latest green run, commit log matches git log.
EOF
