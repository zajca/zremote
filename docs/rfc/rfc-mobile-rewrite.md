# RFC: ZRemote Mobile App Rewrite — Pure Swift via Skip Fuse

## Status: Proposed (2026-04-10)

Supersedes:
- `docs/rfc/rfc-mobile-app.md`
- `docs/rfc/rfc-mobile-app-improvements.md`
- `docs/prompts/implement-mobile-improvements.md`

PR zajca/zremote#9 (branch `worktree-rfc-mobile-update`) will be closed in favor of this plan.

## Context & Motivation

The current mobile effort on branch `worktree-rfc-mobile-update` has two parts:

1. **`crates/zremote-ffi`** — Rust UniFFI crate consumed by a Kotlin Android app.
2. **`android/`** — Kotlin / Jetpack Compose MVP. UX is broken: missing empty/error/loading states, silent exception swallowing, stub Projects screen, battery-hungry foreground service.

After two rounds of exploration the project owner made the following calls:

- **Rust FFI has not paid off.** UniFFI + mobile bindings is painful to ship, hard to debug, and every schema change ripples into Kotlin. We are done fighting it.
- **The mobile app will be 100% Swift.** No Rust bridge, no UniFFI, no FFI layer at all.
- **The `zremote-client` surface is small.** It is a REST API plus two WebSocket streams (`/ws/events`, `/ws/terminal/:id`). Porting it to Swift is cheaper than maintaining an FFI layer.
- **One Swift codebase for iOS and Android** via **Skip Fuse** — Skip compiles Swift natively for Android using the official Swift 6.3 Android SDK and maps SwiftUI to Jetpack Compose.
- **Terminal-centric UI** similar to the current GPUI desktop client: no bottom tab bar, a swipe-from-left sidebar drawer, the terminal as the main canvas, and the command palette as an overlay modal.
- **Clean slate.** Delete `android/`, delete `crates/zremote-ffi`, delete the old mobile RFCs, scripts and CI workflows.

This removes the highest-risk piece of the original plan (Rust staticlib + UniFFI + Skip Fuse, a combination with no precedent). The remaining hard unknown is **WebSocket support on Skip Fuse Android**, for which we have a concrete fallback strategy (see Phase 0 and Risks).

## Goals

- One Swift codebase that ships as a real iOS app and a real Android app.
- UX on par with the desktop GPUI client for the workflows that matter on mobile: monitor loops, drive a terminal, approve channel permissions, react to push notifications.
- Zero server changes for the initial milestones. We consume the existing REST + WS API as-is.
- Polished from day one: every screen has loading / empty / error states, no layout shifts on data arrival, no silent exception swallowing.
- Clean exit from the Rust FFI experiment so the main repo is no longer carrying dead mobile code.

## Non-Goals

- Continuing the Kotlin/Compose MVP on `worktree-rfc-mobile-update`.
- Shipping a Rust-powered mobile runtime (UniFFI, staticlib, cdylib, JNI).
- Bundled offline cache in the first milestones. The app is online-first.
- Knowledge/memory search UI in the first milestones.
- Write-path project/worktree management in the first milestones (read-only is fine).

## Architecture

```
                            mobile/
                               |
                               v
                   Package.swift (Skip Fuse SPM)
                               |
           +-------------------+-------------------+
           |                                       |
           v                                       v
    ZRemoteApp.swift                       ZRemoteKit (library targets)
    (SwiftUI @main)                                 |
           |                                        |
           v                                        v
    Views / ViewModels                    +---------+---------+
                                          |                   |
                                          v                   v
                                    ZRemoteClient       ZRemoteProtocol
                                    (REST + WS)         (Codable types)
                                          |
                                          v
                              +-----------+-----------+
                              |                       |
                              v                       v
                      URLSession (REST)         WebSocket
                                                    |
                                          +---------+----------+
                                          |                    |
                                    iOS / Android          [fallback]
                                URLSessionWebSocketTask    swift-nio
                                (preferred)                WebSocketKit
                                                           (if Skip Android
                                                            libcurl ws fails)
                                          |
                                          v
                              REST /api/* + WS /ws/events, /ws/terminal/:id
                                    (server unchanged)
```

**Key decision.** A local Swift package `ZRemoteKit` inside `mobile/` contains:

- `ZRemoteProtocol` — Codable models, ported from `crates/zremote-protocol/src/events.rs` and `crates/zremote-client/src/types.rs`.
- `ZRemoteClient` — an `actor ApiClient` with async/await REST methods over `URLSession`, plus `EventStream` and `TerminalSession` as AsyncSequence-backed WebSocket wrappers.
- Zero Rust dependencies.
- Zero platform-specific code (the same Swift files compile on iOS and Android through Skip Fuse).
- Shared between the main app target and unit tests.

