#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
derived_data="${1:-/tmp/ainb-fleet-macos-e02-release-derived-data}"
binary="$derived_data/Build/Products/Release/AINBFleet.app/Contents/MacOS/AINBFleet"

xcodebuild build \
  -project "$repo_root/apps/ainb-fleet-macos/AINBFleet.xcodeproj" \
  -scheme AINBFleet \
  -configuration Release \
  -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath "$derived_data" \
  CODE_SIGNING_ALLOWED=NO ARCHS=arm64 ONLY_ACTIVE_ARCH=YES

test -x "$binary"
! strings "$binary" | rg -F -- '--fleet-test-read-range'
! strings "$binary" | rg -F -- '--fleet-test-open-window'
