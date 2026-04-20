---
name: mobile-e2e-mcp
description: "End-to-end mobile testing of Expo/React Native apps via claude-in-mobile MCP + mcporter. Android emulator preferred (iOS needs WebDriverAgent). Covers full setup: emulator boot, Metro start, Firebase auth, MCP tool usage, tap/screenshot patterns. Use when asked to \"test functionality\" on a mobile app, \"walk through the app\", or run E2E validation of user journeys on an Expo/React Native project."
---

# Mobile E2E Testing via MCP

Verified workflow for running functional (not render-only) tests on Expo/React Native apps using `mcporter` CLI + `claude-in-mobile` MCP server. Derived from a real debugging session on biolift/JSTLFT — 24/24 functional tests passed against real production Firebase on Android emulator.

## When to use
- User wants functional validation of a mobile app (set logging, form submission, navigation + state persistence, etc.)
- Expo Go SDK 54+ or a custom dev build
- App uses Firebase (Auth + Firestore) or a similar cloud backend
- Target: Android emulator (**strongly preferred**) or iOS simulator (requires WDA setup — see Gotchas)

## Prerequisites
- `mcporter` CLI installed globally: `npm install -g mcporter`
- `claude-in-mobile` MCP server registered in `~/.mcporter/mcporter.json`:
  ```json
  {
    "mcpServers": {
      "mobile": {
        "command": "npx",
        "args": ["-y", "claude-in-mobile@latest"],
        "description": "Native app testing via simulator/emulator (iOS + Android)"
      }
    }
  }
  ```
- Android SDK with at least one AVD created (e.g. `Medium_Phone_API_35`)
- ADB in PATH or at `~/Library/Android/sdk/platform-tools/adb`
- Real credentials for the app's backend (or Firebase emulator, or a mock-auth code path)

## Android workflow (recommended)

### 1. Boot emulator in tmux
```bash
tmux new-session -d -s emu
tmux send-keys -t emu "~/Library/Android/sdk/emulator/emulator -avd <avd-name> -no-snapshot-save -no-boot-anim" C-m
~/Library/Android/sdk/platform-tools/adb wait-for-device
~/Library/Android/sdk/platform-tools/adb shell 'while [[ -z $(getprop sys.boot_completed) ]]; do sleep 1; done; echo boot-completed'
```

### 2. Configure app to use real backend
Grep the repo for production config files: `find . -name "*-config.json" -not -path "*/node_modules/*"`. Common names: `firebase-applet-config.json`, `credentials.json`, `google-services.json`. Copy credentials into `mobile/.env.local` (gitignored). Don't waste time with dummy Firebase keys — they get rejected with `auth/api-key-not-valid` at the Auth layer.

### 3. Start Metro in tmux
```bash
tmux new-session -d -s metro -c /path/to/mobile
tmux send-keys -t metro "EXPO_NO_TELEMETRY=1 npx expo start --android --clear" C-m
# Expo shows interactive auth prompt — answer it programmatically:
sleep 15
tmux send-keys -t metro Down C-m  # selects "Proceed anonymously"
```

### 4. Port forward (critical)
```bash
~/Library/Android/sdk/platform-tools/adb reverse tcp:8081 tcp:8081
```
Without this the emulator can't reach Metro and Expo Go shows "Something went wrong / Failed to download remote update".

### 5. Set mcporter target device
```bash
mcporter call mobile.device_set --args '{"deviceId": "emulator-5554", "platform": "android"}'
```

### 6. Dismiss first-launch dev menu
On first launch, Expo Go shows a bottom sheet with "Continue" and an X. **Tap the X at top-right (approx 966, 1740), not Continue** — Continue advances to the Tools page instead of dismissing.

### 7. Screenshot + tap loop
```bash
# Screenshot (always decode base64 via python)
mcporter call mobile.screen_capture --args '{"platform": "android"}' --output json > /tmp/mcp.json
python3 -c "
import json, base64
with open('/tmp/mcp.json') as f: d = json.load(f)
for item in d.get('content', []):
    if item.get('type') == 'image':
        with open('/tmp/shot.jpg', 'wb') as f: f.write(base64.b64decode(item['data']))
"

# Find elements precisely (never guess coordinates)
mcporter call mobile.ui_find --args '{"platform": "android", "text": "Get Started", "clickable": true}'
# Returns: [27] <ViewGroup> desc="Get Started" (clickable) @ (540, 1620)

# Tap at returned coordinates
mcporter call mobile.input_tap --args '{"platform": "android", "x": 540, "y": 1620}'

# Type into focused field
mcporter call mobile.input_text --args '{"platform": "android", "text": "chicken"}'
```

