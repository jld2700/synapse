#!/usr/bin/env bash
# Build the Synapse iOS app from a Rust workspace.
#
# This script:
#   1. Compiles the Rust app crate into an iOS arm64 static library.
#   2. (If Xcode is installed) opens the Xcode project so you can build/run on
#      a device or simulator. The Xcode "Build Rust staticlib" run-script phase
#      keeps the .a in sync on every build.
#
# Requirements:
#   - `rustup target add aarch64-apple-ios`
#   - A full Xcode installation (`xcode-select -p` must point at Xcode.app, not
#     just CommandLineTools) to link/launch the app. The Rust static-lib step
#     itself only needs the target + CommandLineTools.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="aarch64-apple-ios"
CRATE="synapse-app"

echo "==> Building $CRATE static library for $TARGET (release)"
cd "$ROOT"
cargo rustc -p "$CRATE" --lib --target "$TARGET" --crate-type staticlib --release

LIB="$ROOT/target/$TARGET/release/libsynapse_app.a"
if [[ ! -f "$LIB" ]]; then
  echo "error: expected $LIB not found" >&2
  exit 1
fi
echo "==> Built $LIB ($(du -h "$LIB" | cut -f1))"

# Detect a *full* Xcode installation (CommandLineTools alone cannot sign or
# build an .app for a device). `xcrun xcodebuild` only resolves under Xcode.app.
if xcrun xcodebuild -version >/dev/null 2>&1; then
  echo "==> Xcode detected. Building .app via xcodebuild (Release, iphoneos)..."
  TEAM_FLAG=()
  if [[ -n "${DEVELOPMENT_TEAM:-}" ]]; then
    echo "    Signing with DEVELOPMENT_TEAM=$DEVELOPMENT_TEAM"
    TEAM_FLAG=(DEVELOPMENT_TEAM="$DEVELOPMENT_TEAM" CODE_SIGN_IDENTITY="Apple Development")
  else
    echo "    DEVELOPMENT_TEAM not set; building ad-hoc (CODE_SIGN_IDENTITY='-')."
    echo "    To install on a real iPhone set DEVELOPMENT_TEAM to your Apple ID team:"
    echo "      DEVELOPMENT_TEAM=ABCDE12345 $0"
    TEAM_FLAG=(CODE_SIGN_IDENTITY="-")
  fi
  (
    cd "$ROOT/mobile/ios"
    xcodebuild \
      -project Synapse.xcodeproj \
      -scheme Synapse \
      -configuration Release \
      -sdk iphoneos \
      -derivedDataPath build \
      "${TEAM_FLAG[@]}" \
      build || echo "(xcodebuild failed; open $ROOT/mobile/ios/Synapse.xcodeproj in Xcode to run on a device.)"
  )
else
  echo "==> Xcode not found (only CommandLineTools detected)."
  echo "    The Rust static library is ready at $LIB."
  echo "    To produce an installable .app you need a full Xcode.app:"
  echo "      1. Install Xcode from the App Store."
  echo "      2. Run: sudo xcode-select -s /Applications/Xcode.app/Contents/Developer"
  echo "      3. Open $ROOT/mobile/ios/Synapse.xcodeproj in Xcode, pick your team,"
  echo "         and run on your iPhone (or: DEVELOPMENT_TEAM=XXXXX $0)."
fi