**Server stays as-is.** The Swift client consumes existing `/api/*` and `/ws/*` endpoints exactly as the desktop GUI does today. Protocol compatibility is handled with strict Codable versioning and `case unknown(String)` variants on enums (matching the Rust `#[serde(other)]` pattern) plus a wire-format fixture test suite.

## UI Design — Terminal-Centric

Strongly inspired by the desktop GPUI client (`crates/zremote-gui/src/views/main_view.rs`, `views/sidebar.rs`, `views/terminal_panel.rs`, `views/command_palette/mod.rs`). The project owner explicitly rejected a bottom tab bar.

### Home screen (`HomeView.swift`)

```
+-------------------------------------+
|  [=]  term-1               [gear]   |  44pt header
|         myremote . main             |  (fades when keyboard opens)
+-------------------------------------+
|                                     |
|                                     |
|         TERMINAL CANVAS             |
|      (full-bleed, 100% width)       |
|                                     |
|     - JetBrainsMono 13pt            |
|     - pinch zoom 10-24pt            |
|     - tap & hold = select           |
|     - double tap = kbd input        |
|                                     |
|       [ 65% . $0.34 ]  <- CC badge  |  bottom-right, 8pt
|       [ connected  ]  <- conn badge |
+-------------------------------------+
|  [Q] [Esc] [Tab] [arrows] [paste]   |  44pt quick-key bar
+-------------------------------------+  (shown when input focused)
```

**Gestures**

- Swipe from left edge -> sidebar drawer (85% width, dark backdrop, tap outside to close).
- Swipe from right edge -> session detail panel (CC metrics, loop history, recent tasks, pending approval cards).
- Swipe down from header -> Command Palette modal.
- Long press on terminal -> iOS context menu (Copy / Paste / Select All / Clear).
- Swipe right on a sidebar row -> quick actions (close session, reconnect).
- Double tap on terminal -> show virtual keyboard + quick-key bar.
- Pull-down on sidebar -> refresh hosts/sessions.

### Sidebar drawer (`SidebarDrawerView.swift`)

Mirrors the structure of `views/sidebar.rs`:

```
+---------------------------+
|  ZRemote         [?] [wifi]|
+---------------------------+
|  v server-1.local  *      |
|    [proj] myremote        |
|       + New session       |
|       o term-1   (running)|
|         L claude-3.5 67%  |
|       o term-2   (done)   |
|    [proj] other-project   |
|       o term-3            |
|    (orphan sessions)      |
|       o bash-1            |
|                           |
|  v pi-home  *             |
|    [proj] homelab         |
|       o top     (live)    |
+---------------------------+
|  [+ New]     [Refresh]    |
+---------------------------+
```

### Session detail panel (`SessionDetailView.swift`)

```
+---------------------------+
|  term-1                   |
|  ~/myremote . main        |
+---------------------------+
|  LOOP                     |
|  claude-3.5-sonnet        |
|  67% context . $0.34      |
|  12,340 in / 4,210 out    |
|  +142 / -33 LOC           |
|  Rate 5h: 34% . 7d: 12%   |
+---------------------------+
|  PENDING APPROVAL         |  <- if ChannelPermissionRequested
|  ! Edit src/main.rs       |
|  [Deny]  [Approve]        |  <- calls existing endpoint
+---------------------------+
|  RECENT TASKS             |
|  + Fix tests (2m)         |
|  ~ Refactor api (pending) |
+---------------------------+
|  [View full history ->]   |
+---------------------------+
```

### Command Palette (`CommandPaletteView.swift`)

`.sheet()` with a text field on top and a result list below. Segmented control `[All | Sessions | Projects | Actions]`. Fuzzy search with drill-down (tap on Project -> create session in that project). Mirrors `views/command_palette/mod.rs`.

### Session Switcher (`SessionSwitcherView.swift`)

Full-screen modal: horizontal cards, swipe between sessions, tap to switch. Simplified: list-only on narrow screens, no live previews in the first milestone.

### Onboarding (`OnboardingView.swift`)

Welcome -> server URL input -> optional auth token -> `client.getModeInfo()` validation -> HomeView. QR-code pairing is post-MVP.

### Toast overlay (`ToastOverlay.swift`)

Notifications in the top-right corner for `WaitingForInput`, `ClaudeTaskEnded`, `WorktreeError`, `ChannelPermissionRequested`. Auto-dismiss in 3-10 s. Tap to navigate to the relevant session. Suppressed when that session is already foregrounded.

## Project Structure

