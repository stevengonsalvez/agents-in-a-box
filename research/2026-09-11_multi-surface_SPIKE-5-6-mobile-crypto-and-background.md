# Spikes 5 and 6: phone wire crate through uniffi, background socket lifetime

Measured 2026-09-13 and 2026-09-14 in two lanes: the emulator lane on an Intel
mac without Xcode (Android 15 emulator, sections 2 to 4), and the hardware lane
on Stevie's Apple silicon mac with Xcode 26.4.1 (iOS simulator and an arm64
Android 14 emulator, sections 5 and 6). Every number comes from a command
recorded in section 9. Inferences are tagged `[inference]`.

Answers spec v1.5 spike rows 5 and 6, feeding M1.

**Scope honesty first. No measurement in this report ran on a real phone.**
The goal asked for a real iPhone and a real Android device. The hardware lane's
iPhone (iOS 26.4.2) was paired with the mac but never reachable: no USB link
and no wireless CoreDevice link in a 10-minute wait (`devicectl` state
`unavailable` throughout). No Android phone was attached to either box. On
Stevie's call, the iOS column was measured on the Xcode iOS simulator and the
Android column on emulators. Every cell below says which.

| measurement | real iPhone | iOS | Android |
|---|---|---|---|
| handshake, `auth/hello`, `fleet/subscribe` through the crate | **not run** (phone unreachable) | **simulator**, iPhone 17, iOS 26.4.1 | **emulator**: Android 15 x86_64 (emulator lane), Android 14 arm64 (hardware lane) |
| cold start, 5 runs | not run | **simulator**, two 5-run sets | **emulator**, both images |
| bundle delta, release with and without the crate | not run | **release `iphoneos` arm64 builds** (device build, size does not depend on a device) | **release arm64-v8a APKs** |
| key custody (API, update, reinstall, biometric gate) | not run | **simulator** (keychain semantics differ from a phone, section 5.4) | **emulator**, both images |
| socket lifetime, background and locked, 5 runs each | not run | **simulator** | **emulator**, both images |
| local banner at 1, 5, 30 min | not run | **simulator** | **emulator**, both images |

Tags used below: `[simulator]`, `[emulator A15]`, `[emulator A14]`,
`[release build]`.

---

## 1. Decision input

The numbers this section rests on:

| signal | iOS `[simulator]` | Android 15 `[emulator A15]` | Android 14 `[emulator A14]` |
|---|---|---|---|
| crate completes Noise IK, `auth/hello`, `fleet/subscribe` under AEAD in a release app | **yes**, 11 of 11 cold starts | yes, 5 of 5 | yes, 5 of 5 |
| Noise IK on the device clock | 0.9 to 11.9 ms | 7 to 44 ms | 1.6 to 3.1 ms (cold starts and first launch) |
| launch to `auth/hello` at the peer, 5 runs | 1.05 to 1.68 s (set A); 1.17 to 9.72 s (set B, host under heavier memory pressure) | 3.9 to 5.9 s | 1.26 to 1.29 s |
| release size added by the crate | **+2.45 MiB** `.app`, +0.82 MiB zipped `.ipa` | +1.73 MiB APK with `--exclude-libs` | same APK: 1,720,192 byte `.so` |
| heartbeats after Home or lock (live session) | **none after +1.5 s** in 11 of 11 runs (9 sent none, 2 sent one within 1.5 s); first missed beat 3.6 to 16.5 s after the transition | 0 in 10 of 10 | 40 of 40 in 10 of 10: alive for the whole 600 s |
| socket survival after the transition (spec threshold) | **600 s in 6 of 11 runs**; reset at 264 to 424 s in 5 (2 verified as memory kills) | 6.0 to 6.4 s in 9 of 10; 1 locked run lost every beat with no reset | none in 600 s |
| local banner lateness at 1, 5, 30 min, locked | **+1 s, +1 s, +7 s** | +48 s, +164 s, +366 s | +47 s, +226 s, +364 s |
| device key after uninstall and reinstall | survived (simulator keychain) | new key | new key |
| biometric gate enforced | **no**: gated read succeeded unenrolled and with a non-matching face | gated write rejected with no enrolment | not rerun |

