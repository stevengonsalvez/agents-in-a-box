# /goal Spike 3 is answered with measurements across two real boxes: a peer WebSocket with Noise IK carries the daemon wire over tailnet and over ssh -L, survives the client closing, resyncs on reconnect, and holds the 50 MB terminal gate

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme, phase R1 (peer daemon on the box, WebSocket + Noise IK transport, per-device tokens). Spec: `docs/plans/2026-09-11-multi-surface-decisions-spec.md` D11, D13, "D13 pairing, keys, scopes" contract, phase row R1, spike row 3, edge cases for reconnect storms and listings. Evidence: `research/2026-09-11_multi-surface_B-remote-federation.md` sections 4 and 8 (streaming constants: 48 KiB chunks, 512 KiB initial window, 8 MiB total, acks batched at 192 KiB or 4 ms; session authority), `F-ours-current.md` section 3 (JSON-RPC 2.0 over LSP Content-Length framing, `fleet/subscribe {after_revision}` cursor replay, `SnapshotReset`). Programme DAG: `docs/plans/2026-09-12-desktop-programme.md`.
· Stack: Rust scratch crate (`tokio`, `tokio-tungstenite`, `snow` for Noise IK, `ainb-hangar-proto` as a path dependency for the envelope); two boxes reachable over tailscale: the paired Orca runtimes `claude-gcp` and `claude-hetzner` (this session runs on one of them and reaches the other by its tailnet name and by `ssh -L`); the real `ainb-hangar-daemon` may be started on a private `AINB_HANGAR_HOME` on the remote box as the thing being proxied, or a stub daemon speaking the same framing if the real one cannot run there.
· Current state: today the daemon has one door, a `0600` unix socket guarded by `SO_PEERCRED`; `ainb-hangar-client` hardcodes `UnixStream::connect` at three sites; no `HostId` exists; the spec plans `ssh -L` as one carrier among tailnet and LAN. This spike is the go/no-go for R1's scope and for rescoping D4 before it starts.
· Working dir: the Orca worktree this session was launched in, branched from `origin/v2`; scratch code outside the crates; the report goes to `research/2026-09-11_multi-surface_SPIKE-3-peer-ws-noise.md` (force-add) plus an optional PR to `v2` updating the spike-3 row in the programme doc.
· Constraints: read-only on the repo except the report and the programme row; never touch the boxes' real `~/.agents-in-a-box` daemon or its socket, use a private `AINB_HANGAR_HOME` and a private port; never store secrets in the report; Noise IK via `snow` with the host static key pinned client-side and the handshake transcript binding transport kind and a placeholder host id; one multiplexed WS per host carrying request/response and subscriptions; measure with `timeout` and record exact commands; no prior-art product names; no em-dashes; signed commits; when driving the other box use `orca terminal` on the paired runtime or plain ssh, never a bulk process kill, clean up every process you started by PID.
· Audience: the R1 implementer and Stevie, deciding R1 scope and whether the terminal stream needs a second unencrypted WS inside `ssh -L`.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. A client on box A completes a Noise IK handshake to a daemon-proxy on box B over the tailnet address and separately through `ssh -L`, issues at least `auth/hello`, `fleet/snapshot` and a `fleet/subscribe` against the real proto envelope, kills itself mid-subscription, waits 60 s while the remote side keeps emitting events, reconnects with `after_revision`, and receives a `Complete` replay with no gap; numbers for handshake time and reconnect-to-resync time are in the report for both carriers.
2. `cat` of a 50 MB file through the terminal stream path (48 KiB chunks, ack window as specified) completes under 2 s on the tailnet carrier and the `ssh -L` carrier, or the report shows the measured throughput for each and states whether the double encryption inside `ssh -L` is the cause, with the fallback (second WS without Noise inside `ssh -L`) measured too.
3. A laptop sleep/wake storm is simulated (5 hosts x 3 devices reconnecting at once, jittered backoff 1 s to 60 s, at most 2 concurrent resyncs per client) and the report records connection count, resync count and peak daemon-proxy RSS, ending with a "Decision input" section that says go or no-go for R1 as scoped and what D4 must be rescoped to.
4. Final deliverable runs without errors
5. You can show proof (screenshot · test output · URL)

— OPERATING RULES — NON-NEGOTIABLE —
1. PLAN FIRST. Output a numbered task list before writing any code.
2. WORK AUTONOMOUSLY. Don't ask clarifying Qs unless genuinely blocked.
3. SELF-VERIFY. After every step: run tests, inspect output, confirm it worked.
4. DEBUG YOURSELF. If it fails, diagnose + fix. Don't hand it back.
5. USE EVERY TOOL. MCPs · terminal · web · code exec · pull real data.
6. NO PLACEHOLDERS. No TODOs · no stubs · real components + real states.
7. PROGRESS LOG. Track completed · in-flight · decisions · blockers.
8. STAY ON GOAL. Discoveries off-spec? Note + keep moving.
9. IF BLOCKED. Log the wall · continue everything parallelizable.
10. CHECK SUCCESS BEFORE STOPPING. Re-read criteria · confirm each is met.

— QUALITY BAR —
· Code: clean, typed, follows project conventions
· Design: looks like a well-funded startup shipped it
· Output: survives a senior code review
· Docs: every new pattern / env var / decision logged

— FINAL DELIVERABLE —
✅ Confirmation each criterion is satisfied
📂 Every file created / modified
🚀 How to run / test / deploy
📊 Proof (screenshot · test output · URL)
📝 Decisions made + anything to know
⚠️ Known limitations + follow-ups

Begin by outputting your plan. Then execute end-to-end without checking
in until done or genuinely blocked.
