Mode: focused-query

# Reference Implementation Analysis — daemon / PTY process model

**Repo (local clone)** `/tmp/claude-1000/-home-claude--ref-agents-in-a-box-desktop-app-part2/7ec9b5c8-5403-4a49-b0ee-f6b71be9354a/scratchpad/ref`
**Commit** `9aa0f7e77d366c23a3cc8de2da32ae550d397dc0` (2026-09-11)
**Research query** How does the reference implementation run and persist agent terminal sessions without tmux, and what would we gain or lose by adopting that model instead of our tmux-based one?
**Analyzed** 2026-09-11

> **Archaeology limit** `git log --oneline | wc -l` returns `1`. This clone is a squashed
> single-commit export, so commit-message history is unavailable. Every "why" below is cited
> from in-tree code comments, module `AGENTS.md` files, or `docs/reference/*`, never from
> commit messages. [fact]

---

## 0. The shape in one picture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│ VIEWERS (N)                                                                 │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌────────────────┐  │
│  │ desktop      │  │ 2nd desktop  │  │ web client   │  │ mobile app     │  │
│  │ renderer     │  │ (remote)     │  │ (no page rnd)│  │ (E2EE)         │  │
│  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └───────┬────────┘  │
└─────────┼─────────────────┼─────────────────┼──────────────────┼───────────┘
          │  IPC            │   WebSocket / relay, binary terminal-stream     │
          ▼                 ▼                 ▼                  ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ APP / RUNTIME PROCESS  (Electron main, or `refd` = plain Node)             │
│                                                                             │
│  runtime RPC + terminal multiplexer  ── fan-out, driver floor, layout SM    │
│  DaemonPtyAdapter  (IPtyProvider)    ── THE single daemon client            │
│  HistoryManager + HistoryReader      ── writes durable scrollback to DISK   │
│      ▲ 5s checkpoint tick: takePendingOutput / getSnapshot                  │
└──────┼──────────────────────────────────────────────────────────────────────┘
       │  NDJSON over unix socket (POSIX) / named pipe (win32)
       │  TWO sockets per client: role=control  +  role=stream
       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│ TERMINAL DAEMON  ("refd" paths: daemon-entry.js)  — detached, plain Node   │
│                                                                             │
│  DaemonServer ── DaemonClientConnections ── DaemonRequestRouter             │
│       │                                                                     │
│  TerminalHost  Map<sessionId, Session>                                      │
│       │                                                                     │
│  Session ── SessionOutputPlane ── HeadlessEmulator (1000-row RAM window)    │
│       │                       └── pendingOutputRecords (2 MB cap)           │
│       ▼                                                                     │
│  SubprocessHandle ── node-pty ── THE PTY ── shell ── agent process          │
└─────────────────────────────────────────────────────────────────────────────┘
```

The headline: **there is no multiplexer.** The daemon is a plain Node process that owns
`node-pty` handles and speaks a versioned NDJSON RPC over a unix socket. Persistence is not a
server-side scrollback buffer; it is a **checkpoint + incremental log written by the app**, and
the terminal is reconstructed by replaying bytes into a headless emulator. [fact]

---

## 1. Daemon lifecycle

### 1.1 Two long-lived processes, deliberately

| | `refd` / app | terminal daemon |
|---|---|---|
| Started by | the supervisor (systemd / launchd / the user) | the app, **detached** |
| Owns | RPC, git, worktrees, persistence, history files | every local PTY |
| Lifetime | one supervised run | outlives its parent |
| Endpoint | `ws://<bind>:<port>` | `<data-root>/daemon/daemon-v<N>.sock` |

Source: `docs/reference/refd-operations.md:9-16`. [fact]

`refd` detaches the daemon and calls `disconnectDaemon()`, **never** `shutdownDaemon()`
(`docs/reference/refd-operations.md:18`, enforced at
`src/main/refd/refd-daemon-supervision.ts:61-66`). The desktop app does the same on quit:
`src/main/startup/main-process-quit.ts:227` picks `shutdownDaemon()` only when a dev parent
process died, otherwise `disconnectDaemon()`. [fact]

```
┌──────────────┐        ┌────────────────────┐        ┌─────────────────────┐
│ app quits    │──────▶ │ disconnectOnly()   │──────▶ │ daemon stays alive  │
│ (normal)     │        │ final checkpoints  │        │ PTYs keep running   │
└──────────────┘        └────────────────────┘        └─────────────────────┘
┌──────────────┐        ┌────────────────────┐        ┌─────────────────────┐
│ dev parent   │──────▶ │ shutdownDaemon()   │──────▶ │ daemon + PTYs die   │
│ died         │        │ spawner.shutdown() │        │                     │
└──────────────┘        └────────────────────┘        └─────────────────────┘
```

`disconnectDaemon` is documented as "Disconnect without killing: the daemon survives app quit
so sessions stay warm for reattach. Leave history sessions marked 'unclean' so a daemon crash
while ref is closed stays recoverable." (`src/main/daemon/daemon-provider-state.ts:124-129`). [fact]

### 1.2 Startup, singleton, and the socket-name ownership protocol

There is **no lockfile for the daemon**. The canonical socket path *is* the lock, guarded by a
link/rename protocol. The module's own `AGENTS.md` states the invariant:

> "Only a daemon publishing itself onto the canonical endpoint may mutate that directory entry,
> and only by replacing an entry it has itself just proven dead. No actor removes a name it did
> not create." — `src/main/daemon/AGENTS.md:8-11` [fact]

Publish sequence (`src/main/daemon/daemon-endpoint-ownership.ts:63-111`):

```
bind private ".p<hex>"  ──▶ link() to canonical
      │                          │ EEXIST
      │                          ▼
      │                  prove incumbent dead by connect()
      │                          │ connected → OCCUPIED (adopt, never fork)
      │                          │ not-proven-dead → INCONCLUSIVE (leave alone)
      │                          ▼ proven dead
      │                  re-check entry didn't change hands (dev+ino)
      │                          ▼
      │                  probe ONCE MORE
      │                          ▼
      │                  rename() in ONE syscall
      ▼                          ▼
   confirmPublishedEndpoint(): stat canonical, compare dev+ino → published | lost
```

Hard-won constraints recorded in `src/main/daemon/AGENTS.md:24-46`:

| Trap | Rule | Cite |
|---|---|---|
| "can't tell" ≠ "dead" | only `connected` proves occupied; only `refused`/`missing` prove death; timeout/`EPERM` must decline | `AGENTS.md:26-31`, `daemon-endpoint-ownership.ts:143-146` |
| `link` first, never bare `rename` | `rename` replaces whatever it finds | `AGENTS.md:29-31`, `daemon-endpoint-ownership.ts:81` |
| `rename`, not unlink-then-link | unlink+link gapped on "essentially every observation"; `rename` gapped on none in ~14,500 probes | `AGENTS.md:32-34` |
| Never identify by `birthtimeMs` | Node may report ctime; some filesystems report epoch | `AGENTS.md:35-38` |
| **Do not add a sweeper** | "the last one produced five defects, including deleting a live listener's only pathname" | `AGENTS.md:39-41` |
| Never remove the endpoint on shutdown | a departing daemon leaves a dead entry; the next publisher replaces it | `AGENTS.md:45-46`, `daemon-endpoint-lifecycle.ts:97` |

Residual risk is named: the final probe and the `rename` are two syscalls and POSIX has no
rename-if-target-is-inode-X; the harm is separately unreachable because a daemon never creates a
session on an endpoint it no longer holds (`AGENTS.md:48-50`). [fact]

Windows takes a different path entirely: named pipes are exclusive by name and vanish with the
process, so `publishDaemonEndpoint` short-circuits to `published` and `listen` is the whole
protocol (`daemon-endpoint-ownership.ts:68-71`). Pipe name embeds a hash of the runtime dir plus
the protocol version (`daemon-spawner.ts:131-134`). [fact]

An **ownership watchdog** polls every 30s and needs 2 consecutive confirmations before retiring
(`daemon-endpoint-lifecycle.ts:27-28, 136-158`). A daemon that loses ownership drains rather than
serving on: `hasLostOwnership()` refuses new non-attach creates
(`daemon-terminal-admission.ts:62-65`). [fact]

### 1.3 Launch and adoption

`DaemonSpawner.ensureRunning()` (`daemon-spawner.ts:72-90`) funnels every launch — first,
post-death, post-restart — through one crash-loop admission gate, "the only place a crash loop
cannot route around". Budget: **5 launches per 60s rolling window per app run**, past which
launches fail with `daemon_crash_loop` (`docs/reference/refd-operations.md:139-143`). An
operator-initiated daemon restart clears it via `resetRespawnWindow()`
(`daemon-spawner.ts:110-112`); a successful fork does not, because "a crash loop is a run of
successful forks whose daemons then die". [fact]

