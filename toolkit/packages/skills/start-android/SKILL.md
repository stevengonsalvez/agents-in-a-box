---
name: start-android
description: Start Android development with Emulator, dev server, and optional Poltergeist auto-rebuild
user-invocable: true
---

# /start-android - Start Android Development Environment in tmux

Start Android development with Emulator, dev server, and optional Poltergeist auto-rebuild.

## Usage

```bash
/start-android                # debug build, .env.development
/start-android staging        # staging build, .env.staging
/start-android release        # release build, .env.production
/start-android debug Pixel_7  # Specific emulator
```

## Process

### Step 1: Determine Build Variant

```bash
VARIANT=${1:-debug}
DEVICE=${2:-"Pixel_7_Pro"}

case $VARIANT in
    debug)
        ENV_FILE=".env.development"
        BUILD_TYPE="Debug"
        ;;
    staging)
        ENV_FILE=".env.staging"
        BUILD_TYPE="Staging"
        ;;
    release|production)
        ENV_FILE=".env.production"
        BUILD_TYPE="Release"
        ;;
esac

[ ! -f "$ENV_FILE" ] && ENV_FILE=".env"
```

### Step 2: Detect Project Type

```bash
detect_android_project() {
    if [ -d "android" ] && [ -f "android/build.gradle" ]; then
        [ -f "package.json" ] && grep -q "react-native" package.json && echo "react-native" && return
        [ -f "capacitor.config.json" ] && echo "capacitor" && return
        [ -f "pubspec.yaml" ] && echo "flutter" && return
        echo "native"
    else
        echo "unknown"
    fi
}

PROJECT_TYPE=$(detect_android_project)

[ ! -d "android" ] && echo "❌ android/ directory not found" && exit 1
```

### Step 3: Install Dependencies

```bash
# Gradle wrapper
[ -f "android/gradlew" ] && chmod +x android/gradlew

# npm
if [ -f "package.json" ] && [ ! -d "node_modules" ]; then
    npm install
fi
```

### Step 4: Setup Android Emulator

```bash
! command -v adb &> /dev/null && echo "❌ adb not found. Is Android SDK installed?" && exit 1

! emulator -list-avds 2>/dev/null | grep -q "^${DEVICE}$" && echo "❌ Emulator '$DEVICE' not found" && emulator -list-avds && exit 1

RUNNING_EMULATOR=$(adb devices | grep "emulator" | cut -f1)

if [ -z "$RUNNING_EMULATOR" ]; then
    emulator -avd "$DEVICE" -no-snapshot-load -no-boot-anim &
    adb wait-for-device
    sleep 5
    while [ "$(adb shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" != "1" ]; do
        sleep 2
    done
fi

EMULATOR_SERIAL=$(adb devices | grep "emulator" | cut -f1 | head -1)
```

### Step 5: Setup Port Forwarding

```bash
# For dev server access from emulator
if [ "$PROJECT_TYPE" = "react-native" ] || grep -q "\"dev\":" package.json 2>/dev/null; then
    DEV_PORT=$(shuf -i 3000-9999 -n 1)
    adb -s "$EMULATOR_SERIAL" reverse tcp:$DEV_PORT tcp:$DEV_PORT
fi
```

### Step 6: Configure Poltergeist (Optional)

```bash
POLTERGEIST_AVAILABLE=false

if command -v poltergeist &> /dev/null; then
    POLTERGEIST_AVAILABLE=true

    [ ! -f ".poltergeist.yml" ] && cat > .poltergeist.yml <<EOF
platform: android
watchPaths:
  - android/app/src/**/*.kt
  - android/app/src/**/*.java
  - android/app/src/**/*.xml
ignorePaths:
  - android/app/build/**
  - android/.gradle/**
buildCommand: |
  cd android && ./gradlew assemble${BUILD_TYPE} && cd ..
installCommand: |
  adb -s $EMULATOR_SERIAL install -r android/app/build/outputs/apk/${VARIANT}/app-${VARIANT}.apk
EOF
fi
```

### Step 7: Create tmux Session

```bash
PROJECT_NAME=$(basename "$(pwd)")
BRANCH=$(git branch --show-current 2>/dev/null || echo "main")
TIMESTAMP=$(date +%s)
SESSION="android-${PROJECT_NAME}-${TIMESTAMP}"

tmux new-session -d -s "$SESSION" -n build
```

### Step 8: Build & Install

```bash
case $PROJECT_TYPE in
    react-native)
        tmux send-keys -t "$SESSION:build" "npx react-native run-android --variant=$VARIANT --deviceId=$EMULATOR_SERIAL" C-m
        ;;
    capacitor)
        tmux send-keys -t "$SESSION:build" "npx cap sync android && npx cap run android --target=$EMULATOR_SERIAL" C-m
        ;;
    flutter)
        tmux send-keys -t "$SESSION:build" "flutter run -d $EMULATOR_SERIAL --flavor $VARIANT" C-m
        ;;
    native)
        tmux send-keys -t "$SESSION:build" "cd android && ./gradlew install${BUILD_TYPE} && cd .." C-m
        ;;
esac
```

### Step 9: Setup Additional Windows

```bash
# Dev server (if needed)
if [ "$PROJECT_TYPE" = "react-native" ] || grep -q "\"dev\":" package.json 2>/dev/null; then
    tmux new-window -t "$SESSION" -n dev-server
    tmux send-keys -t "$SESSION:dev-server" "PORT=$DEV_PORT npm start 2>&1 | tee dev-server.log" C-m
fi

# Poltergeist (if available)
if [ "$POLTERGEIST_AVAILABLE" = true ]; then
    tmux new-window -t "$SESSION" -n poltergeist
    tmux send-keys -t "$SESSION:poltergeist" "poltergeist watch --platform android 2>&1 | tee poltergeist.log" C-m
fi

# Logs
tmux new-window -t "$SESSION" -n logs
tmux send-keys -t "$SESSION:logs" "adb -s $EMULATOR_SERIAL logcat -v color" C-m

# Git
tmux new-window -t "$SESSION" -n git
tmux send-keys -t "$SESSION:git" "git status" C-m
```

### Step 10: Save Metadata

```bash
cat > .tmux-android-session.json <<EOF
{
  "session": "$SESSION",
  "project_name": "$PROJECT_NAME",
  "branch": "$BRANCH",
  "type": "$PROJECT_TYPE",
  "variant": "$VARIANT",
  "environment": "$ENV_FILE",
  "emulator": {
    "name": "$DEVICE",
    "serial": "$EMULATOR_SERIAL"
  },
  "dev_port": ${DEV_PORT:-null},
  "poltergeist": $POLTERGEIST_AVAILABLE,
  "created": "$(date -Iseconds)"
}
EOF
```

### Step 11: Display Summary

```bash
echo ""
echo "✨ Android Dev Environment Started: $SESSION"
echo ""
echo "Variant: $VARIANT ($ENV_FILE)"
echo "Emulator: $DEVICE ($EMULATOR_SERIAL)"
[ "$POLTERGEIST_AVAILABLE" = true ] && echo "Poltergeist: Auto-rebuild enabled"
echo ""
echo "Attach: tmux attach -t $SESSION"
echo "Status: /tmux-status"
echo ""
```

## Notes

- Auto-detects: React Native, Capacitor, Flutter, Native Android
- Maps build variant to environment file
- Poltergeist enables auto-rebuild on file changes
- Port forwarding for emulator to access localhost dev server
- Session persists across disconnects
