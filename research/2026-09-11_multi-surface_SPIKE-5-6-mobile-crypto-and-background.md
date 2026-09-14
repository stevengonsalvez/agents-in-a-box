# Spikes 5 and 6: phone wire crate through uniffi, background socket lifetime

Measured 2026-09-13 and 2026-09-14 on Stevie's mac. Every number below comes from a
command recorded in section 8. Inferences are tagged `[inference]`.

Answers spec v1.5 spike rows 5 and 6, feeding M1.

**Scope honesty first.** The goal asked for a real iPhone and a real Android
device. Neither was attached, and the box has no Xcode (Command Line Tools
only, no iOS SDK). Stevie chose an emulator-only report. So:

| measurement | iPhone | Android |
|---|---|---|
| handshake, `auth/hello`, `fleet/subscribe` through the crate | **not run** (no Xcode, no device) | **emulator**, Android 15 |
| cold start, 5 runs | not run | **emulator** |
| bundle delta, release with and without the crate | not run | **real arm64-v8a release APKs** (size does not depend on the device) |
| key custody | not run | **emulator** |
| socket lifetime, background and locked, 5 runs each | not run | **emulator** |
| local banner at 1, 5, 30 min | not run | **emulator** |

No simulator was used for anything, because no iOS simulator exists without Xcode.

---

## 1. Decision input

| question | answer | basis |
|---|---|---|
| uniffi as the M1 wire shape? | **Go on Android, not yet shown on iOS.** Make an iPhone run of this same harness the first gate of M1, before any UI work. | Android measured end to end. iOS not built. |
| Does D13 need a JS Noise review? | **No, on the evidence so far.** The Rust crate did the whole handshake and framing on the phone. No JS crypto was needed. This flips only if the iPhone leg fails. | measured on Android |
| Does the push reopen row move to M1+1? | **Yes.** The spec's bar is a grace period under 60 s. Android 15 destroyed the live socket **6.0 to 6.4 s** after the app left the foreground, in 9 of 10 runs. The 10th run lost all heartbeats without a reset. | measured, emulator; iOS unmeasured |
| Can M1 promise a timely local banner from a backgrounded app? | **No.** The 1-minute banner showed up 48 s late and the 5-minute banner 164 s late. The OS scheduled all three as inexact alarms, with windows of 45 s, 3 min 45 s and 22 min 30 s. | measured, emulator |

### What M1 must take from this

1. **The socket is foreground-only on Android 15.** About 5.4 s after the app is backgrounded, Android's
   network policy blocks the app with `blocked=APP_BACKGROUND`. The system then destroys its live TCP
   sockets (`InetDiagMessage: Destroyed live tcp sockets for uids={10209}`).
   The cached-app freezer stops the process at +10 s. A 15 s heartbeat with
   two missed probes as dead never fires in background, because the socket is
   already gone. The heartbeat only matters in the foreground. The spec already says
   "background grace suspends rather than tears down". On Android that phrase
   should read "the OS tears it down at about 6 s. Reconnect with
   `after_revision` on every foreground."
2. **Ship the crate with the symbol-hiding link flags.** A default uniffi build
   links the Rust static library into the turbo module and exports every Rust
   symbol. That alone adds **7.13 MiB** to an arm64 APK. Adding
   `-Wl,--exclude-libs,libainb_wire_mobile.a -Wl,--gc-sections` brings it to
   **1.73 MiB** (722 KiB gzip), and the result was run end to end (section 3.2).
3. **Suspended-app banners are the push row's job.** Local notifications
   scheduled from the app are not a substitute. Android coalesces them into windows of up to
   75 percent of the delay unless the app holds `SCHEDULE_EXACT_ALARM`. On
   Android 14 and later that permission is not granted by default, and the appop read `default`
   here.

### Recommended M1 gate wording