`createOutOfProcessLauncher` (`daemon-out-of-process-launcher.ts:55-221`) runs:

1. Connect to the existing endpoint and reconcile the PID record (`:79-80`).
2. `prepareDaemonReplacement(...)` decides adopt-vs-replace (`:104-113`).
3. Otherwise fork `daemon-entry.js` detached (`:125-135`).
4. Special case: if the child exits with `DAEMON_EXIT_ENDPOINT_OCCUPIED` (20), **adopt** the
   incumbent — "Forking again would lose the same race, and reporting a startup failure strands
   this app on local non-persistent PTYs beside a healthy daemon" (`:137-157`). The exit code
   rather than an IPC message is used because the launcher settles on process exit and a message
   can lose that race (`daemon-endpoint-ownership.ts:36-40`).
5. Last resort **degraded mode**: existing sessions keep routing to the daemon, fresh terminals
   run on the in-process local provider *without persistence*, with an explicit user-facing
   warning (`:208-217`, class `DegradedDaemonPtyProvider`). [fact]

Adoption policy: a daemon already answering is adopted, not replaced, unless it is unhealthy,
foreign, or built from a superseded bundle **and owns no live sessions** — "Replacing a healthy
daemon kills its PTYs, so code freshness always defers to live work"
(`docs/reference/refd-operations.md:133-137`). The three stale-daemon replacement paths all
check the live session count first and bail if it is nonzero or unverifiable
(`daemon-pty-daemon-recovery.ts:119-155, 157-209, 211-249`). [fact]

### 1.4 Self-retirement (the anti-leak rules)

| Condition | Behavior | Cite |
|---|---|---|
| Never adopted within 2 min of publish | retires itself | `daemon-server.ts:33`, `daemon-server-lifecycle.ts:186-198` |
| Last authenticated client disconnects | marks retirement requested; retires **once idle** (no sessions, no in-flight creates) | `daemon-server-lifecycle.ts:106-110`, `daemon-server.ts:226-234` |
| Endpoint ownership lost | retirement requested, drains | `daemon-server-lifecycle.ts:112-115` |
| SIGTERM / SIGINT | graceful, bounded by `SHUTDOWN_TIMEOUT_MS = 5000` then `process.exit(0)` | `daemon-entry.ts:168, 170-198` |

Idle shutdown only fires when `isIdle()` is true, and `isIdle()` is false while any session
exists. So a daemon holding live agents does **not** exit when the last viewer leaves. [fact]

### 1.5 Crash resilience inside the daemon

`daemon-entry.ts:151-161` installs a selective `uncaughtException` handler:

```ts
process.on('uncaughtException', (err) => {
  if (isNativePtyException(err)) {  // node-pty C++ Napi::Error escapes JS try/catch
    daemonLog.log('uncaught-exception-suppressed', ...)
    return                          // the individual PTY is already dead; daemon healthy
  }
  daemonLog.log('uncaught-exception-fatal', ...)
  throw err                          // logic bugs still crash — masking would hide real issues
})
```

Also `process.stderr.on('error', () => {})` at `daemon-entry.ts:111`, because the parent destroys
its end of the startup stderr pipe and a later write would emit an unhandled `error` that "would
kill an otherwise healthy detached daemon". [fact]

### 1.6 Headless Linux server

Two distinct headless entry points, and the distinction matters:

| | `ref serve` | `refd` |
|---|---|---|
| Runtime | Electron — **needs Xvfb** and ~20 GTK/X libs | plain Node |
| Display | auto-starts Xvfb on `:99` when no `DISPLAY`; refuses a `DISPLAY` whose X lock names a dead process | n/a |
| Cite | `docs/reference/headless-linux-server.md:6-15, 22-36, 201-202` | `docs/reference/refd-operations.md:2-3` |

The **headless survival hole is documented, not solved**:

> "Process detachment is not service isolation. A daemon forked by refd, and every PTY it owns,
> remain in the same systemd service cgroup. `KillMode=mixed` does **not** preserve them... 
> `KillMode=control-group` is destructive too. `KillMode=process` leaves service-owned processes
> unmanaged and is not a supported preservation mechanism. Service-restart survival requires
> separately supervised cgroups; the current deployment does not provide them."
> — `docs/reference/refd-operations.md:24-30` [fact]

And restated for `serve`: "Every `systemctl stop` or `restart` therefore ends live terminals and
agent processes, even though their persisted layout and terminal history remain."
(`docs/reference/headless-linux-server.md:236-239`). [fact]

So the survival guarantee is **PID-scoped, not service-scoped**: a PID-scoped update/rollback/
restart of the runtime is non-destructive; a unit-level `systemctl restart` is not.
`docs/reference/refd-operations.md:83-98` prescribes a pre-stop census (`ref-ide terminal list
--json`) whose result must be untruncated, host-scoped, and account for every `omittedHostIds`
entry, else the stop is deferred — and admits "ref does not yet provide an atomic
census-and-stop fence." [fact]

Other headless facts:
- Bind default is `127.0.0.1`; only literal IPs accepted (DNS would decide the interface); `0.0.0.0`/`::` are explicit opt-ins logged on every launch. The bind is **pinned**, and a mobile pairing offer that wants to rebind wide is refused with `network_exposure_failed` (`refd-operations.md:34-45`). [fact]
- Data root `$ref_USER_DATA` → `$XDG_DATA_HOME/ref` → `~/.ref`; `<data-root>/refd.lock` is the **runtime** instance lock and deliberately says nothing about the daemon, "A lock that asked 'is any process using this root' would refuse exactly the restarts a live daemon makes worthwhile" (`refd-operations.md:52-77`). [fact]
- Exit codes: `0` clean, `1` failure, `78` (`EX_CONFIG`) config fault that must go in `RestartPreventExitStatus` (`refd-operations.md:113-122`). [fact]
- **No macOS login-session watch under `refd`** — that watch retires the daemon when the spawning GUI login session dies, and "An refd daemon must survive its SSH session ending" (`refd-operations.md:144-145`; the flag is `--login-session-watch`, `daemon-entry.ts:38, 80-82, 212-214`). [fact]
- Daemon log `<data-root>/logs/daemon.log`, NDJSON, **rotation not implemented, grows unbounded** (`refd-operations.md:124-128, 215-216`). [fact]
- Readiness is one JSON line on stdout, `type: "ref_server_ready"`. There is **no continuous health endpoint** — `health` is published once (`refd-operations.md:104-107, 199-203`). [fact]
- The readiness self-test is a real cross-process PTY spawn inside the daemon (`coverage: 'pty-spawn'`); on win32 it degrades to `coverage: 'handshake'` and is reported separately "rather than folded into `ok` so nobody reads it as a PTY round trip" (`refd-operations.md:180-196`). [fact]

---

## 2. PTY ownership and scrollback persistence

### 2.1 Who owns the PTY

The daemon, exclusively. `spawnSubprocess` is injected at
`daemon-entry.ts:288-297` → `createPtySubprocess` → `spawnNativeDaemonPty` →
`pty.spawn(...)` (`pty-subprocess/native-pty-spawn.ts:36-44`). The `node-pty` handle lives only
in the daemon process. The app never holds a PTY fd for a daemon-owned session. [fact]

`pty-subprocess/` is a **module boundary, not a process boundary.** Files there are
`foreground-process-tracker.ts`, `native-pty-spawn.ts`, `shell-launch-plan.ts`,
`spawn-environment.ts`, `spawn-preflight.ts`, `subprocess-handle.ts` — all imported in-process by
`pty-subprocess.ts:1-15`. There is no per-PTY helper process. "pty-subprocess" means "the
subprocess behind the PTY", i.e. the shell. [inference, from the import graph; no spawn of a
helper exists in these files]

Platform details worth noting: win32 uses `useConptyDll: true` because "bundled ConPTY has the
wrap-marker behavior xterm expects" (`native-pty-spawn.ts:42-43`); win32 also assigns the host
process to a kill-on-close Job object **before the first PTY** so children inherit membership
(`:32-34`); and macOS wraps the shell spawn for TCC attribution (`:31`), with
`reportsChildExitStatus: false` when a wrapper owns the status (`native-pty-spawn.ts:14-15`). [fact]

### 2.2 The three layers of scrollback

