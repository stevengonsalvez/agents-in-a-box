#!/usr/bin/env bash
# Record the Daemons-screen user journeys with vhs.
#
# The tapes are templates because a vhs tape has to name an absolute binary and
# HOME, and those are machine-specific. This seeds a throwaway HOME (so the
# recording shows a clean machine rather than whatever the author has running),
# fills the placeholders, and runs vhs from the crate root — vhs 0.11 rejects an
# absolute `Output` path, so the working directory has to be right.
#
# Usage: scripts/record-daemons-journeys.sh [journey ...]   (default: all)
# No `-u`: macOS ships bash 3.2, which treats "${arr[@]}" as unbound even when
# the array is set. Every variable here is assigned before use.
set -eo pipefail

cd "$(dirname "$0")/.."
CRATE="crates/ainb-core"
AINB="$PWD/target/debug/ainb"

[ -x "$AINB" ] || { echo "build it first: cargo build -p ainb --bin ainb" >&2; exit 1; }
command -v vhs >/dev/null || { echo "vhs not on PATH (brew install vhs)" >&2; exit 1; }

HOME_DIR=$(mktemp -d)
trap 'rm -rf "$HOME_DIR"' EXIT
VERSION=$(awk -F'"' '/^version/ {print $2; exit}' Cargo.toml)
mkdir -p "$HOME_DIR/.agents-in-a-box/config"
# Skip onboarding and the hooks install dialog, or the first keystroke lands in
# a modal instead of the home screen.
cat > "$HOME_DIR/.agents-in-a-box/config/onboarding.toml" <<EOF
completed = true
completed_at = "2026-05-27T00:00:00+00:00"
version = "$VERSION"
skipped_dependencies = []
git_directories = []
EOF
printf '%s' '{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}' \
  > "$HOME_DIR/.agents-in-a-box/install.json"

if [ $# -eq 0 ]; then
  journeys=(daemons-roster daemons-actions daemons-error-view)
else
  journeys=("$@")
fi

mkdir -p "$CRATE/tests/recordings"
for j in "${journeys[@]}"; do
  tmpl="$CRATE/tests/tapes/$j.tape.tmpl"
  [ -f "$tmpl" ] || { echo "no such journey: $j" >&2; exit 1; }
  # BSD mktemp only accepts a trailing X run, so the .tape suffix cannot be in
  # the template. Make the dir instead and name the file inside it.
  tape_dir=$(mktemp -d)
  tape="$tape_dir/$j.tape"
  sed -e "s|__HOME__|$HOME_DIR|g" -e "s|__AINB__|$AINB|g" "$tmpl" > "$tape"
  echo "recording $j…"
  (cd "$CRATE" && vhs "$tape")
  rm -rf "$tape_dir"
done

echo "recordings in $CRATE/tests/recordings/"
echo "to publish them as PR proof: cp $CRATE/tests/recordings/*.gif ../docs/assets/screenshots/"