> Maestro or Detox flow against a fixture daemon: banner tap, answer tap;
> attention row `answered` with `answered_by = device:<id>` and receipt
> `delivered`. **Foreground only**: the phone treats its socket as lost when
> the app leaves the foreground (measured 6 s on Android 15, spike 6), reconnects
> with `after_revision` on every return to foreground, and asserts `Complete` replay. No banner
> timing is gated while the app is backgrounded; suspended-app banners belong to
> the push row, which moves to M1+1. **Entry condition**: the spike 5 harness
> completes Noise IK, `auth/hello` and `fleet/subscribe` through
> `ainb-wire-mobile` on a real iPhone and a real Android device, and the
> turbo module's native library is under 2 MiB per ABI.

---

## 2. What was built

```
 Expo app (Hermes)                       host (macOS)
┌───────────────────────┐              ┌──────────────────────────────┐
│ App.tsx, custody.ts   │              │ peerd (scratch peer)         │
│   │ generated TS      │              │  Noise IK responder          │
│   ▼ JSI turbo module  │  WS binary   │  real HelloResult,           │
│ ainb-wire-mobile (.so)│─────────────▶│  FleetSubscribeResult        │
│  snow IK initiator    │  one Noise   │  JSON line log, host clock   │
│  frame + LSP framing  │  msg per WS  └──────────────────────────────┘
│  tokio, 15 s ping     │  message
└───────────────────────┘
```

Everything is under `research/spikes/spike-5-6/`:

| part | what |
|---|---|
| `rust/ainb-wire-mobile` | The scratch crate. It path-depends on `ainb-tui/crates/ainb-hangar-proto`, so `HelloParams`, `HelloResult`, `FleetSubscribeParams` and `FleetSubscribeResult` are the shipped types. It uses spike 3's 16-byte header, Noise pattern `Noise_IK_25519_ChaChaPoly_BLAKE2s` via `snow` 0.9.6, and spike 3's seven-field prologue binding carrier and host id. Payloads use LSP `Content-Length` framing, fragmented with `FIN` above one Noise message. It adds opcodes 22 `Ping` and 23 `Pong` for the in-crate heartbeat. uniffi 0.31.2 exports `connect`, `WireSession.{hello, fleetSubscribe, mark, stats, close}` and `generateDeviceKeypair`. |
| `peerd`, `wirectl` | Scratch peer and host harness (feature `peer`). `wirectl probe` drives the same `client` code the phone runs. |
| `app/` | Expo SDK 57.0.22, React Native 0.86.3, Hermes, release builds only. Binding generated by `uniffi-bindgen-react-native` 0.31.0-5 as a C++ turbo module. |
| `scripts/` | Cold start, socket lifetime and banner scripts. All timestamps come from the host wall clock, which is also the clock `peerd` logs. |

The TypeScript side never sees a key schedule, nonce or frame header. It passes the key
bytes in and gets typed records out (`HelloSummary`, `SubscribeSummary`,
`SessionStats`).

### 2.1 Host sanity check (loopback, same client code)

`wirectl probe`, 5 runs against `peerd` on 127.0.0.1:

| run | ws connect | Noise IK | `auth/hello` | `fleet/subscribe` | connect to subscribed |
|---|---|---|---|---|---|
| 1 | 1.60 ms | 5.00 ms | 1.14 ms | 0.76 ms | 8.88 ms |
| 2 | 0.91 | 1.75 | 0.99 | 0.72 | 4.51 |
| 3 | 0.89 | 1.82 | 0.99 | 0.63 | 4.53 |
| 4 | 0.79 | 1.58 | 1.07 | 0.73 | 4.31 |
| 5 | 0.94 | 1.82 | 0.88 | 0.77 | 4.58 |

`auth/hello` negotiated protocol 1 with the full 64-entry capability catalogue.
`fleet/subscribe` decoded `replay_state: snapshot_reset` (`Bootstrap`) through
the real custom `Deserialize`. Negative cases all failed closed. A wrong carrier, an
unpinned host key and a wrong host id were each rejected at Noise message 1
(peer log `noise msg 1 rejected`). A wrong token got `-32000 invalid daemon token`.

---

## 3. Spike 5 on Android

### 3.1 Environment