```
┌────────────────────────────────────────────────────────────────────────────┐
│ DAEMON, in RAM (per Session)                                               │
│  HeadlessEmulator      scrollback = 1000 rows  (flat window)               │
│  pendingOutputRecords  cap 2 MB  → on overflow: DROP ALL + set overflowed  │
└────────────────────────────────────────────────────────────────────────────┘
                │ takePendingOutput RPC, pulled by the APP every 5s
                ▼
┌────────────────────────────────────────────────────────────────────────────┐
│ APP, on disk  <historyPath>/<encodeURIComponent(sessionId)>/               │
│  meta.json        cwd, cols, rows, startedAt, endedAt, exitCode   ≤ 64 KB  │
│  output.log       incremental framed batch log                    ≤ 5 MB   │
│  checkpoint.json  snapshotAnsi + scrollbackAnsi (full serialize)  ≤ 200 MB │
└────────────────────────────────────────────────────────────────────────────┘
                │ replay on cold restore
                ▼
┌────────────────────────────────────────────────────────────────────────────┐
│ REBUILD depth = DESKTOP_TERMINAL_SCROLLBACK_ROWS_DEFAULT (not 1000)        │
└────────────────────────────────────────────────────────────────────────────┘
```

| Cap | Value | Cite |
|---|---|---|
| Daemon live emulator window | **1000 rows**, env-overridable 100–5000 | `daemon-session-scrollback-window.ts:8-23` |
| Daemon pending-record buffer | **2 MB** UTF-16 units | `session-output-plane.ts:13` |
| Pending output coalescing | append into last record while `< 64 KB` | `session-output-plane.ts:229-235` |
| `meta.json` | 64 KB | `terminal-history-file-limits.ts:1` |
| `output.log` | **5 MB**, then force a full checkpoint | `terminal-history-file-limits.ts:2` |
| `checkpoint.json` | **200 MB**, oversized snapshots trim oldest rows before commit | `terminal-history-file-limits.ts:3-4` |
| Legacy scrollback file | 16 MB | `terminal-history-file-limits.ts:5` |
| Restore depth | desktop default (larger than the live window) | `daemon-restore-scrollback-depth.ts:1-5` |
| NDJSON max line | 16 MB (referenced as the wire ceiling) | `session-output-plane.ts:12-13` |

The 1000-row flat window has a named failure behind it:

> "retained grid is the daemon's dominant heap term and session count is unbounded — a host owning
> 100+ terminals at full depth retained ~1 GB of grid and was OOM-killed, taking every session it
> owned with it." — `daemon-session-scrollback-window.ts:3-7` [fact]

So the answer to "ring buffer or file?" is: **a small RAM window in the daemon plus a
checkpoint-and-log pair on disk owned by the app.** Not a ring buffer. [fact]

### 2.3 Who writes the disk history — the app, not the daemon

`HistoryManager` and `HistoryReader` are constructed in `DaemonPtyRuntimeState`, the **client-side**
adapter that implements `IPtyProvider` (`daemon-pty-runtime-state.ts:222-223`; class chain
`DaemonPtyAdapter → DaemonPtyDaemonRecovery → DaemonPtyCheckpointPersistence →
DaemonPtyCheckpointScheduler → DaemonPtyConnectionLifecycle → DaemonPtyEventSubscriptions →
DaemonPtyRuntimeState`). The daemon holds bytes; the app pulls and persists them. [fact]

Checkpoint loop (`daemon-pty-checkpoint-scheduler.ts`, constants at
`daemon-pty-runtime-state.ts:152-156`):

| Knob | Value |
|---|---|
| `CHECKPOINT_INTERVAL_MS` | 5 000 |
| `PERIODIC_CHECKPOINT_DEADLINE_MS` | 15 000 |
| `MAX_CONCURRENT_CHECKPOINTS` | 4 |
| `FULL_CHECKPOINT_COOLDOWN_MS` | 45 000 |

The timer is **dirty-gated**: "a permanent 5s interval woke the main process for idle terminals
with nothing to write" (`daemon-pty-checkpoint-scheduler.ts:28`). Sessions are marked dirty by
data events (`daemon-pty-adapter.ts:18`). Dirty-version filtering avoids re-serializing idle
sessions without dropping mid-checkpoint writes (`:61-82`). Passes never overlap (`:32`).

Incremental vs full (`daemon-pty-checkpoint-persistence.ts:9-90`):

```
5s tick ──▶ takePendingOutput  ──▶ appendIncrements(seq, records) to output.log
                 │                          │ 'needs-checkpoint' (5 MB cap)
                 │ overflowed = true        ▼
                 └──────────────────▶ full snapshot + reset log
```

A full serialize happens only on: clean disconnect, pending-buffer overflow, log cap, or an
explicit request (`history-manager.ts:221`; rationale for going incremental at
`types.ts:259-264` — "the 5s checkpoint used to re-serialize the full emulator buffer per tick,
stalling the daemon's PTY pump for O(buffer)"). [fact]

### 2.4 On-disk framing and torn-tail detection

`output.log` format (`terminal-history-log.ts:3-26`):

```
header : 'OCKL' (4B) | u8 formatVersion | u32le generation          = 9 bytes
frame  : u8 kind | u32le payloadLength | payload
         0x01 batch  = u32le seq
         0x02 output = utf8 bytes
         0x03 resize = u16le cols | u16le rows
         0x04 clear  = empty
```

Two integrity mechanisms, both explicitly justified:
- **Length prefixes** so a crash-torn final append is detectable and restore truncates at the last
  complete frame "instead of replaying half an escape sequence ('reading a corrupt checkpoint is
  worse than reading a slightly stale one')" (`terminal-history-log.ts:13-16`; `truncatedTail`
  flag at `:36-38, 96-108`).
- **Seq-gap detection**: a non-contiguous batch seq means a batch was lost (main crashed between
  take and append), so the byte stream has a hole and the whole log is rejected rather than
  replayed (`terminal-history-log.ts:80-84, 115-117`). [fact]

`checkpoint.json` is written tmp+rename: "tmp+rename is atomic (corrupt checkpoint > stale); async
so a sync ~MB write can't stall IPC (worse under Windows AV)"
(`history-manager.ts:244-246`). A per-session checkpoint queue prevents concurrent writes
colliding on the fixed `.tmp` path (`:245`, `CheckpointSessionQueue`). [fact]

History files are mode-hardened because they hold verbatim screen content and the runtime dir
holds the auth token: `PRIVATE_DIR_MODE = 0o700`, `PRIVATE_FILE_MODE = 0o600`, with best-effort
`chmod` repair for paths created by older daemons (`daemon-private-file-modes.ts:1-33`). [fact]

Session dir name is `encodeURIComponent(sessionId)` because real session ids embed worktree
identity and can contain `:` and `/`, invalid in a Windows path segment (`history-paths.ts:1-7`). [fact]

There is a **recovery-freeze / quarantine** layer so a restore in progress cannot be overwritten by
the next checkpoint: `freezeForRecovery` takes a fingerprint, `openSession`/`registerWriter`
refuse if the fingerprint changed (`terminal_history_recovery_generation_changed`) or if a
protection marker exists (`terminal_history_recovery_protected`)
(`history-manager.ts:58-102, 104-126, 136-166`). Deleting history is tombstone-then-reclaim because
trees "reach hundreds of MB" (`:274-282`). [fact]

### 2.5 Warm reattach vs cold restore

```
                      app reconnects to a LIVE daemon session
                                     │
              ┌──────────────────────┴───────────────────────┐
              ▼ WARM                                         ▼ COLD
  createOrAttach → existing.getSnapshot()          daemon session gone / daemon replaced
  → existing.detachAllClients()                    → HistoryReader.detectColdRestore()
  → existing.attachClient(streamClient)            → replay checkpoint + log batches
  → { isNew: false, snapshot }                       into a HeadlessEmulator
  terminal-host-session-create.ts:68-81              via ColdRestoreReplayWriter
```

Warm path returns the daemon's live emulator snapshot synchronously; the comment at
`terminal-host-session-create.ts:61-67` explains why it does **not** wait for shell-ownership
settle: "attach is synchronous by contract... Waiting instead created a race window (cancel, exit,
kill during the await) that produced repeated regressions." A viewer that lands inside an in-flight
recovery proof gets the pre-reset snapshot and the injected reset arrives in-order on its stream —
"a self-healing first frame." [fact]

Cold replay is **budgeted and yields**, so a 200 MB checkpoint cannot block the event loop:
64 KB chars or 1024 operations per turn, then `setImmediate` (`cold-restore-replay-writer.ts:3-4,
61-72`). It also refuses to split a UTF-16 surrogate pair across a slice (`:22-31`). [fact]

