# Android Build Guide (NixOS)

Building the ZRemote Android app on NixOS requires special handling because Android SDK ships pre-built x86_64 Linux binaries (aapt2, zipalign, d8) that expect a standard FHS filesystem layout.

## Quick Start

```bash
nix develop --command bash scripts/build-android-apk.sh --install
```

This builds everything from scratch and installs on a connected phone.

## Prerequisites

### 1. NixOS: programs.nix-ld

Android SDK binaries are dynamically linked against `/lib64/ld-linux-x86-64.so.2`. On NixOS, this is a stub that rejects execution. `programs.nix-ld` replaces the stub with a real dynamic linker.

```nix
# In your NixOS configuration:
programs.nix-ld = {
  enable = true;
  libraries = with pkgs; [
    zlib            # libz.so - used by aapt2, d8
    stdenv.cc.cc.lib  # libstdc++.so - used by various SDK tools
  ];
};
```

Then `sudo nixos-rebuild switch`.

**Without this, every Gradle build fails** with:
```
AAPT2 Daemon startup failed
NixOS cannot run dynamically linked executables intended for generic linux environments
```

### 2. nix develop shell

The `flake.nix` provides everything else:

| Tool | Source | Purpose |
|------|--------|---------|
| Rust + aarch64-linux-android target | rust-overlay | Cross-compile native library |
| cargo-ndk | nixpkgs | Wraps cargo for Android NDK cross-compilation |
| Android SDK (platforms, build-tools) | nixpkgs androidenv | Android compilation toolchain |
| Android NDK 27.x | nixpkgs androidenv | C/C++ cross-compiler for native code |
| Gradle 8.x | nixpkgs | Android build system |
| JDK 21 | nixpkgs (via nix shell) | Required by Gradle and Android toolchain |

### 3. Phone setup (for --install)

1. Settings > About phone > tap "Build number" 7 times
2. Settings > System > Developer options > enable "USB debugging"
3. Connect USB, confirm authorization dialog on phone
4. Verify: `adb devices` shows your device as "device" (not "unauthorized")

## Build Pipeline

The build has 4 stages. The script `scripts/build-android-apk.sh` runs all of them.

### Stage 1: Cross-compile Rust FFI (cargo-ndk)

```bash
cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build --profile release-android -p zremote-ffi
```

Produces `jniLibs/arm64-v8a/libzremote_ffi.so` (~4 MB). This is the native library loaded at runtime via JNA.

### Stage 2: Generate Kotlin bindings (uniffi-bindgen)

```bash
# Build host-platform debug library (needed because uniffi-bindgen can't read cross-compiled .so metadata)
cargo build -p zremote-ffi

# Generate Kotlin from host library
cargo run -p zremote-ffi --bin uniffi-bindgen generate \
    --library target/debug/libzremote_ffi.so \
    --language kotlin \
    --out-dir android/app/src/main/java
```

Produces `android/app/src/main/java/com/zremote/sdk/zremote_ffi.kt` (~8000 lines).

**Why host debug build?** UniFFI's `--library` mode loads the .so and reads embedded metadata via `dlopen`. A cross-compiled arm64 .so can't be loaded on x86_64. The debug build includes full metadata; release builds may strip it.

### Stage 3: Patch UniFFI codegen bugs

UniFFI 0.29 has a known issue where generated Kotlin Exception subclasses declare `val message: String` in the constructor, which conflicts with `Throwable.message` (inherited from `kotlin.Exception`). The script patches this automatically.

Before:
```kotlin
class Http(val `message`: kotlin.String) : FfiException() {
    override val message
        get() = "message=${ `message` }"
}
```

After:
```kotlin
class Http(override val `message`: kotlin.String) : FfiException() {
}
```

### Stage 4: Gradle build

```bash
cd android && gradle assembleDebug --no-daemon
```

Produces `android/app/build/outputs/apk/debug/app-debug.apk`.

## What Can Go Wrong

### "AAPT2 Daemon startup failed"

nix-ld is not configured or missing libraries. See Prerequisites section.

### "Crate not found in libzremote_ffi.so" (uniffi-bindgen)

You're trying to generate bindings from the cross-compiled .so. Use the host debug build instead (Stage 2).

### Empty `com/zremote/sdk/` after binding generation

Same cause -- uniffi-bindgen silently produces nothing when it can't read metadata. Check that `target/debug/libzremote_ffi.so` exists and was built for the host platform.

### "Unresolved reference: FfiError"

The generated bindings use `FfiException` (not `FfiError`). If app code references `FfiError`, update it to `FfiException`.

### ClassNotFoundException on device

Manifest uses relative class names (`.app.ZRemoteApp`) that get prefixed with the namespace (`com.zremote.app`), resulting in `com.zremote.app.app.ZRemoteApp`. Use fully qualified names in AndroidManifest.xml.

### Permission denied on build directory

If Docker was used for a previous build attempt, files may be owned by root. Fix with `sudo chown -R $USER:users android/`.

## Manual Steps

### Install APK
```bash
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
```

### View crash logs
```bash
adb logcat -s AndroidRuntime | grep -A 20 "FATAL"
```

### Release build

Requires signing key. See `android/README.md` for keystore setup.

## Files

| File | Purpose |
|------|---------|
| `scripts/build-android-apk.sh` | Full build pipeline script |
| `scripts/build-android.sh` | Native library + binding generation only |
| `android/app/src/main/jniLibs/` | Cross-compiled .so (gitignored) |
| `android/app/src/main/java/com/zremote/sdk/` | Generated UniFFI bindings (gitignored) |
| `crates/zremote-ffi/` | Rust FFI crate (source of truth) |
| `crates/zremote-ffi/uniffi.toml` | UniFFI config (package name, cdylib name) |
