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

    # The manifest lives next to the crate. Most plugin crates are under
    # `crates/<crate>/`, but the Hangar plugin lives outside the workspace root
    # at `../plugins/<plugin_id>/` (its crate dir is named by plugin id, not
    # crate name). Search every known layout for a `manifest.toml`/`plugin.toml`.
    local manifest_src=""
    for cand in \
        "crates/$crate/manifest.toml" \
        "crates/$crate/plugin.toml" \
        "../plugins/$plugin_id/manifest.toml" \
        "../plugins/$plugin_id/plugin.toml"; do
        if [[ -f "$cand" ]]; then
            manifest_src="$cand"
            break
        fi
    done
    if [[ -n "$manifest_src" ]]; then
        cp "$manifest_src" "$out_dir/manifest.toml"
    else
        printf 'WARN: no manifest found for %s (plugin id %s)\n' "$crate" "$plugin_id" >&2
    fi

    local size
    size=$(stat -f%z "$out_dir/$plugin_id" 2>/dev/null || stat -c%s "$out_dir/$plugin_id")
    printf 'staged %-30s -> %s (%s bytes)\n' "$crate" "$out_dir/$plugin_id" "$size"
}

build_plugin ainb-plugin-burndown burndown
build_plugin ainb-plugin-session-reader session-reader
# The Hangar control-plane plugin (P4.10). The crate is `ainb-plugin-hangar`
# but its manifest `[plugin].name` — and therefore the discovered plugin id and
# the host `PLUGIN_SCREENS` routing entry — is `hangar-tui`, so it stages under
# `dist/plugins/hangar-tui/`.
build_plugin ainb-plugin-hangar hangar-tui
