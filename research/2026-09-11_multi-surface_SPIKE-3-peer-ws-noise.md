# Spike 3: peer WebSocket with Noise IK over tailnet and `ssh -L`

Measured 2026-09-12 across two real boxes. Every number below is from a command
recorded in section 9, not from a model or an estimate. Where a claim is an
inference rather than a measurement it is tagged `[inference]`.

Answers spec v1.3 spike row 3, feeding phase R1 and the rescope of D4.

---

## 1. Decision input

**Go for R1 as scoped, with one contract change and one carrier caveat.**

| question | answer | confidence |
|---|---|---|
| Does a peer WS with Noise IK carry the daemon wire unmodified? | Yes. The real `ainb-hangar-daemon` 1.28.1 served `auth/hello`, `fleet/snapshot` and `fleet/subscribe` through the proxy on both carriers, unchanged, and rejected a bad token with its own `-32000 invalid daemon token`. | measured |
| Does the host survive the client vanishing? | Yes. `SIGKILL` mid-subscription, 60 s dead, reconnect and resync in 124 ms (tailnet) / 128 ms (`ssh -L`). Zero error lines in any proxy log across 350+ connections. | measured |
| Does `after_revision` replay close the gap? | Yes. 601 events replayed over revisions 2959..3559, `replay_state: complete`, contiguous, reaching head. | measured |
| Does 50 MiB clear 2 s? | **Not at the spec's 512 KiB per-stream window.** 2.97-2.99 s tailnet, 1.86-2.55 s `ssh -L`. At the spec's own 2 MiB per-stream ceiling: 0.83-0.89 s tailnet, 1.02-1.63 s `ssh -L`. | measured |
| Is the double encryption inside `ssh -L` the cause? | **No.** Noise costs 1 to 5 percent of stream time on tailnet and is inside run-to-run variance on `ssh -L`. The bottleneck is the ack window, and on `ssh -L` it is ssh's own per-connection multiplexing. | measured, with an A/B |
| Does the terminal stream need a second unencrypted WS inside `ssh -L`? | **No, and it would not help.** A second WS inside the same ssh process leaves control-plane p50 at 48-73 ms against 16 ms idle, because both forwards share one TCP connection. A second ssh *process* restores 15.1-15.5 ms. | measured, with an A/B |
| Do reconnect storms hurt? | No. 50 clients, 200 connections, 600 resyncs, zero failures, 7.8 MiB peak RSS per proxy. | measured |

### What R1 must change

1. **Raise the per-stream ack window floor from 512 KiB to 2 MiB, or fix credit
   refill.** Throughput is exactly linear in the window (8.5 / 16.7 / 32.0 /
   60.4 MiB/s at 256 KiB / 512 KiB / 1 MiB / 2 MiB) and lands at
   `window / (2 x RTT)`, not `window / RTT`: the sender drains the whole window,
   then idles one full round trip waiting for credit. At the 14.4 ms RTT between
   these two boxes, 512 KiB cannot clear the 50 MiB gate and 2 MiB can. R1
   should either open at the 2 MiB ceiling the same document already allows, or
   return credit before the window drains so throughput becomes `window / RTT`.
   The second is the better fix: it keeps buffering low and makes the gate hold
   at higher RTT too. `[inference]` on which fix is better; the measurement that
   forces the change is not an inference.
2. **Bind the carrier and the host id into the handshake transcript.** Done here
   via the Noise prologue, and all three negative cases fail closed: a client
   declaring `tailnet` against an `ssh-l` listener, an unpinned host key, and a
   wrong host id are each rejected at Noise message 1 with `decrypt error`.
   Carry this into R1's `HostId` work rather than treating it as optional.
