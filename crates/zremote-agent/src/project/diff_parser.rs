//! Unified-diff parser. State machine over the textual output of
//! `git diff --no-color --no-ext-diff`.
//!
//! Why we parse: the GUI must reason about line numbers, hunk boundaries, and
//! comment anchors; handing it raw text would push that work onto every
//! consumer (§4.2 of RFC).
//!
//! Shape mirrors Okena's reference parser: a state machine over the diff's
//! section markers (`diff --git`, `similarity index`, `rename from/to`,
//! `index`, `---`, `+++`, `Binary files`, `@@` hunk headers, context/add/
//! remove lines, and the "No newline at end of file" marker).

use zremote_protocol::project::{
    DiffFile, DiffFileStatus, DiffFileSummary, DiffHunk, DiffLine, DiffLineKind,
};

/// Parse a full `git diff` output into one `DiffFile` per file section.
///
/// Resilient to a truncated / malformed tail: whatever was parsed up to the
/// first parse error is still returned. Callers must not assume success is
/// only signalled by `Ok(_)`.
pub fn parse_unified_diff(input: &str) -> Vec<DiffFile> {
    let mut out = Vec::new();
    let mut it = input.split('\n').peekable();
    // `git diff` trims no trailing newline; `lines()` would also drop a blank
    // trailing line that we rely on as a section terminator. Using split('\n')
    // preserves boundaries exactly.
    while it.peek().is_some() {
        // Advance to the next `diff --git` marker. Anything before it (e.g.
        // preamble headers) is skipped.
        let mut found = false;
        while let Some(&line) = it.peek() {
            if line.starts_with("diff --git ") {
                found = true;
                break;
            }
            it.next();
        }
        if !found {
            break;
        }
        if let Some(file) = parse_one_file(&mut it) {
            out.push(file);
        } else {
            break;
        }
    }
    out
}

