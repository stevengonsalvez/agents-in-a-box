# Spikes 5 and 6 scratch: phone wire crate and background lifetime

Report: `research/2026-09-11_multi-surface_SPIKE-5-6-mobile-crypto-and-background.md`.

```
┌──────────────┐  uniffi   ┌────────────────────┐  WS + Noise IK  ┌────────┐
│ Expo app     │──────────▶│ ainb-wire-mobile   │────────────────▶│ peerd  │
│ App.tsx (TS) │  ubrn JSI │ snow, tokio, proto │  AEAD frames    │ (host) │
└──────────────┘           └────────────────────┘                 └────────┘
```

| path | what |
|---|---|
| `rust/ainb-wire-mobile/src/wire.rs` | spike 3 frame header, prologue, Noise IK builders, LSP framing, fragmentation, 2 unit tests |
| `rust/ainb-wire-mobile/src/client.rs` | uniffi exports: `connect`, `WireSession.hello`, `fleetSubscribe`, `mark`, `stats`, `close`, in-crate 15 s heartbeat |
| `rust/ainb-wire-mobile/src/peer.rs` | scratch peer (feature `peer`): real `HelloResult` / `FleetSubscribeResult`, heartbeat echo, JSON line log on the host clock |
| `rust/ainb-wire-mobile/src/bin/{peerd,wirectl}.rs` | peer binary; host harness (`keygen`, `probe` with the three negative cases and a wrong token) |
| `app/` | Expo SDK 57 app; `custody.ts` holds the device static key; `spike/no-wire.ts` plus `SPIKE_NO_WIRE=1` builds the baseline without the crate |
| `app/modules/ainb-wire/` | turbo module shell; `ubrn.config.yaml` drives generation, everything else under it is generated |
| `scripts/android-*.sh` | cold start, socket lifetime, banner timing; host wall clock throughout |

## Rebuild

Tool locations are session-scratch on the measuring box (Android SDK root with NDK 27.1.12297006, cmake 3.22.1, platform 37.0; `GRADLE_USER_HOME` and the npm cache also scratch), so nothing global is touched.

```bash
# host peer and harness
cd rust/ainb-wire-mobile && cargo test --release --features peer && cargo build --release --features peer
./target/release/wirectl keygen --secrets "$RUN/host-secrets.json" --pairing ../../app/pairing.json \
    --url ws://10.0.2.2:47655/peer --transport lan
./target/release/peerd --listen 127.0.0.1:47655 --secrets "$RUN/host-secrets.json" --log "$RUN/peerd.log"

# app: the template assets come from `npx create-expo-app --template blank-typescript`
cd app && npm install
(cd modules/ainb-wire && npx ubrn build android --config ubrn.config.yaml --and-generate --release)
# size fix measured in the report: append to CMAKE_SHARED_LINKER_FLAGS in the generated
# modules/ainb-wire/android/CMakeLists.txt
#   -Wl,--exclude-libs,libainb_wire_mobile.a -Wl,--gc-sections
CI=1 npx expo prebuild --platform android --no-install
(cd android && ./gradlew assembleRelease -PreactNativeArchitectures=x86_64)
# size delta: arm64 with and without the crate, clean build dirs between the two
(cd android && SPIKE_NO_WIRE=0 ./gradlew assembleRelease -PreactNativeArchitectures=arm64-v8a)
rm -rf android/app/build android/build modules/ainb-wire/android/build
(cd android && SPIKE_NO_WIRE=1 ./gradlew assembleRelease -PreactNativeArchitectures=arm64-v8a)

# measurements
export ADB=... SERIAL=emulator-5554 PEER_LOG="$RUN/peerd.log"
bash scripts/android-cold-start.sh
MODE=background bash scripts/android-socket-lifetime.sh
MODE=lock bash scripts/android-socket-lifetime.sh
bash scripts/android-banners.sh
```

`pairing.json` holds a scratch host key and token and is gitignored.

## iOS (simulator, or a phone over USB)

Needs Xcode. The simulator reaches `peerd` on loopback; a phone needs the mac's
LAN address in `pairing.json` and signing with the team Xcode already has.

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
(cd app/modules/ainb-wire && npx ubrn build ios --config ubrn.config.yaml --and-generate --release)
cd app && CI=1 npx expo prebuild --platform ios
# size delta: unsigned device builds with and without the crate
(cd ios && xcodebuild -workspace ainbwirespike.xcworkspace -scheme ainbwirespike -configuration Release \
    -sdk iphoneos -destination generic/platform=iOS -derivedDataPath "$RUN/dd-wire" CODE_SIGNING_ALLOWED=NO build)
(cd ios && SPIKE_NO_WIRE=1 pod install && SPIKE_NO_WIRE=1 xcodebuild -workspace ainbwirespike.xcworkspace \
    -scheme ainbwirespike -configuration Release -sdk iphoneos -destination generic/platform=iOS \
    -derivedDataPath "$RUN/dd-nowire" CODE_SIGNING_ALLOWED=NO build && pod install)

# simulator actuator: simctl cannot lock the device or see banners
GEM_HOME="$(brew --prefix cocoapods)/libexec" ruby ../ios/add-actuator-target.rb
(cd ios && xcodebuild build-for-testing -workspace ainbwirespike.xcworkspace -scheme AinbSpikeUITests \
    -configuration Release -destination "id=$SIM" -derivedDataPath "$RUN/dd-uit" ARCHS=arm64)
xcrun simctl install "$SIM" "$RUN/dd-uit/Build/Products/Release-iphonesimulator/ainbwirespike.app"

# measurements
export SIM XCTESTRUN="$(ls "$RUN"/dd-uit/Build/Products/*.xctestrun)" PEER_LOG="$RUN/peerd.log" EVENTS="$RUN/events.log" \
    APP="$RUN/dd-uit/Build/Products/Release-iphonesimulator/ainbwirespike.app"
bash scripts/ios-sim-cold-start.sh
MODE=background bash scripts/ios-sim-socket-lifetime.sh
MODE=lock bash scripts/ios-sim-socket-lifetime.sh
bash scripts/ios-sim-custody.sh
LOCK=1 bash scripts/ios-sim-banners.sh
```

Use a dedicated simulator device (`xcrun simctl create`), and never quit the
Simulator app mid-run: it shuts the booted device down.
