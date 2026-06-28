#!/usr/bin/env bash
# Build the Synapse Android app (arm64-v8a APK) from the Rust workspace.
#
# Slint on Android uses the android-activity backend with the Skia renderer, so
# this needs the Android NDK to compile Skia. The app crate exposes an `android`
# cargo feature that enables `slint/backend-android-activity-06`.
#
# Requirements:
#   - rustup target add aarch64-linux-android
#   - Android NDK installed, with ANDROID_NDK pointing at it, e.g.:
#       export ANDROID_NDK="$HOME/Library/Android/sdk/ndk/27.0.12077973"
#   - cargo-apk:  cargo install cargo-apk
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TARGET="aarch64-linux-android"

if [[ -z "${ANDROID_NDK:-}" ]]; then
  echo "error: ANDROID_NDK is not set. Install the NDK and export ANDROID_NDK." >&2
  echo "  (Android Studio: SDK Manager -> SDK Tools -> NDK Side by side)" >&2
  exit 1
fi
export ANDROID_NDK

# Configure the NDK linker for the target so cargo can produce a shared lib.
NDK_TOOLCHAIN="$ANDROID_NDK/toolchains/llvm/prebuilt/$(uname -s | tr A-Z a-z)-$(uname -m)"
if [[ -d "$ANDROID_NDK/toolchains/llvm/prebuilt/darwin-arm64" ]]; then
  NDK_TOOLCHAIN="$ANDROID_NDK/toolchains/llvm/prebuilt/darwin-arm64"
fi
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$NDK_TOOLCHAIN/bin/aarch64-linux-android24-clang"
export AR_aarch64_linux_android="$NDK_TOOLCHAIN/bin/llvm-ar"
export CC_aarch64_linux_android="$NDK_TOOLCHAIN/bin/aarch64-linux-android24-clang"
export CXX_aarch64_linux_android="$NDK_TOOLCHAIN/bin/aarch64-linux-android24-clang++"

cd "$ROOT"

if command -v cargo-apk >/dev/null 2>&1; then
  echo "==> Building APK with cargo-apk (arm64-v8a)..."
  cargo apk build -p synapse-app --lib --target "$TARGET" --features android --release
  echo "==> APK: $ROOT/target/$TARGET/release/apk/synapse-app.apk"
else
  echo "==> cargo-apk not found; building the Android shared library only..."
  cargo build -p synapse-app --lib --target "$TARGET" --features android --release
  echo "==> Built $ROOT/target/$TARGET/release/libsynapse_app.so"
  echo "    Install cargo-apk (cargo install cargo-apk) to package an APK."
fi
