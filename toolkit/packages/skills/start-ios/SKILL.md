---
name: start-ios
description: Start iOS development with Simulator, dev server, and optional Poltergeist auto-rebuild
user-invocable: true
---

# /start-ios - Start iOS Development Environment in tmux

Start iOS development with Simulator, dev server, and optional Poltergeist auto-rebuild.

## Usage

```bash
/start-ios                     # Debug build, .env.development
/start-ios Staging             # Staging build, .env.staging
/start-ios Release             # Release build, .env.production
/start-ios Debug iPhone15Pro   # Specific simulator
```

## Process

### Step 1: Determine Build Configuration

```bash
CONFIGURATION=${1:-Debug}
DEVICE=${2:-"iPhone 15 Pro"}

case $CONFIGURATION in
    Debug)
        ENV_FILE=".env.development"
        SCHEME="Debug"
        ;;
    Staging)
        ENV_FILE=".env.staging"
        SCHEME="Staging"
        ;;
    Production|Release)
        ENV_FILE=".env.production"
        SCHEME="Release"
        ;;
esac

[ ! -f "$ENV_FILE" ] && ENV_FILE=".env"
```

### Step 2: Detect Project Type

```bash
detect_ios_project() {
    if [ -d "ios" ] && [ -f "ios/Podfile" ]; then
        echo "react-native"
    elif [ -f "capacitor.config.json" ] || [ -f "capacitor.config.ts" ]; then
        echo "capacitor"
    elif [ -f "ios/"*.xcworkspace ]; then
        echo "native-pods"
    elif [ -f "ios/"*.xcodeproj ]; then
        echo "native"
    else
        echo "unknown"
    fi
}

PROJECT_TYPE=$(detect_ios_project)

[ ! -d "ios" ] && echo "❌ ios/ directory not found" && exit 1
```

### Step 3: Install Dependencies

```bash
# CocoaPods
if [ -f "ios/Podfile" ] && [ ! -d "ios/Pods" ]; then
    cd ios && pod install && cd ..
fi

# npm
if [ -f "package.json" ] && [ ! -d "node_modules" ]; then
    npm install
fi
```

### Step 4: Setup Simulator

```bash
SIMULATOR_UDID=$(xcrun simctl list devices | grep "$DEVICE" | grep -v "unavailable" | head -1 | grep -oE '\([A-F0-9-]+\)' | tr -d '()')

[ -z "$SIMULATOR_UDID" ] && echo "❌ Simulator '$DEVICE' not found" && xcrun simctl list devices | grep -E "iPhone|iPad" && exit 1

SIMULATOR_STATE=$(xcrun simctl list devices | grep "$SIMULATOR_UDID" | grep -oE '\((Booted|Shutdown)\)' | tr -d '()')

if [ "$SIMULATOR_STATE" != "Booted" ]; then
    xcrun simctl boot "$SIMULATOR_UDID"
    open -a Simulator
    sleep 3
fi
```

### Step 5: Configure Poltergeist (Optional)

```bash
POLTERGEIST_AVAILABLE=false

if command -v poltergeist &> /dev/null; then
    POLTERGEIST_AVAILABLE=true

    [ ! -f ".poltergeist.yml" ] && cat > .poltergeist.yml <<EOF
platform: ios
watchPaths:
  - ios/**/*.swift
  - ios/**/*.m
  - ios/**/*.h
ignorePaths:
  - ios/Pods/**
  - ios/build/**
buildCommand: |
  xcodebuild -workspace ios/*.xcworkspace -scheme $SCHEME -configuration $CONFIGURATION -destination "id=$SIMULATOR_UDID" build
EOF
fi
```

### Step 6: Create tmux Session

```bash
PROJECT_NAME=$(basename "$(pwd)")
BRANCH=$(git branch --show-current 2>/dev/null || echo "main")
TIMESTAMP=$(date +%s)
SESSION="ios-${PROJECT_NAME}-${TIMESTAMP}"

tmux new-session -d -s "$SESSION" -n build
```

### Step 7: Build & Install

```bash
case $PROJECT_TYPE in
    react-native)
        tmux send-keys -t "$SESSION:build" "npx react-native run-ios --simulator='$DEVICE' --configuration $CONFIGURATION" C-m
        ;;
    capacitor)
        tmux send-keys -t "$SESSION:build" "npx cap sync ios && npx cap run ios --target='$SIMULATOR_UDID' --configuration=$CONFIGURATION" C-m
        ;;
    native-pods|native)
        WORKSPACE=$(find ios -name "*.xcworkspace" -maxdepth 1 | head -1)
        if [ -n "$WORKSPACE" ]; then
            tmux send-keys -t "$SESSION:build" "xcodebuild -workspace $WORKSPACE -scheme $SCHEME -configuration $CONFIGURATION -destination 'id=$SIMULATOR_UDID' build" C-m
        fi
        ;;
esac
```

### Step 8: Setup Additional Windows

```bash
# Dev server (if needed)
if [ "$PROJECT_TYPE" = "react-native" ] || grep -q "\"dev\":" package.json 2>/dev/null; then
    DEV_PORT=$(shuf -i 3000-9999 -n 1)
    tmux new-window -t "$SESSION" -n dev-server
    tmux send-keys -t "$SESSION:dev-server" "PORT=$DEV_PORT npm start 2>&1 | tee dev-server.log" C-m
fi

# Poltergeist (if available)
if [ "$POLTERGEIST_AVAILABLE" = true ]; then
    tmux new-window -t "$SESSION" -n poltergeist
    tmux send-keys -t "$SESSION:poltergeist" "poltergeist watch --platform ios 2>&1 | tee poltergeist.log" C-m
fi

# Logs
tmux new-window -t "$SESSION" -n logs
tmux send-keys -t "$SESSION:logs" "xcrun simctl spawn $SIMULATOR_UDID log stream --level debug" C-m

# Git
tmux new-window -t "$SESSION" -n git
tmux send-keys -t "$SESSION:git" "git status" C-m
```

### Step 9: Save Metadata

```bash
cat > .tmux-ios-session.json <<EOF
{
  "session": "$SESSION",
  "project_name": "$PROJECT_NAME",
  "branch": "$BRANCH",
  "type": "$PROJECT_TYPE",
  "configuration": "$CONFIGURATION",
  "environment": "$ENV_FILE",
  "simulator": {
    "name": "$DEVICE",
    "udid": "$SIMULATOR_UDID"
  },
  "dev_port": ${DEV_PORT:-null},
  "poltergeist": $POLTERGEIST_AVAILABLE,
  "created": "$(date -Iseconds)"
}
EOF
```

### Step 10: Display Summary

```bash
echo ""
echo "✨ iOS Dev Environment Started: $SESSION"
echo ""
echo "Configuration: $CONFIGURATION ($ENV_FILE)"
echo "Simulator: $DEVICE"
[ "$POLTERGEIST_AVAILABLE" = true ] && echo "Poltergeist: Auto-rebuild enabled"
echo ""
echo "Attach: tmux attach -t $SESSION"
echo "Status: /tmux-status"
echo ""
```

## Notes

- Auto-detects: React Native, Capacitor, Native iOS
- Maps build config to environment file
- Poltergeist enables auto-rebuild on file changes
- Session persists across disconnects
