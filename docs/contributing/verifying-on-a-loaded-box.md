---
title: "Verifying on a loaded box"
---

# Verifying on a loaded box

Notes from a long multi-agent session on the shared build host, where hours went
into diagnosing "test failures" that were infrastructure. Every signature below
was met, misread at least once, and then confirmed. Read this before concluding
a red run means broken code.

## Builds kill each other

An `earlyoom` daemon runs with `-m 8` on a 7751 MB host:

- **SIGTERM** when available memory drops below 8% (620 MB)
- **SIGKILL** below 4% (310 MB)

It selects by **resident size, not by who started last**, so a careless build
kills someone else's work rather than its own. `rustc` is in neither its
`--avoid` nor its `--prefer` list; it simply wins by RSS.

Confirmed numerically against two independent deaths: 539 MB available produced
`signal: 15`, and 225 MB produced exit `137` (128+9). Both land on the correct
side of the correct threshold.

Consequence: waiting for a clear window is not politeness, it is the only way
two builds do not sabotage each other.

## Failure signatures that are NOT code failures

Recognising these takes thirty seconds. Meeting one cold costs twenty minutes.

| Signature | Cause |
|---|---|
| `rustc-LLVM ERROR: IO failure on output stream: No space left on device` | disk exhausted during codegen |
| `could not execute process rustc ... (never executed)` | an artifact was removed under a live build |
| `extern location for <crate> does not exist` | same |
| `could not parse/generate dep info at .../<crate>-<hash>.d` | same, but it names an **unrelated third-party crate**, which is maximally confusing. Clears on a plain re-run, no target-dir surgery |
| exit `137`, or `signal: 15` with no `test result:` line | earlyoom, not a test |
| `error connecting to <path>/default (File name too long)` | `TMUX_TMPDIR` past the 104-byte AF_UNIX socket cap. Use a short dir such as `/tmp/tmxb` |

## The only reliable idle gate

```bash
ps -eo comm | grep -cE '^(rustc|cargo|rust-lld|collect2)$'
```

`comm` is the executable name, so the shell running the check is `zsh` and never
`cargo`. The two obvious alternatives both fail, in opposite directions:

- a bare process count **false-negatives**: cargo idles between `rustc`
  invocations, and those gaps are exactly where you get caught
- grepping full argv **false-positives**: it matches any shell that merely
  mentions cargo, including the polling command itself

Pair it with a free-space check. A gate on both conditions once held for 47
polls (~23 minutes) before clearing; a count-only gate would have started inside
one of those windows.

## Never pipe cargo

```bash
cargo test ... | tail -30     # reports the exit status of TAIL
```

A build that ran **zero tests** reports success this way. Proven:

```
zsh -c '(exit 101) | tail -5; echo $?'                   -> 0
zsh -c 'set -o pipefail; (exit 101) | tail -5; echo $?'  -> 101
```

Redirect to a log, echo `$?`, grep the log, and require the status and the
content to **agree** before calling anything green. `scripts/hangar/run_all_tripwires.sh`
already sets `pipefail`; ad-hoc shell runs are where this bites.

## Memory is usually the binding constraint, not disk

`CARGO_PROFILE_TEST_STRIP=debuginfo` does **not** reduce peak RSS: stripping
happens after the debug info is generated. Use:

```bash
OPENSSL_NO_VENDOR=1 CARGO_INCREMENTAL=0 \
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 \
cargo test -j1 ...
```

`OPENSSL_NO_VENDOR=1` is required here or `openssl-sys` fails to find headers
after building its own vendored copy.

## Working rules

- Full-suite verification belongs in CI, which has the machine for it. Local
  cargo is for targeted single tests.
- Announce before pruning anything under `target/`, and never prune while the
  idle gate is non-zero. The exception is below roughly a gigabyte free, where a
  running build is already dead and only has not noticed.
- Killing background jobs by script name is unsafe: every agent writes the same
  helper into the same scratchpad path. Use unique per-run names.
- A build killed by earlyoom is an **infrastructure** failure. Never report it
  as a red suite.
