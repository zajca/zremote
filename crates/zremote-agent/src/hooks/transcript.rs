use std::io;

/// Maximum bytes read for a single transcript line. JSONL records are small;
/// cap the per-line read so a malformed/hostile transcript (a multi-GB line, or
/// no newline at all) cannot OOM the parser (CWE-400). Over-long lines are
/// skipped, not truncated-then-parsed (a partial JSON object would not parse).
const MAX_LINE_BYTES: u64 = 1_048_576;

/// Extract the `slug` field from a JSONL transcript file starting at `offset`.
///
/// Reads lines from `offset`, looking for the first JSON object with a `"slug"` field.
/// Returns `(slug, new_offset)` where `new_offset` is the end of the file.
///
/// Each line read is bounded to [`MAX_LINE_BYTES`]; a line longer than that is
/// skipped (treated as not containing a parseable record).
pub fn extract_slug(path: &str, offset: u64) -> Result<(Option<String>, u64), io::Error> {
    use std::io::{Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let file_len = file.metadata()?.len();
    file.seek(SeekFrom::Start(offset))?;

    let mut reader = io::BufReader::new(file);
    let mut slug = None;

    loop {
        let (line, hit_eof) = read_capped_line(&mut reader)?;
        match line {
            None if hit_eof => break, // clean EOF
            None => continue,         // over-long line was skipped; keep going
            Some(line) => {
                let Ok(text) = std::str::from_utf8(&line) else {
                    if hit_eof {
                        break;
                    }
                    continue; // non-UTF-8 line, skip
                };
                let text = text.trim_end_matches(['\n', '\r']);
                // Quick check before full parse
                if !text.is_empty()
                    && text.contains("\"slug\"")
                    && let Ok(obj) = serde_json::from_str::<serde_json::Value>(text)
                    && let Some(s) = obj.get("slug").and_then(|v| v.as_str())
                {
                    slug = Some(s.to_string());
                    break;
                }
                if hit_eof {
                    break;
                }
            }
        }
    }

    Ok((slug, file_len))
}

/// Read one line (up to and including its `\n`) capped at [`MAX_LINE_BYTES`].
///
/// Returns `(Some(line), hit_eof)` for a within-cap line. If a line exceeds the
/// cap it is consumed up to its newline and `(None, false)` is returned so the
/// caller skips it without OOM (a partial JSON object would not parse anyway).
/// `(None, true)` signals clean EOF with no further data.
fn read_capped_line<R: std::io::BufRead>(
    reader: &mut R,
) -> Result<(Option<Vec<u8>>, bool), io::Error> {
    let mut line = Vec::new();
    let mut over_cap = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            // EOF: return whatever we have (None if nothing/over-cap).
            return Ok((
                if over_cap || line.is_empty() {
                    None
                } else {
                    Some(line)
                },
                true,
            ));
        }
        match available.iter().position(|&b| b == b'\n') {
            Some(idx) => {
                // `idx + 1` bytes (including the newline) would be appended;
                // clippy prefers the strict-`<` form of `len + idx + 1 <= MAX`.
                if !over_cap && line.len() + idx < MAX_LINE_BYTES as usize {
                    line.extend_from_slice(&available[..=idx]);
                } else {
                    over_cap = true;
                }
                reader.consume(idx + 1);
                return Ok((if over_cap { None } else { Some(line) }, false));
            }
            None => {
                let take = available.len();
                if !over_cap && line.len() + take <= MAX_LINE_BYTES as usize {
                    line.extend_from_slice(available);
                } else {
                    over_cap = true; // discard rest of this over-long line
                }
                reader.consume(take);
            }
        }
    }
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
        writeln!(f, r#"{{"type":"result","slug":"{large_slug}"}}"#).unwrap();

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

    #[test]
    fn extract_slug_skips_overlong_line_then_finds_next() {
        // An over-long line (> MAX_LINE_BYTES) must be skipped without OOM, and a
        // valid slug on a following line must still be found (resync works).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        // A single line larger than the 1 MB cap (no embedded newline).
        let huge = "x".repeat((MAX_LINE_BYTES as usize) + 1024);
        writeln!(f, r#"{{"type":"junk","data":"{huge}"}}"#).unwrap();
        // A normal slug line after it.
        writeln!(f, r#"{{"type":"result","slug":"after-huge"}}"#).unwrap();

        let (slug, _) = extract_slug(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(
            slug.as_deref(),
            Some("after-huge"),
            "must skip the over-long line and find the next valid slug"
        );
    }
}
