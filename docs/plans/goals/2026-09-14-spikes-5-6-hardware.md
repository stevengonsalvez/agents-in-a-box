# /goal Spikes 5 and 6 are measured on real hardware: the uniffi wire crate and Expo app from the emulator lane run on the attached iPhone (and the Android emulator, no Android phone is attached), and the report's decision input cites real-device numbers

— CONTEXT —
· Project: agents-in-a-box (ainb) desktop programme, slice 6 (M1 mobile companion), spikes 5 and 6 from `docs/plans/2026-09-11-multi-surface-decisions-spec.md` (spikes table rows 5 and 6, D13, D16, the `ainb-wire-mobile` row, the push gateway row). The emulator lane's goal is `docs/plans/goals/2026-09-13-spikes-5-6-mobile.md`; read it first, its constraints all apply here. Programme DAG: `docs/plans/2026-09-12-desktop-programme.md`.
· Stack: this box is an Apple-silicon mac on macOS 26.4.1 with Xcode 26.4.1 at `/Applications/Xcode.app`, one real iPhone on iOS 26.4.2 attached over USB (see `xcrun xctrace list devices`), an Android emulator `emulator-5554` (sdk_gphone64_arm64) already booted, Android SDK under `~/Library/Android/sdk`, node v26, rustc 1.96, cargo on PATH. No real Android device is attached; say so in the report and use the emulator for the Android column.
· Current state: the emulator lane (another mac, no Xcode) is building the scratch `ainb-wire-mobile` uniffi crate, the Expo app and the harness scripts on branch `stevengonsalvez/spikes-5-6` under `research/spikes/spike-5-6/` (gitignored, force-added) with the report draft at `research/2026-09-11_multi-surface_SPIKE-5-6-mobile-crypto-and-background.md`. It has been told to push that branch now and again as it lands steps. Start from it, do not rebuild the crate or the app from scratch; if the branch is not on origin yet, poll `git ls-remote origin stevengonsalvez/spikes-5-6` every five minutes for at most one hour while you do the device-side preparation below, then say so and continue with what you can.
· Working dir: the Orca worktree this session was launched in, branched from `origin/v2`; check out `stevengonsalvez/spikes-5-6` on top once it exists and work on branch `stevengonsalvez/spikes-5-6-hw` from it.
· Constraints: read-only on the repo except the report, the scratch directory and the two doc rows; the iPhone needs Developer Mode on and a trusted pairing, use the signing team Xcode already has configured with automatic signing and never write the team id, device UDID or any token into the report or a commit; commit with the explicit per-command `git -c commit.gpgsign=false commit` (the GPG key on this box is locked and pinentry cannot be answered by you) and say so in the PR body, one concern per commit, named paths; no em-dashes on lines you author; never name a third-party product as prior art; this is Stevie's daily machine: no global config changes, no Xcode or Android Studio settings changes, no keychain entries left behind, no simulator or emulator left running that you started, and never kill processes by name, only by pid you started; device measurements use real clocks with the exact commands recorded, five runs each as the emulator goal specifies; the emulator lane keeps the emulator-only columns, you add the iPhone and Android-emulator columns and write the final "Decision input" section; the PR that closes both spikes is yours, it updates the spikes 5/6 rows in the programme doc and the spec's spikes table, and it lists which measurements are real-device and which are emulator.
· Audience: the M1 implementer and Stevie, deciding whether `ainb-wire-mobile` via uniffi is the shape for M1, whether D13 needs a JS Noise implementation review, and whether the push reopen row moves to M1+1.

— SUCCESS CRITERIA (ALL MUST BE TRUE) —
1. Spike 5 on the real iPhone: the Expo app with the uniffi crate installs and launches on the attached iPhone, completes a Noise IK handshake over WebSocket against the scratch peer, exchanges `auth/hello` and one `fleet/subscribe` frame under AEAD framing, and the report tables cold-start cost (five runs), bundle size delta (release with and without the crate) and the keychain custody path (which API, survives reinstall, biometric gate) with real-device numbers; the Android column is measured on `emulator-5554` and labelled emulator.
2. Spike 6 on the real iPhone: time to first missed 15 s heartbeat after backgrounding and after screen lock (five runs each), and whether a locally scheduled needs-input banner fires at 1, 5 and 30 minutes ahead with the app backgrounded, with iOS version and Low Power Mode state recorded; the same on the Android emulator, labelled emulator.
3. The report ends with a "Decision input" section citing the measured numbers: go or no-go for uniffi as the M1 wire shape, whether D13 needs a JS Noise implementation review, whether the push reopen row moves to M1+1 (threshold: grace under 60 s), and the exact M1 gate wording; one PR to `v2` carries the report, the scratch directory and the two doc rows, with every measurement labelled real-device or emulator.
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