| question | answer | basis |
|---|---|---|
| uniffi as the M1 wire shape? | **Go.** The same crate, unchanged, ran the full handshake and both framed requests inside a release Expo app on the iOS simulator and on two Android emulator images. The iOS device slice (`aarch64-apple-ios`) compiled and linked into a release `iphoneos` build. The crate's share of cold start is milliseconds on every target. What no run has shown yet is the device slice executing on a phone. The only phone-specific risks left are signing, the local network permission prompt and ATS `[inference]`, none of which touch the crate. So the go stays, with a real-device run as M1's first gate. | `[simulator]`, `[emulator A15]`, `[emulator A14]`, `[release build]` |
| Does D13 need a JS Noise implementation review? | **No.** The whole Noise handshake, key schedule, nonces and AEAD framing ran inside the crate on every target, and no Noise or AEAD code runs in JavaScript. The TypeScript side never sees a key schedule, nonce or frame header (section 2). It does handle one secret: the device's static private key crosses the JS heap once as base64 on its way to and from secure storage (`app/custody.ts`, section 3.5). That is a custody issue for M1 to remove (item 4 below), not a reason to review a JS Noise implementation, because none exists. A JS Noise review becomes relevant only if the first-gate iPhone run fails in the crate itself, which the simulator run gives no reason to expect. | `[simulator]`, `[emulator A15]`, `[emulator A14]` |
| Does the push reopen row move to M1+1? (threshold: grace under 60 s) | **Yes, carried by Android 15.** The spec's threshold measures socket survival, and the two platforms answer it differently. **Android 15**: the socket was destroyed 6.0 to 6.4 s after the transition in 9 of 10 runs, and the tenth lost every beat. That is under 60 s. **iOS simulator, socket survival**: the socket stayed open for the whole 600 s window in 6 of 11 runs, and reset at 264 to 424 s in the other 5. That is over 60 s. **iOS simulator, a live session**: no heartbeat left the app later than 1.5 s after Home or lock in any run, and the first missed beat came 3.6 to 16.5 s after the transition. JS ran for up to 4.2 s, then the app was suspended `[inference: read from the stopped heartbeat and marks, not an OS log]`. So on iOS the socket survives as a suspended, silent connection that can deliver nothing to the user until the app is foregrounded. It does not keep a session live in the background. Android 15 alone crosses the threshold, and on iOS a suspended socket cannot raise a banner, which is push's job. Android 14 on AC power kept the socket for 600 s, but M1 cannot rely on an OS version older than the phones it targets. | `[simulator]`, `[emulator A15]`, `[emulator A14]` |
| Can M1 promise a timely local banner from a backgrounded app? | **Only on iOS, and only for banners scheduled while the app was in front.** The iOS simulator delivered all three within 7 s. Both Android images, with `SCHEDULE_EXACT_ALARM` not granted, coalesced them into inexact windows, 47 s to 6 min late. The granted path was not measured. Banners for events that happen while the app is suspended need push, which is the M1+1 row. | `[simulator]`, `[emulator A15]`, `[emulator A14]` |

### Real-device gap, and what a USB run adds

The simulator shares the mac's kernel, network stack and memory. A later run on
the iPhone over USB, with the same harness and scripts, would add:

1. **Proof that the device slice executes**, signed with the team Xcode already
   has, including the iOS local network permission prompt on first connect.
2. **WS connect over Wi-Fi** in place of loopback, which closes the cold start
   column for real.
3. **Keychain behaviour on a phone**: whether the item survives uninstall, and
   whether `requireAuthentication` actually gates the read with Face ID. The
   simulator answered neither (section 5.4).
4. **Suspension and reclaim timing** under the phone's own memory pressure,
   instead of the mac's. The reset column (264 to 424 s here) is the number most
   likely to move.
5. **Low Power Mode**, which the simulator does not have.
6. **`.ipa` size after App Store thinning and encryption**, which only an
   archive and TestFlight upload can show.

### What M1 must take from this

1. **The socket is foreground-only on both platforms.** On iOS the app is
   suspended within a few seconds of the transition, so the in-crate heartbeat
   stops (none later than 1.5 s). On return to the foreground in the runs the OS
   had not reclaimed, the tokio heartbeat fired every missed tick in one burst,
   about 40 pings in the same millisecond, so the socket was still open. M1's heartbeat must use
   `MissedTickBehavior::Skip` or `Delay` `[inference from the burst]`. On
   Android 15, about 5.4 s after the app is backgrounded, the network policy
   blocks the app with `blocked=APP_BACKGROUND`. The system then destroys its
   live TCP sockets (`InetDiagMessage: Destroyed live tcp sockets for uids={10209}`),
   and the cached-app freezer stops the process at +10 s. A 15 s heartbeat with
   two missed probes as dead never fires in the background on either platform:
   on iOS the process is suspended, on Android 15 the socket is already gone.
   The heartbeat only matters in the foreground. The spec says "background
   grace suspends rather than tears down". That matches iOS until the OS reclaims
   the app (264 to 424 s in 5 of 11 simulator runs), and not Android 15, which
   tears the socket down at about 6 s in 9 of 10 runs. Either way, M1 reconnects with
   `after_revision` on every return to the foreground and never assumes the old
   socket survived.
2. **Ship the crate with the symbol-hiding link flags.** A default uniffi build
   links the Rust static library into the turbo module and exports every Rust
   symbol. That alone adds **7.13 MiB** to an arm64 APK. Adding
   `-Wl,--exclude-libs,libainb_wire_mobile.a -Wl,--gc-sections` brings it to
   **1.73 MiB** (722 KiB gzip), and the result was run end to end (section 3.2).
   On iOS the static library links into the app binary and dead-strips by
   default: +2.45 MiB `.app`, +0.82 MiB zipped (section 5.3).
3. **Suspended-app banners are the push row's job.** Local notifications
   scheduled from the app are not a substitute. On both Android images the
   `SCHEDULE_EXACT_ALARM` appop read `default` (not granted), even though
   `app.json` declares the permission. In that state the OS coalesced the
   banners into windows of 75 percent of the delay. **The granted path was not
   measured**: no run granted the appop, so this report does not show whether
   `expo-notifications` would schedule exact alarms with it granted. Since the
   permission is not granted by default on Android 14 and later, the ungranted
   path is what an M1 user gets unless M1 asks for it. iOS delivered the
   banners within 7 s, but only for events known before the app left the
   foreground.
