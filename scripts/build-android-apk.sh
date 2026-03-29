#!/usr/bin/env bash
set -euo pipefail

# Build ZRemote Android APK from scratch on NixOS.
#
# This script handles the full pipeline:
#   1. Cross-compile Rust FFI library for arm64 (via cargo-ndk)
#   2. Generate Kotlin bindings from host debug build (uniffi-bindgen)
#   3. Fix known UniFFI Kotlin codegen issues (Exception.message conflict)
#   4. Build debug APK (via Gradle)
#   5. Optionally install on connected device (via adb)
#
# All tools come from `nix develop` -- no manual SDK/NDK installation needed.
#
# Prerequisites:
#   - NixOS with programs.nix-ld enabled (for Android SDK pre-built binaries)
#   - nix develop shell (provides: cargo-ndk, gradle, Android SDK/NDK, Rust aarch64 target)
#   - For install: USB-connected Android device with USB debugging enabled
#
# Usage:
#   nix develop --command bash scripts/build-android-apk.sh          # build only
#   nix develop --command bash scripts/build-android-apk.sh --install  # build + install
#   nix develop --command bash scripts/build-android-apk.sh --clean    # clean + build

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ANDROID_DIR="${ROOT_DIR}/android"
JNILIBS_DIR="${ANDROID_DIR}/app/src/main/jniLibs"
BINDINGS_DIR="${ANDROID_DIR}/app/src/main/java"
SDK_DIR="${BINDINGS_DIR}/com/zremote/sdk"
PROFILE="release-android"

DO_INSTALL=false
DO_CLEAN=false

for arg in "$@"; do
    case "$arg" in
        --install) DO_INSTALL=true ;;
        --clean) DO_CLEAN=true ;;
        --help|-h)
            echo "Usage: nix develop --command bash $0 [--install] [--clean]"
            echo ""
            echo "Options:"
            echo "  --install    Install APK on connected device after build"
            echo "  --clean      Clean build directory before building"
            exit 0
            ;;
        *) echo "Unknown argument: $arg"; exit 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Validation
# ---------------------------------------------------------------------------

if [ -z "${ANDROID_HOME:-}" ]; then
    echo "Error: ANDROID_HOME not set. Run inside: nix develop --command bash $0"
    exit 1
fi

if ! command -v cargo-ndk &> /dev/null; then
    echo "Error: cargo-ndk not found. Run inside: nix develop --command bash $0"
    exit 1
fi

if ! command -v gradle &> /dev/null; then
    echo "Error: gradle not found. Run inside: nix develop --command bash $0"
    exit 1
fi

# Check nix-ld is active (not stub-ld) -- required for aapt2 and other SDK binaries
if readlink /lib64/ld-linux-x86-64.so.2 2>/dev/null | grep -q "stub-ld"; then
    echo "Error: nix-ld is not configured. Android SDK binaries won't run."
    echo ""
    echo "Add to your NixOS config:"
    echo "  programs.nix-ld.enable = true;"
    echo "  programs.nix-ld.libraries = with pkgs; [ zlib stdenv.cc.cc.lib ];"
    echo ""
    echo "Then: sudo nixos-rebuild switch"
    exit 1
fi

# ---------------------------------------------------------------------------
# Step 1: Cross-compile native library
# ---------------------------------------------------------------------------

echo ""
echo "=== Step 1: Building native library (arm64-v8a) ==="
mkdir -p "$JNILIBS_DIR"
cargo ndk -t arm64-v8a \
    -o "$JNILIBS_DIR" \
    build --profile "$PROFILE" -p zremote-ffi

SO_FILE=$(find "$JNILIBS_DIR" -name "libzremote_ffi.so" -print -quit)
echo "Built: $SO_FILE ($(du -h "$SO_FILE" | cut -f1))"

# ---------------------------------------------------------------------------
# Step 2: Generate Kotlin bindings
# ---------------------------------------------------------------------------

echo ""
echo "=== Step 2: Generating Kotlin bindings ==="

# UniFFI --library mode needs to read metadata from the .so.
# Cross-compiled .so can't be dlopen'd on the host, so we build a host debug
# library and generate bindings from that instead.
cargo build -p zremote-ffi 2>&1 | tail -3

