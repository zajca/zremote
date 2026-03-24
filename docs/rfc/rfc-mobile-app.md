# RFC: ZRemote Mobile App

## Status: Idea / Exploration

## Context & Motivation

ZRemote currently has a native GPUI desktop client and a web-accessible server. A mobile client (Android, optionally iOS) would enable:

- Monitoring agentic loops and terminal sessions on the go
- Receiving push notifications for loop completions, errors, permission requests
- Quick actions: approve/deny tool calls, view transcripts, check host status
- Lightweight terminal viewing (read-only or limited input)

The existing codebase is written entirely in Rust. The key question is: **how much code can be reused for mobile, and what's the best architecture?**

## Prerequisite: `zremote-client` SDK Crate

Before building any mobile app, extract a shared **`zremote-client`** SDK crate that all GUI clients (desktop GPUI, mobile, future CLI tools) depend on. This eliminates code duplication and ensures consistent API behavior across all frontends.

### What to Extract from `zremote-gui`

The desktop GUI currently contains platform-independent networking code that belongs in a shared crate:

| Source file | What to extract | Notes |
|---|---|---|
| `zremote-gui/src/api.rs` | `ApiClient` (REST client) | 178 lines. reqwest-based. Zero GPUI dependencies. Move as-is. |
| `zremote-gui/src/types.rs` | All API types (`Host`, `Session`, `Project`, `ServerEvent`, `TerminalServerMessage`, etc.) | 195 lines. Pure serde structs. Move as-is. |
| `zremote-gui/src/events_ws.rs` | `run_events_ws()` (event stream with auto-reconnect) | 59 lines. Uses tokio-tungstenite + flume. Move as-is. |
| `zremote-gui/src/terminal_ws.rs` | `TerminalWsHandle`, `connect()`, terminal WS protocol | 206 lines. Binary frame parsing, scrollback buffering. Move as-is. |

### SDK Crate Structure

```
crates/zremote-client/
  Cargo.toml          # deps: reqwest, tokio, tokio-tungstenite, serde, serde_json,
                      #       futures-util, flume, tracing, uuid, chrono
                      # depends on: zremote-protocol
  src/
    lib.rs            # Re-exports
    api.rs            # ApiClient - REST endpoints
    types.rs          # Host, Session, Project, ServerEvent, Terminal messages
    events.rs         # run_events_ws() - event stream with reconnect
    terminal.rs       # TerminalWsHandle, connect() - terminal WS I/O
    error.rs          # ApiError (currently in api.rs)
```

### Dependency Graph After Extraction

```
zremote-protocol          (types only, no runtime)
       │
       ▼
zremote-client            (SDK: REST + WS client, platform-independent)
       │
  ┌────┼────────┐
  ▼    ▼        ▼
 GUI  Mobile   CLI tools
(GPUI) (UniFFI/  (future)
       Dioxus/
       etc.)
```

### Migration Plan

1. Create `crates/zremote-client/` with dependencies from above
2. Move `api.rs`, `types.rs`, `events_ws.rs`, `terminal_ws.rs` from GUI to SDK
3. GUI becomes a thin presentation layer: `zremote-gui` depends on `zremote-client`
4. All `use crate::types::*` in GUI views change to `use zremote_client::*`
5. No behavior change — purely structural refactor

### Channel Abstraction

Currently the code uses `flume` channels for tokio-to-GUI communication. The SDK should keep `flume` as the default (it works on all platforms including mobile), but the channel types are part of the public API:

```rust
// zremote-client/src/terminal.rs
pub struct TerminalWsHandle {
    pub input_tx: flume::Sender<Vec<u8>>,
    pub output_rx: flume::Receiver<TerminalEvent>,
    pub resize_tx: flume::Sender<(u16, u16)>,
    pub image_paste_tx: flume::Sender<String>,
}
```

This works for both GPUI (current) and mobile frameworks (Dioxus reads from flume, UniFFI can wrap in callback). If a future client needs `tokio::sync::mpsc` instead, a feature flag or generic channel trait can be added later.