fn parse_one_file<'a, I: Iterator<Item = &'a str>>(
    it: &mut std::iter::Peekable<I>,
) -> Option<DiffFile> {
    let header = it.next()?; // "diff --git a/foo b/bar"
    // Extract a-path and b-path for fallback when no ---/+++ line is seen.
    let (a_path_raw, b_path_raw) = parse_diff_git_header(header)?;

    let mut old_path: Option<String> = None;
    let mut new_path: Option<String> = None;
    let mut status: Option<DiffFileStatus> = None;
    let mut binary = false;
    let mut old_sha: Option<String> = None;
    let mut new_sha: Option<String> = None;
    let mut old_mode: Option<String> = None;
    let mut new_mode: Option<String> = None;
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut submodule = false;

    // Walk the pre-hunk header lines until we hit a `@@` hunk, a blank line
    // that precedes the next `diff --git`, EOF, or a Binary marker.
    while let Some(&line) = it.peek() {
        if line.starts_with("diff --git ") {
            break;
        }
        if line.starts_with("@@ ") {
            break;
        }
        let line = it.next().unwrap();
        if let Some(rest) = line.strip_prefix("similarity index ") {
            // rename/copy — status determined by later "rename from/to" or
            // "copy from/to" lines. `rest` is the percentage; not used yet.
            let _ = rest;
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            old_path = Some(rest.to_string());
            status = Some(DiffFileStatus::Renamed);
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            new_path = Some(rest.to_string());
            status = Some(DiffFileStatus::Renamed);
        } else if let Some(rest) = line.strip_prefix("copy from ") {
            old_path = Some(rest.to_string());
            status = Some(DiffFileStatus::Copied);
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            new_path = Some(rest.to_string());
            status = Some(DiffFileStatus::Copied);
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            new_mode = Some(rest.to_string());
            if status.is_none() {
                status = Some(DiffFileStatus::Added);
            }
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            old_mode = Some(rest.to_string());
            if status.is_none() {
                status = Some(DiffFileStatus::Deleted);
            }
        } else if let Some(rest) = line.strip_prefix("old mode ") {
            old_mode = Some(rest.to_string());
            if status.is_none() {
                status = Some(DiffFileStatus::TypeChanged);
            }
        } else if let Some(rest) = line.strip_prefix("new mode ") {
            new_mode = Some(rest.to_string());
            if status.is_none() {
                status = Some(DiffFileStatus::TypeChanged);
            }
        } else if let Some(rest) = line.strip_prefix("index ") {
            // "index <old>..<new> <mode>" (mode optional)
            let before_space = rest.split(' ').next().unwrap_or(rest);
            if let Some((o, n)) = before_space.split_once("..") {
                if !o.chars().all(|c| c == '0') {
                    old_sha = Some(o.to_string());
                }
                if !n.chars().all(|c| c == '0') {
                    new_sha = Some(n.to_string());
                }
            }
        } else if let Some(rest) = line.strip_prefix("--- ") {
            // "--- a/<path>", "--- /dev/null", or with mnemonicPrefix
            // enabled one of i/, w/, c/, o/.
            if rest == "/dev/null" {
                if status.is_none() {
                    status = Some(DiffFileStatus::Added);
                }
            } else {
                old_path.get_or_insert_with(|| strip_diff_prefix(rest));
            }
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            if rest == "/dev/null" {
                if status.is_none() {
                    status = Some(DiffFileStatus::Deleted);
                }
            } else {
                new_path.get_or_insert_with(|| strip_diff_prefix(rest));
            }
        } else if line.starts_with("Binary files ") && line.ends_with(" differ") {
            binary = true;
            if status.is_none() {
                status = Some(DiffFileStatus::Modified);
            }
        } else if line.starts_with("GIT binary patch") {
            binary = true;
            // Swallow following lines until a blank separator or next file.
            while let Some(&next) = it.peek() {
                if next.is_empty() || next.starts_with("diff --git ") {
                    break;
                }
                it.next();
            }
        } else if line.starts_with("Submodule ") {
            submodule = true;
            if status.is_none() {
                status = Some(DiffFileStatus::Modified);
            }
        } else {
            // Unknown header — ignore (forward compat).
        }
    }

    // Hunks.
    while let Some(&line) = it.peek() {
        if !line.starts_with("@@ ") {
            break;
        }
        if let Some(hunk) = parse_hunk(it) {
            hunks.push(hunk);
        } else {
            break;
        }
    }

    // Fallbacks: if `---`/`+++` never fired (e.g. new-file or pure-rename),
    // fall back to the a/ b/ paths in the `diff --git` header.
    let final_new = new_path.clone().unwrap_or_else(|| b_path_raw.clone());
    let final_old_for_status = old_path.clone().unwrap_or_else(|| a_path_raw.clone());
    let display_path = final_new.clone();

    // Default to Modified if nothing set it.
    let status = status.unwrap_or(DiffFileStatus::Modified);

    // Old-path on the summary: only set for rename/copy where it differs.
    let summary_old_path = match status {
        DiffFileStatus::Renamed | DiffFileStatus::Copied => {
            if final_old_for_status == display_path {
                None
            } else {
                Some(final_old_for_status)
            }
        }
        DiffFileStatus::Deleted => Some(final_old_for_status),
        _ => None,
    };

    let (additions, deletions) = count_add_del(&hunks);

    Some(DiffFile {
        summary: DiffFileSummary {
            path: display_path,
            old_path: summary_old_path,
            status,
            binary,
            submodule,
            too_large: false,
            additions,
            deletions,
            old_sha,
            new_sha,
            old_mode,
            new_mode,
        },
        hunks,
    })
}

/// Strip the 2-character diff-side prefix from a `--- ` / `+++ ` line.
/// Git's default is `a/` / `b/`, but with `diff.mnemonicPrefix=true` the
/// prefixes become `i/`, `w/`, `c/`, `o/` to disambiguate index / working
/// tree / commit / output sides.
fn strip_diff_prefix(s: &str) -> String {
    const PREFIXES: &[&str] = &["a/", "b/", "i/", "w/", "c/", "o/"];
    for p in PREFIXES {
        if let Some(stripped) = s.strip_prefix(p) {
            return stripped.to_string();
        }
    }
    s.to_string()
}