HOST_LIB="${ROOT_DIR}/target/debug/libzremote_ffi.so"
if [ ! -f "$HOST_LIB" ]; then
    echo "Error: Host debug library not found at $HOST_LIB"
    exit 1
fi

mkdir -p "$SDK_DIR"
cargo run -p zremote-ffi --bin uniffi-bindgen generate \
    --library "$HOST_LIB" \
    --language kotlin \
    --no-format \
    --out-dir "$BINDINGS_DIR" 2>&1

if [ ! -f "${SDK_DIR}/zremote_ffi.kt" ]; then
    echo "Error: Kotlin bindings were not generated"
    exit 1
fi

echo "Generated: ${SDK_DIR}/zremote_ffi.kt ($(wc -l < "${SDK_DIR}/zremote_ffi.kt") lines)"

# ---------------------------------------------------------------------------
# Step 3: Fix UniFFI codegen issues
# ---------------------------------------------------------------------------

echo ""
echo "=== Step 3: Patching UniFFI codegen issues ==="

# Issue: UniFFI 0.29 generates `val message: String` in Exception subclass
# constructors, which conflicts with `Throwable.message`. The generated code
# also adds a redundant `override val message` getter. Fix: make the
# constructor parameter `override val` and remove the duplicate getter.
python3 - "${SDK_DIR}/zremote_ffi.kt" << 'PYEOF'
import sys

path = sys.argv[1]
with open(path, 'r') as f:
    lines = f.readlines()

result = []
i = 0
while i < len(lines):
    line = lines[i]
    # Match: val `message`: kotlin.String inside FfiException subclass
    if 'val `message`: kotlin.String' in line:
        context = ''.join(lines[max(0, i-5):min(len(lines), i+10)])
        if 'FfiException()' in context:
            line = line.replace('val `message`', 'override val `message`')
            result.append(line)
            i += 1
            # Skip the duplicate override getter that follows
            while i < len(lines):
                cur = lines[i].strip()
                if cur == 'override val message':
                    # skip this line and the get() = ... line
                    i += 1
                    if i < len(lines) and 'get()' in lines[i]:
                        i += 1
                    break
                else:
                    result.append(lines[i])
                    i += 1
            continue
    result.append(line)
    i += 1

with open(path, 'w') as f:
    f.writelines(result)

print("Patched FfiException.message conflicts")
PYEOF

# Add OVERLOAD_RESOLUTION_AMBIGUITY suppress (safety net for edge cases)
if ! grep -q "OVERLOAD_RESOLUTION_AMBIGUITY" "${SDK_DIR}/zremote_ffi.kt"; then
    sed -i 's/@file:Suppress("NAME_SHADOWING")/@file:Suppress("NAME_SHADOWING", "OVERLOAD_RESOLUTION_AMBIGUITY")/' \
        "${SDK_DIR}/zremote_ffi.kt"
    echo "Added OVERLOAD_RESOLUTION_AMBIGUITY suppress"
fi

# ---------------------------------------------------------------------------
# Step 4: Build APK
# ---------------------------------------------------------------------------

if [ "$DO_CLEAN" = true ]; then
    echo ""
    echo "=== Cleaning build directory ==="
    rm -rf "${ANDROID_DIR}/app/build"
fi

echo ""
echo "=== Step 4: Building debug APK ==="
cd "$ANDROID_DIR"
gradle assembleDebug --no-daemon --console=plain 2>&1

APK="${ANDROID_DIR}/app/build/outputs/apk/debug/app-debug.apk"
if [ ! -f "$APK" ]; then
    echo "Error: APK not found at $APK"
    exit 1
fi

echo ""
echo "APK built: $APK ($(du -h "$APK" | cut -f1))"

# ---------------------------------------------------------------------------
# Step 5: Install (optional)
# ---------------------------------------------------------------------------

if [ "$DO_INSTALL" = true ]; then
    echo ""
    echo "=== Step 5: Installing on device ==="
    if ! adb devices 2>/dev/null | grep -q "device$"; then
        echo "Error: No Android device connected. Connect via USB and enable USB debugging."
        exit 1
    fi
    adb install -r "$APK"
    echo ""
    echo "Installed. Launch ZRemote from the app drawer."
fi

echo ""
echo "=== Done ==="
