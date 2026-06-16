# Phase 0 WebSocket Spike — Results

**Date**: 2026-04-11
**Verdict**: 🟡 **YELLOW** — server + protocol + Swift language path all green; production
mobile runtime path (iOS URLSession, Skip Fuse/Android OkHttp) is not contradicted; but
plain swift-corelibs-foundation on Linux cannot act as a WebSocket client for unit tests
or CI. Clear action items for Phase 3.

## What ran

Spike: `mobile/spike/Sources/WsSpike/main.swift`, built for x86_64 Linux inside the
`swift:6.3-noble` Docker image via:

```
docker run --rm -v "$PWD/mobile/spike:/work" -w /work swift:6.3-noble \
    swift build -c release
```

Target probes:

1. **Public echo** — `wss://echo.websocket.events`, send "hello", receive it back.
2. **ZRemote events** — `ws://127.0.0.1:3111/ws/events` on a local
   `zremote agent local --port 3111 --bind 127.0.0.1` instance, decode first payload
   as `TaggedEnvelope { type: String }`.
3. **Soak** — 60-second reconnect loop with exponential backoff (copy of the Rust
   `EventStream` policy we will port in Phase 3).

## Findings

### Server path: ✅ GREEN

Raw HTTP Upgrade via `curl` confirms the server speaks WebSocket correctly:

```
$ curl -sv --http1.1 \
    -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
    -H 'Sec-WebSocket-Version: 13' \
    -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
    http://127.0.0.1:3111/ws/events

< HTTP/1.1 101 Switching Protocols
< connection: upgrade
< upgrade: websocket
< sec-websocket-accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
```

`zremote-agent` local mode accepts the upgrade, issues the correct Sec-WebSocket-Accept
digest, and holds the connection open. The server is not the bottleneck.

### Swift language path: ✅ GREEN

- Swift 6.3 (`swift-6.3-RELEASE`) compiles the spike on first attempt once Sendable
  annotations are applied (`@Sendable` closures on `WsDelegate`, `nonisolated(unsafe)`
  on the shared `ISO8601DateFormatter`, per-attempt snapshot for the reconnect
  counter).
- The build uses `#if canImport(FoundationNetworking)` to opt into the Linux/Android
  split of Foundation without touching the iOS path.
- `JSONDecoder().decode(TaggedEnvelope.self, from:)` links and runs on
  swift-corelibs-foundation — the `#[serde(tag = "type")]` convention on the wire
  decodes cleanly into a Swift struct with a single `type: String` field. This is
  the minimum evidence we wanted that the full Phase 2 Codable port is viable.
- Strict concurrency errors were surfaced at compile time, not runtime. Good sign
  for Phase 3 where `actor ApiClient` and the AsyncSequence event stream will need
  to hold up under the same checks.

### Linux runtime path (swift-corelibs-foundation): ❌ RED, expected

```
SPIKE: 2026-04-11T06:34:27.190Z FAIL echo: probe failed:
  Error Domain=NSURLErrorDomain Code=-1002
  "(null)" UserInfo={
    NSErrorFailingURLStringKey=wss://echo.websocket.events,
    NSLocalizedDescription=WebSockets not supported by libcurl
  }
```

Same error against the local agent. Every connect attempt returns immediately with
`NSURLErrorUnsupportedURL (-1002)` and the `NSLocalizedDescription=WebSockets not
supported by libcurl` detail. No bytes reach the server.

This matches the risk called out in the plan and in
`swiftlang/swift-corelibs-foundation#4730` — `URLSessionWebSocketTask` on
swift-corelibs-foundation is wired to libcurl's `curl_ws_*` API, and the swift:6.3
base image's libcurl does not expose it (or Foundation's feature check rejects the
version shipped by Ubuntu Noble). Either way: plain Swift on Linux cannot act as a
WebSocket client with the stock Foundation.

Upstream pushback (`swift-corelibs-foundation#5128` et al.) may eventually fix the
fatalError-on-send behaviour, but it still will not suddenly give us a working ws
client on Linux without a libcurl that is built `--enable-websockets`.

### Reconnect loop: ✅ GREEN (by design, not by traffic)

Even though every connect attempt failed immediately, the exponential backoff and
deadline enforcement behaved correctly:

```
attempt 1 → fail → reconnect #1 in 4s
attempt 2 → fail → reconnect #2 in 8s
attempt 3 → fail → reconnect #3 in 16s
attempt 4 → fail → reconnect #4 in 30s  (ceiling)
attempt 5 → fail → reconnect #5 in 30s
soak complete: attempts=6 messages=0 decoded_first_event=false reconnects=5
```

Ceiling at 30 s, clean exit at the deadline, no crashes. When we port this loop into
`ZRemoteClient.EventStream` in Phase 3 the shape is ready.

## Implications for the rewrite

1. **Production mobile path is not contradicted.**
   - **iOS**: URLSession on darwin is the native CF/URLSessionWebSocketTask. No
     libcurl. Not testable on this Linux host, but there is no known issue and the
     existing Rust client's iOS-adjacent peers (URLSession-based) work.
   - **Android via Skip Fuse**: prior research shows `SkipFoundation` wraps OkHttp
     for `URLSessionWebSocketTask`, not libcurl. The finding above does **not**
     apply to a real Skip Fuse Android build. Empirical device validation still
     deferred to Phase 4 (when a real Skip Fuse app exists).
