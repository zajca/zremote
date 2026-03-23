use std::io;

/// Extract the `slug` field from a JSONL transcript file starting at `offset`.
///
/// Reads lines from `offset`, looking for the first JSON object with a `"slug"` field.
/// Returns `(slug, new_offset)` where `new_offset` is the end of the file.
pub fn extract_slug(path: &str, offset: u64) -> Result<(Option<String>, u64), io::Error> {
    use std::io::{BufRead, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    file.seek(SeekFrom::Start(offset))?;

    let reader = io::BufReader::new(file);
    let mut slug = None;

    for line in reader.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        // Quick check before full parse
        if !line.contains("\"slug\"") {
            continue;
        }
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line)
            && let Some(s) = obj.get("slug").and_then(|v| v.as_str())
        {
            slug = Some(s.to_string());
            break;
        }
    }

    Ok((slug, file_len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn extract_slug_from_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"message","role":"user"}}"#).unwrap();
        writeln!(f, r#"{{"type":"result","slug":"fix-tests","cost":0.5}}"#).unwrap();

        let (slug, new_offset) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(slug.as_deref(), Some("fix-tests"));
        assert!(new_offset > 0);
    }

    #[test]
    fn extract_slug_not_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"message","role":"user"}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert!(slug.is_none());
    }

    #[test]
    fn extract_slug_with_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let line1 = r#"{"type":"result","slug":"old-slug"}"#;
        writeln!(f, "{line1}").unwrap();
        let offset = (line1.len() + 1) as u64; // +1 for newline
        writeln!(f, r#"{{"type":"result","slug":"new-slug"}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), offset).unwrap();
        assert_eq!(slug.as_deref(), Some("new-slug"));
    }

    #[test]
    fn extract_slug_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        std::fs::File::create(&path).unwrap();

        let (slug, offset) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert!(slug.is_none());
        assert_eq!(offset, 0);
    }

    #[test]
    fn extract_slug_nonexistent_file() {
        let result = extract_slug("/nonexistent/path/transcript.jsonl", 0);
        assert!(result.is_err());
    }
}