| | |
|---|---|
| host | macOS 15.7.3, Intel i9-8950HK, 32 GB |
| device | **emulator**: `sdk_gphone64_x86_64`, Android 15 (API 35, build AE3A.240806.043, patch 2024-09-05), emulator 36.6.11, 4 GB guest RAM, swiftshader GPU |
| power | battery saver off (`low_power=0`), not on AC, battery 100 percent, Doze not forced |
| network | guest to host via the emulator NAT (`10.0.2.2`) |
| toolchain | rustc 1.94.0, NDK 27.1.12297006, Gradle 9.3.1, targets `x86_64-linux-android` and `aarch64-linux-android` |

### 3.2 Handshake and framed messages on the phone

The first launch after install connected, handshook, sent `auth/hello` and
`fleet/subscribe` under AEAD, and the in-crate heartbeat got its pong. The peer
log for that connection:

```
ws_accept → handshake (device static key logged) → ping seq 0 → hello (protocol {min 1, max 1})
          → fleet_subscribe after_revision 0 → mark cold_start ...
```

In-app, first launch: Noise IK 176.3 ms, `auth/hello` 3.7 ms, `fleet/subscribe`
16.0 ms, pong RTT 2.7 ms (screenshot `research/spikes/spike-5-6/proof/android-emu-first-launch.png`).

The `--exclude-libs` build (x86_64 `.so` 1,843,360 bytes against 7,756,344 by default) was installed fresh on the same emulator and completed the same sequence: handshake, `auth/hello`, `fleet/subscribe`, heartbeat pong, and a warm reconnect with WS 1,043.9 ms and Noise 14.4 ms. In-app: Noise IK 2.5 ms, `auth/hello` 3.2 ms, `fleet/subscribe` 2.2 ms (`proof/android-emu-biometric.png`). Hiding the symbols breaks nothing the binding uses.

### 3.3 Cold start, app launch to first framed message

The timer runs from the host issuing `am start -W` after `am force-stop`, until `peerd`
receives `auth/hello` from the new process. Both ends use the host clock. An `adb shell` round trip on
this box costs 612 to 755 ms and is inside every number in the first column.

| run | launch to `auth/hello` at peer | `am start` TotalTime | JS start to first frame (in app) |
|---|---|---|---|
| 1 | 5,870 ms | 2,757 ms | 2,914 ms |
| 2 | 4,690 | 2,829 | 1,710 |
| 3 | 3,884 | 2,005 | 1,791 |
| 4 | 5,327 | 2,864 | 2,157 |
| 5 | 5,211 | 2,586 | 2,172 |

How the in-app time breaks down, measured on the phone clock:

| run | JS start to key ready | key ready to Noise session | of which WS connect | of which Noise IK | `auth/hello` reply | warm reconnect, same process (WS / Noise) |
|---|---|---|---|---|---|---|
| 1 | 2,010 ms | 892 | 805.4 | 8.5 | 12 | 961.1 / 31.4 |
| 2 | 970 | 724 | 699.6 | 7.4 | 16 | 1,089.0 / 11.1 |
| 3 | 978 | 756 | 701.5 | 44.3 | 57 | 956.1 / 18.7 |
| 4 | 1,116 | 930 | 895.9 | 20.8 | 111 | 950.8 / 5.2 |
| 5 | 1,115 | 1,033 | 1,011.3 | 13.6 | 24 | 991.5 / 77.5 |

The crate's own cost is **7 to 44 ms of Noise IK plus a 12 to 111 ms `auth/hello`
round trip**. The 700 to 1,000 ms WS connect is the emulator's NAT, not the
crate. A warm reconnect in the same process costs the same 950 to 1,090 ms, so
this is not first-use cost. A bare `toybox nc -z 10.0.2.2 <port>` from the guest takes
342 to 582 ms, against 0.8 to 1.6 ms for the same client code on host
loopback. On a real phone over Wi-Fi, WS connect will be LAN latency
`[inference]`, not measured here. The 1 to 2 s from JS start to key ready is
React Native start-up, the secure-store read and the notification channel.
It was not split further.

### 3.4 Bundle size, release, arm64-v8a