```
myremote/
|- crates/                     <-- UNCHANGED
|  |- zremote-protocol/
|  |- zremote-client/          <-- kept for desktop GPUI; Swift SDK is a separate port
|  |- zremote-server/
|  |- zremote-agent/
|  |- zremote-core/
|  `- zremote-gui/
|
|- mobile/                     <-- NEW workspace
|  |- README.md
|  |- Package.swift            <-- Skip Fuse SPM (root)
|  |- ZRemote.xcodeproj/       <-- generated by `skip init`
|  |
|  |- Sources/
|  |  |- ZRemoteProtocol/          <-- Swift mirror of crates/zremote-protocol
|  |  |  |- Events.swift           <-- ServerEvent enum (26 variants, Codable)
|  |  |  |- Hosts.swift            <-- Host, HostInfo, HostStatus
|  |  |  |- Sessions.swift         <-- Session, SessionInfo, CreateSessionRequest
|  |  |  |- Projects.swift         <-- Project, Worktree, AddProjectRequest
|  |  |  |- Loops.swift            <-- AgenticLoop, LoopInfo, LoopStatus
|  |  |  |- Tasks.swift            <-- ClaudeTask, ClaudeSessionMetrics
|  |  |  |- Terminal.swift         <-- TerminalMessage variants
|  |  |  |- Knowledge.swift        <-- Memory, SearchResult, KnowledgeBase
|  |  |  `- Ids.swift              <-- typed wrappers: HostId, SessionId, LoopId, ProjectId
|  |  |
|  |  |- ZRemoteClient/            <-- Swift port of crates/zremote-client
|  |  |  |- ApiClient.swift        <-- actor with async methods (60+)
|  |  |  |- Endpoints.swift        <-- URL builders, headers, token
|  |  |  |- Transport.swift        <-- URLSession wrapper + retry policy
|  |  |  |- EventStream.swift      <-- AsyncSequence over /ws/events, auto-reconnect
|  |  |  |- TerminalSession.swift  <-- bidirectional /ws/terminal/:id, AsyncStream
|  |  |  |- WebSocketKit.swift     <-- abstraction over URLSessionWebSocketTask
|  |  |  |                              + swift-nio fallback (see Phase 0)
|  |  |  `- Errors.swift           <-- ApiError enum (per domain)
|  |  |
|  |  `- ZRemote/                  <-- main app
|  |     |- ZRemoteApp.swift       <-- @main App
|  |     |- AppState.swift         <-- @Observable root state, holds ApiClient
|  |     |- ConnectionManager.swift
|  |     |- Persistence.swift      <-- SwiftData for MRU sessions, UserDefaults for server URL
|  |     |
|  |     |- Views/
|  |     |  |- Home/
|  |     |  |  |- HomeView.swift
|  |     |  |  |- TerminalCanvasView.swift
|  |     |  |  |- TerminalStatusBadge.swift
|  |     |  |  |- ConnectionBadge.swift
|  |     |  |  `- QuickKeyBarView.swift
|  |     |  |- Sidebar/
|  |     |  |  |- SidebarDrawerView.swift
|  |     |  |  |- HostSectionView.swift
|  |     |  |  |- ProjectSectionView.swift
|  |     |  |  `- SessionRowView.swift
|  |     |  |- Detail/
|  |     |  |  |- SessionDetailView.swift
|  |     |  |  |- CcMetricsPanel.swift
|  |     |  |  |- PendingApprovalCard.swift
|  |     |  |  `- RecentTasksList.swift
|  |     |  |- Overlays/
|  |     |  |  |- CommandPaletteView.swift
|  |     |  |  |- SessionSwitcherView.swift
|  |     |  |  |- HelpSheet.swift
|  |     |  |  `- ToastOverlay.swift
|  |     |  |- Onboarding/
|  |     |  |  |- OnboardingFlow.swift
|  |     |  |  `- ServerConnectionView.swift
|  |     |  `- Settings/
|  |     |     |- SettingsView.swift
|  |     |     `- NotificationPrefsView.swift
|  |     |
|  |     |- Terminal/
|  |     |  |- TerminalEmulator.swift     <-- protocol, state machine (VT100)
|  |     |  |- AnsiParser.swift           <-- SGR, cursor, erase, alternate screen
|  |     |  |- GridCell.swift             <-- Cell struct (char, fg, bg, attrs)
|  |     |  |- TerminalGrid.swift         <-- grid + scrollback
|  |     |  |- GlyphCache.swift           <-- LRU per (char, style) -> shaped glyph
|  |     |  |- CanvasTerminalView.swift   <-- SwiftUI Canvas renderer (iOS + Android via Skip)
|  |     |  `- InputHandler.swift         <-- keyboard -> byte stream
|  |     |
|  |     |- Theme/
|  |     |  |- Theme.swift                <-- colors, fonts, spacing
|  |     |  `- Icons.swift                <-- SF Symbols -> Material Icons mapping
|  |     |
|  |     `- Shared/
|  |        |- EventObserver.swift        <-- @Observable, subscribes to EventStream
|  |        |- ErrorBanner.swift
|  |        |- EmptyState.swift
|  |        |- LoadingState.swift
|  |        `- ErrorState.swift
|  |
|  |- Tests/
|  |  |- ZRemoteProtocolTests/
|  |  |  |- EventCodableTests.swift       <-- decode/encode wire format
|  |  |  `- ForwardCompatTests.swift      <-- unknown variants, new fields
|  |  |- ZRemoteClientTests/
|  |  |  |- ApiClientTests.swift          <-- mock HTTPServer
|  |  |  |- EventStreamTests.swift        <-- mock WS
|  |  |  `- TerminalSessionTests.swift
|  |  |- ZRemoteTests/
|  |  |  |- AppStateTests.swift
|  |  |  |- EventObserverTests.swift
|  |  |  `- TerminalEmulatorTests.swift   <-- xterm vttest subset
|  |  `- Fixtures/
|  |     `- wire_format/                  <-- JSON samples captured from the real server
|  |
|  |- Darwin/
|  |  |- Info.plist
|  |  |- ZRemote.entitlements
|  |  `- AppIcon.appiconset/
|  |
|  |- Android/
|  |  |- AndroidManifest.xml.override
|  |  |- res/
|  |  `- src/main/google-services.json    <-- .gitignore
|  |
|  `- Resources/
|     `- JetBrainsMono-Regular.ttf
|
|- scripts/
|  |- mobile-dev.sh              <-- shortcuts: ios-sim, android-emu, test, lint
|  |- mobile-spike-ws.sh         <-- Phase 0 WebSocket spike runner
|  `- mobile-release.sh          <-- versioning, archive, IPA + AAB
|
|- docs/
|  `- rfc/
|     `- rfc-mobile-rewrite.md   <-- this RFC
|
`- .github/
   `- workflows/
      `- mobile-build.yml        <-- build + test + sign + artifact (IPA + AAB)
```