### What Stays in GUI

| Module | Why it stays |
|---|---|
| `main.rs` | GPUI Application launch, CLI parsing |
| `app_state.rs` | GPUI-specific state (Entity, tokio handle) |
| `theme.rs` | GPUI color palette |
| `icons.rs` | GPUI SVG icon system |
| `assets.rs` | rust-embed AssetSource for GPUI |
| `views/` | All GPUI views (sidebar, terminal panel, terminal element) |

### Benefits

- **Single source of truth** for API interactions — bug fixes propagate to all clients
- **Mobile app starts with a working SDK** — no need to rewrite networking
- **Testable independently** — SDK can have integration tests against a real server
- **UniFFI-ready** — SDK types are simple serde structs, trivial to annotate with `#[uniffi::export]`
- **Future-proof** — CLI tools, TUI clients, or web frontends can all use the same SDK

---

## Existing Code Reuse Analysis

### Directly Reusable (platform-independent)

| Crate / Module | Reuse | Dependencies | Notes |
|---|---|---|---|
| `zremote-protocol` | **100%** | serde, uuid, chrono | All message types, enums, IDs. Zero platform assumptions. |
| `zremote-gui/api.rs` | **95%** | reqwest, tokio | REST client (ApiClient). Thin wrapper, directly extractable. |
| `zremote-gui/types.rs` | **95%** | serde | Host, Session, Project, ServerEvent structs. Plain data types. |
| `zremote-core/queries/` | **50-80%** | sqlx (SQLite) | SQL queries are portable. sqlx works on mobile with SQLite. |
| `zremote-core/state.rs` | **70%** | serde, tokio | SessionState, AgenticLoopState types. Drop DashMap/Axum parts. |

### Not Reusable

| Module | Reason |
|---|---|
| `zremote-gui` (GPUI views) | GPUI is desktop-only. Complete UI rebuild needed. |
| `zremote-core/error.rs` | Axum-specific (IntoResponse). Needs decoupling. |
| `zremote-agent` | PTY/tmux, process tree BFS, hooks. Platform-specific binary. |

### Extraction Strategy

Create a new **`zremote-client`** crate:
- Extract `api.rs` + `types.rs` from `zremote-gui`
- Add WebSocket client for events stream and terminal I/O
- Depends on: `zremote-protocol`, `reqwest`, `tokio`, `serde`
- Platform-independent, usable by desktop GUI, mobile, and CLI tools

---

## Option 1: Shared Rust Core + Native UI (UniFFI)

### Architecture

```
┌─────────────────────────────────────────────┐
│  Rust Core (zremote-client + zremote-protocol) │
│  - REST API client                            │
│  - WebSocket event stream                     │
│  - WebSocket terminal I/O                     │
│  - Local state / caching (SQLite optional)    │
│  - Business logic (session mgmt, loop state)  │
└──────────┬──────────────────┬────────────────┘
           │ UniFFI           │ UniFFI
    ┌──────▼──────┐    ┌─────▼───────┐
    │  Kotlin      │    │  Swift       │
    │  Jetpack     │    │  SwiftUI     │
    │  Compose     │    │              │
    └─────────────┘    └──────────────┘
```

### How It Works