A deliberate crash-vs-clean distinction drives which path is taken. The macOS login-session death
watch exits with **code 1 and no PTY teardown** specifically so session meta stays unclean and the
replacement daemon cold-restores scrollback (`daemon-entry.ts:249-253`). Conversely
`HistoryManager.dispose()` stamps `endedAt` on open sessions so they "don't trigger false
cold-restores next launch" (`history-manager.ts:304-315`), and a failure to write `endedAt` is
called out as "an unwritten endedAt looks like an unclean shutdown → false cold restore next
launch" (`:269`). [fact]

`TerminalHost.dispose()` force-kills live sessions and does **not** fan out `onExit`, because "the
renderer reconnects cold after daemon exit" (`session.ts:335-336`). [fact]

### 2.6 Resize policy

At the daemon: last write wins, unconditionally, with clamping.
`Session.resize` (`session.ts:162-168`) drops the call if exited/disposed or
`!isValidPtySize(cols, rows)`, then applies to emulator **then** PTY; the emulator apply order is
mirrored into the record stream "or cold-restore replay reflows at the wrong point"
(`session-output-plane.ts:110-112`). There is no per-viewer size negotiation and no
smallest-common-denominator. [fact]

Resize is a **fire-and-forget notify**, not an RPC (`types.ts:122-130` + `NOTIFY_PREFIX` at
`:396-399`). To close the resulting blind spot, `getSize` returns the size the PTY actually
applied so the renderer can diff it against xterm and re-assert a dropped/coerced resize
(`types.ts:248-257`, reader at `daemon-pty-applied-size.ts:23-87`). `getSize` shipped without a
protocol bump, so the reader catches `isUnknownRequestTypeError`, caches the negative capability,
and falls back to reading `cols`/`rows` out of `listSessions` (`daemon-pty-applied-size.ts:69-81`). [fact]

Multi-viewer resize arbitration is **entirely above the daemon** — see §3. [fact]

---

## 3. Multi-viewer

### 3.1 At the daemon: one viewer per PTY, last attach wins

This is the single most important structural fact for our comparison.

`terminal-host-session-create.ts:68-81`:

```ts
if (existing && existing.isAlive && !existing.isTerminating) {
  const snapshot = existing.getSnapshot()
  existing.detachAllClients()          // ◀── STEALS the session
  const token = existing.attachClient(opts.streamClient)
  return { isNew: false, snapshot, ... }
}
```

And `DaemonSessionAttachments` keys by session, holding exactly one client id and one token:

```ts
private readonly clientIdBySessionId = new Map<string, string>()
private readonly tokenBySessionId    = new Map<string, symbol>()
```
(`daemon-session-attachments.ts:5-14`)

`SessionOutputPlane.attachedClients` is an array and `broadcastExit` loops over it
(`session-output-plane.ts:38, 77-106`), so the *mechanism* supports N — but the only caller
detaches all first. **Two daemon clients cannot concurrently view one PTY; the second evicts the
first.** [fact]

Corroborating: `DaemonClientConnections.installControlSocket` destroys a previous connection with
the same `clientId` (`daemon-client-connections.ts:178-195`), and each `DaemonClient` mints one
`clientId = randomUUID()` per instance (`client.ts:43`). [fact]

### 3.2 Above the daemon: fan-out plus a driver floor

```
       daemon: 1 stream ──▶ DaemonPtyAdapter (the only daemon client)
                                  │ dataListeners fan-out
                ┌─────────────────┼─────────────────┬──────────────────┐
                ▼                 ▼                 ▼                  ▼
        local renderer     remote desktop      web client         mobile
                                  │                                   │
                     RemoteDesktopTerminalFloor            RuntimeTerminalDriverController
                     (resize owner per subscription)       (input floor: idle | mobile)
```

`DaemonPtyAdapter` fans data out to `dataListeners` / `exitListeners` /
`backgroundStreamListeners` (`daemon-pty-adapter.ts:17-30, 66-109`;
arrays at `daemon-pty-runtime-state.ts:100-114`). Everything multi-viewer lives in the runtime. [fact]

**Input contention — the "driver" floor.** Per-PTY state `idle | mobile{clientId}`:

> "The 'driver' is whoever currently owns the input/resize floor. While `kind === 'mobile'` the
> desktop renderer drops xterm.onData/onResize and shows the lock banner; `terminal.send` /
> `pty:write` and `pty:resize` IPC handlers also drop desktop-side calls server-side as
> defense-in-depth." — `ref-runtime-terminal-drivers.ts:20-27` [fact]

The claim protocol is generation-counted with commit/rollback so concurrent phone writers cannot
interleave into a wrong final owner (`runtime-terminal-driver-controller.ts:81-149`). A write
reserves the floor before the PTY write and commits after
(`terminal/terminal-input-delivery.ts:121-135`). Enforcement is checked at
`isTerminalInputLockedForClient` (`:15-28`), which treats a **missing** client as legacy mobile and
leaves it unlocked — a deliberate back-compat hole. [fact]

There is a 250 ms **resubscribe-grace window** so a mobile client that tears its stream down to
reconfigure does not flash the desktop lock banner or capture an already-phone-fitted size as its
restore baseline (`ref-runtime-terminal-drivers.ts:104-131`). [fact]

**Resize contention — subscription-scoped ownership.** `RemoteDesktopTerminalFloor` keys viewers by
*subscription*, not client, "so duplicate streams release independently"
(`remote-desktop-terminal-floor.ts:17-18`). One owner drives the PTY size; others observe. An
activity counter breaks ties, and when the owner unsubscribes the highest-activity remaining
viewer inherits (`:187-219`). A `hostReclaimTargets` map preserves true host geometry so a failed
or superseded reclaim can retry (`:61-79, 88-115`). Stream attachment observes geometry **without**
taking control; a later `ClaimViewport` frame makes it authoritative
(`terminal/terminal-viewport-update.ts:19-20`). [fact]

Serialization: a per-PTY async layout queue, because "two concurrent triggers can interleave around
the `ptyController.resize` await and bump seq in the wrong order, defeating seq-as-truth". It
coalesces same-kind same-owner viewport ticks so a keyboard show/hide animation does not queue 10+
resizes, but mode flips, take-floor, and different-owner targets always append "(preserves
multi-mobile fairness)" (`ref-runtime-terminal-drivers.ts:186-195`). A monotonic `seq` rides the
mobile stream so clients drop stale events (`:161-178`). [fact]

> **Unverifiable** `docs/mobile-presence-lock.md` and `docs/mobile-terminal-layout-state-machine.md`
> are referenced from these comments but **do not exist** in this checkout (`docs/*.md` contains only
> `STYLEGUIDE.md` and `agent-skill-sharing-implementation-checklist.md`). The design intent is only
> readable from code comments here. [fact]

### 3.3 Known multi-viewer hazards the reference implementation admits

`docs/reference/remote-wire-compatibility.md:228-276` documents that for a **client-placed** browser
page, the host publishes `title`/`url`/`loading`/`canGoBack`/`canGoForward` from a copy it can only
learn second-hand, starting at registry defaults, and "while those publishes are failing it never
leaves them." Only the client whose guest actually runs the page refuses those fields; every other
viewer keeps tracking the host, "which is the only reason a mirrored viewer shows anything but its
first snapshot forever." That is a browser-tab surface, not the terminal, but it is the clearest
in-tree statement of the cost of one-authoritative-host fan-out. [fact]

---

## 4. Wire protocol (app ↔ daemon)

```
  ┌────────── control socket (role='control') ──────────┐
  │  hello → RPC requests → responses                    │
  │  notify_* ids = fire-and-forget, no response          │
  └───────────────────────────────────────────────────────┘
  ┌────────── stream socket  (role='stream')  ───────────┐
  │  hello → server-push events: data | exit | dataGap    │
  │          transientFact | sessionBackgroundMarker      │
  └───────────────────────────────────────────────────────┘
```

| Dimension | Answer | Cite |
|---|---|---|
| Transport | unix domain socket; **named pipe on win32** | `daemon-spawner.ts:124-136`, `daemon-client-socket-connect.ts:6` |
| Framing | **NDJSON** (newline-delimited JSON), `NDJSON_MAX_LINE_BYTES` = 16 MB | `ndjson.ts:1-7`, `session-output-plane.ts:12-13` |
| Sockets per client | **two**, sequential: control then stream | `client.ts:147-175` |
| Auth | shared secret in `<runtime>/daemon-v<N>.token`, mode `0o600`, minted fresh per daemon (`randomUUID()`) | `daemon-server.ts:103`, `daemon-endpoint-lifecycle.ts:77` |
| Handshake | `{type:'hello', version, token, clientId, role}`; server checks type, then **exact** version equality, then token, then role | `daemon-client-connections.ts:106-136` |
| Version | `PROTOCOL_VERSION = 36`; `PREVIOUS_DAEMON_PROTOCOL_VERSIONS = [1..35]` all still reachable via legacy adapters | `daemon-protocol-version.ts:3, 31-34` |
| Identity | daemon returns `{pid, startedAtMs, launchNonce, entryPath?, appVersion?, spawnerExecPath?}` | `daemon-client-connections.ts:139-157` |
| Socket mode | bind path `chmod 0o600` immediately after listen | `daemon-endpoint-lifecycle.ts:42-48` |

