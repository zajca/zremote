use std::path::PathBuf;

use zremote_protocol::SessionId;

/// Return the path to the port file for a given session.
pub fn port_file_path(session_id: &SessionId) -> Result<PathBuf, std::io::Error> {
    let home = std::env::var("HOME")
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set"))?;
    Ok(PathBuf::from(home)
        .join(".zremote")
        .join(format!("channel-{session_id}.port")))
}

/// Write the port number to the port file.
pub async fn write_port_file(session_id: &SessionId, port: u16) -> Result<(), std::io::Error> {
    let path = port_file_path(session_id)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(&path, port.to_string()).await?;
    // Restrict to owner-only to limit local attack surface
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    tracing::debug!(path = %path.display(), port, "wrote channel port file");
    Ok(())
}

/// Remove the port file for a given session.
pub async fn remove_port_file(session_id: &SessionId) -> Result<(), std::io::Error> {
    let path = port_file_path(session_id)?;
    tokio::fs::remove_file(&path).await
}

/// Read the port number from a session's port file.
pub async fn read_port_file(session_id: &SessionId) -> Result<u16, std::io::Error> {
    let path = port_file_path(session_id)?;
    let content = tokio::fs::read_to_string(&path).await?;
    content
        .trim()
        .parse::<u16>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn port_file_path_format() {
        if std::env::var("HOME").is_ok() {
            let id = Uuid::new_v4();
            let path = port_file_path(&id).unwrap();
            assert!(path.to_string_lossy().contains(".zremote"));
            assert!(
                path.to_string_lossy()
                    .contains(&format!("channel-{id}.port"))
            );
        }
    }

    #[tokio::test]
    async fn write_read_remove_lifecycle() {
        // Test the file I/O directly using a temp directory
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let path = dir.path().join(format!("channel-{id}.port"));

        // Write
        tokio::fs::write(&path, "12345").await.unwrap();
        let content = tokio::fs::read_to_string(&path).await.unwrap();
        let port: u16 = content.trim().parse().unwrap();
        assert_eq!(port, 12345);

        // Remove
        tokio::fs::remove_file(&path).await.unwrap();
        assert!(tokio::fs::read_to_string(&path).await.is_err());
    }

    #[tokio::test]
    async fn read_nonexistent_port_file() {
        let id = Uuid::new_v4();
        // With real HOME this file shouldn't exist
        let result = read_port_file(&id).await;
        assert!(result.is_err());
    }
}