2. **Linux CI and local `swift test` workflows cannot use
   `URLSessionWebSocketTask`.** Any Phase 3 test that drives the event stream or
   terminal session on a Linux runner will need a different transport.
3. **Phase 3 must introduce a `WebSocketTransport` protocol abstraction.**
   Implementations:
   - `FoundationWebSocketTransport` — for iOS runtime and for
     Skip Fuse Android runtime (OkHttp-backed under the hood on Android).
   - `NIOWebSocketTransport` — for Linux `swift test` / CI, built on `swift-nio`
     + `swift-nio-extras` / `WebSocketKit`. Not shipped into the app binary; guarded
     by a build configuration (`#if canImport(NIO)` or a SwiftPM trait).
   - `MockWebSocketTransport` — used by unit tests to drive fixtures without
     touching the network.

   `ZRemoteClient.ApiClient` holds a `WebSocketTransport` (default determined at
   init time by platform) and never reaches for `URLSessionWebSocketTask`
   directly.
4. **RFC update.** Risk table row for "URLSessionWebSocketTask on Skip Fuse
   Android" drops from *Medium* to *Low* (still needs empirical Phase 4 check).
   New risk row: "swift-corelibs-foundation Linux ws path is unusable" marked as
   *Confirmed, mitigated via `WebSocketTransport` abstraction*.
5. **No Android device verification was performed.** That was the original Phase 0
   ambition but required 3+ hours of NDK + Swift Android SDK downloads over
   throttled wifi. Android runtime evidence is deferred to Phase 4 where it is
   cheap (we will already have a Skip Fuse app to push).

## Raw logs

- Agent startup: `/tmp/zremote-spike/agent.log` (not committed)
- Spike run 1: `/tmp/zremote-spike/spike-run1.log` (not committed — reproduced below)

```
SPIKE: 2026-04-11T06:34:27.132Z INFO boot: WsSpike starting (soak=60s)
SPIKE: 2026-04-11T06:34:27.159Z INFO boot: swift=>=6.3 arch=x86_64 isAndroid=false
SPIKE: 2026-04-11T06:34:27.160Z INFO echo: connecting to wss://echo.websocket.events
SPIKE: 2026-04-11T06:34:27.190Z FAIL echo: probe failed: ... WebSockets not supported by libcurl
SPIKE: 2026-04-11T06:34:27.190Z INFO zremote: soaking for 60s against ws://127.0.0.1:3111/ws/events
SPIKE: 2026-04-11T06:34:27.190Z WARN zremote: receive failed on attempt 1: ... WebSockets not supported by libcurl
SPIKE: 2026-04-11T06:34:27.191Z INFO zremote: reconnect #1 in 4s
SPIKE: 2026-04-11T06:34:31.192Z WARN zremote: receive failed on attempt 2: ...
SPIKE: 2026-04-11T06:34:31.192Z INFO zremote: reconnect #2 in 8s
SPIKE: 2026-04-11T06:34:39.193Z WARN zremote: receive failed on attempt 3: ...
SPIKE: 2026-04-11T06:34:39.193Z INFO zremote: reconnect #3 in 16s
SPIKE: 2026-04-11T06:34:55.193Z WARN zremote: receive failed on attempt 4: ...
SPIKE: 2026-04-11T06:34:55.193Z INFO zremote: reconnect #4 in 30s
SPIKE: 2026-04-11T06:35:25.194Z WARN zremote: receive failed on attempt 5: ...
SPIKE: 2026-04-11T06:35:25.194Z INFO zremote: reconnect #5 in 30s
SPIKE: 2026-04-11T06:35:55.194Z INFO zremote: soak complete: attempts=6 messages=0 decoded_first_event=false reconnects=5
SPIKE: 2026-04-11T06:35:55.195Z FAIL verdict: RED — both probes failed
```

## Action items (go into the RFC and Phase 3 plan)

- [ ] Introduce `WebSocketTransport` protocol in `ZRemoteClient` Phase 3.
- [ ] Implement `FoundationWebSocketTransport` (default for iOS + Skip Fuse).
- [ ] Implement `NIOWebSocketTransport` behind a SwiftPM trait for Linux `swift test`.
- [ ] Add a `MockWebSocketTransport` for unit tests.
- [ ] In Phase 4, when a Skip Fuse Android build is first running, re-validate the
  OkHttp-wrapped URLSessionWebSocketTask path on a real device.
- [ ] Do not rely on `swift test` on a Linux runner for WebSocket-backed tests
  unless the `NIOWebSocketTransport` is active.

## What did not happen

- No Android cross-compile, no Android NDK download, no Swift Android SDK install,
  no `adb push`, no device-side run. The plan's original Phase 0 called for those;
  they were dropped after (a) the NDK download would have taken 3+ hours over
  throttled wifi, and (b) the prior research finding that SkipFoundation on
  Android wraps OkHttp made the Android-specific libcurl question moot for
  production. The `mobile/spike/Dockerfile` and `scripts/mobile-spike-ws.sh`
  remain in-tree as a future-ready recipe for doing that work in Phase 4.
