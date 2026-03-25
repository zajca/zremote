use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Maximum frame size: 1 MB.
pub const MAX_FRAME_SIZE: u32 = 1_048_576;

/// Ring buffer capacity for scrollback: 100 KB.
pub const RING_BUFFER_CAPACITY: usize = 102_400;

/// Requests sent from the agent to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum DaemonRequest {
    /// Write data to PTY stdin.
    Input { data: Vec<u8> },
    /// Resize the PTY terminal.
    Resize { cols: u16, rows: u16 },
    /// Request current daemon state + scrollback.
    GetState,
    /// Gracefully shut down the daemon.
    Shutdown,
    /// Keepalive ping.
    Ping,
}

/// Responses sent from the daemon to the agent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type")]
pub enum DaemonResponse {
    /// PTY output data.
    Output { data: Vec<u8> },
    /// Shell process exited.
    Exited { code: Option<i32> },
    /// Current daemon state with scrollback.
    State {
        session_id: String,
        shell_pid: u32,
        daemon_pid: u32,
        cols: u16,
        rows: u16,
        scrollback: Vec<u8>,
        started_at: String,
    },
    /// Keepalive pong.
    Pong,
}

/// Write a length-prefixed frame to an async writer.
pub async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, data: &[u8]) -> io::Result<()> {
    let len = u32::try_from(data.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame data exceeds u32::MAX bytes",
        )
    })?;
    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("frame size {len} exceeds MAX_FRAME_SIZE {MAX_FRAME_SIZE}"),
        ));
    }
    writer.write_all(&len.to_le_bytes()).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

/// Read a length-prefixed frame from an async reader.
pub async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_le_bytes(len_buf);

    if len > MAX_FRAME_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame size {len} exceeds MAX_FRAME_SIZE {MAX_FRAME_SIZE}"),
        ));
    }

    let mut buf = vec![0u8; len as usize];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Send a serialized request over the stream.
pub async fn send_request<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    request: &DaemonRequest,
) -> io::Result<()> {
    let data =
        serde_json::to_vec(request).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(writer, &data).await
}

/// Send a serialized response over the stream.
pub async fn send_response<W: AsyncWriteExt + Unpin>(
    writer: &mut W,
    response: &DaemonResponse,
) -> io::Result<()> {
    let data =
        serde_json::to_vec(response).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    write_frame(writer, &data).await
}