fn parse_diff_git_header(header: &str) -> Option<(String, String)> {
    // "diff --git a/foo b/foo" (or with mnemonicPrefix: "i/foo w/foo" etc.)
    //
    // File paths may contain spaces if quoted; git escapes them with C-style
    // quoting. For v1 we accept the common case where neither side has a
    // space. Fallback to whitespace split keeps the parser alive on unusual
    // inputs (the ---/+++ lines usually carry the truth).
    let rest = header.strip_prefix("diff --git ")?;
    // Find a ` <prefix>/` that separates the two paths. Scanning from the
    // right handles paths that happen to contain a prefix-looking substring.
    for sep in [" b/", " a/", " w/", " i/", " c/", " o/"] {
        if let Some(pos) = rest.rfind(sep) {
            let a_part = &rest[..pos];
            let b_part = &rest[pos + 1..]; // include the leading prefix char
            let a_clean = strip_diff_prefix(a_part);
            let b_clean = strip_diff_prefix(b_part);
            if !a_clean.is_empty() && !b_clean.is_empty() {
                return Some((a_clean, b_clean));
            }
        }
    }
    // Fallback: split on whitespace, strip well-known prefixes from last two.
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() >= 2 {
        let a = strip_diff_prefix(parts[parts.len() - 2]);
        let b = strip_diff_prefix(parts[parts.len() - 1]);
        return Some((a, b));
    }
    None
}

fn parse_hunk<'a, I: Iterator<Item = &'a str>>(
    it: &mut std::iter::Peekable<I>,
) -> Option<DiffHunk> {
    let header_line = it.next()?; // "@@ -10,7 +10,8 @@ optional section heading"
    let (old_start, old_lines, new_start, new_lines) = parse_hunk_header(header_line)?;

    let mut lines: Vec<DiffLine> = Vec::new();
    let mut old_ln = old_start;
    let mut new_ln = new_start;
    let mut old_remaining = old_lines;
    let mut new_remaining = new_lines;

    while let Some(&line) = it.peek() {
        if line.starts_with("@@ ") || line.starts_with("diff --git ") {
            break;
        }
        // The final "\" line is tied to the immediately preceding +/- line,
        // not a new body line. It doesn't consume any hunk counters.
        if line.starts_with('\\') {
            let l = it.next().unwrap();
            lines.push(DiffLine {
                kind: DiffLineKind::NoNewlineMarker,
                old_lineno: None,
                new_lineno: None,
                content: l.trim_start_matches('\\').trim_start().to_string(),
            });
            continue;
        }
        // A hunk ends when we've consumed all promised lines, OR when the next
        // line is blank AND blank counts are exhausted — but `git diff` emits
        // body lines with the leading space for context even when empty, so a
        // truly blank line only appears after all body lines. Use the counts.
        if old_remaining == 0 && new_remaining == 0 {
            // Allow trailing "\" markers handled above; otherwise stop.
            break;
        }
        let l = it.next().unwrap();
        let (kind, rest) = match l.chars().next() {
            Some('+') => (DiffLineKind::Added, &l[1..]),
            Some('-') => (DiffLineKind::Removed, &l[1..]),
            Some(' ') => (DiffLineKind::Context, &l[1..]),
            None => (DiffLineKind::Context, ""),
            _ => {
                // Malformed line — treat as context so we don't desync.
                (DiffLineKind::Context, l)
            }
        };
        match kind {
            DiffLineKind::Context => {
                lines.push(DiffLine {
                    kind,
                    old_lineno: Some(old_ln),
                    new_lineno: Some(new_ln),
                    content: rest.to_string(),
                });
                old_ln += 1;
                new_ln += 1;
                old_remaining = old_remaining.saturating_sub(1);
                new_remaining = new_remaining.saturating_sub(1);
            }
            DiffLineKind::Added => {
                lines.push(DiffLine {
                    kind,
                    old_lineno: None,
                    new_lineno: Some(new_ln),
                    content: rest.to_string(),
                });
                new_ln += 1;
                new_remaining = new_remaining.saturating_sub(1);
            }
            DiffLineKind::Removed => {
                lines.push(DiffLine {
                    kind,
                    old_lineno: Some(old_ln),
                    new_lineno: None,
                    content: rest.to_string(),
                });
                old_ln += 1;
                old_remaining = old_remaining.saturating_sub(1);
            }
            DiffLineKind::NoNewlineMarker => unreachable!(),
        }
    }

    Some(DiffHunk {
        old_start,
        old_lines,
        new_start,
        new_lines,
        header: header_line.to_string(),
        lines,
    })
}

