# amfi.md — macOS AMFI silent SIGKILL of staged plugins

## Symptom

You stage a plugin binary via `cp target/debug/X dist/plugins/Y/Y`.
Standalone the binary appears fine. When launched by the ainb runtime:

- Host JSONL shows `eager spawn failed: protocol: transport: Broken pipe (os error 32)`
- Then `plugin exited / pipe closed`
- Plugin's `eprintln!` at the top of `main()` produces NO stderr
- TUI is stuck on `⏳ Waiting for <plugin> plugin...`

## Root cause

Cargo links debug binaries with **ad-hoc, linker-signed Mach-O signatures
bound to the original build path**. The signature hash includes the file
path. `cp` to a new path invalidates the signature. macOS AMFI (Apple
Mobile File Integrity) silently SIGKILLs the process at `exec()` time —
no stderr, no crash log, exit 137 in <1ms. The host's first `write_frame`
hits the closed pipe and reports `Broken pipe`, which looks like a
runtime wire bug rather than a kernel-level kill.

This affects ANY copy/move of a debug binary on macOS (Apple Silicon and
Intel). Release binaries (`--release`) are also linker-signed but rarely
moved, so this trap hits dev workflows specifically.

## Diagnostic recipe

One command tells you definitively:

```bash
./dist/plugins/<id>/<id> </dev/null; echo "exit=$?"
```

| Output | Diagnosis |
|---|---|
| `exit=137` in <1ms, no stderr | AMFI kill — needs re-sign |
| `exit=0` and plugin ran for a moment | Binary is fine, look elsewhere |
| Plugin prints stderr then hangs | Not AMFI — actual code path bug |

Other useful probes:

```bash
codesign -dv ./dist/plugins/<id>/<id> 2>&1
# Look for: "Format=Mach-O thin (arm64)" "Signature=adhoc" "linker-signed"

cmp target/debug/<crate> dist/plugins/<id>/<id> && echo "identical bytes"
# Confirms cp produced identical content — AMFI is about PATH, not bytes

shasum target/debug/<crate> dist/plugins/<id>/<id>
# Same shasum, different runtime behaviour = AMFI path binding
```

## Fix

Re-sign after copy:

```bash
codesign --remove-signature <path-to-staged-bin>
codesign --sign - <path-to-staged-bin>
```

Already baked into `scripts/build-plugins.sh`:

```bash
resign_macos() {
    local bin="$1"
    [[ "$(uname -s)" != "Darwin" ]] && return 0
    codesign --remove-signature "$bin" >/dev/null 2>&1 || true
    codesign --sign - "$bin" >/dev/null 2>&1
}
```

Just run `just stage-plugins` — it handles cp + re-sign in one pass.

## Misleading red herrings

| What looks like the cause | What it actually means |
|---|---|
| `Broken pipe (os error 32)` in host log | The pipe IS broken — but because AMFI killed the child, not because of a wire bug |
| `spctl -a -t exec <bin>` returns "rejected" | Normal for ad-hoc binaries; not the cause of the kill |
| No stderr from plugin's `eprintln!` in `main()` | `main()` literally never executed; the kernel killed the process between `exec()` and the first instruction |
| Identical shasum, identical xattrs, identical permissions | Doesn't matter — AMFI hashes the PATH into the signature check |
| Burndown loads but session-reader doesn't | Pattern: session-reader is `spawn = "eager"` so it spawns at registration; burndown is `spawn = "lazy"` so it only spawns on first request. Lazy plugins reveal the bug later. Both are affected. |

## CI / Linux

Linux doesn't have AMFI. The `resign_macos` function noops on
`uname -s != Darwin`. CI on Linux runs without the trap.

## Why we don't ship signed builds

Re-signing with a Developer ID is the "proper" fix but requires Apple
Developer enrollment + entitlements + notarization. For dev workflow
and `just stage-plugins`, ad-hoc re-sign (`codesign --sign -`) is
sufficient — AMFI accepts ad-hoc signatures bound to the current path.