Notable protocol properties:

- **Exact-match version, with N-1 compatibility elsewhere.** The socket path itself embeds
  `daemon-v<N>`, so an older daemon is never reused after a breaking change
  (`daemon-spawner.ts:127-135`). The app keeps legacy adapters for every prior version because
  "daemons survive app updates, so wire behavior must be version-gated"
  (`daemon-protocol-version.ts:1-2`) — a `DaemonPtyRouter` routes per-session to the right
  adapter generation (`daemon-provider-state.ts:103-116`). [fact]
- **Identity cross-check.** The two sockets' hello responses must report the same daemon or the
  connect fails with "Daemon identity changed during connection" (`client.ts:168-170`) — this is
  what catches a daemon swap mid-handshake. [fact]
- **UTF-8 safety.** A `StringDecoder` per socket, because "daemon socket chunks can split
  emoji/box-drawing UTF-8 bytes. Decoding each Buffer independently would permanently inject
  U+FFFD" (`daemon-client-hello-handshake.ts:54-56`; same on the server,
  `daemon-client-connections.ts:64, 198`). [fact]
- **Handshake timeout.** "a stale daemon can accept the socket but never answer hello; without a
  handshake timeout, startup waits forever" (`daemon-client-hello-handshake.ts:91-92`).
  `CONNECT_TIMEOUT_MS = 5000`, `REQUEST_TIMEOUT_MS = 30000`,
  `NOTIFY_SETTLEMENT_TIMEOUT_MS = 5000` (`client.ts:28-31`). [fact]
- **Backpressure.** Output goes through `DaemonStreamDataBatcher`; the daemon flushes on socket
  `drain` (`daemon-client-connections.ts:233`). Interactive echo bypasses batching: `≤ 1024` chars
  arriving within `100 ms` of the last input flush immediately
  (`daemon-terminal-admission.ts:42-43, 176-190`). Backgrounded sessions become **droppable**, with
  a salvage pass that still extracts query replies out of dropped bytes
  (`daemon-server.ts:76-88`), and dropped volume is reported to the client as a `dataGap` event
  carrying `droppedChars` (`daemon-pty-adapter.ts:43-51`). [fact]
- **Flow control.** `pausePty`/`resumePty` stop reading the PTY fd so a flooding child blocks on
  write. They are notifies, not RPCs — "pause/resume ride the hot data path and are best-effort —
  the daemon-side 5s failsafe, not an RPC reply, is what guarantees a paused shell can never stay
  wedged" (`types.ts:132-150`). Detaching the last client eagerly resumes, since nobody would send
  the resume (`session.ts:211-217`). [fact]

### 4.1 Reconnect / resync

| Mechanism | Behavior | Cite |
|---|---|---|
| Connection generation | each `doConnect` bumps a generation; stale `close` events from old sockets are ignored, so "that event would tear down the fresh connection" | `client.ts:49-53, 324-326` |
| Connect lock | one `connectingPromise`; concurrent pane mounts would otherwise each start a connection and overwrite sockets | `client.ts:54-57, 101-106` |
| Respawn on death | `withDaemonRetry` → `doRespawn` → reconnect → retry the operation once | `daemon-pty-daemon-recovery.ts:17-45` |
| Write-failure recovery | a rejected PTY write fans `WriteUnavailable` to **every** active session first, "so background panes remount + re-attach alongside the one that was written, instead of being left frozen with silently dropped input until each is typed into" | `daemon-pty-daemon-recovery.ts:47-82` |
| Post-reconnect resync | re-notify `setSessionBackground` for every backgrounded id (daemon-side process state died with the old daemon) and flush owed `resumePty` | `daemon-pty-connection-lifecycle.ts:30-38, 144-160` |
| Adoption lease | a respawn launcher holds a temporary connection pair until the adapter's permanent reconnect, "preventing both gaps and leaks" | `daemon-pty-connection-lifecycle.ts:26-27` |

### 4.2 The outer wire (client ↔ runtime) — three rules

`docs/reference/remote-wire-compatibility.md` is the contract for the **mobile/remote** hop, and
its premise is "**Mixed versions are the normal state**, not an edge case" (`:5-7`).

| Rule | Statement | Cite |
|---|---|---|
| 1 | A new **optional JSON field** is safe — decoders `.strip()` unknown keys. Safe only while every reader treats it as optional | `:12-30` |
| 2 | A new **stream opcode is NOT safe**. `decodeTerminalStreamFrame` returns `null` and the frame is silently dropped, so "the feature behind it appears to hang." Must be negotiated in the `Subscribe` handshake and echoed in `capabilities` | `:32-59` |
| 3 | **Changing what the host publishes** breaks old clients with no wire change, because clients react to frame *content* | `:61-77` |

Terminal stream framing is binary, 16-byte header: kind `0x74`, version `1`, u8 opcode, u32le
streamId, u64le seq (`src/shared/terminal-stream-protocol.ts:3-5, 45-58`). Opcode numbers are
permanent even after the feature is removed — `Ack = 13` renumbered around a `Metadata = 12` that
already shipped to mobile, and `ClaimViewport = 14` sits above it
(`terminal-stream-protocol.ts:25-30`). `RUNTIME_PROTOCOL_VERSION = 3` with a compatibility window
of `MIN_COMPATIBLE_* = 2` on both sides (`src/shared/protocol-version.ts:32-34`). [fact]

Enforcement is a real cross-version harness: it checks out the newest release tag and runs the real
host RPC methods against the real renderer multiplexer **in both skew directions** over one scripted
journey (subscribe, input, hide/reveal snapshot, drop, reconnect)
(`remote-wire-compatibility.md:78-98`). Its own governing rule is "Never write down what the old
side has" — a `not.toHaveProperty` rots on the next release cut and "trains people to ignore it"
(`:100-135`). Coverage gaps are named: the harness does **not** cover the session-tab sync channel,
file/Git RPCs, mobile/E2EE framing, or the relay transport (`:157-160`). [fact]

Three-state optionality is worth stealing: `agentWait` distinguishes **present object** (waiting,
with evidence), **present null** (evaluated, nothing proves a wait), and **absent** (never
evaluated). "absence must read as _unknown_ and never as _not waiting_. Collapsing absent into
`null` at any hop — including a convenience `?? null` in an RPC handler — makes an old or
unreachable peer indistinguishable from a healthy idle worker" (`:162-177`). [fact]

---

## 5. Supervision of the agent child

### 5.1 Crash detection and exit

Exit flows `node-pty onExit` → `SubprocessHandle.onExit` → `Session.handleSubprocessExit`
(`session.ts:95, 357-390`) → `broadcastExit` to attached clients
(`session-output-plane.ts:99-106`) → `onSessionExit` reaper
(`terminal-host.ts:126-139, 197-205`) → daemon emits an `exit` stream event
(`daemon-terminal-admission.ts:192-213`).

Teardown ordering is heavily annotated, and the annotations are the value:

- `_exitCode` / `_state = 'exited'` are set **before** `broadcastExit` (`session.ts:372-374`).
- The ptmx fd is released at exit, not at dispose, "or node-pty's `_socket` leaks the master fd
  until GC" (`session.ts:382-384`).
- Dead sessions are reaped and their emulator disposed "so exited terminals don't pin their
  scrollback window for the daemon's life" (`terminal-host.ts:196-205`).
- `disposeSubprocess()` is a separate fd-release-only path for already-exited sessions that skips
  SIGKILL, "Separate method because a reaped pid is eligible for POSIX reuse, so SIGKILL could
  otherwise hit an unrelated process" (`session.ts:325-331`). [fact]

Exit **cause** is a first-class field, not just a code: `TerminalExitCause` is resolved and attached
to the broadcast, with a fallback so "a handle that predates exit causes still reports a code, and
every client deserves the same shape" (`session-output-plane.ts:99-106`). `incarnationId` (a
per-Session `randomUUID()`, `session.ts:27`) fences races: the adapter ignores an `exit` whose
incarnation is not the generation it currently publishes (`daemon-pty-adapter.ts:86-99`), and
`retiredIncarnations` keeps a 2 s tombstone so a just-exited session can still answer process
inspection (`terminal-host.ts:45, 128-135, 238-249`). [fact]