/// Read and deserialize a request from the stream.
pub async fn read_request<R: AsyncReadExt + Unpin>(reader: &mut R) -> io::Result<DaemonRequest> {
    let data = read_frame(reader).await?;
    serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Read and deserialize a response from the stream.
pub async fn read_response<R: AsyncReadExt + Unpin>(reader: &mut R) -> io::Result<DaemonResponse> {
    let data = read_frame(reader).await?;
    serde_json::from_slice(&data).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_request_serde_round_trip_input() {
        let req = DaemonRequest::Input {
            data: vec![0x1b, 0x5b, 0x41],
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn daemon_request_serde_round_trip_resize() {
        let req = DaemonRequest::Resize {
            cols: 120,
            rows: 40,
        };
        let json = serde_json::to_string(&req).unwrap();
        let decoded: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn daemon_request_serde_round_trip_get_state() {
        let req = DaemonRequest::GetState;
        let json = serde_json::to_string(&req).unwrap();
        let decoded: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn daemon_request_serde_round_trip_shutdown() {
        let req = DaemonRequest::Shutdown;
        let json = serde_json::to_string(&req).unwrap();
        let decoded: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn daemon_request_serde_round_trip_ping() {
        let req = DaemonRequest::Ping;
        let json = serde_json::to_string(&req).unwrap();
        let decoded: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn daemon_response_serde_round_trip_output() {
        let resp = DaemonResponse::Output {
            data: b"hello world".to_vec(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);
    }

    #[test]
    fn daemon_response_serde_round_trip_exited() {
        let resp = DaemonResponse::Exited { code: Some(0) };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);

        let resp_none = DaemonResponse::Exited { code: None };
        let json_none = serde_json::to_string(&resp_none).unwrap();
        let decoded_none: DaemonResponse = serde_json::from_str(&json_none).unwrap();
        assert_eq!(resp_none, decoded_none);
    }

    #[test]
    fn daemon_response_serde_round_trip_state() {
        let resp = DaemonResponse::State {
            session_id: "abc-123".to_string(),
            shell_pid: 1234,
            daemon_pid: 1235,
            cols: 80,
            rows: 24,
            scrollback: vec![0x41, 0x42, 0x43],
            started_at: "2026-03-25T10:00:00Z".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);
    }

    #[test]
    fn daemon_response_serde_round_trip_pong() {
        let resp = DaemonResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        let decoded: DaemonResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, decoded);
    }

    #[tokio::test]
    async fn frame_encode_decode_round_trip() {
        let data = b"hello frame protocol";
        let mut buf = Vec::new();
        write_frame(&mut buf, data).await.unwrap();

        let mut cursor = io::Cursor::new(buf);
        let decoded = read_frame(&mut cursor).await.unwrap();
        assert_eq!(decoded, data);
    }

    #[tokio::test]
    async fn frame_encode_decode_empty() {
        let data = b"";
        let mut buf = Vec::new();
        write_frame(&mut buf, data).await.unwrap();

        let mut cursor = io::Cursor::new(buf);
        let decoded = read_frame(&mut cursor).await.unwrap();
        assert_eq!(decoded, data);
    }

    #[tokio::test]
    async fn frame_max_size_rejection_on_read() {
        // Craft a frame header claiming a size larger than MAX_FRAME_SIZE
        let bad_len = (MAX_FRAME_SIZE + 1).to_le_bytes();
        let mut cursor = io::Cursor::new(bad_len.to_vec());
        let result = read_frame(&mut cursor).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("MAX_FRAME_SIZE"),
            "error should mention MAX_FRAME_SIZE, got: {err}"
        );
    }

    #[tokio::test]
    async fn frame_max_size_rejection_on_write() {
        let big_data = vec![0u8; (MAX_FRAME_SIZE + 1) as usize];
        let mut buf = Vec::new();
        let result = write_frame(&mut buf, &big_data).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn send_and_read_request_round_trip() {
        let req = DaemonRequest::Resize {
            cols: 200,
            rows: 50,
        };
        let mut buf = Vec::new();
        send_request(&mut buf, &req).await.unwrap();

        let mut cursor = io::Cursor::new(buf);
        let decoded = read_request(&mut cursor).await.unwrap();
        assert_eq!(req, decoded);
    }

    #[tokio::test]
    async fn send_and_read_response_round_trip() {
        let resp = DaemonResponse::Output {
            data: b"test output".to_vec(),
        };
        let mut buf = Vec::new();
        send_response(&mut buf, &resp).await.unwrap();

        let mut cursor = io::Cursor::new(buf);
        let decoded = read_response(&mut cursor).await.unwrap();
        assert_eq!(resp, decoded);
    }

    #[test]
    fn ring_buffer_push_and_truncate() {
        use std::collections::VecDeque;

        let mut ring: VecDeque<u8> = VecDeque::with_capacity(RING_BUFFER_CAPACITY);

        // Fill past capacity
        let chunk = vec![0x41u8; 1000];
        for _ in 0..110 {
            ring.extend(&chunk);
        }
        // Should be 110_000 bytes now (over RING_BUFFER_CAPACITY of 102_400)
        assert_eq!(ring.len(), 110_000);

        // Truncate front to capacity using drain (same pattern as daemon event loop)
        let overflow = ring.len().saturating_sub(RING_BUFFER_CAPACITY);
        ring.drain(..overflow);
        assert_eq!(ring.len(), RING_BUFFER_CAPACITY);

        // Add more data and verify truncation
        ring.extend(&[0x42u8; 5000]);
        let overflow = ring.len().saturating_sub(RING_BUFFER_CAPACITY);
        ring.drain(..overflow);
        assert_eq!(ring.len(), RING_BUFFER_CAPACITY);

        // Last bytes should be 0x42
        assert_eq!(ring.back().copied(), Some(0x42));
    }

    #[test]
    fn request_json_has_type_tag() {
        let req = DaemonRequest::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            json.contains(r#""type":"Ping"#),
            "JSON should contain type tag: {json}"
        );
    }

    #[test]
    fn response_json_has_type_tag() {
        let resp = DaemonResponse::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert!(
            json.contains(r#""type":"Pong"#),
            "JSON should contain type tag: {json}"
        );
    }
}
