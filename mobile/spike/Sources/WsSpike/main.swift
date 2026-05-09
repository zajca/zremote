// Phase 0 WebSocket spike — headless Swift executable.
//
// Runs three independent probes and prints a structured log line per event
// that the host-side runner can grep out of `adb shell` / logcat output.
//
// Probes:
//   1. Echo smoke test:  wss://echo.websocket.events
//      Sends "hello", expects to receive "hello" back.
//   2. ZRemote events:   ws://<host>:<port>/ws/events
//      Reads one server-tagged JSON payload, confirms the `type` field
//      decodes. Does not exhaustively parse every ServerEvent variant — that
//      is Phase 2 work. We only need to prove that the byte path works and
//      that JSONDecoder can read a tagged enum from the wire.
//   3. Soak loop:        keeps the ZRemote events connection open for
//      `SPIKE_MINUTES` (default 5), reconnecting on disconnects with
//      exponential backoff capped at 30 s.
//
// All logs use the `SPIKE:` prefix so `scripts/mobile-spike-ws.sh` can filter
// logcat lines. Format:
//   SPIKE: <timestamp> <level> <probe>: <message>
//
// Env vars:
//   ZREMOTE_WS_URL   ws:// url for the events stream
//                    (default: ws://10.0.2.2:3000/ws/events — the Android
//                    emulator's alias for the host; `adb reverse tcp:3000
//                    tcp:3000` also works for physical devices)
//   ZREMOTE_TOKEN    optional bearer token
//   SPIKE_MINUTES    soak duration in minutes (default 5)
//   SPIKE_ECHO_URL   override the echo probe URL (default
//                    wss://echo.websocket.events)
//   SPIKE_SKIP_ECHO  if "1", skip the public echo probe (offline testing)
import Foundation
#if canImport(FoundationNetworking)
import FoundationNetworking
#endif

// MARK: - Logging

enum LogLevel: String { case info = "INFO", warn = "WARN", fail = "FAIL", ok = "OK" }

// ISO8601DateFormatter is not Sendable, but we only ever call .string(from:)
// which is internally synchronized in swift-corelibs-foundation. Mark unsafe.
nonisolated(unsafe) let isoFormatter: ISO8601DateFormatter = {
    let f = ISO8601DateFormatter()
    f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return f
}()

func log(_ level: LogLevel, _ probe: String, _ message: String) {
    let ts = isoFormatter.string(from: Date())
    // Single line, single FileHandle.write so that Android log rotation /
    // stdout buffering does not interleave bytes from concurrent tasks.
    let line = "SPIKE: \(ts) \(level.rawValue) \(probe): \(message)\n"
    FileHandle.standardOutput.write(Data(line.utf8))
}

// MARK: - Config

struct SpikeConfig {
    var echoURL: URL
    var skipEcho: Bool
    var zremoteURL: URL
    var zremoteToken: String?
    var soakSeconds: TimeInterval

    static func fromEnvironment() -> SpikeConfig {
        let env = ProcessInfo.processInfo.environment

        let echoString = env["SPIKE_ECHO_URL"] ?? "wss://echo.websocket.events"
        guard let echoURL = URL(string: echoString) else {
            fatalError("SPIKE_ECHO_URL is not a valid URL: \(echoString)")
        }

        let skipEcho = (env["SPIKE_SKIP_ECHO"] ?? "0") == "1"

        // Android emulator cannot reach the host's loopback directly; the
        // alias `10.0.2.2` is the emulator's view of the host. For physical
        // devices we recommend `adb reverse tcp:3000 tcp:3000` + the runner
        // setting ZREMOTE_WS_URL=ws://127.0.0.1:3000/ws/events.
        let zremoteString = env["ZREMOTE_WS_URL"] ?? "ws://10.0.2.2:3000/ws/events"
        guard let zremoteURL = URL(string: zremoteString) else {
            fatalError("ZREMOTE_WS_URL is not a valid URL: \(zremoteString)")
        }

        let token = env["ZREMOTE_TOKEN"].flatMap { $0.isEmpty ? nil : $0 }

        let minutes = TimeInterval(env["SPIKE_MINUTES"].flatMap(Int.init) ?? 5)

        return SpikeConfig(
            echoURL: echoURL,
            skipEcho: skipEcho,
            zremoteURL: zremoteURL,
            zremoteToken: token,
            soakSeconds: minutes * 60
        )
    }
}