### 5.2 Restart

**The daemon does not restart agent children.** There is no supervisor loop over the shell.

What *is* restarted is the **daemon** (`DaemonPtyDaemonRecovery.doRespawn`,
`daemon-pty-daemon-recovery.ts:268-283`; "The adapter respawns the daemon on death, transparently
to callers", `docs/reference/refd-operations.md:138`). A restarted daemon starts with **no
sessions**; the app then cold-restores each pane's scrollback from disk and spawns a fresh child.
So "restart" means "restore the transcript", not "resume the process". [fact]

The one respawn-shaped thing for children is the tmux-shim's `respawn-pane`, which closes and
re-splits because "ref panes are PTYs that cannot swap their program in place"
(`claude-agent-teams-tmux-dispatcher.ts:140-145`) — see §6. [fact]

Kill is a two-stage controller: graceful signal, then a force-kill deadline
(`session-termination-controller.ts`, `IMMEDIATE_KILL_PHYSICAL_EXIT_TIMEOUT_MS`,
`session.ts:8-10, 197-201`). `beginTermination()` claims termination **synchronously** "so
attach/re-entry cannot race async teardown preparation" (`session.ts:128-131`). A create racing a
teardown waits it out rather than failing, because "a pane respawning onto its own stable id
reaches this a beat after the attach that retired it, and refusing surfaced the raw
SessionNotFoundError to the user" (`terminal-host-session-create.ts:42-59`). An `attachOnly`
request in that window is refused instead — "An attach must not adopt a doomed session". [fact]

Kill confirmation is explicit rather than assumed: the tmux shim checks `close.ptyKilled` and
raises `describeUnconfirmedAgentStop(close)` when the PTY was not provably killed
(`claude-agent-teams-tmux-dispatcher.ts:168-172, 274-277`). [fact]

### 5.3 The hang watchdog is NOT an agent watchdog

`src/main/hang-watchdog/` watches the **Electron main thread**, not agent children:

| Property | Value | Cite |
|---|---|---|
| Scope | `process.platform !== 'darwin'` → returns null; packaged builds only unless forced | `main-thread-hang-watchdog.ts:26-32` |
| Mechanism | a `worker_threads` Worker, because "the worker survives an AppKit main-thread deadlock without another Electron process" | `:44-49` |
| Heartbeat | 2 000 ms | `hang-watchdog-worker-protocol.ts:1` |
| Timeout | 45 000 ms | `:2` |
| Check tick | 5 000 ms | `:3` |
| Sleep discrimination | a tick gap `> 3×` the check interval means system suspension, not a parent hang, so the wait restarts | `hang-watchdog-detection-loop.ts:39-43` |
| Resolution | heartbeats resuming after detection fires `onHangResolved` — "the main thread was stalled, not deadlocked" | `hang-watchdog-detection-loop.ts:6-7, 22-29` |
| Action | writes a marker file; it does **not** kill or restart anything | `main-thread-hang-watchdog.ts:36` (`hangDetectionMarkerPath`) |

The **agent-level** equivalent of "is this thing stuck" is not a watchdog at all. It is the
`agentWait` field (§4.2) plus the agent status store. [fact]

### 5.4 Agent status: hooks and OSC, not screen scraping

This is the sharpest contrast with a tmux `capture-pane` classifier.

> "**The execution host owns agent status, in one store, and every reader subscribes to it.**"
> — `docs/reference/agent-status-store.md` ("The rule")

Producers converge on one `applyNormalizedStatus` path stamped with authority id
`main-agent-hooks`: **hook HTTP posts**, WSL and SSH relay receivers, and main's own **OSC escape
sequence parse** from the PTY stream. The store alone holds pane authority (launch tokens and
hashed commitments, retired-pane fences, pane-key aliases, per-connection ordering watermarks,
an evidence-age map that outlives a transport clear), and alone persists, to
`last-status.json` with a **seven-day hydrate window** and a `restoredUnconfirmed` stamp "that keeps
a hydrated row from ever reading as live truth". [fact]

The doc is candid that this is mid-migration: an audit on 2026-09-09 found six producers, three
consumers, and three separate copies of the same row inside the main process alone, with the
second copy a literal duplicate write. PR 1a has landed; PR 1b (deleting `RuntimeAgentRowStore`)
and PR 2 (renderer becomes a subscriber) have not. [fact]

Structured (non-PTY) chat sessions publish into the same store with two rules: never persist a
structured row (the journal is durable truth and a hydrated row "would fight the live republish"),
and never let it fight a hook row. Dropping the session and dropping its row are one operation,
because "a deletion path that forgot the row would strand a permanently working-looking agent". [fact]

---

## 6. What they deliberately did NOT build

### 6.1 No multiplexer — and a fake `tmux` instead

The repo contains **zero** use of tmux, screen, dtach, or abduco as a runtime dependency. tmux
appears in exactly three roles:

1. **As a guest program to be tolerated.** OSC 52 clipboard writes are allowed by default so
   tmux/Zellij/Neovim/fzf copy-from-TUI works over SSH
   (`docs/site/content/docs/terminal.mdx:15`, `docs/site/content/docs/settings.mdx:48`).
2. **As an agent-recognition string.** `claude-tmux` is a launch key meaning "Claude Agent Teams
   via ref native panes" (`src/shared/tui-agent.ts`, `src/shared/tui-agent-display-names.ts`,
   `src/shared/tui-agent-config.ts`), with a comment noting the wrapper's child is the real process
   (`src/shared/agent-process-recognition.ts`).
3. **As a CLI surface they emulate.** This is the interesting one. [fact]

They ship a **fake `tmux` binary** so that agent tooling which shells out to tmux keeps working on
top of native panes:

```
┌──────────────────┐   PATH-prepended shim dir   ┌────────────────────────┐
│ Claude Agent     │   ~/.ref/claude-agent-     │ sh script `tmux`       │
│ Teams (expects   │──▶ teams-bin/tmux          ─▶│ exec $BIN              │
│ real tmux)       │                             │   agent-teams-tmux "$@"│
└──────────────────┘                             └───────────┬────────────┘
                                                             │ RPC
                                                 ┌───────────▼────────────┐
                                                 │ agentTeams.tmuxCompat  │
                                                 │ ClaudeAgentTeamsTmux-  │
                                                 │ Dispatcher → native    │
                                                 │ splitTerminal / send / │
                                                 │ readTerminal / focus   │
                                                 └────────────────────────┘
```

| Piece | Location |
|---|---|
| Shim script writer | `src/main/runtime/claude-agent-teams-shim-env.ts:20-27, 135-150` (POSIX), `:152-170` (`tmux.cmd`) |
| Shim dir | `~/.ref/claude-agent-teams-bin` (`:86-88`) |
| CLI entry | `src/cli/index.ts:78-79, 206-226` (`agent-teams-tmux`) |
| RPC method | `agentTeams.tmuxCompat` (`src/cli/index.ts:212`) |
| Verb dispatcher | `src/main/runtime/claude-agent-teams-tmux-dispatcher.ts:19-82` |
| Arg/format parser | `src/shared/claude-agent-teams-tmux-compat.ts` (193 lines) |
| PATH ordering | `src/main/ipc/pty/host-env/path.ts:36` — "host env injection prepends ref's shims; Claude Agent Teams must still resolve our fake tmux before any real tmux" |

It reports `tmux 3.4` for `-V` (`claude-agent-teams-tmux-dispatcher.ts:27-29`) and implements:
`show-options`, `display-message`, `split-window`, `respawn-pane`, `select-layout`, `resize-pane`
(no-op), `list-panes`, `send-keys`, `capture-pane`, `select-pane`, `kill-pane`, `last-pane`; a
batch of verbs return empty (`set-option`, `set-hook`, `refresh-client`, `attach-session`,
`detach-client`, `source-file`, `wait-for`, `has-session`); anything else throws
`unsupported command` (`:26-81`). Fake pane ids are `%N` with a stable-id contract "so later
send-keys/kill-pane/list-panes still resolve" (`:140-145`). `capture-pane` maps onto
`api.readTerminal(handle, { limit: 1000 })` (`:242`). Leader-pane kill and respawn are refused
(`:154, 271`). [fact]

There is a documented three-way mode: `off | in-process | native-panes-shim`
(`claude-agent-teams-tmux-compat.ts:1`), and it **degrades to `in-process`** on win32 or when no
absolute CLI path can be qualified — "without an absolute CLI path the shim would resolve a bare
`ref` against the pane cwd, so degrade instead"
(`claude-agent-teams-shim-env.ts:39-52`). The shim refuses to run unless
`ref_AGENT_TEAMS_SHIM_BIN` is absolute, on both POSIX and Windows, because "a stray `ref` next to
the agent's files would run with the team token" (`:132-150, 152-170`). The Windows batch shim
avoids `call` because "its extra percent-expansion pass would rewrite tmux pane args such as `%2`
into batch parameters" (`:162`). The feature is **disabled by default**, opt-in under Settings →
Agents (`docs/site/content/docs/agents/supported.mdx`). [fact]

