# Phase 0 — WebSocket spike

> **Status (2026-04-11):** Ran as a **Linux x86_64 native binary** inside the
> stock `swift:6.3-noble` Docker image, against a local `zremote agent local`.
> Verdict: 🟡 YELLOW. Full write-up: `RESULTS.md`. Scope was trimmed: the
> Android cross-compile (originally the whole point of this spike) was dropped
> because (a) the NDK + Swift Android SDK download would take 3+ hours on the
> available uplink, and (b) prior research already shows that SkipFoundation
> wraps OkHttp on Android so the plain swift-corelibs-foundation question is
> moot for the production path. The `Dockerfile`, `scripts/build-in-docker.sh`,
> and `../scripts/mobile-spike-ws.sh` stay in tree as a ready recipe for Phase 4
> on-device validation.

A headless Swift executable that was originally to be cross-compiled for
`aarch64-unknown-linux-android28`. Purpose: verify empirically that a Swift
client can keep a WebSocket connection alive against a real ZRemote agent
before we commit to the full RFC Phase 1+.

This spike is intentionally **not a Skip Fuse app**. It does not contain a UI,
an `Android/` manifest, or any Skip bridging. We reserve that work for Phase 4,
when we actually build the app. The spike only answers one question: "does the
Swift WebSocket client layer work on Android?".

## Why not an APK via Skip CLI?

Skip CLI on Linux has preliminary support limited to framework projects and the
`skip android` SDK frontend. Full app project creation still requires macOS. For
a Phase 0 spike this is irrelevant — a native CLI executable run via
`adb shell /data/local/tmp/spike` is cheaper, faster to iterate on, and tests
the exact network stack (`FoundationNetworking.URLSessionWebSocketTask`) that
`SkipFoundation` wraps with OkHttp on Android anyway.

If the CLI spike succeeds, the Skip Fuse OkHttp-backed path in Phase 4 will be
at least as robust (OkHttp is the battle-tested Android WebSocket library). If
the CLI spike fails on anything more subtle than "swift-corelibs-foundation on
Android is shaky", we still have the `SkipFoundation` OkHttp fallback ready.

## Research findings that shaped this spike

- **SkipFoundation uses OkHttp** on Android for `URLSessionWebSocketTask`
  (see `github.com/skiptools/skip-foundation/Sources/SkipFoundation/URLSessionTask.swift`).
  Skip's on-device WebSocket is not swift-corelibs-foundation — it is a thin
  Swift wrapper over `okhttp3.WebSocket` + `okhttp3.WebSocketListener`. This
  means the original Phase 0 risk ("libcurl ws/wss disabled in Android libc")
  does not apply once we are inside a real Skip app. For the spike we still
  exercise `FoundationNetworking` directly to find out whether we can also use
  the plain Swift Android SDK path (cheaper to build, faster to iterate on).
- **swift-corelibs-foundation#4730** was closed by PR #5128 (Nov 2024). The
  fix removes the `fatalError` on unsupported `URLSessionWebSocketTask`, but
  the underlying libcurl dependency still needs `ws,wss` built in. Android
  NDK's shipped libcurl does not.
- **Swift 6.3 was released 2026-03-24** with a stable Swift Android SDK. The
  artifactbundle is installed with a single `swift sdk install` invocation.
  Minimum supported Android API is 28.

## Dev environment

- Host: NixOS 26.05, Linux x86_64. No macOS. No iOS simulator.
- Host has Docker (`docker --version` >= 29.3), `adb` from the nix Android SDK,
  and NDK r27.2 under `$ANDROID_HOME/ndk`. We do NOT reuse the nix NDK —
  inside Docker we install a fresh r27d LTS with a known SHA1, to keep the
  build reproducible.
- Toolchain lives inside Docker image `zremote-mobile-spike:6.3`:
  - Base: `library/swift:6.3-noble` (Swift 6.3 on Ubuntu 24.04).
  - Plus: Android NDK r27d LTS from `dl.google.com`.
  - Plus: Swift Android SDK artifactbundle from `download.swift.org`.
  - Plus: a few apt packages re-added on top of the slim Swift base
    (`curl`, `ca-certificates`, `build-essential`, `python3`,
    `openjdk-21-jdk-headless`).