3. **Treat `ssh -L` as a degraded carrier for interactive work, not an equal
   one.** It is fine for control plane alone (16.5 ms p50) and it clears the
   50 MiB gate at a 2 MiB window, but a saturated terminal stream raises
   control-plane p50 to 48-98 ms no matter how the WS sockets are arranged
   inside it, and it is 2 to 3 times more variable run to run than the tailnet.
   The host switcher should prefer tailnet whenever both are reachable.

### What D4 must be rescoped to

D4 was already rescoped to D4' (updater and release matrix) with the host
switcher moved into R1. This spike does not move the boundary back. It adds one
requirement to the R1 half of that move: **the host switcher needs a carrier
column, not just a reachability flag.** The two carriers differ by 2 to 6 times
in stream throughput, by 3 to 6 times in control-plane latency under load, and
in whether a second socket helps. A switcher that shows "reachable" without
showing "over which carrier" cannot explain why the same host feels different
on two days. Nothing else in D4' needs to change.

---

## 2. What was built and how it maps to the contract

```
box A (claude-gcp-1)                                box B (claude-hetzner)
┌──────────┐   WS + Noise IK   ┌───────────┐  unix sock  ┌────────────────┐
│ peerctl  │ ─────────────────▶│  peerd    │ ───────────▶│ origin daemon  │
│ client   │  tailnet :47300   │  proxy    │  LSP frames │ real 1.28.1 or │
└──────────┘  or ssh -L :47410 └───────────┘             │ stub event src │
                                                          └────────────────┘
```

Three scratch binaries in one Rust crate, path-depending on the real
`ainb-hangar-proto` so every envelope on the wire is the shipped type:

| binary | role |
|---|---|
| `peerd` | the peer proxy. Terminates Noise, splices `Rpc` frames byte-for-byte onto a per-connection unix socket, serves terminal streams under the ack discipline. |
| `peerctl` | the client and the whole measurement harness. |
| `origind` | a stub origin daemon on a private unix socket, used only because the real daemon emits no fleet events without live agents (measured: head revision stayed at 6 across 45 s). Every type it serves is the real proto type. |

### 2.1 Transport layers

```
 byte 0    1       2        3      4..7        8..11      12..15   16..
┌──────┬───────┬────────┬───────┬──────────┬───────────┬─────────┬──────────┐
│ 0x74 │ ver=1 │ opcode │ flags │ streamId │ seq_high  │ seq_low │ payload  │
└──────┴───────┴────────┴───────┴──────────┴───────────┴─────────┴──────────┘
```

The 16-byte header and the 64-bit sequence split across two little-endian
`uint32`s come from research doc B section 4.1. Opcodes 1 `Output`, 7 `Input`,
9 `Subscribe`, 13 `Ack` keep their numbering from there; 20 `Rpc` and 21
`StreamEnd` are new, because this spike puts the control plane on the same
socket rather than a second one. `flags` bit 0 is `FIN`, marking the last
fragment of a logical message: a Noise transport message caps at 65535 bytes,
so a `fleet/snapshot` result larger than that is fragmented and rejoined.

`Rpc` payloads are the daemon's own bytes, LSP `Content-Length` framing and all
(research doc F section 3). The proxy never parses them. That is what let the
real daemon sit behind it with no changes: the proxy cannot corrupt a method it
has never heard of, and it cannot leak one either.

### 2.2 Noise IK and the transcript binding

Pattern `Noise_IK_25519_ChaChaPoly_BLAKE2s` via `snow` 0.9. IK because the
client already pins the host's static key, which is what R1 plans, and because
it costs exactly one round trip.

The prologue is a length-prefixed record of seven named fields, mixed into the
handshake hash so any disagreement fails the handshake instead of producing a
session whose two ends disagree about what they negotiated:

```
protocol       = "ainb-peer-ws"
framing        = "1"
payload_kinds  = "binary"
initiator      = "client"
responder      = "host"
transport      = "tailnet" | "ssh-l" | "loopback"
host_id        = 16 chars of [A-Za-z0-9_-]
```

