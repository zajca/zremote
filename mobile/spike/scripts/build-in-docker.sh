#!/usr/bin/env bash
# Runs `swift build` for the Android target inside the spike Docker image.
# Invoked with the spike directory mounted at /workspace. Produces a release
# binary at .build/aarch64-unknown-linux-android28/release/WsSpike plus the
# runtime .so files we need to ship alongside it on the device.
#
# This script assumes it is running INSIDE the `zremote-mobile-spike:6.3`
# container — do not run it on the host.
set -euo pipefail

TARGET_TRIPLE="aarch64-unknown-linux-android28"
OUT_DIR="/workspace/out"

echo "[build-in-docker] swift version: $(swift --version | head -1)"
echo "[build-in-docker] swift sdk list:"
swift sdk list
echo "[build-in-docker] ANDROID_NDK_ROOT=${ANDROID_NDK_ROOT:-<unset>} ANDROID_NDK_HOME=${ANDROID_NDK_HOME:-<unset>}"

# Finagolfin warning: having ANDROID_NDK_ROOT set at build time breaks the
# Swift Android SDK sysroot resolution. The Dockerfile clears it, but be
# defensive in case someone passes -e ANDROID_NDK_ROOT=... when running.
unset ANDROID_NDK_ROOT

cd /workspace
swift build -c release --swift-sdk "${TARGET_TRIPLE}"

BIN_PATH=".build/${TARGET_TRIPLE}/release/WsSpike"
if [[ ! -f "${BIN_PATH}" ]]; then
    echo "[build-in-docker] FATAL: expected ${BIN_PATH} was not produced"
    ls -R .build || true
    exit 1
fi

mkdir -p "${OUT_DIR}"
cp -f "${BIN_PATH}" "${OUT_DIR}/WsSpike"
file "${OUT_DIR}/WsSpike"

# Collect the runtime .so files from the installed Swift Android SDK. The
# layout is:
#   ~/.swiftpm/swift-sdks/<bundle>.artifactbundle/<bundle>/swift-android-sysroot/usr/lib/aarch64-linux-android/
SDK_BASE="$(find /root/.swiftpm/swift-sdks -maxdepth 1 -type d -name '*android*' | head -1)"
if [[ -z "${SDK_BASE}" ]]; then
    echo "[build-in-docker] FATAL: could not locate Swift Android SDK under /root/.swiftpm/swift-sdks"
    ls /root/.swiftpm/swift-sdks || true
    exit 1
fi
echo "[build-in-docker] SDK base: ${SDK_BASE}"

# The sysroot directory name has varied across 6.2/6.3; search for it rather
# than hardcoding.
SYSROOT_LIB="$(find "${SDK_BASE}" -type d -path '*aarch64-linux-android*' -name 'lib' | head -1)"
if [[ -z "${SYSROOT_LIB}" ]]; then
    # Fall back to the Swift-side runtime shared libs.
    SYSROOT_LIB="$(find "${SDK_BASE}" -type d -name 'aarch64-linux-android' | head -1)"
fi
echo "[build-in-docker] sysroot lib dir: ${SYSROOT_LIB:-<none>}"

if [[ -n "${SYSROOT_LIB}" ]]; then
    mkdir -p "${OUT_DIR}/lib"
    # Copy every swift runtime .so we can find. Don't try to be too clever —
    # shipping extras is harmless on /data/local/tmp.
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libswift*.so' -exec cp -f {} "${OUT_DIR}/lib/" \;
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libBlocks*.so' -exec cp -f {} "${OUT_DIR}/lib/" \; || true
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libFoundation*.so' -exec cp -f {} "${OUT_DIR}/lib/" \; || true
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libdispatch*.so' -exec cp -f {} "${OUT_DIR}/lib/" \; || true
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libicu*.so' -exec cp -f {} "${OUT_DIR}/lib/" \; || true
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libxml2*.so' -exec cp -f {} "${OUT_DIR}/lib/" \; || true
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libcurl*.so' -exec cp -f {} "${OUT_DIR}/lib/" \; || true
    find "${SYSROOT_LIB}" -maxdepth 3 -type f -name 'libc++_shared.so' -exec cp -f {} "${OUT_DIR}/lib/" \; || true
    echo "[build-in-docker] shipped runtime libs:"
    ls -l "${OUT_DIR}/lib" || true
else
    echo "[build-in-docker] WARN: could not locate sysroot lib dir; runner will attempt to run without shipping runtime libs"
fi

# Also grab libc++_shared.so straight from the NDK — Swift for Android links
# against it as a runtime dep.
NDK_CXX="$(find "${ANDROID_NDK_HOME}" -path '*aarch64-linux-android*libc++_shared.so' 2>/dev/null | head -1 || true)"
if [[ -n "${NDK_CXX}" ]]; then
    mkdir -p "${OUT_DIR}/lib"
    cp -f "${NDK_CXX}" "${OUT_DIR}/lib/"
    echo "[build-in-docker] shipped NDK libc++_shared.so from ${NDK_CXX}"
fi

echo "[build-in-docker] OK: artifacts in ${OUT_DIR}"