fn parse_hunk_header(s: &str) -> Option<(u32, u32, u32, u32)> {
    // "@@ -10,7 +10,8 @@ optional section heading"
    let rest = s.strip_prefix("@@ ")?;
    let close = rest.find(" @@")?;
    let ranges = &rest[..close];
    // "<- old> <+ new>"
    let mut parts = ranges.split_whitespace();
    let old_part = parts.next()?.strip_prefix('-')?;
    let new_part = parts.next()?.strip_prefix('+')?;
    let (old_start, old_lines) = parse_range(old_part);
    let (new_start, new_lines) = parse_range(new_part);
    Some((old_start, old_lines, new_start, new_lines))
}

fn parse_range(s: &str) -> (u32, u32) {
    // "10,7" or "10" (single-line; implicit count = 1)
    let mut it = s.splitn(2, ',');
    let start: u32 = it.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let count: u32 = it.next().and_then(|p| p.parse().ok()).unwrap_or(1);
    (start, count)
}

fn count_add_del(hunks: &[DiffHunk]) -> (u32, u32) {
    let mut add = 0u32;
    let mut del = 0u32;
    for h in hunks {
        for l in &h.lines {
            match l.kind {
                DiffLineKind::Added => add = add.saturating_add(1),
                DiffLineKind::Removed => del = del.saturating_add(1),
                _ => {}
            }
        }
    }
    (add, del)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_modification() {
        let input = "diff --git a/foo.txt b/foo.txt\n\
index abc..def 100644\n\
--- a/foo.txt\n\
+++ b/foo.txt\n\
@@ -1,3 +1,3 @@\n\
 line1\n\
-old\n\
+new\n\
 line3\n";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.summary.path, "foo.txt");
        assert_eq!(f.summary.status, DiffFileStatus::Modified);
        assert_eq!(f.summary.additions, 1);
        assert_eq!(f.summary.deletions, 1);
        assert_eq!(f.hunks.len(), 1);
        assert_eq!(f.hunks[0].lines.len(), 4);
        assert_eq!(f.hunks[0].lines[0].kind, DiffLineKind::Context);
        assert_eq!(f.hunks[0].lines[1].kind, DiffLineKind::Removed);
        assert_eq!(f.hunks[0].lines[2].kind, DiffLineKind::Added);
        assert_eq!(f.summary.old_sha.as_deref(), Some("abc"));
        assert_eq!(f.summary.new_sha.as_deref(), Some("def"));
    }

    #[test]
    fn parse_addition() {
        let input = "diff --git a/new.txt b/new.txt\n\
new file mode 100644\n\
index 0000000..def\n\
--- /dev/null\n\
+++ b/new.txt\n\
@@ -0,0 +1,2 @@\n\
+hello\n\
+world\n";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.summary.path, "new.txt");
        assert_eq!(f.summary.status, DiffFileStatus::Added);
        assert_eq!(f.summary.additions, 2);
        assert_eq!(f.summary.deletions, 0);
        assert_eq!(f.summary.old_sha, None);
        assert_eq!(f.summary.new_sha.as_deref(), Some("def"));
    }

    #[test]
    fn parse_deletion() {
        let input = "diff --git a/gone.txt b/gone.txt\n\
deleted file mode 100644\n\
index abc..0000000\n\
--- a/gone.txt\n\
+++ /dev/null\n\
@@ -1,2 +0,0 @@\n\
-bye\n\
-world\n";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.summary.path, "gone.txt");
        assert_eq!(f.summary.status, DiffFileStatus::Deleted);
        assert_eq!(f.summary.additions, 0);
        assert_eq!(f.summary.deletions, 2);
        assert_eq!(f.summary.old_path.as_deref(), Some("gone.txt"));
    }

    #[test]
    fn parse_rename() {
        let input = "diff --git a/old.txt b/new.txt\n\
similarity index 100%\n\
rename from old.txt\n\
rename to new.txt\n";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.summary.path, "new.txt");
        assert_eq!(f.summary.old_path.as_deref(), Some("old.txt"));
        assert_eq!(f.summary.status, DiffFileStatus::Renamed);
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn parse_binary_marker() {
        let input = "diff --git a/img.png b/img.png\n\
index abc..def 100644\n\
Binary files a/img.png and b/img.png differ\n";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(f.summary.binary);
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn parse_no_newline_at_eof() {
        let input = "diff --git a/a.txt b/a.txt\n\
index abc..def 100644\n\
--- a/a.txt\n\
+++ b/a.txt\n\
@@ -1 +1 @@\n\
-old\n\
\\ No newline at end of file\n\
+new\n\
\\ No newline at end of file\n";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 1);
        let hunk = &files[0].hunks[0];
        let has_marker = hunk
            .lines
            .iter()
            .any(|l| l.kind == DiffLineKind::NoNewlineMarker);
        assert!(has_marker, "parser must keep \\ No newline marker");
    }

    #[test]
    fn parse_malformed_tail_returns_prefix() {
        // Truncated in the middle of a hunk — we should still get the first
        // file's partial result (or at least not panic).
        let input = "diff --git a/a.txt b/a.txt\n\
--- a/a.txt\n\
+++ b/a.txt\n\
@@ -1,2 +1,2";
        let _files = parse_unified_diff(input);
        // Expectation: parser doesn't panic. Content is best-effort.
    }

    #[test]
    fn parse_multiple_files() {
        let input = "diff --git a/a.txt b/a.txt\n\
index 1..2\n\
--- a/a.txt\n\
+++ b/a.txt\n\
@@ -1 +1 @@\n\
-old\n\
+new\n\
diff --git a/b.txt b/b.txt\n\
index 3..4\n\
--- a/b.txt\n\
+++ b/b.txt\n\
@@ -1 +1 @@\n\
-foo\n\
+bar\n";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].summary.path, "a.txt");
        assert_eq!(files[1].summary.path, "b.txt");
    }

    #[test]
    fn parse_line_numbers_track_correctly() {
        // Hunk header: `@@ -10,3 +20,4 @@`. One context, one remove, two add.
        let input = "diff --git a/f.txt b/f.txt\n\
index 1..2\n\
--- a/f.txt\n\
+++ b/f.txt\n\
@@ -10,3 +20,4 @@\n\
 ctx\n\
-rm\n\
+a1\n\
+a2\n";
        let files = parse_unified_diff(input);
        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.old_start, 10);
        assert_eq!(hunk.new_start, 20);
        // ctx → old=10, new=20
        assert_eq!(hunk.lines[0].old_lineno, Some(10));
        assert_eq!(hunk.lines[0].new_lineno, Some(20));
        // rm → old=11, new=None
        assert_eq!(hunk.lines[1].old_lineno, Some(11));
        assert_eq!(hunk.lines[1].new_lineno, None);
        // a1 → old=None, new=21
        assert_eq!(hunk.lines[2].old_lineno, None);
        assert_eq!(hunk.lines[2].new_lineno, Some(21));
        // a2 → old=None, new=22
        assert_eq!(hunk.lines[3].old_lineno, None);
        assert_eq!(hunk.lines[3].new_lineno, Some(22));
    }

    #[test]
    fn parse_crlf_lines_in_content() {
        // A context line whose content contains a literal '\r' must survive
        // intact — `git diff` emits LF-terminated records but the body can
        // carry any byte except LF.
        let input = "diff --git a/f.txt b/f.txt\n\
index 1..2\n\
--- a/f.txt\n\
+++ b/f.txt\n\
@@ -1 +1 @@\n\
-abc\r\n\
+def\r\n";
        let files = parse_unified_diff(input);
        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.lines[0].content, "abc\r");
        assert_eq!(hunk.lines[1].content, "def\r");
    }

    #[test]
    fn parse_empty_input() {
        assert!(parse_unified_diff("").is_empty());
    }
}