`transport` and `host_id` are the two that matter. Research doc B section 4.3
records that the prior art binds both, and calls it the single most important
cryptographic detail to copy. It is copied, and it is tested.

### 2.3 Flow control

Constants taken verbatim from research doc B section 4.2:

| constant | value |
|---|---|
| chunk | 48 KiB |
| per-stream window, initial / max | 512 KiB / 2 MiB |
| connection-wide window, initial / max | 2 MiB / 8 MiB |
| ack batch | 192 KiB |
| ack flush timer | 4 ms |

The sender spends credit before a chunk leaves and an `Ack` restores it. The
client flushes an ack at 192 KiB or after 4 ms, whichever comes first. With one
stream the connection-wide window never binds, so every number in section 5 is
the per-stream window's doing.

---

## 3. Environment

| | box A | box B |
|---|---|---|
| tailnet name | `claude-gcp-1` | `claude-hetzner` |
| tailnet address | 100.66.160.98 | 100.125.222.64 |
| cores / memory | 4 / 24 GB | 8 / 15 GB |
| OS | Ubuntu 24.04.4, glibc 2.39 | Ubuntu 24.04.4, glibc 2.39 |

- tailscale path: `direct 49.13.80.61:41641`, not a relay.
- tailnet ICMP RTT over 10 packets: min/avg/max/mdev **14.090 / 14.369 / 15.030 / 0.271 ms**.
- "50 MB" throughout means 52,428,800 bytes (50 MiB), a file of repeating
  printable ASCII with no newline, so a pseudo-terminal's `ONLCR` translation
  cannot inflate the byte count. Host and client byte counts matched exactly on
  every run.
- The real daemon ran under `AINB_HANGAR_HOME=~/spike3-run/home` on box B. Its
  socket is `~/spike3-run/home/hangar.sock`; the box's real
  `~/.agents-in-a-box/hangar.sock` was not opened and its mtime (Sep 9) is
  unchanged.

---

## 4. Criterion 1: handshake, the three methods, kill, 60 s, resync

### 4.1 Handshake, 30 sequential connections per lane

Timer covers TCP connect, WebSocket upgrade, and the full Noise IK exchange.

| carrier | Noise | min | p50 | p95 | max | mean |
|---|---|---|---|---|---|---|
| loopback (box A to box A) | yes | 1.03 | 1.27 | 6.83 | 6.90 | 2.13 |
| tailnet | yes | 42.97 | 44.03 | 51.54 | 52.09 | 45.10 |
| `ssh -L` | yes | 44.38 | 47.57 | 67.91 | 71.96 | 50.58 |
| `ssh -L` | no | 29.25 | 30.92 | 39.81 | 139.71 | 35.67 |

All values in milliseconds. Noise costs one round trip: 47.6 against 30.9 at
p50 on `ssh -L`, which is 16.7 ms against a 14.4 ms RTT. Loopback at 1.27 ms is
the cost of the cryptography alone, so on any real carrier the handshake is
latency, not maths.

### 4.2 The three methods, against the real daemon

`ainb-hangar-daemon 1.28.1`, booted on box B under a private hangar home, fronted
by `peerd`, reached from box A:

| carrier | `auth/hello` | `fleet/snapshot` | `fleet/subscribe` | sessions | head |
|---|---|---|---|---|---|
| tailnet | 21.9 ms | 21.2 ms | 16.1 ms | 6 | 6 |
| `ssh -L` | 15.0 ms | 17.3 ms | 15.4 ms | 6 | 6 |

The six sessions are the ones the daemon actually discovered on box B. A wrong
token gets the daemon's own answer, through the proxy, unmodified:
`auth/hello rejected: -32000 invalid daemon token`.

Against the stub origin (12 sessions, 100 ms event cadence): tailnet 15.9 /
15.7 / 15.4 ms, `ssh -L` 14.9 / 19.7 / 16.9 ms. Every call is one RTT, so the
proxy adds nothing measurable to a round trip.