## Screen coordinate reference
- Original resolution on Medium_Phone_API_35: **1080×2400** — use these in `input_tap`
- Screenshot preview display: 540×1200 (×2 scale factor)
- Bottom tab bar y ≈ 2310
- For N tabs across 1080 width: tab N center at `x = (1080/N)/2 + index*(1080/N)`

## Three ways to unblock Firebase auth

1. **Production credentials** (fastest if available):
   - Grep repo for `*-config.json`
   - Copy to `mobile/.env.local`
   - Works for real auth + real Firestore

2. **Firebase emulator**:
   - `firebase emulators:start --only auth,firestore --project <dummy-project>`
   - Requires JDK 21+ (firebase-tools 15+)
   - Requires `connectAuthEmulator` / `connectFirestoreEmulator` wiring in the app
   - Android emulator uses `10.0.2.2` as host loopback alias (not `localhost`)
   - Set env flag like `FIREBASE_USE_EMULATOR=true` to gate the wiring

3. **Mock auth bypass**:
   - Add an `EXPO_PUBLIC_DEV_MOCK_AUTH` env var check in your Auth provider
   - Skip real sign-in, inject a fake `User` object, pretend onboarding completed
   - Fastest iteration but limited to UI-only testing (Firestore calls will still fail)

Pick the first one that applies — production creds are almost always the simplest.

## Common Gotchas

| Symptom | Cause | Fix |
|---|---|---|
| `auth/api-key-not-valid` | Dummy Firebase config in `.env.local` | Copy real creds from repo config file |
| "Failed to download remote update" | No adb reverse | `adb reverse tcp:8081 tcp:8081` after both emulator + Metro are up |
| Metro halts at "Log in / Proceed anonymously" | Interactive prompt blocks auto-execution | `tmux send-keys <session> Down C-m` selects anonymous |
| Tap does nothing visible | Expo dev menu overlay absorbing taps | Tap the X at top-right, NOT "Continue" |
| Inspector mode activated accidentally | Tapped "Toggle element inspector" in Tools page | Reload app (send `r` to Metro tmux) |
| `VirtualView*NativeComponent` red screen | RN version mismatch with Expo SDK | `npx expo install --fix` to realign dep matrix |
| `Cannot find native module 'ExponentAV'` | expo-av removed in SDK 54+ | Migrate to `expo-audio` (`createAudioPlayer`, `setAudioModeAsync`) |
| `expo-notifications: Android Push notifications removed` | SDK 53+ removed expo-notifications from Android Expo Go | Create platform-split shim: `notificationsShim.android.ts` (noop stubs) + `notificationsShim.ios.ts` (re-export) + default `notificationsShim.ts` |
| MCP tools not available in Claude Code session | Added mid-session, schemas not loaded | Use `mcporter call` from shell — bypasses schema requirement |
| iOS tap returns "WDA not found" | WebDriverAgent not compiled + installed | **Switch to Android.** Don't go down the WDA rabbit hole unless iOS-specific testing is mandatory |
| `mcporter config add --scope home mobile` fails | Flag parser treats `--scope` as positional name | Edit `~/.mcporter/mcporter.json` directly instead |

## Firestore integration smoke test

A great indicator that the full stack is working: the cross-tab integration test.

1. Complete a workout in the Workout tab (log sets → finish)
2. Navigate to Progress tab
3. Assert the PR count incremented by the number of new PRs you set

If this works, Firestore reads/writes are healthy and the app's realtime subscriptions are wired correctly.

## Anti-patterns to avoid

- **Guessing tap coordinates from screenshots.** Use `ui_find` every time. Coordinates drift between devices, orientations, and theme changes.
- **Declaring "tested" on render.** A screen can render perfectly and still have broken onClick handlers. Always tap through.
- **Starting Metro before the emulator is ready.** Metro crashes with `adb shell pm list packages ... exited with non-zero code: 1`. Wait for `boot_completed` first.
- **Reloading via Expo CLI `r` during an auth prompt session.** Each reload re-triggers the auth prompt. Do all reloads after the prompt is already answered.
- **Trying iOS first.** WDA setup burns hours. Default to Android.

## Evidence

This skill was derived from a real 2-hour debugging session testing the biolift (JSTLFT) mobile app on Android via mcporter. 24 functional tests passed end-to-end against real production Firebase, including:
- Firebase Anonymous Auth (real production sign-in)
- Onboarding wizard (sport → position → program)
- Live workout execution with inline PR detection + rest timer
- Nutrition meal logging with macro ring updates
- Theme switch with AsyncStorage cross-screen persistence
- Cross-tab PR count verification

Full test log: search the project for `research/*functional-test-results.md`.
