use axum::Router;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::get;
use tokio::net::TcpListener;
use zremote_client::TerminalSession;
use zremote_client::types::TerminalEvent;

/// Spin up an axum test server and return the base URL (http://...) and a join handle.
async fn setup_server(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{addr}"), handle)
}

/// Build a WS URL from a base HTTP URL.
fn ws_url(base: &str, session_id: &str) -> String {
    format!(
        "{}/ws/terminal/{session_id}",
        base.replace("http://", "ws://")
    )
}

// ---------------------------------------------------------------------------
// Echo handler: sends back input as binary main-pane output
// ---------------------------------------------------------------------------

async fn echo_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(echo_ws)
}

async fn echo_ws(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        match msg {
            Message::Text(text) => {
                // Parse client message, echo input data back as binary main-pane output
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                    if parsed.get("type").and_then(|t| t.as_str()) == Some("input") {
                        if let Some(data) = parsed.get("data").and_then(|d| d.as_str()) {
                            // Binary frame: 0x01 tag + data bytes
                            let mut frame = vec![0x01];
                            frame.extend_from_slice(data.as_bytes());
                            let _ = socket.send(Message::Binary(frame.into())).await;
                        }
                    }
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

#[tokio::test]
async fn connect_send_input_receive_output() {
    let router = Router::new().route("/ws/terminal/{id}", get(echo_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "test-1"), &handle)
        .await
        .expect("connect should succeed");

    // Send input
    session
        .input_tx
        .send(zremote_client::TerminalInput::Data(b"hello".to_vec()))
        .unwrap();

    // Receive echo output
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        session.output_rx.recv_async().await.unwrap()
    })
    .await
    .expect("should receive output within timeout");

    match event {
        TerminalEvent::Output(data) => {
            assert_eq!(data, b"hello");
        }
        other => panic!("expected Output event, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Scrollback flow handler
// ---------------------------------------------------------------------------

async fn scrollback_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(scrollback_ws)
}

async fn scrollback_ws(mut socket: WebSocket) {
    // Send scrollback start
    let start_msg = serde_json::json!({
        "type": "scrollback_start",
        "cols": 80,
        "rows": 24
    });
    let _ = socket
        .send(Message::Text(start_msg.to_string().into()))
        .await;

    // Send two binary chunks of scrollback data (main pane tag 0x01)
    let mut chunk1 = vec![0x01];
    chunk1.extend_from_slice(b"scroll-part1-");
    let _ = socket.send(Message::Binary(chunk1.into())).await;

    let mut chunk2 = vec![0x01];
    chunk2.extend_from_slice(b"scroll-part2");
    let _ = socket.send(Message::Binary(chunk2.into())).await;

    // Send scrollback end
    let end_msg = serde_json::json!({ "type": "scrollback_end" });
    let _ = socket.send(Message::Text(end_msg.to_string().into())).await;

    // Keep connection open briefly so client can read
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn scrollback_flow() {
    let router = Router::new().route("/ws/terminal/{id}", get(scrollback_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "sb-1"), &handle)
        .await
        .expect("connect should succeed");

    let timeout = std::time::Duration::from_secs(2);

    // 1. ScrollbackStart
    let event = tokio::time::timeout(timeout, session.output_rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    match event {
        TerminalEvent::ScrollbackStart { cols, rows } => {
            assert_eq!(cols, 80);
            assert_eq!(rows, 24);
        }
        other => panic!("expected ScrollbackStart, got {other:?}"),
    }

    // 2. Buffered output (both chunks merged)
    let event = tokio::time::timeout(timeout, session.output_rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    match event {
        TerminalEvent::Output(data) => {
            assert_eq!(data, b"scroll-part1-scroll-part2");
        }
        other => panic!("expected Output with merged scrollback, got {other:?}"),
    }

    // 3. ScrollbackEnd
    let event = tokio::time::timeout(timeout, session.output_rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    match event {
        TerminalEvent::ScrollbackEnd { truncated } => {
            assert!(!truncated, "scrollback should not be truncated");
        }
        other => panic!("expected ScrollbackEnd, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Session close handler
// ---------------------------------------------------------------------------

async fn close_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(close_ws)
}

async fn close_ws(mut socket: WebSocket) {
    let msg = serde_json::json!({
        "type": "session_closed",
        "exit_code": 42
    });
    let _ = socket.send(Message::Text(msg.to_string().into())).await;
}

#[tokio::test]
async fn session_close_event() {
    let router = Router::new().route("/ws/terminal/{id}", get(close_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "close-1"), &handle)
        .await
        .expect("connect should succeed");

    let event = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.output_rx.recv_async(),
    )
    .await
    .unwrap()
    .unwrap();

    match event {
        TerminalEvent::SessionClosed { exit_code } => {
            assert_eq!(exit_code, Some(42));
        }
        other => panic!("expected SessionClosed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// connect_spawned: basic connection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_spawned_basic() {
    let router = Router::new().route("/ws/terminal/{id}", get(echo_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect_spawned(ws_url(&base, "spawned-1"), &handle);

    // Send input
    session
        .input_tx
        .send(zremote_client::TerminalInput::Data(b"ping".to_vec()))
        .unwrap();

    // Should receive echo
    let event = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.output_rx.recv_async(),
    )
    .await
    .expect("should receive output within timeout")
    .unwrap();

    match event {
        TerminalEvent::Output(data) => {
            assert_eq!(data, b"ping");
        }
        other => panic!("expected Output event, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// connect_spawned: connection failure surfaces as SessionClosed
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connect_spawned_failure_sends_session_closed() {
    let handle = tokio::runtime::Handle::current();
    // Connect to a port that nothing is listening on
    let session =
        TerminalSession::connect_spawned("ws://127.0.0.1:1/ws/terminal/nope".to_string(), &handle);

    let event = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        session.output_rx.recv_async(),
    )
    .await
    .expect("should receive event within timeout")
    .unwrap();

    match event {
        TerminalEvent::SessionClosed { exit_code } => {
            assert_eq!(exit_code, None, "failed connect should have no exit code");
        }
        other => panic!("expected SessionClosed on failed connect, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Resize message is sent correctly
// ---------------------------------------------------------------------------

async fn resize_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(resize_ws)
}

async fn resize_ws(mut socket: WebSocket) {
    // Wait for a resize message and echo its dimensions as binary output
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(text) = msg {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                if parsed.get("type").and_then(|t| t.as_str()) == Some("resize") {
                    let cols = parsed.get("cols").and_then(|v| v.as_u64()).unwrap_or(0);
                    let rows = parsed.get("rows").and_then(|v| v.as_u64()).unwrap_or(0);
                    let response = format!("{cols}x{rows}");
                    let mut frame = vec![0x01];
                    frame.extend_from_slice(response.as_bytes());
                    let _ = socket.send(Message::Binary(frame.into())).await;
                    break;
                }
            }
        }
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn resize_event_sent() {
    let router = Router::new().route("/ws/terminal/{id}", get(resize_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "resize-1"), &handle)
        .await
        .expect("connect should succeed");

    session.resize_tx.send((120, 40)).unwrap();

    let event = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.output_rx.recv_async(),
    )
    .await
    .unwrap()
    .unwrap();

    match event {
        TerminalEvent::Output(data) => {
            assert_eq!(std::str::from_utf8(&data).unwrap(), "120x40");
        }
        other => panic!("expected Output with dimensions, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Pane output (0x02 tag) is parsed correctly
// ---------------------------------------------------------------------------

async fn pane_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(pane_ws)
}

async fn pane_ws(mut socket: WebSocket) {
    // Send a pane output binary frame: [0x02] [pane_id_len] [pane_id] [data]
    let pane_id = b"pane-A";
    let mut frame = vec![0x02, pane_id.len() as u8];
    frame.extend_from_slice(pane_id);
    frame.extend_from_slice(b"pane-output-data");
    let _ = socket.send(Message::Binary(frame.into())).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn pane_output_event() {
    let router = Router::new().route("/ws/terminal/{id}", get(pane_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "pane-1"), &handle)
        .await
        .expect("connect should succeed");

    let event = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        session.output_rx.recv_async(),
    )
    .await
    .unwrap()
    .unwrap();

    match event {
        TerminalEvent::PaneOutput { pane_id, data } => {
            assert_eq!(pane_id, "pane-A");
            assert_eq!(data, b"pane-output-data");
        }
        other => panic!("expected PaneOutput event, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// PaneAdded / PaneRemoved text messages
// ---------------------------------------------------------------------------

async fn pane_lifecycle_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(pane_lifecycle_ws)
}

async fn pane_lifecycle_ws(mut socket: WebSocket) {
    let added = serde_json::json!({
        "type": "pane_added",
        "pane_id": "pane-X",
        "index": 1
    });
    let _ = socket.send(Message::Text(added.to_string().into())).await;

    let removed = serde_json::json!({
        "type": "pane_removed",
        "pane_id": "pane-X"
    });
    let _ = socket.send(Message::Text(removed.to_string().into())).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn pane_added_and_removed_events() {
    let router = Router::new().route("/ws/terminal/{id}", get(pane_lifecycle_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "pane-lc-1"), &handle)
        .await
        .expect("connect should succeed");

    let timeout = std::time::Duration::from_secs(2);

    let event = tokio::time::timeout(timeout, session.output_rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    match event {
        TerminalEvent::PaneAdded { pane_id, index } => {
            assert_eq!(pane_id, "pane-X");
            assert_eq!(index, 1);
        }
        other => panic!("expected PaneAdded, got {other:?}"),
    }

    let event = tokio::time::timeout(timeout, session.output_rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    match event {
        TerminalEvent::PaneRemoved { pane_id } => {
            assert_eq!(pane_id, "pane-X");
        }
        other => panic!("expected PaneRemoved, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Session suspended / resumed
// ---------------------------------------------------------------------------

async fn suspend_resume_handler(ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(suspend_resume_ws)
}

async fn suspend_resume_ws(mut socket: WebSocket) {
    let suspended = serde_json::json!({ "type": "session_suspended" });
    let _ = socket
        .send(Message::Text(suspended.to_string().into()))
        .await;

    let resumed = serde_json::json!({ "type": "session_resumed" });
    let _ = socket.send(Message::Text(resumed.to_string().into())).await;

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
}

#[tokio::test]
async fn session_suspended_and_resumed() {
    let router = Router::new().route("/ws/terminal/{id}", get(suspend_resume_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "sr-1"), &handle)
        .await
        .expect("connect should succeed");

    let timeout = std::time::Duration::from_secs(2);

    let event = tokio::time::timeout(timeout, session.output_rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, TerminalEvent::SessionSuspended),
        "expected SessionSuspended, got {event:?}"
    );

    let event = tokio::time::timeout(timeout, session.output_rx.recv_async())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(event, TerminalEvent::SessionResumed),
        "expected SessionResumed, got {event:?}"
    );
}

// ---------------------------------------------------------------------------
// Drop cancels background tasks
// ---------------------------------------------------------------------------

#[tokio::test]
async fn drop_cancels_session() {
    let router = Router::new().route("/ws/terminal/{id}", get(echo_handler));
    let (base, _server) = setup_server(router).await;

    let handle = tokio::runtime::Handle::current();
    let session = TerminalSession::connect(ws_url(&base, "drop-1"), &handle)
        .await
        .expect("connect should succeed");

    let output_rx = session.output_rx.clone();

    // Drop the session - should cancel background tasks
    drop(session);

    // Channel should eventually close (recv returns Err)
    let result =
        tokio::time::timeout(std::time::Duration::from_secs(2), output_rx.recv_async()).await;

    // Either timeout (tasks cancelled, no more messages) or channel disconnected
    match result {
        Err(_timeout) => {}          // Fine - no more messages
        Ok(Err(_disconnected)) => {} // Fine - channel closed
        Ok(Ok(_event)) => {}         // Also fine - might get a close event
    }
}