### 4.3 Kill, wait 60 s, resync

The client forks a child that authenticates, subscribes and pages its cursor
forward every 250 ms. The parent `SIGKILL`s the child mid-subscription (no close
frame, no goodbye, which is the desktop-closed case), sleeps, then reconnects
with the child's last committed `after_revision`.

| lane | origin | cursor at kill | dead | reconnect to resync | replay | state | contiguous | reaches head |
|---|---|---|---|---|---|---|---|---|
| tailnet | stub | 2958 | 60.125 s | **123.9 ms** | 601 events, 2959..3559 | `complete` | yes | yes |
| `ssh -L` | stub | 2958 | 60.129 s | **127.6 ms** | 601 events, 2959..3559 | `complete` | yes | yes |
| tailnet | real daemon | 6 | 60.076 s | **75.0 ms** | 0 events | `complete` | yes | yes |

601 events over 60.1 s at a 100 ms cadence is the expected count. Contiguity was
checked revision by revision from `cursor + 1`, not by count.

The real-daemon row replays zero events because the real daemon emitted none:
its head revision was 6 at `t0` and still 6 after 45 s, with no live agent
activity under its private home. That is why the stub exists, and it is the
honest reason, recorded rather than hidden. What the real-daemon row does prove
is that the cursor round trip, the `complete` verdict and the socket survival
are the real daemon's behaviour, not the stub's.

### 4.4 The host survives the client vanishing

Across the whole session (350+ connections, 5 `SIGKILL`ed subscribers, three
storms) **every `peerd` log contains exactly its one startup line and nothing
else**. No error, no panic, no leaked connection. The two long-lived proxies
were still serving after 10 minutes and 19 seconds of uptime.

### 4.5 Negative cases, all fail closed

| case | result |
|---|---|
| client declares `tailnet`, listener bound `ssh-l` | `noise msg 1 rejected: decrypt error` |
| client pins the wrong host static key | `noise msg 1 rejected: decrypt error` |
| client declares a different 16-char host id | `noise msg 1 rejected: decrypt error` |
| wrong daemon token, stub origin | `-32000 token rejected` |
| wrong daemon token, real daemon | `-32000 invalid daemon token` |

The first three never reach the token check: the transcript binding rejects them
before a single application byte exists.

---

## 5. Criterion 2: 50 MiB through the terminal stream

### 5.1 The gate

Three repetitions per cell, pseudo-terminal source, elapsed seconds for
52,428,800 bytes:

| lane | 512 KiB window (the spec's initial) | 2 MiB window (the spec's ceiling) |
|---|---|---|
| tailnet, Noise | 2.99 / 2.99 / 2.99 s, 16.7 MiB/s, **fails** | 0.89 / 0.85 / 0.88 s, 56.8 MiB/s, passes |
| tailnet, no Noise | 3.08 s, 16.2 MiB/s, **fails** | 0.82 / 0.83 / 0.83 s, 60.4 MiB/s, passes |
| `ssh -L`, Noise | 1.99 / 2.18 / 2.28 s, 22.9 MiB/s, **fails 2 of 3** | 1.30 / 1.02 / 1.55 s, 38.3 MiB/s, passes |
| `ssh -L`, no Noise | 1.91 / 1.86 / 2.55 s, 26.2 MiB/s, **fails 1 of 3** | 1.63 / 1.26 / 1.08 s, 39.7 MiB/s, passes |
| loopback, Noise | 0.39 s, 128.2 MiB/s, passes | |

**The 2 s gate fails at the specified 512 KiB window on both carriers and passes
at 2 MiB on both.** Median values quoted; all nine repetitions per row are in
section 9's raw output.

### 5.2 Why, with the falsifying experiment

Throughput is exactly linear in the per-stream window, tailnet with Noise, two
repetitions each:

