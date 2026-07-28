# No orphaned `codex app-server` processes

## Problem

Twice in 26 hours a 32 GB Mac hung machine-wide (every new window "not
responding", `kernel_task` pinned) because background daemons had quietly
manufactured hundreds of orphaned `codex app-server` processes. Round 1: 1017
procs / 16.4 GB RSS / 909 orphaned at `ppid == 1`, swap 27.4 of 28.6 GB. Round
2 (~26 h later) refilled to 567 / 11.1 GB / 554 orphans.

Two independent spawners leak, and they share one bug class: **all cleanup
lives on the happy exit path**, which never runs on `SIGKILL`, OOM reap, crash,
or power cut.

- **Source A, the ainb hangar daemon.** Spawns one shared `codex app-server` on
  `~/.agents-in-a-box/codex-app-server.sock`. Two failures: (1) `prepare_socket`
  did an unlocked check-then-act and `wait_for_socket` only proved the socket
  *path* appeared, not that *this* child bound it, so concurrent spawns each kept
  a duplicate server (four codex procs held the same socket at once, by `lsof`);
  (2) every teardown is Rust `Drop` (`kill_on_drop`), which `SIGKILL` skips.
- **Source B, the `openai-codex` Claude Code plugin broker (`/codex:rescue`).**
  `lib/broker-lifecycle.mjs` mkdtemps a fresh dir per session
  (`${tmpdir}/cxc-XXXXXX/broker.sock`) and spawns the broker `detached: true` +
  `child.unref()`. The only reaper is the `SessionEnd` hook, and because each
  socket path is unique, nothing ever adopts a survivor, so every leak is
  permanent and additive. ~624 stale `cxc-*` temp dirs were standing.

## Process-shape facts (verified live, informs every filter)

| Process | argv | ppid | reaped? |
|---|---|---|---|
| Daemon-spawned server (Source A) | `node /Users/…/bin/codex app-server --listen unix://…` | 1 when orphaned | **yes** |
| Plugin broker server (Source B) | `node /…/bin/codex app-server --listen unix:///var/folders/…/cxc-…/broker.sock` | 1 when orphaned | **yes** |
| Desktop Codex/ChatGPT app | `/Applications/ChatGPT.app/Contents/Resources/codex … app-server …` | ≠ 1 | **never** |

`codex` on PATH is a node shim, so a daemon that spawns bare `codex` still shows
`node …/bin/codex app-server` in `ps args`, which is why the spec's
`grep '[b]in/codex'` filter catches Source-A orphans even though the daemon
never passes a full path. The desktop app is excluded twice over: no `bin/codex`
in its path **and** `ppid != 1`. The reap filter is therefore:
`ppid == 1` AND argv contains `bin/codex` AND `app-server`.

## Fix

Three layers, none of which depend on an exit path running:

1. **Race fix (Source A).** `socket_listener_pids()` (returns `None` when `lsof`
   cannot answer, never "nobody") + `bound_by_child()` so a race loser reaps
   its own child and adopts the winner's server instead of stranding it.
   (`codex_manager.rs`, committed separately.)
2. **Startup reaper (both sources).** `reap_orphaned_codex_servers()` runs once
   at daemon boot, *before* we spawn our own server: it reads `ps -Ao
   pid,ppid,args`, selects `ppid == 1` codex-app-server processes, spares any pid
   currently listening on our live socket (a server we may adopt), and `SIGTERM`s
   the rest by exact pid via `nix`. This is the only backstop that survives
   `SIGKILL`/OOM. It reaps **both** sources' orphans because both share the
   `bin/codex … app-server` shape. A twin Node reaper
   (`scripts/reap-codex-orphans.mjs`) runs at Claude Code `SessionStart` and also
   sweeps stale `cxc-*` temp dirs.
3. **Cap that fails loud.** `spawn()` refuses to start a server once
   `AINB_CODEX_MAX_SERVERS` (default 8) live `codex app-server` processes already
   exist, turning a silent 900-process pileup into a visible error at spawn ~9.

### Why the Node reaper is a wrapper, not a plugin fork

The `openai-codex` plugin is third-party, vendored under
`~/.claude/plugins/marketplaces/`, and is overwritten on every plugin update.
We do **not** edit it. Instead `scripts/reap-codex-orphans.mjs` is repo-owned and
registered as an *additional* user-scope `SessionStart` hook in
`~/.claude/settings.json` (installer:
`scripts/install-codex-reaper-hook.mjs`, idempotent, keeps a `.bak`). Claude Code
merges settings hooks with plugin hooks, so both fire and a plugin update cannot
erase our backstop. A proper fix (shared-socket broker, or a broker that
self-terminates on parent death) is upstream territory (logged below).

## Decision: retain the hand-rolled socket ownership (success criterion 2)

The spec offered a choice: delete `prepare_socket` / `socket_owner_marker` /
`repair_owner_marker` / `socket_disposition` / `bound_by_child` in favour of
`codex app-server daemon start|stop`, **or** retain them behind a documented
rationale. We retain them.

- `codex app-server daemon start` exists and gives single-instance for free, but
  migrating the working Fleet transport onto it is a large, risky rewrite of the
  read/write actor, proxy wiring, and cleanup, for a guarantee we already
  replicate now that `bound_by_child` closes the duplicate-spawn race.
- It would **not** remove the reaper. `codex app-server --help` exposes no idle /
  timeout / parent-death / shutdown flag, so a daemon-managed server still
  orphans on `SIGKILL`. The startup reaper is required either way, and it is the
  layer that actually satisfies "zero orphans after 20 SIGKILLs".
- The retained code is already tested (the `codex_manager` unit suite) and its
  correctness is not what caused the leak: the missing SIGKILL-independent
  backstop was. Adopting the codex daemon is filed as a follow-up, not a blocker.

## How to run / test

```bash
# Rust: reaper + cap + race fix
cargo test -p ainb-hangar-daemon
cargo clippy -p ainb-hangar-daemon --all-targets -- -D warnings
cargo fmt --check

# Node reaper
node --test scripts/reap-codex-orphans.test.mjs
node scripts/reap-codex-orphans.mjs          # safe to run anytime; exits 0

# Wire the SessionStart hook (idempotent, backs up settings.json)
node scripts/install-codex-reaper-hook.mjs --dry-run   # preview
node scripts/install-codex-reaper-hook.mjs             # apply (idempotent; upgrades in place)
node scripts/install-codex-reaper-hook.mjs --uninstall # remove (keeps a .bak)

# Soak proof (success criterion 1): 20x SIGKILL the daemon, prove no accumulation
scripts/soak-codex-orphans.sh 20

# Scale proof: reap a 25-orphan pile to 0, spare an in-use (proxied) server,
# then reap it once its session ends; desktop app untouched throughout.
scripts/reap-stress-codex-orphans.sh 25
```

The soak isolates `AINB_HANGAR_HOME`, SIGKILLs the daemon 20×, and reports the
**peak** orphan count (which stays ~1, proving `bound_by_child` + boot adoption
never accumulate; the pre-fix incident reached 900+ here), then fires the
SessionStart reaper against the now-idle machine and proves the count reaches 0
while the desktop app server stays alive.

## Why the reaper is safe: `ppid == 1` means the parent died

`codex app-server --listen unix://<sock>` does NOT self-daemonize. Its `node
.../bin/codex app-server --listen` launcher stays alive and the native listener
is that launcher's child (`ppid == launcher`), so a healthy in-use server is
never `ppid == 1`. A server reaches `ppid == 1` only when its parent launcher
dies: a SIGKILLed or crashed daemon, or spawn-cleanup killing the launcher after
an `initialize` failure. So `ppid == 1` is a sound orphan signal and neither
reaper can hit a live in-use server.

Both reapers key on `ppid == 1`. The Node SessionStart reaper adds a defensive
extra layer: it spares a `ppid == 1` server that still has a live `app-server
proxy` consumer on its socket (the transient "launcher gone but a proxy lingers"
state). The Rust boot reaper instead spares the holder of the daemon's own live
socket, so a booting daemon can *adopt* a still-listening server left by a prior
daemon rather than kill-and-respawn it.

## Known limitations / follow-ups

- **Boot-only, not periodic.** The daemon reaps at boot, not on a timer. With the
  race fixed a running daemon no longer manufactures orphans, so boot + frequent
  `SessionStart` reaping + the cap suffice. A periodic sweep is a cheap add if a
  long-lived daemon ever regresses.
- **Native grandchildren.** A reaped `node …/bin/codex` launcher may leave a
  `vendor/…/bin/codex` grandchild that reparents to init; it is caught on the
  next boot/SessionStart reap (eventual convergence).
- **User-run `codex app-server`.** The `ppid==1` heuristic cannot tell an
  ainb/plugin orphan from a `codex app-server --listen` someone runs on purpose
  (e.g. a launchd `KeepAlive` service on a foreign socket with no ainb proxy):
  both look orphaned and would be reaped. Acceptable given the incident's scope; a
  socket-location allowlist would exclude such sockets if it ever matters.
- **PID-reuse TOCTOU.** A pid can in principle be recycled between the `ps` read
  and the `SIGTERM`. The window is tiny and the signal is `SIGTERM` (not
  `SIGKILL`); the socket-owner path guards with a process-start fingerprint, the
  reaper does not re-verify argv before signalling.
- **`input_box_ready` markers.** `ainb run -p` readiness keys on the CLIs' footer
  strings ("? for shortcuts", …); a copy change degrades it to the 30 s timeout,
  which still SENDS the prompt (never drops it).
- **Upstream ask (Source B).** The real Source-B fix is a shared-socket broker or
  a broker that dies with its parent (`PR_SET_PDEATHSIG` is Linux-only; macOS
  needs a supervisor/pipe-EOF wrapper). Filed upstream against `openai-codex`;
  our SessionStart reaper is the local backstop until then.
- **`ainb run -p` fix** (separate bug, fixed alongside): the initial prompt was
  sent after a fixed 2 s sleep into a not-yet-ready Claude splash and lost; now it
  polls `capture-pane` for the input box before sending, and targets the session
  by name (no hardcoded `:0`).