[UniFFI](https://github.com/mozilla/uniffi-rs) (Mozilla) auto-generates Kotlin and Swift bindings from Rust. You annotate Rust functions/types with `#[uniffi::export]` and get type-safe foreign bindings.

```rust
// zremote-client/src/lib.rs
#[uniffi::export]
pub async fn list_hosts(server_url: String) -> Result<Vec<Host>, ClientError> {
    let client = ApiClient::new(&server_url);
    client.list_hosts().await
}

#[derive(uniffi::Record)]
pub struct Host {
    pub id: String,
    pub hostname: String,
    pub status: String,
    pub agent_version: Option<String>,
}
```

Kotlin side (auto-generated):
```kotlin
val hosts = listHosts("http://myserver:3000")
hosts.forEach { host ->
    Text("${host.hostname}: ${host.status}")
}
```

### Pros

- **Production-proven**: Used by Firefox on all platforms, Bitwarden, several fintech apps
- **Maximum native UX**: Full access to platform APIs, gestures, notifications, widgets
- **Type-safe FFI**: No manual JNI/C bridging. Types auto-generated
- **60-80% Rust code reuse**: All business logic, networking, state in Rust
- **Independent platform evolution**: iOS and Android UI can diverge where needed
- **Mature ecosystem**: UniFFI is stable (v0.28+), well-documented, actively maintained

### Cons

- **Two UI codebases**: Jetpack Compose (Android) + SwiftUI (iOS)
- **Build complexity**: Cross-compilation targets (aarch64-linux-android, aarch64-apple-ios)
- **Learning curve**: Need Kotlin/Swift knowledge alongside Rust
- **Sync overhead**: Keeping both UIs in sync with Rust API changes

### Effort Estimate

- `zremote-client` extraction: 1-2 weeks
- UniFFI bindings setup: 1 week
- Android UI (Jetpack Compose): 3-5 weeks
- iOS UI (SwiftUI): 3-5 weeks (if desired)
- **Total (Android only): 5-8 weeks**
- **Total (both platforms): 8-13 weeks**

### When to Choose

- You want polished, native-feeling apps
- You plan to ship to both platforms eventually
- You have (or will hire) Kotlin/Swift expertise
- Long-term maintenance is a priority

---

## Option 2: Shared Rust Core + Native UI (Crux)

### Architecture

```
┌──────────────────────────────────────────────┐
│  Crux App Core (Rust)                         │
│  - Pure state machine (no side effects)       │
│  - update(event, model) -> (model, effects)   │
│  - Effects: Http, WebSocket, Notifications    │
│  - Depends on: zremote-protocol, zremote-client│
└──────────┬──────────────────┬────────────────┘
           │ crux_core FFI    │ crux_core FFI
    ┌──────▼──────┐    ┌─────▼───────┐
    │  Kotlin      │    │  Swift       │
    │  Jetpack     │    │  SwiftUI     │
    │  Compose     │    │              │
    │  (Shell)     │    │  (Shell)     │
    └─────────────┘    └──────────────┘
```

### How It Works

[Crux](https://github.com/redbadger/crux) enforces a strict architecture: the Rust core is a **pure function** (no I/O, no async). Side effects are described as data and executed by the platform shell.

```rust
// Crux app definition
pub struct App;

#[derive(Default)]
pub struct Model {
    hosts: Vec<Host>,
    sessions: Vec<Session>,
    active_loops: Vec<LoopInfo>,
}

pub enum Event {
    LoadHosts,
    HostsLoaded(Vec<Host>),
    SelectSession(SessionId),
    LoopStatusChanged(AgenticLoopId, AgenticStatus),
}

pub enum Effect {
    Http(HttpRequest),
    WebSocket(WsMessage),
    Notification(String),
}

impl crux_core::App for App {
    fn update(&self, event: Event, model: &mut Model, caps: &Capabilities) {
        match event {
            Event::LoadHosts => {
                caps.http.get("/api/hosts").send(Event::HostsLoaded);
            }
            Event::HostsLoaded(hosts) => {
                model.hosts = hosts;
            }
            // ...
        }
    }
}
```

### Pros

- **Testable by design**: Core is pure Rust, test without emulators or mocks
- **Strong architecture**: Elm-like, prevents spaghetti state management
- **Auto-generated bindings**: Swift and Kotlin types from Rust (like UniFFI)
- **Consistent behavior**: Same logic on all platforms, guaranteed
- **Good for ZRemote**: Event-driven nature maps well to WebSocket events

### Cons

- **Pre-1.0**: API may change. Smaller community than UniFFI
- **Opinionated**: Forces functional architecture; not everyone's style
- **Effect overhead**: Every I/O operation goes through the shell (extra indirection)
- **Learning curve**: Crux-specific patterns on top of Rust + native UI
- **Two UI codebases**: Still need Kotlin + Swift shells

### Effort Estimate

- Crux app core setup: 2 weeks
- Integration with zremote-protocol: 1 week
- Android shell (Jetpack Compose): 3-5 weeks
- iOS shell (SwiftUI): 3-5 weeks
- **Total (Android only): 6-8 weeks**
- **Total (both platforms): 9-13 weeks**

### When to Choose

- You value strong architectural guarantees and testability
- You like Elm/Redux-style state management
- You want the core to be extremely easy to unit test
- You're OK with a pre-1.0 framework

---

## Option 3: Full Rust UI with Dioxus

### Architecture

```
┌─────────────────────────────────────────┐
│  Dioxus App (Rust)                       │
│  - React-like declarative UI             │
│  - Uses zremote-protocol directly        │
│  - Uses zremote-client directly          │
│  - Single codebase for all platforms     │
└──────────┬──────────────────┬───────────┘
           │ cargo-mobile2    │ cargo-mobile2
    ┌──────▼──────┐    ┌─────▼───────┐
    │  Android     │    │  iOS         │
    │  (WebView    │    │  (WebView    │
    │   or native) │    │   or native) │
    └─────────────┘    └──────────────┘
```

### How It Works

[Dioxus](https://github.com/DioxusLabs/dioxus) (v0.6.x) provides a React-like API in Rust. Components are functions with hooks.

```rust
fn HostList() -> Element {
    let hosts = use_resource(|| async {
        let client = ApiClient::new("http://server:3000");
        client.list_hosts().await
    });

    rsx! {
        for host in hosts.read().iter().flatten() {
            div { class: "host-card",
                h3 { "{host.hostname}" }
                span { class: "status", "{host.status}" }
            }
        }
    }
}

fn App() -> Element {
    rsx! {
        Router::<Route> {}
    }
}
```

### Pros

- **Single codebase**: One Rust project for Android, iOS, desktop, and web
- **100% Rust**: No Kotlin/Swift needed. All team expertise stays in Rust
- **Direct crate reuse**: `use zremote_protocol::*` — no FFI layer
- **Fast prototyping**: Ship a working app quickly
- **Hot reload**: `dx serve` for rapid iteration
- **Web target**: Same code compiles to WASM for a web client bonus

### Cons

- **Mobile is experimental**: Android support works but tooling is rough
- **Not native UI**: Custom rendering, doesn't look like native Android/iOS
- **Platform integration gaps**: Notifications, background services, deep links need workarounds
- **Framework risk**: If Dioxus mobile stalls, you're stuck
- **Performance**: WebView-based mobile rendering is slower than native
- **Android build issues**: NDK configuration, signing, and deployment are manual

### Effort Estimate

- Dioxus project setup + mobile targets: 1 week
- UI implementation: 3-5 weeks
- Platform-specific workarounds (notifications, etc.): 2-3 weeks
- **Total: 6-9 weeks**

### When to Choose

- Android-only or prototype/MVP
- Team is Rust-only, no Kotlin/Swift expertise
- You accept non-native look and feel
- Speed of initial delivery matters more than polish

---

## Option 4: Full Rust UI with Makepad

### Architecture

```
┌─────────────────────────────────────────┐
│  Makepad App (Rust)                      │
│  - GPU-accelerated rendering             │
│  - Custom DSL for layouts                │
│  - Uses zremote-protocol directly        │
│  - Single codebase                       │
└──────────┬──────────────────┬───────────┘
           │                  │
    ┌──────▼──────┐    ┌─────▼───────┐
    │  Android     │    │  iOS         │
    │  (GPU)       │    │  (GPU)       │
    └─────────────┘    └──────────────┘
```

### How It Works

[Makepad](https://github.com/makepad/makepad) (v1.0, May 2025) uses GPU shaders for rendering and has its own DSL.

```rust
live_design! {
    HostCard = <View> {
        flow: Down,
        padding: 10,
        <Label> { text: "hostname" }
        <Label> { text: "status", draw_text: { color: #4ade80 } }
    }

    HostList = <PortalList> {
        HostCard = <HostCard> {}
    }
}
```

### Pros

- **GPU-accelerated**: Smooth animations, 120fps capable
- **Single codebase**: Rust everywhere, all platforms
- **Live design**: Edit DSL and see changes instantly
- **Good for terminal rendering**: GPU rendering could handle terminal grid efficiently
- **v1.0 released**: First stable release (May 2025)

### Cons

- **Very small community**: Few production apps, limited documentation
- **Custom DSL**: Another thing to learn, not standard Rust
- **Unproven at scale**: v1.0 is recent, unknown production reliability
- **No ecosystem**: No component libraries, no established patterns
- **Platform integration**: Same gaps as Dioxus (notifications, etc.)
- **Risk**: Single-maintainer project vibes

### Effort Estimate

- Makepad setup + learning DSL: 2 weeks
- UI implementation: 4-6 weeks
- Platform workarounds: 2-3 weeks
- **Total: 8-11 weeks**

### When to Choose

- You want to experiment with cutting-edge Rust UI
- GPU rendering is important (terminal grid rendering)
- You're OK with high risk and small community
- Research/exploration project, not production deadline

---

## Option 5: Tauri Mobile

### Architecture

```
┌─────────────────────────────────────────┐
│  Web Frontend (HTML/CSS/JS or WASM)      │
│  - Could reuse existing server web UI    │
│  - Or build new with React/Svelte/Leptos │
└──────────────────┬──────────────────────┘
                   │ WebView
┌──────────────────▼──────────────────────┐
│  Tauri Rust Backend                      │
│  - Uses zremote-client directly          │
│  - Native API access via plugins         │
│  - Push notifications, background tasks  │
└──────────┬──────────────────┬───────────┘
           │                  │
    ┌──────▼──────┐    ┌─────▼───────┐
    │  Android     │    │  iOS         │
    │  (WebView)   │    │  (WKWebView) │
    └─────────────┘    └──────────────┘
```

### Pros

- **Web skills reusable**: If you build a web frontend, it works on mobile too
- **Rust backend**: Tauri's backend is Rust, direct crate reuse
- **Plugin ecosystem**: Camera, notifications, biometrics via Tauri plugins
- **Tauri v2**: Mobile support improved significantly

### Cons

- **WebView performance**: Not suitable for terminal rendering at 60fps
- **Not native feel**: Web UI in a wrapper
- **Bundle size**: Includes WebView runtime
- **ZRemote has no web frontend**: Would need to build one from scratch
- **Limited for terminal**: WebView is wrong tool for real-time terminal I/O

### Effort Estimate

- Web frontend (if none exists): 4-6 weeks
- Tauri mobile setup: 1-2 weeks
- Platform integration: 2-3 weeks
- **Total: 7-11 weeks**

### When to Choose

- You also want a web client
- Terminal rendering is not needed (monitoring/status only)
- Team has web development experience

---

## Option 6: Kotlin Multiplatform + Rust Core

### Architecture

```
┌─────────────────────────────────────────┐
│  Rust Core (via UniFFI)                  │
│  - zremote-client, zremote-protocol      │
└──────────────────┬──────────────────────┘
                   │ UniFFI bindings
┌──────────────────▼──────────────────────┐
│  Kotlin Multiplatform (KMP)              │
│  - Shared Kotlin layer (viewmodels)      │
│  - expect/actual for platform specifics  │
└──────────┬──────────────────┬───────────┘
           │                  │
    ┌──────▼──────┐    ┌─────▼───────┐
    │  Android     │    │  iOS         │
    │  Jetpack     │    │  SwiftUI     │
    │  Compose     │    │  (via KMP)   │
    └─────────────┘    └──────────────┘
```

### Pros

- **KMP is mainstream**: Google-backed, large community, production-ready
- **Shared viewmodels**: Business logic in Kotlin shared across platforms
- **Best Android story**: Jetpack Compose is the standard
- **Rust where it matters**: Core protocol/networking in Rust via UniFFI

### Cons

- **Three languages**: Rust + Kotlin + Swift (unless iOS uses KMP Compose)
- **KMP iOS**: Compose Multiplatform for iOS is less mature
- **Extra layer**: Kotlin sits between Rust and UI, more indirection
- **Build complexity**: Gradle + Cargo + Xcode

### Effort Estimate

- UniFFI Rust bindings: 1-2 weeks
- KMP shared module: 2-3 weeks
- Android UI: 3-4 weeks
- iOS UI: 3-5 weeks
- **Total (Android only): 6-9 weeks**

---

## Comparison Matrix

| Criteria | UniFFI + Native | Crux + Native | Dioxus | Makepad | Tauri | KMP + Rust |
|---|---|---|---|---|---|---|
| **Rust code reuse** | 60-80% | 60-80% | 100% | 100% | 40-60% | 40-60% |
| **Native UX quality** | Excellent | Excellent | Poor | Custom | Poor | Excellent |
| **Maturity** | Production | Pre-1.0 | Experimental | Early v1 | Beta | Production |
| **Android effort** | 5-8w | 6-8w | 6-9w | 8-11w | 7-11w | 6-9w |
| **Both platforms** | 8-13w | 9-13w | 6-9w | 8-11w | 7-11w | 9-14w |
| **Terminal rendering** | Native perf | Native perf | WebView/limited | GPU/good | WebView/bad | Native perf |
| **Team skills needed** | Rust+Kotlin+Swift | Rust+Kotlin+Swift | Rust only | Rust only | Rust+Web | Rust+Kotlin |
| **Risk level** | Low | Medium | High | Very High | Medium | Low |
| **Community size** | Large | Small | Medium | Very Small | Large | Very Large |
| **Push notifications** | Native | Native | Manual | Manual | Plugin | Native |

## Feature Scope (MVP)

Regardless of approach, the mobile MVP should include:

### Must Have (P0)
- Host list with online/offline status
- Session list per host with status indicators
- Agentic loop monitoring (active loops, status, token usage)
- Push notifications for: loop completion, errors, permission requests
- Approve/deny tool call permissions
- View loop transcripts

### Should Have (P1)
- Read-only terminal viewer (scrollback)
- Project list with git status
- Basic terminal input (quick commands)
- Dark theme (match desktop)

### Nice to Have (P2)
- Full terminal interaction
- Session creation/management
- Analytics dashboard
- Telegram-like notification preferences

## Recommendation

### Step 1: Extract `zremote-client` SDK (regardless of mobile approach)

This is a prerequisite for any mobile option and improves the codebase independently:
- Clean separation of concerns in the desktop GUI
- SDK is testable, reusable, and UniFFI-ready
- Effort: ~1-2 days (pure structural refactor, no behavior change)

### Step 2: Choose mobile framework

**Option 1 (UniFFI + Native UI)** is the recommended production path:

1. **Lowest risk**: UniFFI is battle-tested (Firefox ships with it on all platforms)
2. **Best UX**: Native Jetpack Compose / SwiftUI gives platform-appropriate experience
3. **Terminal rendering**: Native Android Canvas / iOS CoreText can handle terminal grid efficiently
4. **Notification support**: Direct access to FCM (Android) / APNs (iOS)
5. **SDK types map 1:1**: `Host`, `Session`, `ServerEvent` become Kotlin/Swift data classes automatically

**Alternative: Dioxus (Option 3)** for Android-only prototype — faster to ship, but may need rewrite for production quality.

### Implementation Order

```
Phase 0: Extract zremote-client SDK from zremote-gui
Phase 1: Add UniFFI annotations to SDK types + build Android .aar
Phase 2: Android MVP (Jetpack Compose) — host list, session list, loop monitoring
Phase 3: Terminal viewer (read-only) + push notifications
Phase 4: (Optional) iOS app using same SDK
```
