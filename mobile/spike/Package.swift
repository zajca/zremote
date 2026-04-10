// swift-tools-version:6.1
//
// Phase 0 WebSocket spike — single headless executable.
//
// We deliberately avoid every optional Skip/SwiftUI/UIKit dependency. The
// point of the spike is to isolate `URLSessionWebSocketTask` and `JSONDecoder`
// behavior on the plain Swift Android SDK toolchain before Phase 1 begins.
//
// Built via:
//   swift build -c release --swift-sdk aarch64-unknown-linux-android28
//
// The output binary lives at
//   .build/aarch64-unknown-linux-android28/release/WsSpike
// and is pushed onto the device by ../scripts/mobile-spike-ws.sh .
import PackageDescription

let package = Package(
    name: "WsSpike",
    platforms: [
        // Android deployment target is declared via the --swift-sdk flag at
        // build time (triple `aarch64-unknown-linux-android28`). We still set
        // macOS here so `swift build` on a host SDK keeps working for local
        // iteration before we cross-compile.
        .macOS(.v13),
    ],
    targets: [
        .executableTarget(
            name: "WsSpike",
            path: "Sources/WsSpike"
        ),
    ]
)
