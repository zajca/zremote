# Phase 0 WebSocket spike toolchain.
#
# Produces a reproducible Swift 6.3 + Swift Android SDK environment capable of
# cross-compiling a Swift executable for `aarch64-unknown-linux-android28`.
#
# We deliberately do NOT install Skip CLI in this image. The spike is a CLI
# executable run via `adb shell /data/local/tmp/spike`, not a wrapped APK. Skip
# Fuse + OkHttp wrapping is reserved for Phase 4 when we actually build the app.
#
# Build:
#   docker build -t zremote-mobile-spike:6.3 mobile/spike
# Use (driven by ../scripts/mobile-spike-ws.sh):
#   docker run --rm -v $(pwd)/mobile/spike:/workspace -w /workspace \
#     zremote-mobile-spike:6.3 \
#     swift build -c release --swift-sdk aarch64-unknown-linux-android28
FROM swift:6.3-noble

# The swift:*-noble base image installs curl during bootstrap and then purges
# it to keep the image small. We also need a JDK for any future Android tooling
# (aapt, d8, apksigner) even though the bare spike does not invoke them.
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        curl \
        ca-certificates \
        build-essential \
        python3 \
        openjdk-21-jdk-headless \
        file \
    && rm -rf /var/lib/apt/lists/*

# Android NDK r27d LTS, pinned to the SHA1 published on developer.android.com.
# The SHA1 is the only checksum Google publishes for NDK archives.
ARG NDK_VERSION=r27d
ARG NDK_SHA1=22105e410cf29afcf163760cc95522b9fb981121
ENV ANDROID_NDK_HOME=/opt/android-ndk-${NDK_VERSION}
RUN curl -fsSLO "https://dl.google.com/android/repository/android-ndk-${NDK_VERSION}-linux.zip" && \
    echo "${NDK_SHA1}  android-ndk-${NDK_VERSION}-linux.zip" | sha1sum -c - && \
    unzip -q "android-ndk-${NDK_VERSION}-linux.zip" -d /opt && \
    rm "android-ndk-${NDK_VERSION}-linux.zip" && \
    test -d "${ANDROID_NDK_HOME}"

# Swift Android SDK 6.3. `swift sdk install` downloads the artifactbundle,
# validates the checksum, unpacks it into ~/.swiftpm/swift-sdks/, and runs the
# bundle's setup script which links itself against $ANDROID_NDK_HOME. After
# install we verify `swift sdk list` reports the bundle.
ARG SWIFT_SDK_URL=https://download.swift.org/swift-6.3-release/android-sdk/swift-6.3-RELEASE/swift-6.3-RELEASE_android.artifactbundle.tar.gz
ARG SWIFT_SDK_SHA256=2f2942c4bcea7965a08665206212c66991dabe23725aeec7c4365fc91acad088
RUN swift sdk install "${SWIFT_SDK_URL}" --checksum "${SWIFT_SDK_SHA256}" && \
    swift sdk list

# CRITICAL: having ANDROID_NDK_ROOT set at `swift build` time breaks the Swift
# Android SDK's sysroot resolution (known issue, finagolfin on swift forums).
# We only needed ANDROID_NDK_HOME during `swift sdk install`. Clear it for
# builds.
ENV ANDROID_NDK_ROOT=""

# Sanity probes baked into the image — visible if someone runs the image
# interactively to debug.
RUN swift --version && \
    echo "--- swift sdk list ---" && swift sdk list && \
    echo "--- NDK ---" && ls "${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt" && \
    echo "--- artifactbundle ---" && ls /root/.swiftpm/swift-sdks

WORKDIR /workspace

# Default to a dry-run `swift sdk list`; the runner script overrides this with
# the real `swift build` invocation.
CMD ["swift", "sdk", "list"]