Same app, same dependencies, `-PreactNativeArchitectures=arm64-v8a`, clean
build directories between variants. The baseline sets `SPIKE_NO_WIRE=1`: the native module
is dropped from autolinking and Metro swaps the package for a JS stub with the
same exports.

| variant | APK bytes | APK gzip -9 | turbo module `.so` | delta vs baseline |
|---|---|---|---|---|
| without crate | 27,749,389 | 15,607,113 | none | |
| with crate, ubrn default link | 35,228,533 | 18,149,464 | 7,393,384 | **+7,479,144 (7.13 MiB)**, +2,542,351 gzip |
| with crate, `--exclude-libs` + `--gc-sections` | 29,559,669 | 16,346,612 | 1,724,888 | **+1,810,280 (1.73 MiB)**, +739,499 gzip |

Other deltas are small. The JS bundle grows by 57,104 bytes (generated bindings), `classes.dex`
by 2,868 bytes, `libappmodules.so` by 17,864 bytes. In the default `.so`, `.dynstr` is 1.66 MB and
`.dynsym` 0.38 MB, which is the exported Rust symbol table. `.text` is 2.49 MB, and 0.90 MB after
`--gc-sections`. Native libraries are stored uncompressed in the APK, so the gzip column
approximates the download `[inference]`.

### 3.5 Device static key custody

| question | Android (emulator) | iOS |
|---|---|---|
| API | `expo-secure-store` 57.0.4: AES-GCM key in Android Keystore wrapping ciphertext in SharedPreferences. `keychainAccessible: AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY` is an iOS-only option, ignored here. | not run. The same code would use Keychain `kSecClassGenericPassword` with `AfterFirstUnlockThisDeviceOnly` `[inference from the module's API]`. |
| key minted where | `generateDeviceKeypair()` in the crate. The private key crosses the JS heap once as base64 on its way into secure storage. | |
| survives app update (`adb install -r`) | **yes**: `key_created=false` on every launch after the update, same fingerprint | |
| survives uninstall and reinstall | **no**: after `adb uninstall`, keystore2 logged `clearNamespace(r#APP, nspace=10209)`. The reinstalled app minted a new key (`key_created=true`, different fingerprint). `allowBackup` is false, so no backup restores it. M1 re-pairs after a reinstall. | |
| biometric gate | **not gated**, deliberately: the key must be readable while the phone is locked, so a foreground reconnect after unlock never prompts. The probe wrote a throwaway item with `requireAuthentication: true`: `canUseBiometricAuthentication()` was `false` with no enrolment and no screen lock, and the write was rejected with `Could not Authenticate`. A gated key therefore cannot exist on a phone without enrolled biometrics. The positive case with a fingerprint enrolled was not run. | |

M1 should not route the private key through JS. Mint and store it
inside the crate (Keystore or Keychain via platform calls) and export only a
handle `[inference]`. This spike did not build that.

---

## 4. Spike 6 on Android

### 4.1 Socket lifetime with a 15 s heartbeat

Each run force-stops the app, launches it, waits for `auth/hello` and 20 s of
foreground heartbeats, then either presses HOME with the screen kept on
(`svc power stayon true`) or turns the screen off with the app in front
(`KEYCODE_SLEEP`). It then watches the peer for 600 s. "First missed heartbeat" is
the first 15 s slot after the transition with no ping within 5 s of it.
"Socket destroyed" is when the peer saw the TCP reset.

| run | backgrounded: first missed beat | backgrounded: socket destroyed | locked: first missed beat | locked: socket destroyed |
|---|---|---|---|---|
| 1 | 9.8 s | 6.0 s | 9.7 s | 6.4 s |
| 2 | 9.8 | 6.0 | 9.7 | 6.2 |
| 3 | 9.6 | 6.0 | 9.6 | 6.2 |
| 4 | 9.7 | 6.1 | 9.6 | 6.2 |
| 5 | 9.7 | 6.1 | 9.8 | no reset seen in 600 s |