| window | elapsed | throughput | `window / (2 x RTT)` |
|---|---|---|---|
| 256 KiB | 5.902 / 5.919 s | 8.5 MiB/s | 8.7 MiB/s |
| 512 KiB | 2.986 / 2.965 s | 16.7 MiB/s | 17.4 MiB/s |
| 1 MiB | 1.562 / 1.567 s | 32.0 MiB/s | 34.7 MiB/s |
| 2 MiB | 0.828 / 0.830 s | 60.4 MiB/s | 69.4 MiB/s |

Doubling the window doubles the throughput, four times over, and every point
sits within 12 percent of `window / (2 x RTT)`. That is the signature of a
sender that drains its whole window and then idles one full round trip waiting
for credit, rather than one that keeps credit flowing. It is not a link limit
(the same path does 60 MiB/s at 2 MiB) and it is not the cryptography.

At 14.4 ms RTT, the smallest window that clears 50 MiB in 2 s under this refill
behaviour is about 737 KiB. 512 KiB cannot, at any link speed.

### 5.3 Is the double encryption inside `ssh -L` the cause? No.

Measured directly, same file, same window, three repetitions:

| lane | Noise | no Noise | Noise cost |
|---|---|---|---|
| tailnet, 2 MiB | 0.85-0.89 s | 0.82-0.83 s | **about 5 percent** |
| tailnet, 512 KiB | 2.99 s | 3.08 s | **none, inside variance** |
| `ssh -L`, 2 MiB | 1.02-1.55 s | 1.08-1.63 s | **none, inside variance** |
| `ssh -L`, 512 KiB | 1.99-2.28 s | 1.86-2.55 s | **none, inside variance** |

The fallback the spike was asked to measure (a second WebSocket with no Noise
inside the `ssh -L` tunnel) is built, measured, and **buys nothing on
throughput**. On the tailnet, where the path is stable enough to see a 5 percent
effect, Noise costs 5 percent. Inside `ssh -L` the run-to-run variance of the
tunnel itself is larger than the whole cryptographic cost.

### 5.4 The real `ssh -L` problem: head-of-line blocking

One socket carrying a saturated terminal stream and a `fleet/snapshot` every
100 ms, against the same call on an idle socket:

| lane | control p50 idle | control p50 under load | control p95 | stream |
|---|---|---|---|---|
| tailnet, one socket | 15.4 ms | 24.8 ms | 105.5 ms | 0.83 s |
| tailnet, two sockets | 15.4 ms | **15.3-15.5 ms** | 16.2-16.9 ms | 0.82-0.85 s |
| `ssh -L`, one socket | 16.5 ms | **50.8-97.8 ms** | 70.6-143.7 ms | 1.56-2.61 s |
| `ssh -L`, two WS in one ssh process | 16.5 ms | **47.7-72.9 ms** | 55.2-84.6 ms | 1.39-1.94 s |
| `ssh -L`, two separate ssh processes | 16.5 ms | **15.1-15.5 ms** | 16.0-17.3 ms | 1.44-2.75 s |

Read the last three rows together. Splitting the stream onto a second WebSocket
inside the same `ssh -L` moves control-plane p50 from 50-98 ms to 48-73 ms,
which is not a fix. Splitting it onto a second ssh *process*, and therefore a
second TCP connection, restores 15.1-15.5 ms, identical to idle. The blocking is
ssh's own per-connection channel multiplexing, not this spike's framing and not
Noise. That is an A/B with the variable isolated, so it is a cause, not a guess.

On the tailnet the same split is nearly free but also nearly unnecessary: one
socket costs 9 ms of control-plane p50, two sockets cost nothing.

---

## 6. Criterion 3: sleep and wake storm

Five `peerd` instances on box B, each with its own origin socket (five hosts),
three client tasks each (three devices), all connecting at once, then
reconnecting on a uniform jittered backoff. Each device resyncs three logical
subscriptions behind a semaphore of two, so at most two resyncs are ever in
flight per client.