### 6.2 The other deliberate omissions

| Not built | Stated reason | Cite |
|---|---|---|
| A stale-socket **sweeper** | "the last one produced five defects, including deleting a live listener's only pathname" | `src/main/daemon/AGENTS.md:39-41` |
| Endpoint removal on shutdown | could delete a replacement's name | `AGENTS.md:45-46`, `daemon-endpoint-lifecycle.ts:97` |
| Daemon-side **query responses** | "the renderer's xterm is the authoritative responder and a daemon reply would race ahead and clobber it" — the daemon emulator has no `onData`, one carve-out for DA1 while the shell-ready barrier holds | `session-output-plane.ts:49-58` |
| Full-depth daemon scrollback | OOM at 100+ terminals | `daemon-session-scrollback-window.ts:3-7` |
| Systemd-isolated daemon supervision | named as not covered | `refd-operations.md:204-205` |
| A continuous health endpoint | `health` is published once, in the readiness payload | `refd-operations.md:199-203` |
| Daemon log rotation | grows unbounded | `refd-operations.md:215-216` |
| Atomic census-and-stop fence | operator procedure only | `refd-operations.md:98` |
| Pinned-port fail-closed | a pinned `--port` still falls back to an OS-assigned port | `refd-operations.md:211` |
| `birthtimeMs` as an identity term | Node may report ctime; some filesystems report epoch | `AGENTS.md:35-38` |

---

## 7. Decision inputs for us

### (a) What our tmux model gives that theirs lacks

1. **True N-viewer attach for free.** tmux natively supports many clients on one session. Theirs
   is strictly one daemon client per PTY, and a second attach *evicts* the first via
   `existing.detachAllClients()` (`terminal-host-session-create.ts:70`). [fact] Every multi-viewer
   property they have — fan-out, input floor, resize ownership — is hand-built above the daemon
   across `RuntimeTerminalDriverController`, `RemoteDesktopTerminalFloor`, a per-PTY layout queue,
   and a 250 ms resubscribe-grace window. That is roughly a thousand lines of arbitration we would
   not have to write. [inference]
2. **Survival independent of our own supervisor.** Their daemon is only PID-scoped-safe; it shares
   the systemd cgroup and dies on `systemctl restart`, which they document as unsolved
   (`refd-operations.md:24-30`; `headless-linux-server.md:236-239`). [fact] A tmux server started
   outside our unit survives a restart of our daemon entirely. [inference]
3. **Unbounded, battle-tested scrollback we do not maintain.** Their daemon keeps 1000 rows in RAM
   after an OOM incident (`daemon-session-scrollback-window.ts:3-7`), and full depth exists only
   as a checkpoint-plus-log pipeline they wrote: framed log, seq-gap detection, torn-tail
   truncation, tmp+rename atomicity, recovery freezes, quarantine, tombstoned deletes. That is
   `terminal-history-log.ts` + `history-manager.ts` + `history-reader.ts` +
   `daemon-pty-checkpoint-*.ts` (roughly 8 modules) replacing `tmux capture-pane -S -`. [fact]
4. **No socket-name ownership problem at all.** Their `AGENTS.md` records seven review rounds and
   twenty-three defects on this one question, and still names a residual two-syscall race
   (`AGENTS.md:16-18, 48-50`). tmux owns its own socket lifecycle. [fact]
5. **Zero protocol-compatibility surface.** They carry `PROTOCOL_VERSION = 36` with all 35 prior
   versions reachable through legacy adapters, plus a separate outer wire at
   `RUNTIME_PROTOCOL_VERSION = 3` with a three-rule compatibility doc and a two-direction skew test
   harness (`daemon-protocol-version.ts:3, 31-34`; `remote-wire-compatibility.md`). [fact]
6. **Operator escape hatch.** A human can `tmux attach` when our software is broken. Nothing in
   their model lets an operator reach a PTY without a working app. [inference]

### (b) What theirs gives that tmux lacks

1. **Structured agent status instead of screen scraping.** Status comes from agent hooks and OSC
   escape sequences, converging on one store with launch tokens, hashed commitments, retired-pane
   fences, per-connection ordering watermarks, a seven-day hydrate window, and a
   `restoredUnconfirmed` stamp (`docs/reference/agent-status-store.md`). [fact] Our
   pane-text classifier is a heuristic over rendered output; theirs is evidence with provenance
   recorded at write time so "Readers never re-adjudicate". This is the single largest
   correctness gap in our favour to close, and it is **independent of the tmux decision** — OSC and
   hook ingestion work fine over a tmux-hosted PTY. [inference]
2. **A real model of the terminal, not a text dump.** `HeadlessEmulator` holds cells, modes, OSC 8
   link ranges, cursor position, wide-character widths, and mouse-tracking mode, and serializes to
   ANSI that reproduces them. Test names show the fidelity bar: `hangul-cell-width-agreement`,
   `headless-emulator-wide-char-repaint`, `terminal-snapshot-osc8-roundtrip`,
   `terminal-snapshot-color-parity`, `repro-12101-mouse-tracking-survives-agent-death`. [fact]
   `capture-pane` gives text; this gives a resumable screen. [inference]
3. **Correct scrollback under reflow.** Resizes are recorded into the byte stream in emulator-apply
   order, "or cold-restore replay reflows at the wrong point"
   (`session-output-plane.ts:110-112`). A tmux text capture has already lost the reflow history. [fact]
4. **Exit *causes* and incarnation fencing.** `TerminalExitCause` plus per-session
   `incarnationId` so a raced exit cannot be attributed to the wrong generation
   (`session.ts:27`; `daemon-pty-adapter.ts:86-99`; `terminal-host.ts:128-135`). tmux gives a pane
   that vanished. [fact]
5. **Real flow control both ways.** `pausePty`/`resumePty` stop reading the PTY fd so a flooding
   child blocks on write, with a 5 s daemon-side failsafe (`types.ts:132-150`); plus per-client
   batching that flushes on socket `drain` and drops droppable backgrounded sessions while
   reporting the loss as a `dataGap` with a `droppedChars` count
   (`daemon-client-connections.ts:233`; `daemon-pty-adapter.ts:43-51`). [fact] tmux has no
   equivalent per-viewer backpressure. [inference]
6. **Cross-platform including Windows.** Named pipes plus ConPTY plus a PowerShell→cmd fallback
   chain plus a kill-on-close Job object (`daemon-spawner.ts:131-134`;
   `native-pty-spawn.ts:32-34, 58-80`). tmux does not exist on native Windows. [fact]
7. **Per-viewer geometry without fighting.** One subscription owns the PTY size while others merely
   observe, with activity-ordered inheritance and a preserved host-reclaim target
   (`remote-desktop-terminal-floor.ts:17-18, 61-79, 187-219`). tmux's aggressive-resize is
   session-global. [fact]
8. **Bounded, event-loop-friendly replay.** 64 KB chars / 1024 ops per turn, surrogate-pair safe
   (`cold-restore-replay-writer.ts:3-4, 22-31, 61-72`). [fact]
9. **Fine-grained update semantics.** A stale daemon is replaced only once its live sessions drain,
   never while it owns work (`daemon-pty-daemon-recovery.ts:157-209`;
   `refd-operations.md:133-137`). [fact]
10. **The degraded mode.** When it cannot own the endpoint it keeps existing sessions routed to the
    incumbent and runs fresh terminals locally without persistence, with an explicit user warning
    rather than a hard failure (`daemon-out-of-process-launcher.ts:208-217`,
    `DegradedDaemonPtyProvider`). [fact]

### (c) A credible hybrid

Yes, and their own repo is the proof of concept for the shim half of it.