Across all 10 runs, **no heartbeat arrived after the transition**, and none resumed
within the 600 s window. The first-missed-beat column is set by the heartbeat
phase: the last foreground ping landed about 5 s before HOME. The socket column is
the real grace. Locked run 5 lost every beat but the reset never reached the
peer `[inference: the guest dropped the RST or the socket was blocked rather than destroyed]`.

Mechanism, from a separate screen-on repro with logcat:

```
00:20:20.385  HOME (guest clock)
00:20:25.834  InetDiagMessage: Destroyed 1 sockets, proto=IPPROTO_TCP ... uids={10209}   (+5.4 s)
              dumpsys netpolicy: UID=10209 procState=LAST blocked=APP_BACKGROUND
peer          disconnect "Connection reset without closing handshake"               (+6.2 s host)

another repro, same build:
00:17:06      HOME
00:17:16.192  ActivityManager: freezing 6859 dev.ainb.spike.wire                    (+10 s)
```

`dumpsys netpolicy` also shows `network_blocked_for_top_sleeping_and_above: true`,
which covers the locked case: an app on top of a sleeping screen is blocked too.
Real Android 15 phones run the same AOSP policy `[inference]`. OEM builds may add
their own kill policies on top, and none were measured.

### 4.2 Local needs-input banner, app backgrounded, screen off

The app was scheduled through its deep link with `expo-notifications` 57.0.18
(`TIME_INTERVAL`, channel importance HIGH, `POST_NOTIFICATIONS` granted). Then
HOME, then `KEYCODE_SLEEP`. The host polled `dumpsys notification` until each banner was posted.
`SCHEDULE_EXACT_ALARM` appop: `default` (not granted).

| scheduled ahead | posted after | lateness | alarm window the OS assigned |
|---|---|---|---|
| 1 min | 108 s | 48 s | `window=+44s583ms` |
| 5 min | 464 s | 164 s | `window=+3m44s803ms` |
| 30 min | 2,166 s | 366 s | `window=+22m29s713ms` |

Every banner fired, and every one was late, the 30 min banner by 6 min 6 s. Each window is 75 percent of its
delay, which is inexact `RTC_WAKEUP` behaviour. None of the three was
suppressed by the socket block or the freezer, because the alarm fires a
broadcast into a fresh process state. Low-power state for the run: battery saver off, not
charging.

---

## 5. iOS: blocked

Nothing ran. To unblock:

1. Install Xcode and sign in with an Apple ID. A personal team signs a
   7-day development build. The team id stays out of this report.
2. `npx ubrn build ios --config ubrn.config.yaml --and-generate --release`
   builds the xcframework for `aarch64-apple-ios` and `aarch64-apple-ios-sim`.
   Then `npx expo prebuild --platform ios` and a Release build to the
   phone.
3. Run the three measurements by hand, with a stopwatch against the peer log.
   iOS offers no `am start` equivalent without Xcode tooling:
   - cold start: swipe the app away, tap the icon, read the `auth/hello` timestamp;
   - background and lock, 5 runs each, same heartbeat column;
   - banners at 1, 5 and 30 min.
4. The bundle delta on iOS is the `.ipa` size with and without the xcframework,
   using the same `SPIKE_NO_WIRE` switch.

Until then, the "no JS Noise review" and "uniffi go" answers rest on Android alone.

---

## 6. Limits

- **Emulator, not phones.** Timing under the emulator's NAT and a software GPU
  overstates start-up and connect cost. Size numbers do not have this problem. The OS
  policy numbers (6 s socket destroy, 10 s freeze, inexact alarms) are
  AOSP behaviour on a Google API image, and may be stricter on OEM builds.
- **One Android version.** Android 14 and earlier do not have the background network
  block `[inference]`, so an older phone may keep the socket longer until the
  freezer or Doze acts. M1's minimum SDK decides whether that matters.
- **No foreground service tried.** A foreground service would likely keep the
  socket alive `[inference]`. It is a product and policy choice for M1+1 alongside push, not
  something this spike measured.
- **The peer is a stub.** It speaks the real envelope and framing, but the
  replay path is not exercised: head revision 0 and a `Bootstrap` reset.
