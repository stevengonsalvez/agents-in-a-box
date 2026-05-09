#!/usr/bin/env bash
# Build all bundled ainb plugins to wasm32-wasip1 and stage into dist/plugins/.
#
# Run from the ainb-tui workspace root:
#   ./scripts/build-plugins.sh [--release]
set -euo pipefail

cd "$(dirname "$0")/.."

# shellcheck source=./wasi-env.sh
source "$(pwd)/scripts/wasi-env.sh"

PROFILE="${PROFILE:-release}"
TARGET="wasm32-wasip1"

build_plugin() {
    local crate="$1"
    local plugin_id="$2"

    # Plugin crate lives in [workspace.exclude] (own sub-workspace) so it
    # must be built from inside its own directory; root `-p` won't see it.
    (cd "crates/$crate" && cargo build --target "$TARGET" --profile "$PROFILE")

    local out_dir="dist/plugins/$plugin_id"
    mkdir -p "$out_dir"
    cp "crates/$crate/target/$TARGET/$PROFILE/${crate//-/_}.wasm" \
       "$out_dir/plugin.wasm"
    if [[ -f "crates/$crate/plugin.toml" ]]; then
        cp "crates/$crate/plugin.toml" "$out_dir/"
    fi

    local size
    size=$(stat -f%z "$out_dir/plugin.wasm" 2>/dev/null || stat -c%s "$out_dir/plugin.wasm")
    printf 'built %s -> %s (%s bytes)\n' "$crate" "$out_dir/plugin.wasm" "$size"
}

build_plugin ainb-plugin-burndown burndown
build_plugin ainb-plugin-session-reader session-reader
