use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

/// Default socket path for `ZRemote` ccline listener.
fn socket_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".zremote").join("ccline.sock"))
}

/// Send raw JSON data to the `ZRemote` agent via Unix socket.
/// Fire-and-forget: silently ignores all errors (socket missing, connection refused, etc.).
pub fn send_to_agent(raw_json: &[u8]) {
    let Some(path) = socket_path() else {
        return;
    };

    // Don't even try if socket file doesn't exist
    if !path.exists() {
        return;
    }

    let Ok(stream) = UnixStream::connect(&path) else {
        return;
    };

    // Set write timeout to avoid blocking the status line
    let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));

    let mut writer = std::io::BufWriter::new(stream);
    let _ = writer.write_all(raw_json);
    let _ = writer.write_all(b"\n");
    let _ = writer.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_to_agent_no_socket_no_panic() {
        // Should silently do nothing when no socket exists
        send_to_agent(b"{}");
    }

    #[test]
    fn socket_path_resolves() {
        let path = socket_path();
        // Should resolve on any system with a home directory
        assert!(path.is_some());
        let p = path.unwrap();
        assert!(p.ends_with(".zremote/ccline.sock"));
    }
}