4. **Do not route the private key through JS, and do not trust the simulator on
   custody.** The key crosses the JS heap once as base64. The simulator kept it
   across uninstall and did not enforce a biometric gate. Both custody answers
   for iOS wait on the real-device gate.

### Recommended M1 gate wording

> **Entry gate, before any M1 UI work**: the spike 5 harness, unchanged,
> completes Noise IK, `auth/hello` and `fleet/subscribe` through
> `ainb-wire-mobile` on a real iPhone over USB and on a real Android 15 or later
> phone. The turbo module's native library is under 2 MiB per Android ABI, and
> the crate adds under 3 MiB to the iOS `.app`. The iPhone run also records
> keychain survival across uninstall and whether a `requireAuthentication` read
> prompts for Face ID.
>
> **Exit gate**: Maestro or Detox flow against a fixture daemon: banner tap,
> answer tap; attention row `answered` with `answered_by = device:<id>` and
> receipt `delivered`. **Foreground only**: the phone treats its socket as lost
> the moment the app leaves the foreground (spike 6: suspended at once on iOS,
> destroyed at 6 s on Android 15), reconnects with `after_revision` on every
> return to the foreground, and asserts `Complete` replay. The heartbeat skips
> missed ticks rather than bursting them. No banner timing is gated while the
> app is backgrounded. Suspended-app banners belong to the push row, which moves
> to M1+1.

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

## 3. Spike 5 on Android 15 (emulator lane)

### 3.1 Environment

| | |
|---|---|
| host | macOS 15.7.3, Intel i9-8950HK, 32 GB |
| device | **emulator**: `sdk_gphone64_x86_64`, Android 15 (API 35, build AE3A.240806.043, patch 2024-09-05), emulator 36.6.11, 4 GB guest RAM, swiftshader GPU |
| power | battery saver off (`low_power=0`), not on AC, battery 100 percent, Doze not forced |
| network | guest to host via the emulator NAT (`10.0.2.2`) |
| toolchain | rustc 1.94.0, NDK 27.1.12297006, Gradle 9.3.1, targets `x86_64-linux-android` and `aarch64-linux-android` |

### 3.2 Handshake and framed messages on the phone `[emulator]`

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

### 3.3 Cold start, app launch to first framed message `[emulator]`

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

### 3.4 Bundle size, release, arm64-v8a `[release APK]`

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

### 3.5 Device static key custody `[emulator]`

| question | Android 15 `[emulator A15]` | iOS and Android 14 |
|---|---|---|
| API | `expo-secure-store` 57.0.4: AES-GCM key in Android Keystore wrapping ciphertext in SharedPreferences. `keychainAccessible: AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY` is an iOS-only option, ignored here. | iOS `[simulator]`: Keychain `kSecClassGenericPassword`, `AfterFirstUnlockThisDeviceOnly`, section 5.4 |
| key minted where | `generateDeviceKeypair()` in the crate. The private key crosses the JS heap once as base64 on its way into secure storage. | sections 5.4 and 6.4 |
| survives app update (`adb install -r`) | **yes**: `key_created=false` on every launch after the update, same fingerprint | sections 5.4 and 6.4 |
| survives uninstall and reinstall | **no**: after `adb uninstall`, keystore2 logged `clearNamespace(r#APP, nspace=10209)`. The reinstalled app minted a new key (`key_created=true`, different fingerprint). `allowBackup` is false, so no backup restores it. M1 re-pairs after a reinstall. | sections 5.4 and 6.4 |
| biometric gate | **not gated**, deliberately: the key must be readable while the phone is locked, so a foreground reconnect after unlock never prompts. The probe wrote a throwaway item with `requireAuthentication: true`: `canUseBiometricAuthentication()` was `false` with no enrolment and no screen lock, and the write was rejected with `Could not Authenticate`. A gated key therefore cannot exist on a phone without enrolled biometrics. The positive case with a fingerprint enrolled was not run. | sections 5.4 and 6.4 |

M1 should not route the private key through JS. Mint and store it
inside the crate (Keystore or Keychain via platform calls) and export only a
handle `[inference]`. This spike did not build that.

---

## 4. Spike 6 on Android 15 (emulator lane)

### 4.1 Socket lifetime with a 15 s heartbeat `[emulator]`

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

### 4.2 Local needs-input banner, app backgrounded, screen off `[emulator]`

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

## 5. Spikes 5 and 6 on the iOS simulator (hardware lane)

### 5.1 Environment and how the device was driven

| | |
|---|---|
| host | Apple M1 Pro, 32 GB, macOS 26.4.1, Xcode 26.4.1, rustc 1.96.0. The host ran other agent sessions throughout and was under memory pressure (free memory 32 to 41 percent) |
| real iPhone | iPhone 15 on iOS 26.4.2, paired, **never reachable**: no USB device in the IOUSB tree, `devicectl` state `unavailable` for a 10-minute wireless wait |
| device used | **simulator**: iPhone 17 (`iPhone18,3`), iOS 26.4.1 runtime (23E254a), arm64 simulator slice of the same crate |
| power | the simulator has no Low Power Mode and no battery model; not applicable |
| network | app to `peerd` on host loopback (`ws://127.0.0.1:47655/peer`) |
| app | same `App.tsx`, Release configuration, `pairing.json` pointing at loopback |