- **Heartbeat phase.** One phase only. The first-missed-beat column moves with it.
  The socket-destroyed column does not.

---

## 7. Files

Committed under `research/spikes/spike-5-6/` (force-added, directory ignored):

| path | what |
|---|---|
| `rust/ainb-wire-mobile/src/{wire,client,peer}.rs`, `src/bin/{peerd,wirectl}.rs` | crate, peer, harness, 2 unit tests |
| `app/{App.tsx,custody.ts,t0.ts,index.ts,app.json,metro.config.js,react-native.config.js,spike/no-wire.ts}` | Expo harness and the no-crate baseline switch |
| `app/modules/ainb-wire/{package.json,ubrn.config.yaml,react-native.config.js}` | binding config; everything else there is generated |
| `scripts/android-{lib,cold-start,socket-lifetime,banners}.sh` | measurement scripts |
| `proof/*.png` | emulator screenshots: first launch, the `--exclude-libs` build after reinstall, the biometric probe |
| `README.md` | rebuild steps |

`app/pairing.json` (scratch host key and token) is gitignored and not in the
repository. Keys, tokens and device identifiers appear nowhere in this report.
The UID above is the emulator's app sandbox UID.

---

## 8. Commands

```bash
# host
cd research/spikes/spike-5-6/rust/ainb-wire-mobile
cargo test --release --features peer            # 2 passed
cargo build --release --features peer
./target/release/wirectl keygen --secrets $RUN/host-secrets.json --pairing ../../app/pairing.json \
    --url ws://10.0.2.2:47655/peer --transport lan
./target/release/peerd --listen 127.0.0.1:47655 --secrets $RUN/host-secrets.json \
    --transport lan --log $RUN/peerd-android-emu.log
./target/release/wirectl probe --secrets $RUN/host-secrets.json --url ws://127.0.0.1:47642/peer \
    --runs 5 --heartbeat-seconds 3

# Android SDK root, Gradle home and npm cache in session scratch
sdkmanager --sdk_root=$SDK "ndk;27.1.12297006" "cmake;3.22.1" "platforms;android-37.0" "build-tools;37.0.0"
avdmanager create avd -n spike56 -k "system-images;android-35;google_apis;x86_64" -d pixel_7
emulator -avd spike56 -no-snapshot -no-boot-anim -no-audio -gpu swiftshader_indirect -memory 4096

# binding and app
cd app/modules/ainb-wire && npx ubrn build android --config ubrn.config.yaml --and-generate --release
cd app && CI=1 npx expo prebuild --platform android --no-install
cd app/android && ./gradlew assembleRelease -PreactNativeArchitectures=x86_64 --no-daemon
SPIKE_NO_WIRE=0 ./gradlew assembleRelease -PreactNativeArchitectures=arm64-v8a --max-workers=3 --no-daemon
SPIKE_NO_WIRE=1 ./gradlew assembleRelease -PreactNativeArchitectures=arm64-v8a --max-workers=3 --no-daemon
llvm-size -A lib/arm64-v8a/libreact-native-ainb-wire.so

# measurements
export ADB=.../platform-tools/adb SERIAL=emulator-5554 PEER_LOG=$RUN/peerd-android-emu.log
bash scripts/android-cold-start.sh
MODE=background WINDOW_S=600 bash scripts/android-socket-lifetime.sh
MODE=lock WINDOW_S=600 bash scripts/android-socket-lifetime.sh
LOCK=1 bash scripts/android-banners.sh
adb shell 'toybox nc -z 10.0.2.2 47655'          # NAT connect cost
adb logcat -d | grep -E "InetDiag|freezing"; adb shell dumpsys netpolicy
adb shell dumpsys alarm | grep -A2 dev.ainb.spike.wire
```

At the end, every process this spike started was stopped by name or PID: the `spike56-emu`,
`spike56-peer` and `spike56-runs` tmux sessions, the emulator, and `peerd`. The scratch AVD,
SDK root, Gradle home, npm cache and the two Rust Android targets added for
the build were removed. The pre-existing adb server was left running.
