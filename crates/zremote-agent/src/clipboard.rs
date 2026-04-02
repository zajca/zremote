//! Shared clipboard image handling with fallback for headless environments.
//!
//! Strategy:
//! 1. Try `arboard::Clipboard::set_image()` directly (works when a display server is present,
//!    including Xvfb with DISPLAY set).
//! 2. If clipboard is unavailable, save the image to a temp file and return the path
//!    so the user can reference it manually (e.g., tell Claude Code to read the file).
//!
//! For headless environments, users can start Xvfb and set `DISPLAY=:99` before
//! running the agent to enable clipboard support.

/// Result of an image paste attempt.
pub enum ImagePasteOutcome {
    /// Clipboard was set successfully.
    Success,
    /// Clipboard failed; image saved to a temp file.
    Fallback { path: String, error: String },
}

/// Decode PNG bytes to RGBA and set on system clipboard via arboard.
fn try_arboard_set(png_bytes: &[u8]) -> Result<(), String> {
    let decoder = png::Decoder::new(png_bytes);
    let mut reader = decoder
        .read_info()
        .map_err(|e| format!("png decode: {e}"))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("png frame: {e}"))?;
    buf.truncate(info.buffer_size());

    let img_data = arboard::ImageData {
        width: info.width as usize,
        height: info.height as usize,
        bytes: std::borrow::Cow::Owned(buf),
    };

    let mut clipboard = arboard::Clipboard::new().map_err(|e| format!("clipboard init: {e}"))?;
    clipboard
        .set_image(img_data)
        .map_err(|e| format!("clipboard set: {e}"))?;
    Ok(())
}

/// Save PNG bytes to a temp file and return the path.
///
/// Uses `O_CREAT | O_EXCL` (via `create_new`) to atomically create the file,
/// preventing symlink race attacks on multi-user systems.
fn save_image_temp_file(png_bytes: &[u8], session_id: uuid::Uuid) -> Result<String, String> {
    use std::io::Write;

    let short_id = &session_id.to_string()[..8];
    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let path = format!("/tmp/zremote-paste-{short_id}-{ts}.png");

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true) // fails if file already exists (prevents symlink TOCTOU)
        .open(&path)
        .map_err(|e| format!("create temp file: {e}"))?;
    file.write_all(png_bytes)
        .map_err(|e| format!("write temp file: {e}"))?;
    Ok(path)
}

/// Try to set clipboard image and return the outcome.
/// On success, the caller should send Ctrl+V to the PTY.
/// On fallback, the caller should notify the user with the file path.
pub fn try_clipboard_paste(png_bytes: &[u8], session_id: uuid::Uuid) -> ImagePasteOutcome {
    // Try arboard directly (works if DISPLAY is set or on macOS/Windows)
    match try_arboard_set(png_bytes) {
        Ok(()) => return ImagePasteOutcome::Success,
        Err(e) => {
            tracing::info!(error = %e, "clipboard set failed, falling back to temp file");
        }
    }

    // Temp file fallback
    match save_image_temp_file(png_bytes, session_id) {
        Ok(path) => ImagePasteOutcome::Fallback {
            path,
            error: "clipboard unavailable (no display server)".to_string(),
        },
        Err(e) => ImagePasteOutcome::Fallback {
            path: String::new(),
            error: format!("clipboard unavailable and temp file save failed: {e}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_minimal_png() -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut buf, 1, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 255]).unwrap();
        }
        buf
    }

    #[test]
    fn save_temp_file_creates_valid_png() {
        let png_bytes = create_minimal_png();
        let session_id = uuid::Uuid::new_v4();
        let result = save_image_temp_file(&png_bytes, session_id);
        assert!(result.is_ok());
        let path = result.unwrap();
        assert!(path.starts_with("/tmp/zremote-paste-"));
        assert!(path.ends_with(".png"));
        let saved = std::fs::read(&path).unwrap();
        // Verify PNG magic bytes
        assert_eq!(&saved[..4], b"\x89PNG");
        // Verify content matches original
        assert_eq!(saved, png_bytes);
        // Cleanup
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn try_clipboard_paste_falls_back_on_headless() {
        // In CI/test environments without a display server, this should
        // fall back to saving a temp file.
        let png_bytes = create_minimal_png();
        let session_id = uuid::Uuid::new_v4();
        let outcome = try_clipboard_paste(&png_bytes, session_id);
        match outcome {
            ImagePasteOutcome::Success => {
                // This is fine too — test machine has a display server
            }
            ImagePasteOutcome::Fallback { path, error } => {
                assert!(!error.is_empty());
                if !path.is_empty() {
                    assert!(std::path::Path::new(&path).exists());
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}