### What to delete (Phase 1)

- `crates/zremote-ffi/` — entire crate (was purely for mobile Kotlin bindings).
- `android/` — the whole Kotlin/Compose project.
- `docs/rfc/rfc-mobile-app.md`
- `docs/rfc/rfc-mobile-app-improvements.md`
- `docs/prompts/implement-mobile-improvements.md`
- `docs/android-build.md`
- `scripts/build-android.sh`, `scripts/build-android-apk.sh`
- `scripts/patch-uniffi-bindings.py`
- `.github/workflows/android-build.yml`
- References to `zremote-ffi` in `Cargo.toml` workspace members.
- `[profile.release-android]` from root `Cargo.toml` (no longer needed).

## Implementation Phases

### Phase 0 — WebSocket spike (BLOCKING)

**Goal.** Verify that `URLSessionWebSocketTask` works on Skip Fuse Android. This is the only serious unknown — REST, Codable and Swift Concurrency are known to work, but `URLSessionWebSocketTask` on swift-corelibs-foundation under Android depends on libcurl ws/wss, which is disabled in many distributions and has known crash invariants (see swift-corelibs-foundation#4730).

Steps:

1. Install Swift 6.3 toolchain + Skip CLI + Android NDK r27+ + Swift Android SDK via `swift sdk install`.
2. `skip init --appid=dev.zajca.zremote.spike --fuse mobile/spike/`
3. Add a simple WebSocket echo test:
   ```swift
   let task = URLSession.shared.webSocketTask(with: URL(string: "wss://echo.websocket.events")!)
   task.resume()
   try await task.send(.string("hello"))
   let msg = try await task.receive()
   print(msg)
   ```
4. Run on a **physical Android device** (not just an emulator) — some crashes are device-specific.
5. Run on an iOS simulator as a sanity check that the spike app builds at all.
6. Run against a real local agent (`cargo run -p zremote -- agent local --port 3000`), connect to `/ws/events` and verify that the first `ClientEvent::Connected` arrives.
7. Longer test: 5-minute running connection, reconnect after dropping the network, parallel terminal + events streams.

**Three possible outcomes.**

- **GREEN — `URLSessionWebSocketTask` works on Android.** Continue with Foundation WS — the simplest path.
- **YELLOW — `URLSessionWebSocketTask` works partially** (e.g. crashes on certain messages, no ping/pong). Implement a `WebSocketKit.swift` abstraction with a `WebSocketTransport` protocol and two implementations: `FoundationWebSocket` (iOS) and `SwiftNIOWebSocket` (Android fallback via `swift-nio` + `swift-nio-extras` / `WebSocketKit`).
- **RED — `URLSessionWebSocketTask` does not work.** Go straight to the `swift-nio` fallback on **both** platforms — slightly more code, but a uniform stack with no libcurl dependency.

**Success criteria.** The spike keeps a live WS connection on a physical Android device for at least 5 minutes, correctly decodes a `ServerEvent::HostConnected` JSON payload, and reconnects after the network drops.

Only after the spike is green/yellow/red can we delete the spike directory and move on to Phase 1.

### Phase 1 — Clean slate

1. Write this RFC (done in this PR).
2. **Delete** everything in the "What to delete" list above.
3. `cargo build --workspace` must still pass — verify that removing `zremote-ffi` did not break anything else (doc-tests, feature gates).
4. Run `cargo test --workspace` — verify that the existing 1401 tests still pass.
5. Commit as: "Remove Rust FFI mobile crate and Kotlin Android app".

### Phase 2 — ZRemoteProtocol Swift package (Codable types)

Port `crates/zremote-protocol/` and `crates/zremote-client/src/types.rs` to Swift. Rules:

1. Every Rust `serde(tag = "type")` enum -> Swift `enum` implementing `Codable` with a custom `init(from:)` switching on the `type` field.
2. UUIDs -> Swift typed wrapper structs (`struct HostId: Hashable, Codable { let raw: String }`) to prevent accidental mix-ups.
3. Timestamps -> `Date` with a custom `ISO8601DateFormatter`.
4. `Option<T>` -> Swift `T?`.
5. `Vec<u8>` -> `Data`.
6. Forward compatibility: every enum has a `case unknown(String)` variant, matching `#[serde(other)]`.
7. New fields added by the server -> use optional `decodeIfPresent` with defaults.

**Wire-format tests.** Capture real JSON samples from a running server into `Tests/Fixtures/wire_format/` (one file per ServerEvent variant) and test round-trip decode/encode. This is the non-negotiable safety net against protocol drift.

Key types to port (~40 structs/enums):

- `Host`, `HostInfo`, `HostStatus`
- `Session`, `SessionInfo`, `SessionStatus`, `CreateSessionRequest`, `CreateSessionResponse`
- `Project`, `Worktree`, `AddProjectRequest`, `CreateWorktreeRequest`
- `AgenticLoop`, `LoopInfo`, `LoopStatus`
- `ClaudeTask`, `ClaudeTaskStatus`, `ClaudeSessionMetrics`
- `Memory`, `KnowledgeBase`, `SearchRequest`, `SearchResult`, `MemoryCategory`, `SearchTier`
- `DirectoryEntry`, `ExecutionNode`
- `ServerEvent` enum (26 variants)
- `TerminalEvent` enum (11 variants)
- `ConfigValue`, `ModeInfo`

Reference files:

- `crates/zremote-protocol/src/events.rs` (ServerEvent + bookkeeping)
- `crates/zremote-client/src/types.rs` (all domain types, 607 lines)
- `crates/zremote-core/src/db/*` (for enum representations in SQL -> JSON wire format)

### Phase 3 — ZRemoteClient Swift package (REST + WebSocket)

Port `crates/zremote-client/src/client.rs` (1304 lines, 60+ methods) to Swift as an `actor ApiClient`:

1. **Construction pattern**:
   ```swift
   public actor ApiClient {
       public let baseURL: URL
       private let token: String?
       private let session: URLSession

       public init(baseURL: URL, token: String? = nil) { ... }

       public func listHosts() async throws -> [Host] {
           let req = endpoint(.listHosts)
           return try await send(req)
       }

       public func listSessions(hostId: HostId) async throws -> [Session] { ... }
       public func createSession(hostId: HostId, request: CreateSessionRequest) async throws -> CreateSessionResponse { ... }
       // ... 60+ more
   }
   ```
2. **Transport.** `Transport.swift` encapsulates `URLSession`, a retry policy (3 attempts with exponential backoff for 5xx/network errors), JSON encode/decode, and error routing (404 -> `.notFound`, 401/403 -> `.unauthorized`, etc.).
3. **Errors.** Per-domain `enum ApiError: Error { case notFound, unauthorized, conflict(String), network(Error), decoding(Error), server(Int, String) }` with `LocalizedError` conformance for the UI.
4. **EventStream.** `AsyncSequence` over WS — internally holds a `Task` that reads messages, decodes them, and yields into an `AsyncStream.Continuation`. Auto-reconnect with exponential backoff (1s-30s) and 25% jitter, matching the Rust implementation in `crates/zremote-client/src/events.rs:151`.
5. **TerminalSession.** Bidirectional WS. Separate AsyncStreams for output (from the server) and a struct with `send(input:)`, `resize(cols:, rows:)`, `paste(image:)` methods. UTF-8 safe chunking for input, binary frames for terminal data. Reference implementation in `crates/zremote-client/src/terminal.rs:546`.
6. **WebSocketKit abstraction.** Protocol-based switching between `URLSessionWebSocketTask` and the `swift-nio` implementation chosen by the Phase 0 result. Enables unit tests via a mock transport.

**Unit tests.** A lightweight mock HTTP server (stdlib `URLSession` + a custom handler) plus a mock WS echo server. Tests cover: every REST method (request serialization, response deserialization, error handling), the EventStream reconnect policy, and TerminalSession input/output chunking.

### Phase 4 — App skeleton + core screens

1. `skip init --appid=dev.zajca.zremote --fuse mobile/` (in the root, after the spike directory is removed).
2. Link the local `ZRemoteProtocol` and `ZRemoteClient` packages from `Package.swift`.
3. `AppState` (`@Observable`) holds:
   - `client: ApiClient?` (nil before onboarding)
   - `hosts: [Host]`, `sessions: [HostId: [Session]]`, `loops: [LoopId: AgenticLoop]`, `tasks: [TaskId: ClaudeTask]`
   - `activeSessionId: SessionId?`
   - `connectionStatus: ConnectionStatus`
4. `EventObserver` subscribes to `client.events()` and updates `AppState` per `ServerEvent` variant.
5. `OnboardingFlow` — server URL input, `/api/mode` validation, persistence to `UserDefaults`.
6. `HomeView` skeleton with a placeholder terminal (solid colored rectangle for now).
7. `SidebarDrawerView` driven by `appState.hosts` and `appState.sessions`.
8. Navigation: `DragGesture` with horizontal translation opens/closes the drawers.
9. `CommandPaletteView` as `.sheet(isPresented:)` with fuzzy match.
10. Loading / empty / error reusable components in `Shared/` — used consistently from the start (unlike the old Kotlin app where only HostListScreen had them).

### Phase 5 — Terminal rendering (one renderer for both platforms)

**Key decision.** Instead of platform-specific adapters (SwiftTerm on iOS, a Compose canvas on Android), write one shared SwiftUI `Canvas`-based renderer. Reasons:

- SwiftTerm depends on UIKit/CoreText/CoreGraphics — it will **not** build through Skip Fuse on Android, even if we only port the parser.
- SwiftUI `Canvas` primitives are mapped to Compose `Canvas` by SkipFuseUI, so the same Swift file runs on both.
- Rewriting ~800 lines of a VT100 parser (ported from `crates/zremote-gui/src/terminal/`) is cheaper than maintaining two renderers.

Structure (in `Sources/ZRemote/Terminal/`):

1. `AnsiParser.swift` — state machine: Ground -> Escape -> CSI -> SGR, supporting:
   - SGR 0-107 (reset, bold, dim, italic, underline, reverse, 8/256/true color)
   - CUP / CUU / CUD / CUF / CUB (cursor movement)
   - ED / EL (erase in display / line)
   - DECSTBM (scroll region)
   - DECSET 1049 (alternate screen buffer)
   - SCS (character set switching — minimum required)
   - OSC 0/2 (window title -> callback for the toolbar)
   - UTF-8 multibyte decoder with error handling (invalid -> U+FFFD)
2. `GridCell.swift` — `struct Cell { char: Character, fg: Color, bg: Color, attrs: CellAttrs }`, `CellAttrs: OptionSet` with bold/dim/italic/underline/reverse.
3. `TerminalGrid.swift` — 2D grid + ring-buffer scrollback (cap 10k lines on mobile), cursor state, dirty region tracking.
4. `GlyphCache.swift` — LRU cache `(Character, CellAttrs, fg, bg) -> shaped glyph`, analogous to the desktop GlyphCache.
5. `CanvasTerminalView.swift` — SwiftUI `Canvas { ctx, size in ... }` draws the cells. Dirty lines from `TerminalGrid` are rendered through the glyph cache. Cursor drawn on top. Scroll via `ScrollView`, pinch zoom via `MagnificationGesture`.
6. `InputHandler.swift` — hidden `TextField` captures the system keyboard, maps key events to VT byte sequences (enter -> `\r`, escape -> `\x1b`, arrows -> CSI A/B/C/D, Tab -> `\t`).
7. `TerminalEmulator.swift` — top-level `actor TerminalEmulator`, holds grid + parser, fed bytes from `TerminalSession.output`, exposes `@Observable` state for the Canvas view.

**Unit tests.** A subset of `xterm vttest` (selected representative categories — cursor movement, attributes, colors). Fixtures in `Tests/Fixtures/vt100/`.

**Performance target.** 60fps under 100 lines/s output, smooth scrollback with 10k lines. Benchmark in `Tests/ZRemoteTests/TerminalPerfTests.swift`.

### Phase 6 — Events, notifications, background

1. `EventObserver` full implementation — per-session state updates, toast triggers.
2. **Pending approval UI.** `PendingApprovalCard` in `SessionDetailView` reacts to `ChannelPermissionRequested` events and calls the existing endpoint `POST /api/sessions/:id/channel/permission/:request_id` (see `crates/zremote-client/src/client.rs:1174-1254`). Because this endpoint **already exists** on the server (unlike the original Kotlin plan where it was missing), approve/deny works without any server work.
3. **iOS push-to-wake.**
   - APNs silent-push registration.
   - New server endpoint `POST /api/notifications/register` with body `{device_token, platform, preferences}` — **a separate server RFC** (see "Server prerequisites" below).
   - `BGAppRefreshTask` for opportunistic event-buffer refresh.
4. **Android FCM high-priority data messages.**
   - Skip Fuse bridge to FCM (Skip offers `SkipFirebaseMessaging` or a manual JNI import).
   - `FirebaseMessagingService` for token registration.
5. **Foreground WebSocket lifecycle.** In the foreground, `EventStream` + any `TerminalSession` run at full tilt. In the background we disconnect WS and rely on push notifications.
6. **Notification preferences screen.** Toggles per event type (loop ended, task ended, pending approval, host disconnected, worktree error).

**Server prerequisites for Phase 6** (separate RFC, blocks mobile Phase 6 only):

- Endpoints `POST /api/notifications/register` and `DELETE /api/notifications/register/:token`.
- SQLite table `notification_registrations (id, device_token, platform, preferences_json, created_at)`.
- Server module `crates/zremote-server/src/notifications/` — APNs HTTP/2 and FCM HTTP v1 dispatcher, with rate limiting (max 1 notification per user per minute per type).
- Event routing: `ChannelPermissionRequested`, `ClaudeTaskEnded`, `AgenticLoopEnded`, `WorktreeError` trigger push dispatch.

This is a separate server RFC, not part of the mobile RFC. Mobile Phase 6 can start only once the server endpoints exist.

### Phase 7 — Polish, a11y, tests

1. VoiceOver / TalkBack labels (Skip maps SwiftUI `.accessibilityLabel` to Compose semantics).
2. Dynamic Type for UI text (the terminal stays monospace but has its own zoom).
3. Dark mode as primary, light mode as opt-in.
4. Haptics via `SensoryFeedback` (iOS) / Compose haptics (Android).
5. Swift unit tests for `AppState`, `EventObserver`, `TerminalEmulator`, every `ApiClient` method, and Codable round-trips.
6. Snapshot tests for the main views (`ViewInspector` or Skip's preview framework).
7. E2E tests: spin up a local agent, connect the app, verify the happy path for every main flow.
8. Review agents (mandatory before merge):
   - `code-reviewer` — the whole `mobile/` workspace (architecture, dead code, missing wiring).
   - `security-reviewer` — token handling, TLS validation, no secrets in logs, URLSession config (no self-signed accept).
9. Fix **all** review findings before merge (no "fix later" TODOs).

## Key Reference Files

| Purpose | File |
|---|---|
| REST API methods (port to Swift) | `crates/zremote-client/src/client.rs` (1304 lines, 60+ methods) |
| Domain types (port to Swift) | `crates/zremote-client/src/types.rs` (607 lines) |
| Wire-format ServerEvent | `crates/zremote-protocol/src/events.rs` |
| EventStream reconnect logic | `crates/zremote-client/src/events.rs:151` |
| TerminalSession WS handling | `crates/zremote-client/src/terminal.rs:546` |
| ApiError patterns | `crates/zremote-client/src/error.rs:130` |
| Desktop layout reference | `crates/zremote-gui/src/views/main_view.rs:1269-1540` |
| Desktop sidebar structure | `crates/zremote-gui/src/views/sidebar.rs:1133-1540` |
| Desktop terminal panel | `crates/zremote-gui/src/views/terminal_panel.rs:1125-1737` |
| Desktop command palette | `crates/zremote-gui/src/views/command_palette/mod.rs` |
| Desktop session switcher | `crates/zremote-gui/src/views/session_switcher.rs` |
| CC widgets & metrics | `crates/zremote-gui/src/views/cc_widgets.rs` |
| Terminal parser reference | `crates/zremote-gui/src/views/terminal_element.rs` |
| Desktop theme | `crates/zremote-gui/src/theme.rs` |
| swift-nio WebSocket fallback | `github.com/apple/swift-nio` + `swift-nio-extras/NIOWebSocket` |
| Skip Fuse reference apps | `github.com/skiptools/skipapp-showcase-fuse`, `github.com/skiptools/skipapp-bookings-fuse` |

## Verification Protocol

### Phase 0 spike verification

```bash
./scripts/mobile-spike-ws.sh
# Launches the spike app on an iOS simulator and a physical Android device.
# Runs for 5 minutes against echo.websocket.events + a local zremote agent.
# Reports: successful message count, reconnect count, latency p50/p99.
```

### ZRemoteProtocol wire-format tests

```bash
cd mobile && swift test --filter ZRemoteProtocolTests
# Decodes JSON fixtures captured from the server, verifies round-trips,
# fails loudly on schema drift.
```

### ZRemoteClient mock tests

```bash
cd mobile && swift test --filter ZRemoteClientTests
# Mock HTTP + WS server, verifies all 60+ methods, reconnect, error handling.
```

### Full app E2E against a real server

```bash
# Terminal 1: run the agent
cargo run -p zremote -- agent local --port 3000

# Terminal 2: iOS
cd mobile && skip run --simulator=iPhone-16

# Terminal 3: Android
cd mobile && skip run --device=<android-device-id>

# Manual checks:
# 1. Open the app, onboarding -> http://localhost:3000.
# 2. Verify hosts list renders (pull-to-refresh works).
# 3. Tap a session -> terminal opens, ANSI colors, cursor blinks.
# 4. Swipe from left -> sidebar drawer, tap another session -> switches.
# 5. Swipe from right -> session detail, CC metrics update live.
# 6. Trigger a pending permission from CLI -> toast appears, PendingApprovalCard
#    shows up, tap Approve -> works.
# 7. Swipe down from header -> Command Palette, fuzzy search -> tap a session.
# 8. Background the app -> foreground -> state is consistent (reconnect happened).
# 9. Kill the server -> connection badge turns red, app does not crash, error banner.
# 10. Restart the server -> auto-reconnect, badge turns green.
```

### Review checklist (blocks merge)

- [ ] `code-reviewer` completed, all findings resolved.
- [ ] `security-reviewer` completed, token never logged, TLS validated correctly.
- [ ] Every screen has loading / empty / error variants (and tests cover them).
- [ ] No layout shifts when data arrives.
- [ ] VoiceOver / TalkBack reads every interactive element.
- [ ] 60fps terminal scroll under load (100 lines/s benchmark).
- [ ] Memory does not grow during a 60-minute soak test.
- [ ] Swift test coverage >= 70% on `ZRemoteProtocol` + `ZRemoteClient`.

## Risks & Mitigations

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| `URLSessionWebSocketTask` on Skip Fuse Android does not work | Medium | Forces swift-nio fallback | Phase 0 spike; if it fails, fall back to `swift-nio` / `WebSocketKit` via the `WebSocketTransport` abstraction. Known issue: swiftlang/swift-corelibs-foundation#4730. |
| SwiftUI `Canvas` performance on Android via Skip | Medium | Slow terminal render | Benchmark in Phase 5. Fallback: `UIViewRepresentable` (iOS) + `AndroidView` bridge (Android) with native text stacks. |
| Swift Foundation on Android has gaps | Medium | Missing APIs | Skip Fuse docs say "many URLRequest/URLSessionConfiguration properties ignored" — test headers, timeouts, TLS custom CA in Phase 2. |
| Skip Fuse production readiness (2026-04) | High | Unknown bugs | Track skip.dev Discord, validate regularly against the showcase apps, be ready to upgrade Skip mid-flight. |
| iOS background WebSocket | High | No real-time in background | Push-to-wake via APNs; full real-time only in the foreground — accepted. |
| Forward-compat protocol drift | Low | Crash on new event | `case unknown(String)` variants everywhere, wire-format tests, CI fails on schema drift. |
| Apple App Store review | Low | Delay | Skip Fuse iOS build is standard SwiftUI; Apple does not care about Android. |
| Google Play Store review | Low | Delay | Skip Fuse produces a standard AAB. |
| SwiftData on Skip Fuse Android | Medium | Forces a different persistence layer | Fallback: `UserDefaults` for settings only, no local cache in MVP (offline mode is post-MVP). |
| Terminal VT parser completeness | Medium | Some escape sequences fail | Start with the xterm vttest subset, add sequences incrementally as real use cases appear. |

## Plan B (if Phase 0 fails even with swift-nio)

If Skip Fuse on Android cannot keep a reliable WebSocket connection even via swift-nio (extremely unlikely, but possible):

**Fallback: dual-native without Rust FFI.**

1. `ios/` — SwiftUI app + `ZRemoteClient` Swift package (exactly as in this RFC, iOS only).
2. `android/` — Jetpack Compose app + a hand-written Kotlin/OkHttp client consuming the same REST + WS endpoints.
3. Shared artifacts: the **protocol definition** (JSON wire format) and the UX design — not the code.
4. More total work, but removes the Skip Fuse dependency.

Plan B is a backup. The project owner prefers the pure Swift path, so we pursue it first.

## Open Questions

1. **Server notification infrastructure** (APNs + FCM dispatcher) — a separate RFC; blocks mobile Phase 6 but not earlier phases. When do we write it?
2. **Knowledge / memory search UI** — P2 scope, outside this RFC?
3. **Projects / worktrees management** — at minimum read-only in Phase 4; write operations (add project, create worktree) post-MVP?
4. **Store presence** — TestFlight + internal track only for now, or set up public listings already?
5. **Crash reporting** — Sentry via a Skip bridge, or server-side logs only? Recommendation: Sentry from Phase 4.
6. **SwiftData vs. manual UserDefaults persistence** — depends on the Phase 0 result (whether SwiftData works on Skip Fuse Android).
7. **`ZRemoteKit` as a separate open-source Swift package** — long-term it could live outside the myremote repo and be reusable by a CLI client, web playground, etc. For now we keep it monorepo for iteration speed.

## Next Step

Merge this RFC, close PR zajca/zremote#9, start Phase 0 (WebSocket spike). Phase 1 (clean slate deletion) lands immediately after the spike result is in.
