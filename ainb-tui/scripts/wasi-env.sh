#!/usr/bin/env bash
# WASI build environment for ainb plugins (wasm32-wasip1).
#
# Source this file before invoking `cargo build --target wasm32-wasip1`:
#   source scripts/wasi-env.sh
#
# Requires wasi-sdk-33+ installed at $WASI_SDK_PATH (default: ~/wasi-sdk).
# Override with: WASI_SDK_PATH=/path/to/wasi-sdk source scripts/wasi-env.sh
#
# rusqlite-bundled needs the WASI emulated libs for mman / getpid /
# process_clocks / signal — wasi-libc ships them as separate emulated libs
# that aren't auto-linked, so we wire them in via CFLAGS + RUSTFLAGS.

: "${WASI_SDK_PATH:=$HOME/wasi-sdk}"

if [[ ! -x "$WASI_SDK_PATH/bin/clang" ]]; then
    echo "wasi-env: WASI_SDK_PATH=$WASI_SDK_PATH does not contain bin/clang" >&2
    echo "  install wasi-sdk-33+ from https://github.com/WebAssembly/wasi-sdk/releases" >&2
    return 1 2>/dev/null || exit 1
fi

export WASI_SDK_PATH
export CC_wasm32_wasip1="$WASI_SDK_PATH/bin/clang"
export AR_wasm32_wasip1="$WASI_SDK_PATH/bin/llvm-ar"
export CFLAGS_wasm32_wasip1="--sysroot=$WASI_SDK_PATH/share/wasi-sysroot -D_WASI_EMULATED_MMAN -D_WASI_EMULATED_GETPID -D_WASI_EMULATED_PROCESS_CLOCKS -D_WASI_EMULATED_SIGNAL"
export RUSTFLAGS_wasm32_wasip1="-C link-arg=-lwasi-emulated-mman -C link-arg=-lwasi-emulated-getpid -C link-arg=-lwasi-emulated-process-clocks -C link-arg=-lwasi-emulated-signal"

echo "wasi-env: ready (WASI_SDK_PATH=$WASI_SDK_PATH, target=wasm32-wasip1)"