simctl has no lock or banner query, and driving the Simulator menu through
`osascript` needs Accessibility rights this box does not grant, so the calls
hung. Changing that would be a settings change. Transitions are therefore
driven by an XCUITest target (`ios/Actuator.swift`, wired into the prebuild
project by `ios/add-actuator-target.rb`) run with `xcodebuild
test-without-building -only-testing`:

| action | how |
|---|---|
| launch, terminate | `xcrun simctl launch` / `terminate` |
| Home | `XCUIDevice.shared.press(.home)` |
| lock | `XCUIDevice.shared.perform(NSSelectorFromString("pressLockButton"))`, a private selector; the lock screen was confirmed by screenshot (`proof/ios-sim-lock-screen.png`) |
| notification permission, banner buttons | taps on SpringBoard's `Allow` and the app's `1 min`, `5 min`, `30 min` buttons. The simulator asks before opening a custom-scheme deep link, so taps replace the Android script's deep link |
| banner seen | SpringBoard element whose label contains `scheduled <n>s ahead`, `waitForExistence` |
| Face ID | `notifyutil` on `com.apple.BiometricKit.enrollmentChanged` and `com.apple.BiometricKit_Sim.pearl.match` / `nomatch` |

Every actuator action stamps the host wall clock into an events file. The
simulator shares that clock with `peerd`, so no clock offset applies.

Two environment faults cost runs, and both are recorded rather than hidden:

- Quitting the Simulator app (a process this lane had started) shut the booted
  device down and reset background run 4 at 383.8 s. That run is discarded.