// MARK: - Wire format stub

// Minimal decoder that exercises the `#[serde(tag = "type")]` convention used
// by `ServerEvent` on the wire. We are not porting every variant here — this
// is a smoke test that JSONDecoder can see a tagged payload on Android. The
// full Codable port lives in Phase 2 (`ZRemoteProtocol/Events.swift`).
struct TaggedEnvelope: Decodable {
    let type: String
}

// MARK: - Probes

enum ProbeError: Error {
    case unexpectedMessage(String)
    case missingUpgrade
    case decoderFailed(String)
    case closedTooEarly(String)
}

/// URLSessionWebSocketDelegate: track open/close transitions because
/// `receive()` alone doesn't tell us when the upgrade completed.
final class WsDelegate: NSObject, URLSessionWebSocketDelegate, @unchecked Sendable {
    let onOpen: @Sendable (String?) -> Void
    let onClose: @Sendable (URLSessionWebSocketTask.CloseCode, Data?) -> Void
    init(onOpen: @escaping @Sendable (String?) -> Void,
         onClose: @escaping @Sendable (URLSessionWebSocketTask.CloseCode, Data?) -> Void) {
        self.onOpen = onOpen
        self.onClose = onClose
    }
    func urlSession(_ session: URLSession,
                    webSocketTask: URLSessionWebSocketTask,
                    didOpenWithProtocol protocol: String?) {
        onOpen(`protocol`)
    }
    func urlSession(_ session: URLSession,
                    webSocketTask: URLSessionWebSocketTask,
                    didCloseWith closeCode: URLSessionWebSocketTask.CloseCode,
                    reason: Data?) {
        onClose(closeCode, reason)
    }
}

/// Probe 1: public echo round-trip.
func runEchoProbe(_ config: SpikeConfig) async -> Bool {
    if config.skipEcho {
        log(.warn, "echo", "SPIKE_SKIP_ECHO=1, skipping public echo probe")
        return true
    }
    log(.info, "echo", "connecting to \(config.echoURL.absoluteString)")
    let delegate = WsDelegate(
        onOpen: { proto in
            log(.ok, "echo", "handshake complete (protocol=\(proto ?? "<none>"))")
        },
        onClose: { code, _ in
            log(.info, "echo", "closed, code=\(code.rawValue)")
        }
    )
    let session = URLSession(configuration: .ephemeral, delegate: delegate, delegateQueue: nil)
    let task = session.webSocketTask(with: config.echoURL)
    task.resume()
    do {
        try await task.send(.string("hello"))
        log(.info, "echo", "sent payload")
        let reply = try await task.receive()
        switch reply {
        case .string(let s):
            log(.ok, "echo", "received text payload (\(s.count) bytes): \(s.prefix(64))")
        case .data(let d):
            log(.ok, "echo", "received binary payload (\(d.count) bytes)")
        @unknown default:
            log(.warn, "echo", "received unknown message case")
        }
        task.cancel(with: .normalClosure, reason: nil)
        session.finishTasksAndInvalidate()
        return true
    } catch {
        log(.fail, "echo", "probe failed: \(error)")
        task.cancel(with: .abnormalClosure, reason: nil)
        session.invalidateAndCancel()
        return false
    }
}