| run | clients | rounds | backoff | connections | resyncs | failures | mean resync | wall |
|---|---|---|---|---|---|---|---|---|
| jittered, as specified | 15 (5 x 3) | 4 | 1-60 s | **60** | **180** | **0** | 25.4 ms | 159.4 s |
| no jitter, worst case | 15 (5 x 3) | 6 | 1 s flat | 90 | 270 | 0 | 24.5 ms | 8.6 s |
| headroom probe | 50 (5 x 10) | 4 | 1 s flat | 200 | 600 | 0 | 25.4 ms | 5.5 s |

Mean resync time is flat at 25 ms from 15 clients to 50 clients with no jitter
at all, which is one RTT plus change. The jitter is doing nothing for the host
here; it is protecting a resource this shape of proxy does not strain.

Peak resident memory per proxy process, sampled every 500 ms on box B:

| phase | per proxy | five proxies |
|---|---|---|
| idle baseline | 3.9-4.1 MiB | 20.1 MiB |
| after the 15-client jittered storm | **5.4-5.5 MiB** | 26.9 MiB |
| after the 50-client herd | **7.4-7.8 MiB** | 38.0 MiB |

About 70 KiB of resident memory per concurrent client connection. `[inference]`
the per-connection cost is dominated by the two 2048-slot frame channels and the
Noise transport state; nothing was profiled to confirm the split.

Zero connect failures and zero proxy log lines beyond the startup banner across
all three runs.

---

## 7. Scope notes and limits

- **The stub origin is load-bearing for the replay test only.** The real daemon
  carried every RPC and survived every kill, but it produced no fleet events
  under a private hangar home with no agents, so the 60 s accumulation had to
  come from somewhere. Every type the stub serves is the shipped proto type and
  the framing is byte-identical, so what the replay test proves about the wire
  is real; what it does not prove is the real daemon's own replay bounds
  (`ReplayLimitExceeded`, retention pruning). R1 should re-run this shape
  against a daemon with a live provider before trusting the bound.
- **One RTT, one pair of boxes.** 14.4 ms is a good European hop. The window
  finding gets worse linearly with RTT: at 50 ms, even a 2 MiB window yields
  about 20 MiB/s and misses the gate. R1 should treat the window as a function
  of measured RTT, not a constant.
- **The `ssh -L` numbers are 2 to 3 times more variable than the tailnet
  numbers** (1.02 to 1.55 s across three identical runs against 0.85 to 0.89 s).
  Both paths cross the same public internet; only the tailnet path is stable.
- **No PTY resize, no input path, no revoke.** `Input` and `Resize` opcodes are
  defined and unused. R1's revoke-closes-socket-4403 requirement was out of
  scope here.
- **One stream per connection.** The connection-wide 8 MiB window was never the
  binding constraint, so it is untested. R2's per-viewer flow control will be
  the first thing to exercise it.
- The terminal source was measured both through a real pseudo-terminal and
  through a plain pipe. On the same lane and window the two are within 4 percent
  (tailnet 512 KiB: 2.99 s pty against 3.39 s pipe; `ssh -L` 2 MiB no Noise:
  0.64 s pty against 0.62 s pipe), so the PTY is not a factor in any conclusion
  above.

---

## 8. Files

Scratch code, deliberately outside the repository (`/home/claude/spike3`):

| path | lines | what |
|---|---|---|
| `src/lib.rs` | 532 | frame codec, Noise prologue and handshake, LSP framing and reassembly, flow constants, 6 unit tests |
| `src/bin/peerd.rs` | 311 | the peer proxy |
| `src/bin/peerctl.rs` | 828 | the client and every measurement subcommand |
| `src/bin/origind.rs` | 264 | the stub origin daemon |

