# Security Review: zremote-client SDK Extraction

**Branch:** `mobile-rfc`
**Worktree:** `/home/zajca/Code/Me/myremote-mobile-rfc`
**Date:** 2026-03-24
**Scope:** `crates/zremote-client/` — new HTTP/WebSocket client SDK
**Reviewer:** security agent

---

## Security Review Checklist

- [x] Injection risks reviewed
- [x] Authentication/Authorization verified (N/A — no auth in SDK currently)
- [x] Secrets handling reviewed
- [x] Dependency audit completed (`cargo audit`)
- [x] Transport security verified
- [x] Logging practices checked
- [x] Concurrency issues reviewed
- [x] IaC and container configs analyzed (N/A)

---

## Findings

### HIGH — Unbounded Scrollback Buffer Allocation (DoS/OOM)

**File:** `crates/zremote-client/src/terminal.rs:157,181,204`
**CWE:** CWE-789 (Uncontrolled Memory Allocation)
**Severity:** HIGH

`scrollback_buf: Vec<u8>` accumulates all binary frames (tags `0x01` and `0x02`) received while `in_scrollback == true`. Each frame is capped at `MAX_TERMINAL_MESSAGE_SIZE = 1MB` per message, but **the buffer itself has no cumulative size limit**. A malicious or compromised server can send an unlimited number of 1MB binary frames during scrollback mode, growing the Vec without bound until OOM.

```rust
// terminal.rs:157
let mut scrollback_buf: Vec<u8> = Vec::new();  // No capacity cap

// terminal.rs:181 — each frame appended without total-size check
scrollback_buf.extend_from_slice(bytes);        // bytes can be ~1MB per call
```