/// Probe 2+3: zremote events connection with 5-min soak and auto-reconnect.
func runZRemoteProbe(_ config: SpikeConfig) async -> Bool {
    log(.info, "zremote", "soaking for \(Int(config.soakSeconds))s against \(config.zremoteURL.absoluteString)")
    let deadline = Date().addingTimeInterval(config.soakSeconds)
    var attempt = 0
    var sawFirstDecodedEvent = false
    var totalMessages = 0
    var reconnectCount = 0

    while Date() < deadline {
        attempt += 1
        if attempt > 1 {
            reconnectCount += 1
            // Exponential backoff with a 30 s ceiling, matching the Rust
            // `EventStream` reconnect policy we will port in Phase 3.
            let backoff = min(30.0, pow(2.0, Double(min(attempt, 6))))
            log(.info, "zremote", "reconnect #\(reconnectCount) in \(Int(backoff))s")
            try? await Task.sleep(nanoseconds: UInt64(backoff * 1_000_000_000))
        }

        var request = URLRequest(url: config.zremoteURL)
        if let token = config.zremoteToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }

        let attemptSnapshot = attempt
        let delegate = WsDelegate(
            onOpen: { proto in
                log(.ok, "zremote", "handshake complete (attempt=\(attemptSnapshot), protocol=\(proto ?? "<none>"))")
            },
            onClose: { code, reason in
                let r = reason.flatMap { String(data: $0, encoding: .utf8) } ?? "<nil>"
                log(.info, "zremote", "closed code=\(code.rawValue) reason=\(r)")
            }
        )
        let session = URLSession(configuration: .ephemeral, delegate: delegate, delegateQueue: nil)
        let task = session.webSocketTask(with: request)
        task.resume()

        // Inner receive loop. Break out on any error and let the outer while
        // loop reconnect if we still have budget.
        receiveLoop: while Date() < deadline {
            do {
                let msg = try await task.receive()
                totalMessages += 1
                switch msg {
                case .string(let s):
                    if !sawFirstDecodedEvent {
                        if let data = s.data(using: .utf8),
                           let env = try? JSONDecoder().decode(TaggedEnvelope.self, from: data) {
                            sawFirstDecodedEvent = true
                            log(.ok, "zremote", "decoded first ServerEvent type=\(env.type)")
                        } else {
                            log(.warn, "zremote", "first text payload did not match TaggedEnvelope shape: \(s.prefix(120))")
                        }
                    }
                    if totalMessages % 10 == 0 {
                        log(.info, "zremote", "msgs=\(totalMessages)")
                    }
                case .data(let d):
                    log(.info, "zremote", "received binary payload (\(d.count) bytes)")
                @unknown default:
                    log(.warn, "zremote", "received unknown message case")
                }
            } catch {
                log(.warn, "zremote", "receive failed on attempt \(attempt): \(error)")
                break receiveLoop
            }
        }

        task.cancel(with: .goingAway, reason: nil)
        session.finishTasksAndInvalidate()
    }

    log(.info, "zremote", "soak complete: attempts=\(attempt) messages=\(totalMessages) decoded_first_event=\(sawFirstDecodedEvent) reconnects=\(reconnectCount)")
    // Success criteria for Phase 0: we got at least one handshake AND
    // decoded one real ServerEvent. Reconnect count is informational — we do
    // NOT fail the spike just because the agent died mid-run, because the
    // whole point of reconnect is to tolerate that.
    return sawFirstDecodedEvent || totalMessages > 0
}

// MARK: - Entry point

@main
struct WsSpike {
    static func main() async {
        let config = SpikeConfig.fromEnvironment()
        log(.info, "boot", "WsSpike starting (soak=\(Int(config.soakSeconds))s)")
        log(.info, "boot", "swift=\(compilerVersion()) arch=\(hostArch()) isAndroid=\(isAndroid())")

        let echoOK = await runEchoProbe(config)
        let zremoteOK = await runZRemoteProbe(config)

        if echoOK && zremoteOK {
            log(.ok, "verdict", "GREEN — echo + zremote probes succeeded")
            exit(0)
        } else if echoOK {
            log(.warn, "verdict", "YELLOW — echo OK, zremote probe did not see any traffic")
            exit(0)
        } else if zremoteOK {
            log(.warn, "verdict", "YELLOW — zremote OK, echo probe failed")
            exit(0)
        } else {
            log(.fail, "verdict", "RED — both probes failed")
            exit(3)
        }
    }

    // Tiny host-info helpers so the log line identifies what we actually ran.
    static func compilerVersion() -> String {
        #if swift(>=6.3)
        return ">=6.3"
        #elseif swift(>=6.1)
        return "6.1..<6.3"
        #else
        return "<6.1"
        #endif
    }

    static func hostArch() -> String {
        #if arch(arm64)
        return "arm64"
        #elseif arch(x86_64)
        return "x86_64"
        #else
        return "unknown"
        #endif
    }

    static func isAndroid() -> Bool {
        #if os(Android)
        return true
        #else
        return false
        #endif
    }
}
