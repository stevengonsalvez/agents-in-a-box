#!/usr/bin/env bash
# Build all bundled ainb plugins (Phase 7 subprocess architecture) and stage
# into dist/plugins/<id>/. Each staged plugin has the layout the runtime's
# discovery expects:
#
#   dist/plugins/<id>/<id>          executable binary
#   dist/plugins/<id>/manifest.toml plugin manifest
#
# On macOS, cargo links binaries with an ad-hoc, linker-signed signature
# bound to the original build path. Copying the binary to a new path
# invalidates that signature and AMFI SIGKILLs the process at exec time
# (silent — no stderr, exit 137). We re-sign in place after each copy.
#
# Run from the ainb-tui workspace root:
#   ./scripts/build-plugins.sh
#   ./scripts/build-plugins.sh --release
set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE="dev"
PROFILE_DIR="debug"
for arg in "$@"; do
    case "$arg" in
        --release)
            PROFILE="release"
            PROFILE_DIR="release"
            ;;
        *)
            printf 'unknown arg: %s\n' "$arg" >&2
            exit 2
            ;;
    esac
done

resign_macos() {
    local bin="$1"
    if [[ "$(uname -s)" != "Darwin" ]]; then
        return 0
    fi
    # Strip the path-bound linker signature, then re-sign ad-hoc at the
    # new path. Without this, AMFI kills the process at exec (exit 137,
    # no stderr) because the linker-signed hash no longer matches the
    # binary's location.
    codesign --remove-signature "$bin" >/dev/null 2>&1 || true
    codesign --sign - "$bin" >/dev/null 2>&1
}

build_plugin() {
    local crate="$1"
    local plugin_id="$2"

    cargo build -p "$crate" --profile "$PROFILE"

    local out_dir="dist/plugins/$plugin_id"
    mkdir -p "$out_dir"

    # Cargo binary name == crate name. Staged binary name == plugin id
    # (the runtime probes <root>/<id>/<id>).
    cp "target/$PROFILE_DIR/$crate" "$out_dir/$plugin_id"
    resign_macos "$out_dir/$plugin_id"

    if [[ -f "crates/$crate/manifest.toml" ]]; then
        cp "crates/$crate/manifest.toml" "$out_dir/manifest.toml"
    elif [[ -f "crates/$crate/plugin.toml" ]]; then
        cp "crates/$crate/plugin.toml" "$out_dir/manifest.toml"
    fi

    local size
    size=$(stat -f%z "$out_dir/$plugin_id" 2>/dev/null || stat -c%s "$out_dir/$plugin_id")
    printf 'staged %-30s -> %s (%s bytes)\n' "$crate" "$out_dir/$plugin_id" "$size"
}

build_plugin ainb-plugin-burndown burndown
build_plugin ainb-plugin-session-reader session-reader
build_plugin ainb-plugin-witr witr
build_plugin ainb-plugin-abtop abtop