**Fix:** Add a cap on total scrollback buffer size (e.g., 100MB — matching the server's in-memory scrollback limit). Drop or truncate if exceeded:

```rust
const MAX_SCROLLBACK_BUF: usize = 100 * 1024 * 1024;

if scrollback_buf.len() + bytes.len() > MAX_SCROLLBACK_BUF {
    warn!("scrollback buffer limit exceeded, truncating");
    break; // or: skip this frame and continue
}
scrollback_buf.extend_from_slice(bytes);
```

---

### MEDIUM — Error Response Body Read Before Truncation (DoS)

**File:** `crates/zremote-client/src/error.rs:56`
**CWE:** CWE-789 (Uncontrolled Memory Allocation)
**Severity:** MEDIUM

`response.text().await` reads the **full response body** into a `String` before the 4KB truncation check is applied. A server returning a 100MB error body will cause 100MB of allocation before the truncation fires.

```rust
// error.rs:56
let body = response.text().await.unwrap_or_default();  // full body loaded first
let message = if body.len() > MAX_ERROR_BODY_SIZE {    // truncation applied after
    format!("{}... (truncated)", &body[..MAX_ERROR_BODY_SIZE])
```

**Fix:** Use `reqwest::Response::bytes_with_limit` if available, or limit via `.take()` on the byte stream, or read at most 4096 bytes from the response body:

```rust
// Using reqwest chunk API to cap allocation:
let mut buf = Vec::with_capacity(MAX_ERROR_BODY_SIZE + 1);
let mut stream = response.bytes_stream();
while let Some(chunk) = stream.next().await {
    let chunk = chunk.unwrap_or_default();
    buf.extend_from_slice(&chunk);
    if buf.len() >= MAX_ERROR_BODY_SIZE {
        buf.truncate(MAX_ERROR_BODY_SIZE);
        return Self::ServerError { status, message: format!("{}... (truncated)", String::from_utf8_lossy(&buf)) };
    }
}
```

---

### MEDIUM — WebSocket Message Size Limit Not Enforced at Connection Level

**Files:** `crates/zremote-client/src/terminal.rs:40`, `crates/zremote-client/src/events.rs:55`
**CWE:** CWE-400 (Uncontrolled Resource Consumption)
**Severity:** MEDIUM

Both WebSocket connections use the bare `connect_async()` which applies **tokio-tungstenite's default `max_message_size` of 64MB**. The manual checks (`MAX_TERMINAL_MESSAGE_SIZE = 1MB`, `MAX_EVENT_MESSAGE_SIZE = 4MB`) fire only after the library has already buffered the full frame in memory. If a server sends a 60MB frame, the data is allocated before the SDK's guard runs.

```rust
// terminal.rs:40 — default config: max_message_size = 64MB
let (ws_stream, _) = connect_async(&url).await?;

// terminal.rs:168 — check fires AFTER tungstenite has buffered the frame
if data.len() > MAX_TERMINAL_MESSAGE_SIZE {
```

**Fix:** Use `connect_async_with_config` to enforce the limit at the library level:

```rust
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

let config = WebSocketConfig {
    max_message_size: Some(MAX_TERMINAL_MESSAGE_SIZE),
    max_frame_size: Some(MAX_TERMINAL_MESSAGE_SIZE),
    ..Default::default()
};
let (ws_stream, _) = connect_async_with_config(&url, Some(config), false).await?;
```

---

### MEDIUM — URL Path Segment Injection via Unencoded Parameters

**Files:** `crates/zremote-client/src/client.rs:69-76`, `client.rs:469-484`
**CWE:** CWE-20 (Improper Input Validation)
**Severity:** MEDIUM (low exploitability in practice, but defense-in-depth gap)

IDs such as `session_id`, `host_id`, `project_id`, `action_name` are interpolated directly into URL paths without percent-encoding. UUID-typed IDs from server responses are safe in practice, but `action_name` (a free-form string) and `key` (config key) are more likely to carry special characters from user input.

```rust
// client.rs:469 — action_name is not a UUID, could contain '/', '?', '#'
format!("{}/api/projects/{project_id}/actions/{action_name}/run", self.base_url)

// client.rs:605 — key is free-form
format!("{}/api/config/{key}", self.base_url)
```

Additionally, `terminal_ws_url` uses a string replace rather than proper URL construction:

```rust
// client.rs:59-65 — will mangle URLs with 'http://' in the path component
let ws_base = self.base_url.as_str()
    .replace("http://", "ws://")
    .replace("https://", "wss://");
```

**Fix:** Use `url::Url`'s path segment API or `percent_encoding` to encode segments:

```rust
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
let encoded = utf8_percent_encode(action_name, NON_ALPHANUMERIC).to_string();
format!("{}/api/projects/{project_id}/actions/{encoded}/run", self.base_url)
```

For the WebSocket URL, derive the scheme from the parsed URL:

```rust
pub fn events_ws_url(&self) -> String {
    let scheme = match self.base_url.scheme() {
        "https" => "wss",
        _ => "ws",
    };
    let mut ws = self.base_url.clone();
    ws.set_scheme(scheme).ok();
    format!("{ws}/ws/events")
}
```

---

### MEDIUM — Dependency: rustls-webpki CRL Validation Bug

**Crate:** `rustls-webpki 0.103.9` (via `tokio-tungstenite` → `rustls`)
**Advisory:** RUSTSEC-2026-0049
**CVE:** (filed 2026-03-20)
**Severity:** MEDIUM

CRLs are not considered authoritative by Distribution Point due to faulty matching logic. This affects TLS certificate revocation checking in WebSocket connections made by `zremote-client`.

**Fix:** Bump `rustls-webpki` to `>=0.103.10`. This is a transitive dependency — update `tokio-tungstenite` or `rustls` in the workspace `Cargo.toml`.

---

### LOW — Synchronous Channel Send Stalls WebSocket Reader (Backpressure DoS)

**File:** `crates/zremote-client/src/events.rs:79`
**CWE:** CWE-667 (Improper Locking / Resource Starvation)
**Severity:** LOW

`tx.send(event)` is the **synchronous** flume send. On a full bounded channel (`EVENT_CHANNEL_CAPACITY = 256`), this blocks the WebSocket reader task indefinitely. While the reader is blocked, it cannot process Ping frames, Close frames, or other messages — this can cause server-side connection timeouts and reconnection storms if the consumer is slow.

```rust
// events.rs:79 — sync send, blocks if channel full
if tx.send(event).is_err() {
```

**Fix:** Use `tx.try_send()` and drop events when the channel is full (with a warning), or use `tx.send_async().await` so the select loop stays responsive:

```rust
// Option A: non-blocking, drop oldest or log and skip
if tx.try_send(event).is_err() {
    warn!("events channel full, dropping event");
}
// Option B: async send inside the select — allows Ping processing
tokio::select! {
    _ = cancel.cancelled() => { ... }
    msg = read.next() => { ... }
    _ = tx.send_async(event) => {}  // awaitable, doesn't block
}
```

---

### LOW — UTF-8 Chunking Corrupts Multi-Byte Sequences

**File:** `crates/zremote-client/src/terminal.rs:108-112`
**CWE:** CWE-116 (Improper Encoding or Escaping of Output)
**Severity:** LOW (data integrity, not directly exploitable)

Input data is chunked at exactly 65536 bytes regardless of UTF-8 boundaries. `from_utf8_lossy` replaces incomplete sequences at the boundary with U+FFFD:

```rust
const MAX_CHUNK: usize = 65_536;
for chunk in data.chunks(MAX_CHUNK) {
    let msg = TerminalClientMessage::Input {
        data: String::from_utf8_lossy(chunk).to_string(),  // may corrupt multi-byte chars
```

This corrupts terminal input containing multi-byte UTF-8 characters (CJK, emoji) when the payload crosses the 64KB threshold. Functionally this is a bug; in a security context it could confuse server-side parsing.

**Fix:** Either send raw bytes (use a binary frame), or split only at valid UTF-8 character boundaries.

---

## Dependency Vulnerabilities

| Crate | Version | Advisory | Severity | Affects zremote-client | Fix |
|-------|---------|----------|----------|------------------------|-----|
| `rustls-webpki` | 0.103.9 | RUSTSEC-2026-0049 | MEDIUM | Yes (via tokio-tungstenite) | Upgrade to >=0.103.10 |
| `rsa` | 0.9.10 | RUSTSEC-2023-0071 | MEDIUM | No (only via sqlx-mysql) | No fix available |
| `async-std` | 1.13.2 | RUSTSEC-2025-0052 | INFO | No | Unmaintained, monitor |

---

## What Looks Good

- **Binary frame parsing** (`terminal.rs:173-215`): Empty frame guard, pane_id length bounds check (`data.len() < 2 + pid_len`), and UTF-8 validation on pane_id are all correct. No buffer overread risk.
- **4KB error body truncation** — the intent is correct, the implementation just needs to limit the read, not the display.
- **Secrets/logging** — no tokens or credentials are logged. `tracing` calls log only URLs and error messages, not request bodies or auth headers. The auth-free design is noted in the RFC.
- **Backoff jitter** (`events.rs:37-41`): Reconnect jitter is correctly implemented (25% variation), preventing thundering herd.
- **CancellationToken** pattern: Used correctly for both reader and writer tasks, with graceful `Close` frame sent on cancellation.
- **reqwest timeouts**: Both `timeout` (30s) and `connect_timeout` (10s) are set, preventing indefinite hangs.
- **Channel capacities**: Bounded channels throughout. `IMAGE_PASTE_CHANNEL_CAPACITY = 4` is intentionally small (backpressure on large pastes).
- **No hardcoded credentials**: No tokens, no secrets anywhere in the new code.

---

## Recommendations

1. **Fix immediately** (HIGH): Add a cumulative size cap on `scrollback_buf` in `terminal.rs`.
2. **Fix before mobile release** (MEDIUM): Switch `connect_async` to `connect_async_with_config` in both WS files to enforce message size at the library level.
3. **Fix before mobile release** (MEDIUM): Read error body with a size cap, not truncate-after-full-read.
4. **Fix before mobile release** (MEDIUM): Upgrade `rustls-webpki` to 0.103.10+.
5. **Schedule this sprint** (MEDIUM): Percent-encode `action_name` and `key` path segments.
6. **Low priority** (LOW): Replace sync `tx.send()` with `try_send()` in the events loop; fix UTF-8 chunking.
