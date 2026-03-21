//! Fuzzy matching for command palette items.
//!
//! Case-insensitive substring matching with scoring based on match quality:
//! prefix matches, consecutive characters, word boundaries, and position.

// Indices and scores are small enough that truncation/wrap never occurs in practice.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss
)]

/// Result of a successful fuzzy match.
#[derive(Debug, Clone)]
pub struct FuzzyMatch {
    /// Higher is better.
    pub score: i32,
    /// Indices into the target string where query characters matched.
    pub matched_indices: Vec<usize>,
}

/// Case-insensitive fuzzy match of `query` against `target`.
///
/// Returns `None` if the query is empty or if any query character cannot be
/// found (in order) within the target.
pub fn fuzzy_match(query: &str, target: &str) -> Option<FuzzyMatch> {
    if query.is_empty() {
        return None;
    }

    let query_chars: Vec<char> = query.chars().collect();
    let target_chars: Vec<char> = target.chars().collect();

    // First pass: check if all query chars exist in order (case-insensitive).
    let mut matched_indices = Vec::with_capacity(query_chars.len());
    let mut search_start = 0;
    for &qc in &query_chars {
        let qc_lower = qc.to_lowercase().next()?;
        let mut found = false;
        for (i, &tc) in target_chars.iter().enumerate().skip(search_start) {
            if tc.to_lowercase().next() == Some(qc_lower) {
                matched_indices.push(i);
                search_start = i + 1;
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
    }

    // Try to optimize: prefer word-boundary and consecutive matches.
    let optimized = optimize_matches(&query_chars, &target_chars);
    let indices = optimized.unwrap_or(matched_indices);

    let score = compute_score(&query_chars, &target_chars, &indices);

    Some(FuzzyMatch {
        score,
        matched_indices: indices,
    })
}

/// Match query against "title subtitle" with reduced weight on subtitle matches.
///
/// Title matches score at full value; subtitle position scores are multiplied
/// by 0.5.
pub fn fuzzy_match_item(query: &str, title: &str, subtitle: &str) -> Option<FuzzyMatch> {
    let combined = format!("{title} {subtitle}");
    let result = fuzzy_match(query, &combined)?;

    let title_len = title.len();
    let mut adjusted_score = 0i32;
    let query_chars: Vec<char> = query.chars().collect();
    let combined_chars: Vec<char> = combined.chars().collect();

    for (qi, &idx) in result.matched_indices.iter().enumerate() {
        let qc = query_chars[qi];
        let tc = combined_chars[idx];

        // Case-exact bonus.
        if qc == tc {
            adjusted_score += 5;
        }

        // Consecutive bonus.
        if qi > 0 && idx == result.matched_indices[qi - 1] + 1 {
            adjusted_score += 10;
        }

        // Word boundary bonus.
        if is_word_boundary(&combined_chars, idx) {
            adjusted_score += 15;
        }

        // CamelCase transition bonus.
        if is_camel_transition(&combined_chars, idx) {
            adjusted_score += 8;
        }

        // Prefix bonus.
        if qi == 0 && idx == 0 {
            adjusted_score += 100;
        }

        // Position penalty with subtitle multiplier.
        let position_penalty = idx as i32;
        if idx > title_len {
            // Subtitle: half weight on position penalty.
            adjusted_score -= position_penalty / 2;
        } else {
            adjusted_score -= position_penalty;
        }

        // Gap penalty.
        if qi > 0 {
            let gap = idx as i32 - result.matched_indices[qi - 1] as i32 - 1;
            if gap > 0 {
                if idx > title_len {
                    adjusted_score -= gap * 3 / 2;
                } else {
                    adjusted_score -= gap * 3;
                }
            }
        }
    }

    Some(FuzzyMatch {
        score: adjusted_score,
        matched_indices: result.matched_indices,
    })
}

/// Try to find better match positions favoring word boundaries and consecutive runs.
fn optimize_matches(query_chars: &[char], target_chars: &[char]) -> Option<Vec<usize>> {
    let n = query_chars.len();
    let m = target_chars.len();
    if n == 0 || m == 0 {
        return None;
    }

    // Collect all candidate positions for each query char.
    let mut candidates: Vec<Vec<usize>> = Vec::with_capacity(n);
    for &qc in query_chars {
        let qc_lower = qc.to_lowercase().next()?;
        let positions: Vec<usize> = target_chars
            .iter()
            .enumerate()
            .filter(|(_, tc)| tc.to_lowercase().next() == Some(qc_lower))
            .map(|(i, _)| i)
            .collect();
        if positions.is_empty() {
            return None;
        }
        candidates.push(positions);
    }

    // Greedy: pick the best position for each query char, constrained to be after the previous.
    let mut best = Vec::with_capacity(n);
    let mut min_pos = 0;

    for (qi, positions) in candidates.iter().enumerate() {
        let mut best_pos = None;
        let mut best_pos_score = i32::MIN;

        for &pos in positions {
            if pos < min_pos {
                continue;
            }
            let mut s = 0i32;
            if is_word_boundary(target_chars, pos) {
                s += 15;
            }
            if is_camel_transition(target_chars, pos) {
                s += 8;
            }
            if qi > 0 && pos == best[qi - 1] + 1 {
                s += 10;
            }
            if qi == 0 && pos == 0 {
                s += 100;
            }
            s -= pos as i32;

            if s > best_pos_score {
                best_pos_score = s;
                best_pos = Some(pos);
            }
        }

        let pos = best_pos?;
        best.push(pos);
        min_pos = pos + 1;
    }

    Some(best)
}

fn compute_score(query_chars: &[char], target_chars: &[char], indices: &[usize]) -> i32 {
    let mut score = 0i32;

    for (qi, &idx) in indices.iter().enumerate() {
        let qc = query_chars[qi];
        let tc = target_chars[idx];

        // Exact prefix bonus.
        if qi == 0 && idx == 0 {
            score += 100;
        }

        // Consecutive match bonus.
        if qi > 0 && idx == indices[qi - 1] + 1 {
            score += 10;
        }

        // Word boundary bonus.
        if is_word_boundary(target_chars, idx) {
            score += 15;
        }

        // CamelCase transition bonus.
        if is_camel_transition(target_chars, idx) {
            score += 8;
        }

        // Position penalty.
        score -= idx as i32;

        // Gap penalty.
        if qi > 0 {
            let gap = idx as i32 - indices[qi - 1] as i32 - 1;
            if gap > 0 {
                score -= gap * 3;
            }
        }

        // Case-exact bonus.
        if qc == tc {
            score += 5;
        }
    }

    score
}

fn is_word_boundary(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    let prev = chars[idx - 1];
    prev == '_' || prev == '-' || prev == '/' || prev == ' '
}

fn is_camel_transition(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return false;
    }
    let prev = chars[idx - 1];
    let curr = chars[idx];
    prev.is_lowercase() && curr.is_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exact_prefix() {
        let m = fuzzy_match("my", "myproject").expect("should match");
        assert!(m.score > 0, "prefix match should score positively");
        assert_eq!(m.matched_indices, vec![0, 1]);
    }

    #[test]
    fn test_no_match() {
        assert!(fuzzy_match("xyz", "myproject").is_none());
    }

    #[test]
    fn test_word_boundary() {
        let m = fuzzy_match("mp", "my-project").expect("should match on boundary");
        assert!(
            m.matched_indices.contains(&3),
            "should match 'p' at word boundary"
        );
    }

    #[test]
    fn test_camel_case() {
        let m = fuzzy_match("mp", "MyProject").expect("should match camelCase");
        assert!(
            m.matched_indices.contains(&2),
            "should match 'P' at camel transition"
        );
    }

    #[test]
    fn test_consecutive_bonus() {
        let consecutive = fuzzy_match("proj", "project").expect("consecutive");
        let scattered = fuzzy_match("proj", "pxrxoxj").expect("scattered");
        assert!(
            consecutive.score > scattered.score,
            "consecutive ({}) should score higher than scattered ({})",
            consecutive.score,
            scattered.score
        );
    }

    #[test]
    fn test_case_exact_bonus() {
        let exact_case = fuzzy_match("My", "MyProject").expect("exact case");
        let wrong_case = fuzzy_match("my", "MyProject").expect("wrong case");
        assert!(
            exact_case.score > wrong_case.score,
            "exact case ({}) should score higher than wrong case ({})",
            exact_case.score,
            wrong_case.score
        );
    }

    #[test]
    fn test_position_penalty() {
        let start = fuzzy_match("ab", "abcdef").expect("start match");
        let end = fuzzy_match("ef", "abcdef").expect("end match");
        assert!(
            start.score > end.score,
            "start match ({}) should score higher than end match ({})",
            start.score,
            end.score
        );
    }

    #[test]
    fn test_empty_query() {
        assert!(fuzzy_match("", "myproject").is_none());
    }

    #[test]
    fn test_subtitle_multiplier() {
        let title_match = fuzzy_match_item("proj", "project", "other").expect("title match");
        let subtitle_match =
            fuzzy_match_item("othe", "something", "other").expect("subtitle match");
        assert!(
            title_match.score > subtitle_match.score,
            "title match ({}) should score higher than subtitle match ({})",
            title_match.score,
            subtitle_match.score
        );
    }
}