`Cargo.toml` path-depends on
`ainb-tui/crates/ainb-hangar-proto`, so the crate breaks the moment the wire
types change. Dependencies: `tokio`, `tokio-tungstenite` 0.24, `snow` 0.9,
`portable-pty` 0.8, `clap`, `serde`, `serde_json`, `rand`, `base64`, `anyhow`.

In the repository, this spike changes two files and nothing else:
`research/2026-09-11_multi-surface_SPIKE-3-peer-ws-noise.md` (this report) and
the spike 3 row in `docs/plans/2026-09-12-desktop-programme.md`.

---

## 9. How to reproduce

Ports 47300-47334 on box B and 47410-47510 on box A were chosen to avoid every
in-use port on either machine. The daemon token and the static keys live in
`~/spike3-run/env.sh` at mode 0600 on both boxes and appear nowhere in this
report.

```bash
# 0. build, box A
cd /home/claude/spike3 && cargo build --release && cargo test --release
cargo build --release -p ainb-hangar-daemon   # in ainb-tui/

# 1. keys and token, box A
peerctl keygen > host-key.json      # host static pair
peerctl keygen > device-key.json    # device static pair

# 2. ship to box B (same glibc, so the binaries move as-is)
scp target/release/{peerd,origind,peerctl} \
    ainb-tui/target/release/ainb-hangar-daemon claude-hetzner:~/spike3-run/bin/

# 3. box B: stub origin, three carriers, and the real daemon
origind --socket ~/spike3-run/origin.sock --token "$TOKEN" --event-interval-ms 100
peerd --listen $(tailscale ip -4):47300 --origin ~/spike3-run/origin.sock \
      --private-key "$HOST_PRIV" --transport-kind tailnet --host-id "$HOST_ID"
peerd --listen 127.0.0.1:47310 --origin ~/spike3-run/origin.sock \
      --private-key "$HOST_PRIV" --transport-kind ssh-l --host-id "$HOST_ID"
peerd --listen 127.0.0.1:47311 --origin ~/spike3-run/origin.sock \
      --private-key "$HOST_PRIV" --transport-kind ssh-l --host-id "$HOST_ID" --no-noise
AINB_HANGAR_HOME=~/spike3-run/home ainb-hangar-daemon
peerd --listen $(tailscale ip -4):47302 --origin ~/spike3-run/home/hangar.sock \
      --private-key "$HOST_PRIV" --transport-kind tailnet --host-id "$HOST_ID"

# 4. box A: the ssh -L carrier
ssh -N -L 127.0.0.1:47410:127.0.0.1:47310 \
       -L 127.0.0.1:47411:127.0.0.1:47311 \
       -L 127.0.0.1:47420:127.0.0.1:47320 claude-hetzner

# 5. criterion 1
peerctl bench-handshake --url ws://100.125.222.64:47300/peer --n 30 \
        --server-key "$HOST_PUB" --client-key "$DEV_PRIV" \
        --transport-kind tailnet --host-id "$HOST_ID" --token "$TOKEN"
peerctl rpc-probe --url ws://100.125.222.64:47302/peer ...   # real daemon
timeout 200 peerctl resync-test --url ws://100.125.222.64:47300/peer \
        --gap-seconds 60 --self-path $(command -v peerctl) --state /tmp/s.json ...

# 6. criterion 2
peerctl term-bench --url ws://100.125.222.64:47300/peer \
        --path ~/spike3-run/fifty.bin --source pty --window 2097152 --with-rpc ...
peerctl rpc-latency --url ws://127.0.0.1:47510/peer --duration-seconds 2 ...

# 7. criterion 3
peerctl storm --urls ws://100.125.222.64:4733{0,1,2,3,4}/peer \
        --devices 3 --rounds 3 --backoff-min 1 --backoff-max 60 --subscriptions 3 ...
```

Every process started by this spike was killed by explicit PID at the end
(`~/spike3-run/pids.txt` on both boxes). Neither box's real
`~/.agents-in-a-box` daemon or socket was opened.
