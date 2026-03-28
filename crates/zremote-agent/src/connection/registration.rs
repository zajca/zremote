use std::time::Duration;

use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use zremote_protocol::{AgentMessage, HostId, ServerMessage};

use super::{ConnectionError, WsStream, send_message};
use crate::config::AgentConfig;

const REGISTER_TIMEOUT: Duration = Duration::from_secs(10);

/// Read the current user's login shell from the passwd database.
fn login_shell_from_passwd() -> Option<String> {
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    let output = std::process::Command::new("getent")
        .args(["passwd", uid.trim()])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())?;
    // passwd format: name:password:uid:gid:gecos:home:shell
    let shell = output.trim().rsplit(':').next()?;
    if shell.is_empty() {
        return None;
    }
    Some(shell.to_string())
}

pub(crate) fn default_shell() -> &'static str {
    static SHELL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SHELL.get_or_init(|| {
        // Read login shell from passwd database instead of $SHELL.
        // $SHELL can be overridden by nix develop to a non-interactive bash
        // (without readline), which breaks PS1 escape processing in PTY sessions.
        // The passwd entry always has the user's actual login shell.
        login_shell_from_passwd()
            .unwrap_or_else(|| std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
    })
}

/// Register with the server and return the assigned host ID.
pub(super) async fn register(
    ws: &mut WsStream,
    config: &AgentConfig,
    supports_persistence: bool,
) -> Result<HostId, ConnectionError> {
    let hostname = hostname::get()
        .map_err(ConnectionError::Hostname)?
        .to_string_lossy()
        .into_owned();

    let register_msg = AgentMessage::Register {
        hostname,
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        token: config.token.clone(),
        supports_persistent_sessions: supports_persistence,
    };

    send_message(ws, &register_msg).await?;
    tracing::debug!("sent Register message, waiting for RegisterAck");

    // Wait for RegisterAck with timeout
    let ack = timeout(REGISTER_TIMEOUT, async {
        while let Some(msg_result) = ws.next().await {
            let msg = msg_result.map_err(ConnectionError::Receive)?;
            match msg {
                Message::Text(text) => {
                    let server_msg: ServerMessage =
                        serde_json::from_str(&text).map_err(ConnectionError::Deserialize)?;
                    return Ok(server_msg);
                }
                Message::Close(_) => return Err(ConnectionError::ConnectionClosed),
                // Skip ping/pong/binary during registration
                _ => {}
            }
        }
        Err(ConnectionError::ConnectionClosed)
    })
    .await
    .map_err(|_| ConnectionError::RegisterTimeout)??;

    match ack {
        ServerMessage::RegisterAck { host_id } => {
            tracing::info!(host_id = %host_id, "registered with server");
            Ok(host_id)
        }
        ServerMessage::Error { message } => Err(ConnectionError::ServerError(message)),
        other => Err(ConnectionError::UnexpectedRegisterResponse(format!(
            "{other:?}"
        ))),
    }
}
