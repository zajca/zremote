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

    #[test]
    fn extract_slug_in_later_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"message","role":"user"}}"#).unwrap();
        writeln!(f, r#"{{"type":"message","role":"assistant"}}"#).unwrap();
        writeln!(f, r#"{{"type":"tool_use","name":"bash"}}"#).unwrap();
        writeln!(f, r#"{{"type":"result","slug":"deep-slug","cost":1.2}}"#).unwrap();
        writeln!(f, r#"{{"type":"done"}}"#).unwrap();

        let (slug, new_offset) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(slug.as_deref(), Some("deep-slug"));
        assert!(new_offset > 0);
    }

    #[test]
    fn extract_slug_malformed_json_lines_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // Malformed line that contains "slug" but is not valid JSON
        writeln!(f, r#"{{not valid json "slug": "bad"}}"#).unwrap();
        // Another malformed line
        writeln!(f, r#"totally not json with "slug" in it"#).unwrap();
        // Valid line with slug
        writeln!(f, r#"{{"type":"result","slug":"valid-slug"}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(slug.as_deref(), Some("valid-slug"));
    }

    #[test]
    fn extract_slug_malformed_json_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{broken "slug": "nope"}}"#).unwrap();
        writeln!(f, r#"also broken "slug""#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert!(slug.is_none());
    }

    #[test]
    fn extract_slug_with_empty_lines_interspersed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"message"}}"#).unwrap();
        writeln!(f).unwrap(); // empty line
        writeln!(f).unwrap(); // another empty line
        writeln!(f, r#"{{"type":"result","slug":"after-blanks"}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(slug.as_deref(), Some("after-blanks"));
    }

    #[test]
    fn extract_slug_large_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let large_slug = "a".repeat(10_000);
        writeln!(f, r#"{{"type":"result","slug":"{}"}}"#, large_slug).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(slug.as_deref(), Some(large_slug.as_str()));
    }

    #[test]
    fn extract_slug_returns_first_match() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"result","slug":"first-slug"}}"#).unwrap();
        writeln!(f, r#"{{"type":"result","slug":"second-slug"}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(slug.as_deref(), Some("first-slug"));
    }

    #[test]
    fn extract_slug_offset_past_end_of_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"type":"result","slug":"some-slug"}}"#).unwrap();

        let (slug, new_offset) = extract_slug(path.to_str().unwrap(), 99999).unwrap();
        assert!(slug.is_none());
        // new_offset is file_len regardless
        assert!(new_offset < 99999);
    }

    #[test]
    fn extract_slug_field_not_string() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // slug is a number, not a string - as_str() should return None
        writeln!(f, r#"{{"type":"result","slug":42}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert!(slug.is_none());
    }

    #[test]
    fn extract_slug_substring_in_value_not_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // Contains "slug" as a substring in a value, not as a key
        writeln!(f, r#"{{"type":"message","content":"the slug is here"}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert!(slug.is_none());
    }

    #[test]
    fn extract_slug_new_offset_equals_file_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        let content = r#"{"type":"result","slug":"test"}"#;
        writeln!(f, "{content}").unwrap();

        let file_len = std::fs::metadata(&path).unwrap().len();
        let (_, new_offset) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(new_offset, file_len);
    }
}