- `CoreSimulatorService` relaunched six times (02:53, 03:21, 03:49, 04:01,
  04:15, 04:26) and shut the device down each time ("encountered in unexpected state
  at launch: Booted. Shutting down."). Runs whose window contains such a
  shutdown are reported as aborted and repeated. The runs also moved from the
  shared `iPhone 17` device to a dedicated `spike56hw-iphone17` device midway.

### 5.2 Handshake and framed messages `[simulator]`

First launch after install: WS connect 24.6 ms, Noise IK 4.6 ms, `auth/hello`
1.9 ms (protocol 1, 64 capabilities), `fleet/subscribe` 1.5 ms (head 0,
`snapshot_reset:Bootstrap`), heartbeat pong 1.5 ms, JS start to first frame
123 ms, key load 20 ms as the app reported it (`proof/ios-sim-first-launch.png`). The peer logged
`ws_accept`, `handshake`, `hello`, `fleet_subscribe`, then the app's
`cold_start` mark and a warm reconnect of 5 ms (WS 1.6 ms, Noise 2.6 ms).

### 5.3 Cold start and bundle size

**Cold start**, launch to `auth/hello`: timer from the host issuing `xcrun
simctl launch` after `simctl terminate`, to `peerd` receiving `auth/hello`.
`simctl` round trips cost 546 to 847 ms (set A) and 603 to 2,920 ms (set B)
and are inside the first column. The split comes from the app's own marks.

Set A, shared device, earlier in the session `[simulator]`:

| run | launch to `auth/hello` | launch to JS start | JS start to key ready | WS connect | Noise IK | warm reconnect (total / Noise) |
|---|---|---|---|---|---|---|
| 1 | 1,681 ms | 1,418 | 238 | 10.5 | 11.9 | 3 / 0.9 |
| 2 | 1,207 | 1,127 | 74 | 1.6 | 3.5 | 1 / 1.0 |
| 3 | 1,317 | 1,252 | 62 | 1.5 | 0.9 | 8 / 3.7 |
| 4 | 1,229 | 1,155 | 63 | 5.0 | 1.8 | 2 / 0.7 |
| 5 | 1,054 | 985 | 67 | 0.5 | 0.9 | 1 / 0.7 |

Set B, dedicated device, host under memory pressure `[simulator]`. Run 1 is the
first launch on a freshly created device (key minted):

| run | launch to `auth/hello` | launch to JS start | JS start to key ready | WS connect | Noise IK | warm reconnect (total / Noise) |
|---|---|---|---|---|---|---|
| 1 | 9,716 ms | 9,628 | 72 | 1.3 | 1.2 | 2 / 0.6 |
| 2 | 2,378 | 2,203 | 130 | 37.8 | 3.2 | 10 / 4.7 |
| 3 | 7,984 | 7,912 | 66 | 2.2 | 1.2 | 3 / 1.4 |
| 4 | 2,889 | 2,820 | 65 | 3.1 | 0.9 | 2 / 1.0 |
| 5 | 1,167 | 1,094 | 71 | 0.8 | 1.1 | 2 / 1.2 |

Key ready to `auth/hello` at the peer was 2 to 45 ms in all ten runs. The
crate's share is **0.9 to 11.9 ms of Noise IK and a 0 to 2 ms `auth/hello`
round trip**. Everything else is process launch and React Native start-up,
which set B shows swinging with host load. Loopback WS connect is not a Wi-Fi
number `[inference]`.

**Bundle size** `[release build]`: `xcodebuild -configuration Release -sdk
iphoneos -destination generic/platform=iOS CODE_SIGNING_ALLOWED=NO`, with and
without the crate. The baseline sets `SPIKE_NO_WIRE=1` for both `pod install`
(drops the `AinbWire` pod) and the build (Metro swaps in the JS stub). Separate
derived-data directories. The `.ipa` column is `zip -9` of `Payload/`.

| variant | `.app` bytes | app binary | `__text` | `main.jsbundle` | zipped `.ipa` |
|---|---|---|---|---|---|
| without crate | 28,484,509 | 1,675,600 | 356,292 | 1,551,603 | 7,870,740 |
| with crate | 31,050,756 | 4,168,856 | 1,156,372 | 1,624,594 | 8,726,537 |
| **delta** | **+2,566,247 (2.45 MiB)** | +2,493,256 | +800,080 | +72,991 | **+855,797 (0.82 MiB)** |

On iOS the static library links into the app executable with the linker's
default dead stripping. 44 `uniffi_ainb_wire` symbols are defined in it.
Unsigned, so code signature bytes are not counted. App Store thinning and
encryption would change the download size `[inference]`.

### 5.4 Device static key custody `[simulator]`

| question | iOS `[simulator]` |
|---|---|
| API | `expo-secure-store` 57.0.4: Keychain `kSecClassGenericPassword`, `keychainAccessible: AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY` |
| survives relaunch | yes: `key_created=false`, same fingerprint, every cold start after the first |
| survives update (`simctl install` over the app) | yes, same fingerprint |
| survives uninstall and reinstall | **yes on the simulator**: same fingerprint after `simctl uninstall` and `simctl install`. A freshly created simulator device minted a new key. Whether a phone keeps a keychain item across uninstall is exactly what the simulator cannot answer. Treat this row as unmeasured for M1 |
| biometric gate | **not enforced on the simulator**. The probe writes and reads a `requireAuthentication: true` item. Face ID not enrolled: `capable=false read=true`. Enrolled with a matching face: `capable=true read=true`. Enrolled with a non-matching face: `capable=true read=true`. The simulator keychain let the gated read through in every case, so this row is also unmeasured for M1 |

Simulator keychain semantics differ from a phone's: the simulator keychain
lives in a host-side file store, and access control flags were not enforced in
this run. The M1 key design stays as section 3.5 concludes: ungated, readable
after first unlock, minted and held by the crate.

### 5.5 Socket lifetime with a 15 s heartbeat `[simulator]`

Same procedure as section 4.1: foreground launch, `auth/hello`, 20 s of
foreground heartbeats, then Home (background) or the lock button (locked), then
a 600 s watch of the peer log. The transition time is the actuator's stamp.

| run | backgrounded: last ping before | first missed beat | pings after | socket reset | locked: last ping before | first missed beat | pings after | socket reset |
|---|---|---|---|---|---|---|---|---|
| 1 | -10.3 s | 4.7 s | 0 | none in 600 s | -1.6 s | 13.4 s | 0 | none in 600 s |
| 2 | -9.7 | 5.3 | 0 | none | -9.6 | 5.4 | 0 | 354.9 s, memory kill (verified) |
| 3 | -9.5 | 5.5 | 0 | none | -11.4 | 3.6 | 0 | none |
| 4 | -13.5 | 16.5 | 1 (at +1.5 s) | 264.3 s, memory kill (verified) | -14.6 | 15.4 | 1 (at +0.4 s) | 318.4 s |
| 5 | -3.5 | 11.5 | 0 | 424.0 s | -11.0 | 4.0 | 0 | 328.4 s |
| 6 | -11.0 | 4.0 | 0 | none | | | | |

Background has six valid runs: the repeat was sized before run 4 was confirmed
as a real memory kill rather than a fault. Not counted: one background run
reset at 383.8 s by quitting the Simulator app; one background run whose window
contained a `CoreSimulatorService` shutdown at +546 s (its socket had already
reset at +357.0 s, inside the same 264 to 424 s band, so the band holds six
resets if it counts); and one background and one locked attempt that failed
at launch because the service had just shut the device down, before any
transition. Background run 4 and locked run 2 ended their windows within
seconds of a service shutdown (02:53 and 03:21), but both resets came minutes
earlier (+264.3 s, +354.9 s), so they are counted.

- **No heartbeat left the app later than 1.5 s after the transition in any of
  the 11 runs.** After Home, `appstate inactive` reached the peer 0.3 s after
  the press and `appstate background` 0.9 to 1.3 s after it. After lock,
  `inactive` came at 0.0 to 0.5 s, then a brief `active`, then `background` at
  1.6 to 4.2 s. After that nothing more arrived, which reads as suspension
  `[inference]`. The first-missed-beat column is only the heartbeat phase.
- **The socket stays open while the app is suspended.** No FIN, no reset. It
  lasts until the OS reclaims the process. For the two resets marked verified,
  the simulator's log shows a sweep of `JETSAM_REASON_MEMORY_IDLE_EXIT` exits,
  then `launchd_sim: [pid/... [ainbwirespike]:] slaying domain` in the same
  second as the reset. The other three resets fell in the same 264 to 424 s
  band, but the simulator's log store did not survive the later device
  shutdowns, so their cause is `[inference]`.
- **Return to the foreground finds the socket still open in the runs the OS
  did not reclaim.** On foreground at +613 to +619 s, about 40 queued ticks of
  the tokio heartbeat reached the peer in the same millisecond. That burst is
  tokio's default `MissedTickBehavior::Burst`. In 5 of those 6 runs, the
  connection reset 0.7 to 1.1 s after the burst, when the script terminated the
  app for the next run `[inference]`. The only run left alone afterwards
  (background run 6) went back to a normal 15 s cadence at +619, +634 and +649 s.
  "Usable after resume" therefore rests on one run.

### 5.6 Local needs-input banner, locked `[simulator]`

`expo-notifications` 57.0.18, `TIME_INTERVAL` trigger, permission granted
through the system alert by the actuator. The actuator tapped `30 min`, `5 min`,
`1 min`, then pressed lock, all in one runner launch. The clock starts at each
`banner_scheduled` mark on this connection. A banner counts as seen when
SpringBoard shows it on the lock screen (`proof/ios-sim-banners-locked.png`, taken while the 30 min banner was still pending).

| scheduled ahead | seen after | lateness |
|---|---|---|
| 1 min | 61 s | 1 s |
| 5 min | 301 s | 1 s |
| 30 min | 1,807 s | 7 s |

Low Power Mode: not applicable on the simulator. iOS version 26.4.1.

---

## 6. Spikes 5 and 6 on the arm64 Android 14 emulator (hardware lane)

### 6.1 Environment

| | |
|---|---|
| device | **emulator** `emulator-5554`: `sdk_gphone64_arm64`, Android 14 (API 34, build UE1A.230829.050), pre-existing AVD already booted on the box |
| power | AC powered, battery 100 percent, battery saver off (`low_power=0`), `stay_on_while_plugged_in=1` |
| network | guest to host through the emulator NAT (`ws://10.0.2.2:47655/peer`) |
| build | same app, `assembleRelease -PreactNativeArchitectures=arm64-v8a` with `-Wl,--exclude-libs,libainb_wire_mobile.a -Wl,--gc-sections`, NDK 27.1.12297006, Android SDK root, Gradle home and `cargo-ndk` in session scratch |

### 6.2 Handshake and cold start `[emulator A14]`

First launch after install: Noise IK 3.1 ms, `auth/hello` 4 ms, WS connect
931.9 ms (`proof/android-emu-arm64-first-launch.png`). Five cold starts with
`scripts/android-cold-start.sh`. `adb shell` round trip was 224 to 296 ms.

| run | launch to `auth/hello` at peer | `am start` TotalTime | JS start to key ready | WS connect | Noise IK | JS start to `auth/hello` (in app) | warm reconnect (WS / Noise) |
|---|---|---|---|---|---|---|---|
| 1 | 1,259 ms | 274 | 82 | 867.3 | 1.6 | 954 | 995.7 / 2.2 |
| 2 | 1,279 | 257 | 68 | 941.3 | 1.9 | 1,018 | 997.1 / 1.7 |
| 3 | 1,278 | 246 | 77 | 929.5 | 2.4 | 1,014 | 998.2 / 1.5 |
| 4 | 1,278 | 239 | 81 | 942.4 | 1.6 | 1,030 | 996.7 / 1.4 |
| 5 | 1,294 | 271 | 67 | 956.2 | 1.7 | 1,030 | 995.9 / 1.8 |

WS connect sits at 867 to 998 ms in these runs, first use and warm alike (566 to 1,006 ms across every launch on this image),
against 0.5 to 38 ms on the simulator's loopback. The emulator NAT, not the
crate, is the likely cost `[inference]`. The release APK carrying the crate is
29,543,321 bytes with a 1,720,192 byte turbo module `.so`, within 0.3 percent of
the emulator lane's 29,559,669 and 1,724,888 (section 3.4), so the size delta
was not rebuilt.

### 6.3 Socket lifetime and banners `[emulator A14]`

Socket lifetime, same script and 600 s window as section 4.1:

| run | backgrounded (HOME, screen on) | locked (screen off) |
|---|---|---|
| 1 to 5 | first missed beat none in 600 s, 40 pings, no reset, in every run | first missed beat none in 600 s, 40 pings, no reset, in every run |

The socket and the heartbeat both lived for the whole window. Two likely
reasons, neither checked with `dumpsys` on this image `[inference]`: Android 14
predates the `blocked=APP_BACKGROUND` network block the emulator lane logged on
Android 15, and on AC power with stay-awake set, Doze should not start. This is
the configuration least like a phone in a pocket. It fits the emulator lane's
reading that the 6 s teardown is an Android 15 policy rather than a crate
artefact, but OS version, architecture, host and power all differ between the
two images, so it does not isolate the cause `[inference]`.

Local banners, backgrounded and screen off, `SCHEDULE_EXACT_ALARM` appop `default` (not granted; the granted path was not measured on either Android image):

| scheduled ahead | posted after | lateness | alarm window the OS assigned |
|---|---|---|---|
| 1 min | 107 s | 47 s | `window=+44s980ms` |
| 5 min | 526 s | 226 s | `window=+3m44s988ms` |
| 30 min | 2,164 s | 364 s | `window=+22m29s982ms` |

The first 30 min attempt reported "not posted": the script's 2,100 s poll
deadline ended inside the OS's 22 min 30 s window, and the script's closing
`force-stop` cancelled the alarm. The script now takes `GRACE_S`, and the rerun
above waited out the full window.

### 6.4 Key custody `[emulator A14]`

`scripts/android-custody.sh`: relaunch kept the key (same fingerprint), `adb
install -r` kept it, `adb uninstall` then `adb install` minted a new one
(`key_created=true`, different fingerprint). This matches Android 15 in
section 3.5. The biometric probe was not rerun on this image.

---

## 7. Limits

- **No real phone anywhere.** The iPhone was unreachable, and no Android phone
  was attached. The simulator runs on the mac's kernel, network and memory. The
  emulators add NAT latency and a guest scheduler. Size numbers do not have this
  problem, and neither does the crate's millisecond share of cold start. The
  OS policy numbers carry the most risk of moving on a phone: iOS suspension and
  reclaim, Android 15's 6 s teardown, inexact alarms.
- **iOS custody is unmeasured.** The simulator kept the key across uninstall
  and did not enforce `requireAuthentication`. Both answers wait on the
  real-device gate.
- **The host was a busy daily machine.** Memory pressure plausibly drove both
  the simulator's reclaim times (264 to 424 s) and six `CoreSimulatorService`
  relaunches `[inference]`. A phone's reclaim time depends on its own memory
  pressure.
- **Two Android versions, both Google images, one of them on AC with
  stay-awake.** Android 14 kept the socket 600 s only in that configuration.
  OEM builds may add their own kill policies, and none were measured.
- **No foreground service or iOS background mode tried.** Either would likely
  change the lifetime columns `[inference]`. That is a product and policy
  choice for M1+1 alongside push, not something this spike measured.
- **The peer is a stub.** It speaks the real envelope and framing, but the
  replay path is not exercised: head revision 0 and a `Bootstrap` reset.
- **Heartbeat phase.** The first-missed-beat column moves with the phase. The
  pings-after and socket-reset columns do not.
- **Private XCUITest selector.** `pressLockButton` is not public API. It locked
  the simulator, as the screenshot shows, but a later Xcode may drop it.

---

## 8. Files

Committed under `research/spikes/spike-5-6/` (force-added, directory ignored):

| path | what |
|---|---|
| `rust/ainb-wire-mobile/src/{wire,client,peer}.rs`, `src/bin/{peerd,wirectl}.rs` | crate, peer, harness, 2 unit tests |
| `app/{App.tsx,custody.ts,t0.ts,index.ts,app.json,metro.config.js,react-native.config.js,spike/no-wire.ts}` | Expo harness and the no-crate baseline switch; `app.json` now also declares `NSLocalNetworkUsageDescription` |
| `app/modules/ainb-wire/{package.json,ubrn.config.yaml,react-native.config.js,AinbWire.podspec}` | binding config; the podspec is ubrn's iOS regeneration that vendors the xcframework; everything else there is generated |
| `ios/Actuator.swift`, `ios/add-actuator-target.rb` | XCUITest actuator for Home, lock, permission, banner taps, banner detection and the Face ID probe, and the script that adds its target to the prebuild project |
| `scripts/android-{lib,cold-start,socket-lifetime,banners,custody}.sh` | Android measurement scripts; `banners` takes `BANNERS` and `GRACE_S` |
| `scripts/ios-sim-{lib,cold-start,socket-lifetime,banners,custody}.sh` | iOS simulator measurement scripts; `socket-lifetime` repeats runs a simulator service restart cuts |
| `proof/android-emu-*.png` | Android 15 emulator screenshots (emulator lane) |
| `proof/android-emu-arm64-first-launch.png` | Android 14 arm64 emulator, first launch |
| `proof/ios-sim-first-launch.png`, `proof/ios-sim-lock-screen.png`, `proof/ios-sim-banners-locked.png` | iOS simulator: first launch with handshake numbers, lock screen from the actuator, locked screen with the delivered needs-input banners |
| `README.md` | rebuild steps |

`app/pairing.json` (scratch host key and token) is gitignored and not in the
repository. Keys, tokens, device identifiers and the signing team appear nowhere
in this report. The UID in section 3 is the emulator's app sandbox UID.

---

## 9. Commands

### Emulator lane (Android 15, Intel mac)

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

### Hardware lane (iOS simulator and Android 14 emulator, Apple silicon mac)

```bash
# device reachability
xcrun xctrace list devices; xcrun devicectl list devices; ioreg -p IOUSB -w0
# peer on all interfaces (the phone would have needed the LAN address), scratch pairing
rustup target add aarch64-apple-ios aarch64-apple-ios-sim aarch64-linux-android x86_64-linux-android
./target/release/wirectl keygen --secrets $RUN/host-secrets.json --pairing $RUN/pairing-ios.json \
    --url ws://$(ipconfig getifaddr en0):47655/peer --transport lan
./target/release/peerd --listen 0.0.0.0:47655 --secrets $RUN/host-secrets.json --transport lan --log $RUN/peerd.log
./target/release/wirectl probe --secrets $RUN/host-secrets.json --url ws://127.0.0.1:47655/peer --runs 5 --heartbeat-seconds 3

# iOS binding, prebuild, size delta (unsigned device builds)
cd app/modules/ainb-wire && npx ubrn build ios --config ubrn.config.yaml --and-generate --release
cd app && CI=1 npx expo prebuild --platform ios
cd app/ios && xcodebuild -workspace ainbwirespike.xcworkspace -scheme ainbwirespike -configuration Release \
    -sdk iphoneos -destination generic/platform=iOS -derivedDataPath $RUN/dd-wire CODE_SIGNING_ALLOWED=NO build
SPIKE_NO_WIRE=1 pod install
SPIKE_NO_WIRE=1 xcodebuild -workspace ainbwirespike.xcworkspace -scheme ainbwirespike -configuration Release \
    -sdk iphoneos -destination generic/platform=iOS -derivedDataPath $RUN/dd-nowire CODE_SIGNING_ALLOWED=NO build
pod install                                       # restore the crate
mkdir -p $RUN/ipa-wire/Payload && cp -R $RUN/dd-wire/Build/Products/Release-iphoneos/ainbwirespike.app $RUN/ipa-wire/Payload/
(cd $RUN/ipa-wire && zip -qr -9 app.ipa Payload)   # same for dd-nowire
size -m ainbwirespike.app/ainbwirespike; nm -U ainbwirespike.app/ainbwirespike | grep -c uniffi_ainb_wire

# simulator app and actuator (pairing.json pointing at ws://127.0.0.1:47655/peer)
xcrun simctl create spike56hw-iphone17 com.apple.CoreSimulator.SimDeviceType.iPhone-17 com.apple.CoreSimulator.SimRuntime.iOS-26-4
xcrun simctl boot $SIM
GEM_HOME=$(brew --prefix cocoapods)/libexec ruby ios/add-actuator-target.rb
xcodebuild build-for-testing -workspace ainbwirespike.xcworkspace -scheme AinbSpikeUITests -configuration Release \
    -destination id=$SIM -derivedDataPath $RUN/dd-uit2 ARCHS=arm64
xcrun simctl install $SIM $RUN/dd-uit2/Build/Products/Release-iphonesimulator/ainbwirespike.app

# iOS measurements
export SIM XCTESTRUN=$RUN/dd-uit2/Build/Products/AinbSpikeUITests_iphonesimulator26.4-arm64.xctestrun \
    PEER_LOG=$RUN/peerd.log EVENTS=$RUN/events.log APP=$RUN/dd-uit2/Build/Products/Release-iphonesimulator/ainbwirespike.app
bash scripts/ios-sim-cold-start.sh
MODE=background bash scripts/ios-sim-socket-lifetime.sh
MODE=lock bash scripts/ios-sim-socket-lifetime.sh
bash scripts/ios-sim-custody.sh
LOCK=1 bash scripts/ios-sim-banners.sh
xcrun simctl spawn $SIM log show --start "$RESET_MINUS_6S" --end "$RESET_PLUS_2S" | grep -E "ainbwirespike.*slaying|JETSAM"
grep "Shutting down" ~/Library/Logs/CoreSimulator/CoreSimulator.log

# Android 14 arm64 emulator (pre-existing emulator-5554), SDK root with platform 37 in scratch
cargo install cargo-ndk --locked --root $RUN/cargo-bin
cd app/modules/ainb-wire && npx ubrn build android --config ubrn.config.yaml --and-generate --release
# append -Wl,--exclude-libs,libainb_wire_mobile.a -Wl,--gc-sections in the generated android/CMakeLists.txt
cd app && CI=1 npx expo prebuild --platform android --no-install
cd app/android && SPIKE_NO_WIRE=0 ./gradlew assembleRelease -PreactNativeArchitectures=arm64-v8a --no-daemon --max-workers=4
adb -s emulator-5554 install -r app-release.apk; adb -s emulator-5554 shell pm grant dev.ainb.spike.wire android.permission.POST_NOTIFICATIONS
export ADB SERIAL=emulator-5554 PEER_LOG=$RUN/peerd.log APK=$RUN/android-wire-arm64.apk
bash scripts/android-cold-start.sh
MODE=background WINDOW_S=600 bash scripts/android-socket-lifetime.sh
MODE=lock WINDOW_S=600 bash scripts/android-socket-lifetime.sh
LOCK=1 bash scripts/android-banners.sh
BANNERS=1800 GRACE_S=1500 LOCK=1 bash scripts/android-banners.sh
bash scripts/android-custody.sh
```

At the end of the emulator lane, every process it started was stopped by name
or PID: the `spike56-emu`, `spike56-peer` and `spike56-runs` tmux sessions, the
emulator, and `peerd`. The scratch AVD, SDK root, Gradle home, npm cache and
the two Rust Android targets added for the build were removed. The pre-existing
adb server was left running.

At the end of the hardware lane, only what it started was stopped, by exact
tmux session name or PID: the `spike56hw-peer`, `spike56hw-ios` and
`spike56hw-android` tmux sessions (with `peerd`), and the `spike56hw-iphone17`
simulator, which was shut down and deleted along with its keychain. On the
shared `iPhone 17` simulator it had booted, the app and the test runner were
uninstalled and the device shut down. Uninstall does not remove keychain items
(section 5.4), so the app's scratch device static key, private half included,
was still in that simulator's keychain. With the device shut down, the single
`genp` row in the simulator's `keychain-2-debug.db` whose access group ends in
`.dev.ainb.spike.wire` was deleted: `genp` went from 40 rows to 39 and rows for
the app from 1 to 0, and `pragma integrity_check` returned `ok`. The device then
booted and shut down cleanly, and the count stayed 0. No other row was touched,
and the keychain was not reset. The app was
uninstalled from `emulator-5554`, which was pre-existing and left running. The
four Rust targets it added were removed, along with the 57 Xcode derived-data
folders its test runs wrote. The session scratch (SDK root, Gradle home, npm
cache, `cargo-ndk`, build output) went with the session. Nothing was written to
the host keychain.
