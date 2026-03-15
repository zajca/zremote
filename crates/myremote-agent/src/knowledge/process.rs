use std::path::PathBuf;
use std::time::Duration;

/// Manages the `OpenViking` subprocess.
pub struct OvProcess {
    child: Option<tokio::process::Child>,
    binary: String,
    port: u16,
    data_dir: PathBuf,
}

impl OvProcess {
    pub fn new(binary: String, port: u16, data_dir: PathBuf) -> Self {
        Self {
            child: None,
            binary,
            port,
            data_dir,
        }
    }

    /// Spawn `OpenViking` as a child process.
    pub async fn start(&mut self) -> Result<(), OvProcessError> {
        if self.child.is_some() {
            return Err(OvProcessError::AlreadyRunning);
        }

        // Ensure data dir exists
        tokio::fs::create_dir_all(&self.data_dir)
            .await
            .map_err(OvProcessError::Io)?;

        let child = tokio::process::Command::new(&self.binary)
            .arg("serve")
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--data-dir")
            .arg(&self.data_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(OvProcessError::Spawn)?;

        self.child = Some(child);

        // Wait for healthy
        self.wait_for_healthy().await?;

        Ok(())
    }

    /// Health check loop: GET http://localhost:{port}/health
    async fn wait_for_healthy(&self) -> Result<(), OvProcessError> {
        let url = format!("http://localhost:{}/health", self.port);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| OvProcessError::HealthCheck(e.to_string()))?;

        for attempt in 0..30 {
            tokio::time::sleep(Duration::from_millis(500)).await;
            match client.get(&url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(attempt, "OpenViking is healthy");
                    return Ok(());
                }
                Ok(resp) => {
                    tracing::debug!(attempt, status = %resp.status(), "OV health check not ready");
                }
                Err(e) => {
                    tracing::debug!(attempt, error = %e, "OV health check pending");
                }
            }
        }
        Err(OvProcessError::HealthCheck(
            "OpenViking failed to become healthy within 15s".to_string(),
        ))
    }

    /// Graceful shutdown: SIGTERM, wait 5s, then kill.
    pub async fn stop(&mut self) -> Result<(), OvProcessError> {
        if let Some(ref mut child) = self.child {
            // Try to kill gracefully first
            let pid = child.id();
            if let Some(pid) = pid {
                // Send SIGTERM via kill command
                let _ = tokio::process::Command::new("kill")
                    .arg("-TERM")
                    .arg(pid.to_string())
                    .output()
                    .await;
            }

            // Wait up to 5 seconds for graceful exit
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => {
                    tracing::info!(?status, "OpenViking stopped gracefully");
                }
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "error waiting for OpenViking");
                }
                Err(_) => {
                    tracing::warn!("OpenViking did not stop within 5s, killing");
                    let _ = child.kill().await;
                }
            }
            self.child = None;
        }
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.child.is_some()
    }
}

impl Drop for OvProcess {
    fn drop(&mut self) {
        if let Some(ref mut child) = self.child {
            // kill_on_drop is set, but let's be explicit
            let _ = child.start_kill();
        }
    }
}

/// Errors from OV process management.
#[derive(Debug)]
pub enum OvProcessError {
    AlreadyRunning,
    Spawn(std::io::Error),
    Io(std::io::Error),
    HealthCheck(String),
}

impl std::fmt::Display for OvProcessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyRunning => write!(f, "OpenViking is already running"),
            Self::Spawn(e) => write!(f, "failed to spawn OpenViking: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::HealthCheck(msg) => write!(f, "health check failed: {msg}"),
        }
    }
}

impl std::error::Error for OvProcessError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_process_is_not_running() {
        let proc = OvProcess::new("openviking".to_string(), 1933, PathBuf::from("/tmp/ov"));
        assert!(!proc.is_running());
    }

    #[test]
    fn error_display() {
        assert!(OvProcessError::AlreadyRunning
            .to_string()
            .contains("already running"));
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        assert!(OvProcessError::Spawn(io_err).to_string().contains("spawn"));
    }
}