```
┌──────────────────────────────────────────────────────────────────────────┐
│ KEEP: tmux as the PTY owner and multi-attach substrate                   │
│   · session survives our daemon restart AND systemd restart              │
│   · N clients attach natively; operator can attach by hand               │
│   · scrollback of record lives in tmux                                   │
└────────────────────────────┬─────────────────────────────────────────────┘
                             │ portable-pty attach (as today)
                             ▼
┌──────────────────────────────────────────────────────────────────────────┐
│ ADD 1: headless emulator in the Rust daemon, fed by the attached stream  │
│   → snapshot on demand for mobile/web first frame                        │
│   → replaces "classify captured pane text"                               │
├──────────────────────────────────────────────────────────────────────────┤
│ ADD 2: OSC + hook agent-status ingestion with provenance                 │
│   → one store, evidence-stamped, hydrate window, retired-pane fences     │
│   → INDEPENDENT of the tmux decision; do this regardless                 │
├──────────────────────────────────────────────────────────────────────────┤
│ ADD 3: driver floor + subscription-scoped resize owner above the bridge  │
│   → tmux gives N viewers, NOT input/resize arbitration between them      │
├──────────────────────────────────────────────────────────────────────────┤
│ ADD 4: versioned wire with a three-rule compat contract + skew test      │
│   → cheapest item on this list, highest cost if deferred                 │
└──────────────────────────────────────────────────────────────────────────┘
```

Ranked by value per unit of work: **ADD 2 first** (largest correctness win, no coupling to the
tmux decision), then **ADD 4** (cheap now, expensive after mobile clients ship), then **ADD 1**
(needed for a mobile first frame), then **ADD 3** (only once two viewers can actually type). [inference]

Two hybrid hazards to price in:
- **tmux is itself an emulator.** Feeding an attached tmux stream into our own emulator means
  parsing tmux's re-render of the child's output, not the child's bytes. Alternate-screen TUIs,
  mouse tracking, and OSC 8 links may not survive that hop intact. Theirs sits directly on the
  ptmx and even then needed `repro-12101-mouse-tracking-survives-agent-death` and
  `repro-13767-shell-ready-marker-lost-to-exec`. [inference]
- **OSC 52 and OSC 133 passthrough.** Their agent-status ingestion depends on OSC sequences
  reaching the host, and their own settings doc treats tmux as a thing that *intercepts* OSC 52
  (`docs/site/content/docs/settings.mdx:48`). Verify OSC 133 shell-integration and any custom
  status OSC survive `tmux` and `set -g allow-passthrough` before committing to ADD 2 over tmux.
  This is a concrete, testable blocker, not a theoretical one. [inference]

### (d) Concrete blockers a tmux-based core would hit for mobile/remote streaming

1. **No snapshot primitive.** A mobile client attaching needs one authoritative first frame with
   colors, modes, cursor, and links. They answer with a serialized emulator snapshot
   (`getSnapshot` → `SnapshotStart/Chunk/End` opcodes 2-4, `terminal-stream-protocol.ts:14-16`).
   `capture-pane` cannot express cursor position, alternate screen, mouse mode, or OSC 8 ranges.
   **This is the hard blocker.** [fact + inference]
2. **No per-viewer backpressure.** One phone on a bad LTE link cannot be allowed to back-pressure
   the PTY and stall the agent. They solve it with per-client batching that flushes on socket
   `drain`, droppability for backgrounded sessions, and a `dataGap{droppedChars}` event so the
   client knows to re-snapshot (`daemon-client-connections.ts:233`;
   `daemon-pty-adapter.ts:43-51`; `daemon-server.ts:76-88`). tmux fans the same bytes to every
   client with no per-client budget. [fact + inference]
3. **Resize is session-global in tmux.** A phone at 40×20 attached alongside a desktop at 200×50
   forces smallest-common-denominator or aggressive-resize thrash. Their answer is
   phone-fit-with-restore driven by a serialized per-PTY layout queue and a preserved host-reclaim
   target (`ref-runtime-terminal-drivers.ts:104-131, 186-195`;
   `remote-desktop-terminal-floor.ts:61-79`). Retrofitting this onto tmux means one tmux client per
   viewer plus our own arbitration on top. [fact + inference]
4. **No input arbitration.** Two tmux clients both type into the pane and the bytes interleave
   mid-escape-sequence. Their floor is generation-counted with commit/rollback specifically so
   concurrent writers cannot land a wrong final owner
   (`runtime-terminal-driver-controller.ts:81-149`). [fact + inference]
5. **Resize is unobservable through a text bridge.** They needed `getSize` to read the size the PTY
   *actually applied*, because resize is a fire-and-forget notify and the renderer must detect a
   dropped or coerced resize (`types.ts:248-257`). Through tmux we would be reading tmux's idea of
   the pane, not the child's `TIOCGWINSZ`. [fact + inference]
6. **Version skew becomes structural.** Mobile and desktop update independently — "Mixed versions
   are the normal state" (`remote-wire-compatibility.md:5-7`). A text-bridge protocol with no
   opcode negotiation fails the way Rule 2 describes: an unknown frame is dropped silently and
   "the feature behind it appears to hang" (`:36-44`). Retrofitting negotiation after mobile ships
   means permanent opcode numbers you cannot reuse, which is exactly why their `Ack` is 13 and
   `ClaimViewport` is 14 (`terminal-stream-protocol.ts:25-30`). [fact + inference]
7. **UTF-8 chunk splitting.** Any byte-stream hop needs a stateful decoder or emoji and
   box-drawing characters permanently corrupt to U+FFFD — they hit this on the daemon socket
   (`daemon-client-hello-handshake.ts:54-56`). A WebSocket bridge over tmux has the same hazard at
   the same place. [fact]
8. **tmux must exist on the target.** Their model reaches native Windows via named pipes and
   ConPTY (`daemon-spawner.ts:131-134`; `native-pty-spawn.ts:42-43`); a tmux core does not. If
   Windows is in scope, a tmux core implies a second, separate PTY path there — which is the
   duplicated-implementation cost they avoided by having exactly one. [fact + inference]

### Open questions for a maintainer

1. Is native Windows in scope? If yes, a tmux core means two PTY backends, and the hybrid's cost
   roughly doubles. *(Unanswerable from their repo.)*
2. Do our target agents emit OSC 133 / status OSC, and do those survive a tmux hop with
   `allow-passthrough`? This gates ADD 2 and is directly testable today.
3. Do we need concurrent *typing* from two viewers, or only concurrent *viewing*? Concurrent
   viewing is nearly free under tmux; concurrent typing needs the full driver floor either way.
4. What is our supervisor-restart requirement? If PTYs must survive `systemctl restart` of our own
   unit, tmux wins outright, because they explicitly do not solve it
   (`refd-operations.md:24-30`). [fact]
5. What scrollback depth must a mobile reattach show? Their split is 1000 rows live in RAM, desktop
   default from durable history (`daemon-session-scrollback-window.ts:8`;
   `daemon-restore-scrollback-depth.ts:5`). Our answer determines whether a checkpoint pipeline is
   needed at all.

### Confidence notes

| Claim | Basis |
|---|---|
| Daemon owns the PTY; app is the only daemon client | **confirmed** — read `daemon-entry.ts` → `pty-subprocess.ts` → `native-pty-spawn.ts`, and `daemon-pty-runtime-state.ts` |
| Attach is exclusive; second viewer evicts the first | **confirmed** — `terminal-host-session-create.ts:70` plus single-valued `daemon-session-attachments.ts:5-14` |
| App, not daemon, writes durable scrollback | **confirmed** — `HistoryManager` constructed in `daemon-pty-runtime-state.ts:222`, driven by `takePendingOutput` RPC from the client |
| `pty-subprocess/` is a module, not a process boundary | **inferred** from the import graph; no helper-process spawn exists in those files |
| Multi-viewer fan-out and arbitration live in the runtime | **confirmed** — `ref-runtime-terminal-drivers.ts`, `runtime-terminal-driver-controller.ts`, `remote-desktop-terminal-floor.ts` |
| No tmux/screen/dtach runtime dependency; a fake `tmux` shim exists | **confirmed** — repo-wide search plus `claude-agent-teams-shim-env.ts`, `claude-agent-teams-tmux-dispatcher.ts` |
| Headless cgroup survival is unsolved | **confirmed** — stated in `refd-operations.md:24-30` and `headless-linux-server.md:236-239` |
| Hang watchdog covers the app main thread, not agent children | **confirmed** — `main-thread-hang-watchdog.ts:26-32`, `hang-watchdog-worker-protocol.ts` |
| Agent status comes from hooks + OSC, not pane-text classification | **confirmed** — `docs/reference/agent-status-store.md` |
| No daemon-side agent restart supervisor | **confirmed by absence** — respawn logic targets the daemon (`daemon-pty-daemon-recovery.ts:268-283`); no loop restarts a shell |
| "Rules of thumb" about what tmux can/cannot express (snapshot fidelity, per-client backpressure, OSC passthrough) | **inferred** from their code's requirements; not verified against a live tmux in this session |
| Design docs `mobile-presence-lock.md`, `mobile-terminal-layout-state-machine.md` | **confirmed absent** from this checkout; referenced only from code comments |
| Commit-message rationale | **unavailable** — single-commit squashed clone |
