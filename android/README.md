# ZRemote Android App

Android client for monitoring ZRemote hosts, terminal sessions, agentic loops, and Claude tasks.

## Features

- **Host monitoring** -- view all connected hosts with online/offline status
- **Session management** -- browse terminal sessions per host
- **Agentic loop tracking** -- real-time loop status with color-coded indicators
- **Claude task monitoring** -- view active tasks, cost, and token usage
- **Terminal viewer** -- read-only terminal output with ANSI color support + quick-command input
- **Push notifications** -- background alerts for loop completions, errors, and permission requests
- **Dark theme** -- matches the desktop GPUI client

## Prerequisites

- Android Studio Ladybug (2024.2) or newer
- JDK 17+
- Android SDK 35
- Android NDK 27.x (for building native library)
- Rust toolchain with `aarch64-linux-android` target
- `cargo-ndk` (`cargo install cargo-ndk`)

## Building the Native Library

The app requires the Rust FFI library (`libzremote_ffi.so`) and generated Kotlin bindings.

### 1. Set up Rust for Android

```bash
# Add Android target
rustup target add aarch64-linux-android

# Install cargo-ndk
cargo install cargo-ndk

# Set NDK path (adjust to your installation)
export ANDROID_NDK_HOME=$HOME/Android/Sdk/ndk/27.2.12479018
```

### 2. Build native library + generate bindings

From the repository root:

```bash
./scripts/build-android.sh
```

This produces:
- `android/app/src/main/jniLibs/arm64-v8a/libzremote_ffi.so` -- native library
- `android/app/src/main/java/com/zremote/sdk/` -- generated Kotlin bindings

For all ABIs (arm64, armv7, x86_64, x86):

```bash
./scripts/build-android.sh --all-abis
```

### 3. Build the Android app

```bash
cd android
./gradlew assembleDebug
```

The APK will be at `android/app/build/outputs/apk/debug/app-debug.apk`.

## Installing Without Google Play

### Option A: Direct APK install (sideload)

1. Build the debug APK:
   ```bash
   cd android && ./gradlew assembleDebug
   ```

2. Transfer the APK to your phone:
   ```bash
   adb install app/build/outputs/apk/debug/app-debug.apk
   ```

   Or copy the APK file to your phone and open it in a file manager.

3. Enable "Install from unknown sources" when prompted on the device.

### Option B: Direct install via ADB

Connect your phone via USB with developer mode enabled:

```bash
cd android
./gradlew installDebug
```

### Option C: Release APK (smaller, optimized)

1. Create a signing key (one-time):
   ```bash
   keytool -genkey -v -keystore zremote-release.keystore \
     -alias zremote -keyalg RSA -keysize 2048 -validity 10000
   ```

2. Create `android/local.properties`:
   ```properties
   sdk.dir=/path/to/Android/Sdk
   ```

3. Create `android/keystore.properties`:
   ```properties
   storeFile=../zremote-release.keystore
   storePassword=your_password
   keyAlias=zremote
   keyPassword=your_password
   ```

4. Build release APK:
   ```bash
   cd android && ./gradlew assembleRelease
   ```

5. Install:
   ```bash
   adb install app/build/outputs/apk/release/app-release.apk
   ```

## Usage

### First Launch

1. Open the app -- you'll see the **Hosts** tab (empty, not connected)
2. Go to **Settings** tab (bottom right)
3. Enter your ZRemote server URL (e.g., `http://192.168.1.100:3000`)
4. Tap **Connect**
5. Navigate back to **Hosts** -- your hosts should appear

### Navigation

| Tab | What it shows |
|-----|---------------|
| **Hosts** | All registered hosts with online/offline status. Tap a host to see its sessions. |
| **Loops** | Active and recent agentic loops across all hosts. Tap for details. |
| **Tasks** | Claude tasks with status, cost, and token usage. |
| **Settings** | Server URL configuration and connection status. |

### Terminal Viewer

1. Go to **Hosts** > tap a host > tap a session
2. Terminal output is displayed with ANSI color support
3. Use the **quick-command bar** at the bottom:
   - `Ctrl+C` -- send interrupt
   - `Tab` -- send tab
   - `Esc` -- send escape
   - `Up`/`Down` -- arrow keys
   - Text field -- type commands, press Send

### Background Notifications

When the app is in the background, you'll receive notifications for:

| Event | Priority | Description |
|-------|----------|-------------|
| Loop completed | Normal | Agentic loop finished successfully |
| Loop error | High | Agentic loop encountered an error |
| Permission request | High | A tool call needs your approval |
| Task completed | Normal | Claude task finished |
| Task error | High | Claude task failed |
| Host disconnected | Low | A host lost connection |

To enable background notifications, grant the "Notifications" permission when prompted.

## Architecture

```
ZRemoteClient (Rust via UniFFI)
       |
       v
ConnectionManager (singleton)
  |-- ZRemoteEventRepository (real-time events via WebSocket)
  |-- SettingsRepository (DataStore persistence)
       |
       v
ViewModel (Hilt, per-screen)
       |
       v
Compose Screen (Material 3 dark theme)
```

The native Rust SDK (`zremote-ffi`) handles all networking:
- REST API calls via `reqwest`
- WebSocket event stream with auto-reconnect
- WebSocket terminal I/O with binary frame support

Kotlin receives data through UniFFI-generated callback interfaces (`EventListener`, `TerminalListener`).

## Troubleshooting

### "Not connected" on Hosts screen
- Go to Settings, verify the server URL is correct
- Make sure the ZRemote server is running and reachable from your phone
- Check that your phone is on the same network as the server

### No notifications
- Check Android Settings > Apps > ZRemote > Notifications
- Ensure notification channels are enabled
- Grant POST_NOTIFICATIONS permission (Android 13+)

### Terminal not showing output
- Verify the session is in "active" status
- Check that the host is online
- The terminal uses a read-only ANSI viewer -- some escape sequences may not render

### Build fails with "libzremote_ffi.so not found"
- Run `./scripts/build-android.sh` from the repo root first
- Check that `ANDROID_NDK_HOME` is set correctly
- Verify `android/app/src/main/jniLibs/arm64-v8a/libzremote_ffi.so` exists
