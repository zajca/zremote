//! Collapsed-hunk rendering for large files. P3 stub — returns hunks
//! unchanged. P6 fills in real collapse/expand semantics.

use zremote_protocol::project::DiffHunk;

/// Maybe collapse hunks in a large file. MVP: pass-through.
#[must_use]
pub fn prepare_hunks(hunks: &[DiffHunk]) -> Vec<DiffHunk> {
    hunks.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_hunks_is_identity_in_mvp() {
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_lines: 1,
            new_start: 1,
            new_lines: 1,
            header: "@@ -1 +1 @@".to_string(),
            lines: vec![],
        }];
        let out = prepare_hunks(&hunks);
        assert_eq!(out.len(), 1);
    }
}
