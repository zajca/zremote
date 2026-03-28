#!/bin/bash
set -euo pipefail

# Build zremote-ffi native library for Android and generate Kotlin bindings.
#
# Prerequisites:
#   - cargo-ndk: cargo install cargo-ndk
#   - Android NDK: set ANDROID_NDK_HOME
#   - Rust targets: rustup target add aarch64-linux-android
#
# Usage:
#   ./scripts/build-android.sh                    # arm64-v8a only (default)
#   ./scripts/build-android.sh --all-abis         # all 4 ABIs
#   ./scripts/build-android.sh --generate-only    # Kotlin bindings only (skip native build)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
OUTPUT_DIR="${ROOT_DIR}/android/app/src/main/jniLibs"
BINDINGS_DIR="${ROOT_DIR}/android/app/src/main/java"
PROFILE="release-android"

ALL_ABIS=false
GENERATE_ONLY=false

for arg in "$@"; do
    case "$arg" in
        --all-abis) ALL_ABIS=true ;;
        --generate-only) GENERATE_ONLY=true ;;
        --help|-h)
            echo "Usage: $0 [--all-abis] [--generate-only]"
            echo ""
            echo "Options:"
            echo "  --all-abis         Build for all Android ABIs (arm64-v8a, armeabi-v7a, x86_64, x86)"
            echo "  --generate-only    Only generate Kotlin bindings, skip native build"
            exit 0
            ;;
        *)
            echo "Unknown argument: $arg"
            exit 1
            ;;
    esac
done

if [ "$GENERATE_ONLY" = false ]; then
    # Validate environment
    if [ -z "${ANDROID_NDK_HOME:-}" ]; then
        echo "Error: ANDROID_NDK_HOME is not set."
        echo "Install Android NDK and set: export ANDROID_NDK_HOME=/path/to/ndk"
        exit 1
    fi

    if ! command -v cargo-ndk &> /dev/null; then
        echo "Error: cargo-ndk not found."
        echo "Install with: cargo install cargo-ndk"
        exit 1
    fi

    # Determine targets
    if [ "$ALL_ABIS" = true ]; then
        TARGETS="-t arm64-v8a -t armeabi-v7a -t x86_64 -t x86"
        echo "Building for all ABIs: arm64-v8a, armeabi-v7a, x86_64, x86"
    else
        TARGETS="-t arm64-v8a"
        echo "Building for arm64-v8a only (use --all-abis for all targets)"
    fi

    # Build native libraries
    echo ""
    echo "=== Building native libraries ==="
    mkdir -p "$OUTPUT_DIR"
    # shellcheck disable=SC2086
    cargo ndk $TARGETS \
        -o "$OUTPUT_DIR" \
        build --profile "$PROFILE" -p zremote-ffi

    echo ""
    echo "Native libraries built:"
    find "$OUTPUT_DIR" -name "*.so" -exec ls -lh {} \;
fi

# Generate Kotlin bindings
echo ""
echo "=== Generating Kotlin bindings ==="
mkdir -p "$BINDINGS_DIR"

# Find the built library for binding generation (any ABI works, prefer host arch)
LIB_PATH=""
for candidate in \
    "target/aarch64-linux-android/${PROFILE}/libzremote_ffi.so" \
    "target/x86_64-linux-android/${PROFILE}/libzremote_ffi.so" \
    "target/${PROFILE}/libzremote_ffi.so"; do
    if [ -f "${ROOT_DIR}/${candidate}" ]; then
        LIB_PATH="${ROOT_DIR}/${candidate}"
        break
    fi
done

if [ -z "$LIB_PATH" ]; then
    echo "Error: Could not find built libzremote_ffi.so"
    echo "Run without --generate-only first to build the native library."
    exit 1
fi

cargo run -p zremote-ffi --bin uniffi-bindgen generate \
    --library "$LIB_PATH" \
    --language kotlin \
    --out-dir "$BINDINGS_DIR"

echo ""
echo "Kotlin bindings generated:"
find "$BINDINGS_DIR" -name "*.kt" -exec ls -lh {} \;

echo ""
echo "=== Done ==="
echo "JNI libraries: $OUTPUT_DIR"
echo "Kotlin source: $BINDINGS_DIR"