- `ANDROID_NDK_ROOT` is **unset** before each `swift build` — having it set
  breaks the Swift Android SDK's sysroot resolution (finagolfin's warning on
  the Swift forums). We only set `ANDROID_NDK_HOME` at install time for
  `setup-android-sdk.sh`.
- Build target triple: `aarch64-unknown-linux-android28`.

## What the spike measures

1. **Build survivability.** Does a minimal `URLSessionWebSocketTask` spike
   actually link against `FoundationNetworking` when cross-compiled with
   `swift build -c release --swift-sdk aarch64-unknown-linux-android28`?
   If this fails, we immediately know Phase 0 is RED on the direct-Swift path
   and that `SkipFoundation`'s OkHttp wrapper is mandatory.
2. **Public echo smoke test.** Can the binary open `wss://echo.websocket.events`,
   send `hello`, receive `hello` back? Confirms DNS, TLS, upgrade handshake,
   text frames.
3. **Real ZRemote smoke test.** Can the binary open
   `ws://<host>:3000/ws/events` against `cargo run -p zremote -- agent local`
   and decode the first tagged JSON `ServerEvent`?
4. **5-minute stability.** Does the connection survive 5 minutes of idle
   traffic + normal event volume without crashing?
5. **Reconnect after network drop.** Kill the agent, restart it, the spike
   must detect the disconnect and reconnect with backoff.

The spike prints JSON-ish log lines prefixed with `SPIKE:` so
`scripts/mobile-spike-ws.sh` can grep them out of logcat / adb shell output.

## Verdicts

`mobile/spike/RESULTS.md` captures the empirical verdict after running the
spike. One of:

- **GREEN** — direct `FoundationNetworking.URLSessionWebSocketTask` works on
  Swift Android SDK 6.3. Phase 1 can proceed on the simplest path; Skip
  `SkipFoundation` OkHttp wrapper is a bonus, not required.
- **YELLOW** — partial: handshake works but frames, ping/pong, or long-running
  stability fails. Phase 4 must go through `SkipFoundation` exclusively.
- **RED** — nothing works at all at the direct-Swift layer. Phase 4 is forced
  through `SkipFoundation` / OkHttp. No change to the RFC, just confirms that
  the Skip wrapper is load-bearing.

All three outcomes unblock Phase 1 (clean slate deletion of `zremote-ffi` and
`android/`). The RED path only restricts *how* we write the client in Phase 3.

## Files

- `Dockerfile` — reproducible toolchain image.
- `Package.swift` — a single executable target `WsSpike`.
- `Sources/WsSpike/main.swift` — the actual spike.
- `scripts/build-in-docker.sh` — used by the host runner to invoke
  `swift build` inside the container.
- `../scripts/mobile-spike-ws.sh` — host-side runner that orchestrates docker
  build + adb push + adb shell + logcat tail + exit code.
- `RESULTS.md` — written after the first successful run. Green/yellow/red
  verdict + raw numbers.

## How to run (summary)

```bash
# 1. Start a ZRemote agent on the host so the spike has something to talk to.
cargo run -p zremote -- agent local --port 3000

# 2. In another terminal, run the spike end to end.
#    (builds Docker image on first run — ~1.5 GB, ~10 min)
./scripts/mobile-spike-ws.sh

# Flags:
#   --skip-build      do not rebuild the Docker image
#   --device <id>     target a specific adb device
#   --no-run          build only, do not push/run
#   --minutes N       override the 5-minute soak duration
```

## Out of scope (do NOT add here)

- A Skip Fuse app project (that is Phase 4).
- The full Codable port of `ServerEvent` (that is Phase 2 — the spike only
  decodes the `type` tag to prove JSON parsing is wired up).
- The actor-based `ApiClient` port (that is Phase 3).
- iOS validation — there is no iOS simulator on the Linux dev host. Run the
  same spike on macOS CI when we have access. iOS is known to work (Apple
  URLSession is a shipping product) — this spike is exclusively about
  verifying the Android half of the Skip Fuse assumption.
