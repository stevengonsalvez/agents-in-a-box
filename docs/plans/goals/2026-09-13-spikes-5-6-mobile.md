# /goal Spikes 5 and 6 are answered with measurements on real phones: a uniffi-compiled Rust wire crate completes a Noise IK handshake and AEAD framing inside an Expo app on iPhone and Android with cold-start cost, bundle size and keychain custody measured, and the background socket lifetime and local banner timing are known for both platforms

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme, slice 6 (M1 mobile companion), spikes 5 and 6 from `docs/plans/2026-09-11-multi-surface-decisions-spec.md` (spikes table rows 5 and 6, D13 transport and auth, D16 mobile, the `ainb-wire-mobile` crate row, the push gateway row). Research: `research/2026-09-11_multi-surface_C-mobile.md`, `research/2026-09-11_multi-surface_SPIKE-3-peer-ws-noise.md` (the Noise IK handshake over WebSocket that R1 will ship, measured on two Linux boxes), `research/2026-09-11_multi-surface_B-remote-federation.md` section 4 (48 KiB chunks, 512 KiB initial window). Programme DAG: `docs/plans/2026-09-12-desktop-programme.md`. Decisions D1-D18 are locked; this spike informs M1's gate wording and whether D13 needs a JS Noise review, it does not reopen D13 or D16.
· Stack: Rust workspace under `ainb-tui/` (`ainb-hangar-proto` for the JSON-RPC envelope, `snow` for Noise IK as used by spike 3 in `research/spikes/spike-3/` if present, else the crate names in that report); a scratch `ainb-wire-mobile` crate built with uniffi for iOS (xcframework) and Android (jniLibs); a scratch Expo app with a thin TS binding; this box is a macOS machine with Xcode and Android Studio expected, and real devices attached over USB where available (say exactly which devices, OS versions and whether any measurement fell back to a simulator or emulator).
· Current state: W0-wire is on v2 (protocol version and capability catalogue in `auth/hello`, op-id envelope); R1 has not started, so there is no daemon WebSocket listener yet; use the spike-3 scratch proxy or a stub speaking the same framing as the peer, on a private port, never the box's real `~/.agents-in-a-box` daemon. Slice 1 and P1 are merging concurrently on v2; you touch no crate under `ainb-tui/crates`.
· Working dir: the Orca worktree this session was launched in, branched from `origin/v2`; scratch code under `research/spikes/spike-5-6/` (force-add, the directory is gitignored); the report goes to `research/2026-09-11_multi-surface_SPIKE-5-6-mobile-crypto-and-background.md` (force-add) plus a PR to `v2` that updates the spikes 5/6 rows in the programme doc and the spec's spikes table.
· Constraints: read-only on the repo except the report, the scratch directory and the two doc rows; never store secrets, device identifiers or Apple team ids in the report; signed commits with `git -c gpg.format=openpgp -c user.signingkey=907EC78C72C6AFF6 commit -S` if the key is unlocked on this box, otherwise the explicit per-command `git -c commit.gpgsign=false commit` and say so in the PR body (the orchestrator re-signs at merge); one concern per commit, named paths; no em-dashes on lines you author; never name a third-party product as prior art; measure with real clocks and record exact commands; clean up every process, simulator and scratch app you started; the box is Stevie's daily machine, so no global config changes, no Xcode or Android Studio settings changes, no keychain entries left behind.
· Audience: the M1 implementer and Stevie, deciding whether `ainb-wire-mobile` via uniffi is the shape for M1, whether D13 needs a JS Noise implementation review, and whether the push reopen row moves to M1+1.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. Spike 5: a scratch `ainb-wire-mobile` crate compiled through uniffi runs inside an Expo app on a real iPhone and a real Android device, completes a Noise IK handshake against the scratch peer over WebSocket using the real proto envelope, exchanges at least `auth/hello` and one `fleet/subscribe` frame under AEAD framing, and the report tables cold-start cost (app launch to first framed message, five runs each), bundle size delta (release build with and without the crate, both platforms), and the keychain custody path for the device static key (which API, whether it survives reinstall, whether biometrics gate it); any fallback to simulator or emulator is named per measurement.
2. Spike 6: on both platforms the report measures how long a live WebSocket with a 15 s heartbeat survives after the app is backgrounded and after the screen locks (time to first missed heartbeat, five runs each, foreground and locked), and whether a locally scheduled needs-input banner fires when scheduled for 1, 5 and 30 minutes ahead with the app backgrounded, with the OS versions and low-power state recorded.
3. The report ends with a "Decision input" section that says, with the measured numbers cited: go or no-go for uniffi as the M1 wire shape, whether D13 needs a JS Noise implementation review, whether the push reopen row moves to M1+1 (the spec's threshold is grace under 60 s), and the exact M1 gate wording it recommends; the PR updates the spikes 5/6 rows in `docs/plans/2026-09-12-desktop-programme.md` and the spec's spikes table.
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
