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

/// Match query against word-initial letters of target.
///
/// Returns the indices of matched word-boundary characters if every query
/// character matches a successive word initial (case-insensitive). Returns
/// `None` if the query is empty or if any query character has no matching
/// word initial.
///
/// Example: `acronym_match("nt", "New Terminal")` → `Some(vec![0, 4])`
pub fn acronym_match(query: &str, target: &str) -> Option<Vec<usize>> {
    if query.is_empty() {
        return None;
    }

    let query_chars: Vec<char> = query.chars().collect();
    let target_chars: Vec<char> = target.chars().collect();

    // Collect positions of word-initial characters.
    let word_starts: Vec<usize> = target_chars
        .iter()
        .enumerate()
        .filter(|&(i, _)| is_word_boundary(&target_chars, i))
        .map(|(i, _)| i)
        .collect();

    let mut matched = Vec::with_capacity(query_chars.len());
    let mut ws_idx = 0;

    for &qc in &query_chars {
        let qc_lower = qc.to_lowercase().next()?;
        let mut found = false;
        while ws_idx < word_starts.len() {
            let pos = word_starts[ws_idx];
            ws_idx += 1;
            if target_chars[pos].to_lowercase().next() == Some(qc_lower) {
                matched.push(pos);
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
    }

    Some(matched)
}

/// Explicit match tier. Lower numeric value = better match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MatchTier {
    /// Query exactly equals the title (case-insensitive).
    ExactTitle = 1,
    /// Title starts with the query (case-insensitive).
    TitlePrefix = 2,
    /// Word-initial letters match (acronym).
    Acronym = 3,
    /// All matched chars land on word boundaries in title.
    WordBoundary = 4,
    /// General fuzzy match in title.
    TitleSubstring = 5,
    /// Match involves subtitle characters.
    SubtitleMatch = 6,
}

/// Extended match result with tier classification.
#[derive(Debug, Clone)]
pub struct TieredMatch {
    pub tier: MatchTier,
    /// Intra-tier tiebreaker score (from legacy fuzzy scoring).
    pub score: i32,
    pub matched_indices: Vec<usize>,
}

/// Tier-aware match of query against title + subtitle.
///
/// Tries tiers top-down (T1 -> T6), returns the best (lowest-numbered) tier
/// that matches. Uses existing `fuzzy_match()`, `fuzzy_match_item()`, and
/// `acronym_match()` internally.
pub fn tiered_match_item(query: &str, title: &str, subtitle: &str) -> Option<TieredMatch> {
    if query.is_empty() {
        return None;
    }

    let q_lower: String = query.chars().flat_map(char::to_lowercase).collect();
    let t_lower: String = title.chars().flat_map(char::to_lowercase).collect();

    // T1: Exact title match (case-insensitive).
    if q_lower == t_lower {
        let indices: Vec<usize> = (0..title.chars().count()).collect();
        return Some(TieredMatch {
            tier: MatchTier::ExactTitle,
            score: i32::MAX,
            matched_indices: indices,
        });
    }

    // T2: Title starts-with (case-insensitive).
    // Indices are the leading character positions — no need for a full fuzzy scan.
    if t_lower.starts_with(&q_lower) {
        let char_count = query.chars().count();
        let indices: Vec<usize> = (0..char_count).collect();
        return Some(TieredMatch {
            tier: MatchTier::TitlePrefix,
            score: i32::MAX - 1,
            matched_indices: indices,
        });
    }

    // T3: Acronym match (word-initial letters).
    if let Some(indices) = acronym_match(query, title) {
        return Some(TieredMatch {
            tier: MatchTier::Acronym,
            score: 0,
            matched_indices: indices,
        });
    }

    // T4/T5: Fuzzy match in title only.
    if let Some(fm) = fuzzy_match(query, title) {
        let title_chars: Vec<char> = title.chars().collect();
        let all_on_boundaries = fm
            .matched_indices
            .iter()
            .all(|&i| is_word_boundary(&title_chars, i) || is_camel_transition(&title_chars, i));
        if all_on_boundaries {
            return Some(TieredMatch {
                tier: MatchTier::WordBoundary,
                score: fm.score,
                matched_indices: fm.matched_indices,
            });
        }
        return Some(TieredMatch {
            tier: MatchTier::TitleSubstring,
            score: fm.score,
            matched_indices: fm.matched_indices,
        });
    }

    // T6: Match involves subtitle.
    if let Some(fm) = fuzzy_match_item(query, title, subtitle) {
        return Some(TieredMatch {
            tier: MatchTier::SubtitleMatch,
            score: fm.score,
            matched_indices: fm.matched_indices,
        });
    }

    None
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

    // --- Acronym matching tests ---

    #[test]
    fn test_acronym_basic() {
        let m = acronym_match("nt", "New Terminal").expect("should match");
        assert_eq!(m, vec![0, 4]);
    }

    #[test]
    fn test_acronym_case_insensitive() {
        let m = acronym_match("cs", "Close Session").expect("should match");
        assert_eq!(m, vec![0, 6]);
    }

    #[test]
    fn test_acronym_three_words() {
        let m = acronym_match("nts", "New Terminal Session").expect("should match");
        assert_eq!(m, vec![0, 4, 13]);
    }

    #[test]
    fn test_acronym_no_match() {
        assert!(acronym_match("xyz", "New Terminal").is_none());
    }

    #[test]
    fn test_acronym_single_char() {
        let m = acronym_match("n", "New Terminal").expect("should match");
        assert_eq!(m, vec![0]);
    }

    #[test]
    fn test_acronym_rejects_non_initial() {
        // "e" is not a word-initial character in "New Terminal"
        assert!(acronym_match("ne", "New Terminal").is_none());
    }

    #[test]
    fn test_acronym_hyphenated() {
        let m = acronym_match("pa", "project-admin").expect("should match hyphenated words");
        assert_eq!(m, vec![0, 8]);
    }

    #[test]
    fn test_acronym_empty_query() {
        assert!(acronym_match("", "New Terminal").is_none());
    }

    #[test]
    fn test_acronym_empty_target() {
        assert!(acronym_match("nt", "").is_none());
    }

    #[test]
    fn test_acronym_more_query_than_words() {
        // 3 query chars but only 2 words
        assert!(acronym_match("nts", "New Terminal").is_none());
    }

    // --- Tiered matching tests ---

    #[test]
    fn test_tier_exact_title() {
        let m = tiered_match_item("New Session", "New Session", "").unwrap();
        assert_eq!(m.tier, MatchTier::ExactTitle);
    }

    #[test]
    fn test_tier_exact_case_insensitive() {
        let m = tiered_match_item("new session", "New Session", "").unwrap();
        assert_eq!(m.tier, MatchTier::ExactTitle);
    }

    #[test]
    fn test_tier_prefix() {
        let m = tiered_match_item("New", "New Session", "").unwrap();
        assert_eq!(m.tier, MatchTier::TitlePrefix);
    }

    #[test]
    fn test_tier_acronym() {
        let m = tiered_match_item("ns", "New Session", "").unwrap();
        assert_eq!(m.tier, MatchTier::Acronym);
    }

    #[test]
    fn test_tier_title_substring() {
        let m = tiered_match_item("essi", "New Session", "").unwrap();
        assert_eq!(m.tier, MatchTier::TitleSubstring);
    }

    #[test]
    fn test_tier_subtitle_match() {
        let m = tiered_match_item("myhost", "New Session", "myhost.example.com").unwrap();
        assert_eq!(m.tier, MatchTier::SubtitleMatch);
    }

    #[test]
    fn test_tier_no_match() {
        assert!(tiered_match_item("zzz", "New Session", "subtitle").is_none());
    }

    #[test]
    fn test_tier_ordering() {
        assert!(MatchTier::ExactTitle < MatchTier::TitlePrefix);
        assert!(MatchTier::TitlePrefix < MatchTier::Acronym);
        assert!(MatchTier::Acronym < MatchTier::WordBoundary);
        assert!(MatchTier::WordBoundary < MatchTier::TitleSubstring);
        assert!(MatchTier::TitleSubstring < MatchTier::SubtitleMatch);
    }

    #[test]
    fn test_tier_empty_query() {
        assert!(tiered_match_item("", "New Session", "").is_none());
    }

    // --- Tier precedence tests (integration-level) ---

    #[test]
    fn test_tier_beats_score() {
        // ExactTitle (T1) must rank above Acronym (T3) regardless of score.
        let exact = tiered_match_item("New Session", "New Session", "").unwrap();
        let acronym = tiered_match_item("ns", "New Session", "").unwrap();
        assert!(exact.tier < acronym.tier, "T1 must beat T3");
    }

    #[test]
    fn test_prefix_beats_acronym() {
        let prefix = tiered_match_item("New", "New Session", "").unwrap();
        let acronym = tiered_match_item("ns", "New Session", "").unwrap();
        assert!(prefix.tier < acronym.tier, "T2 must beat T3");
    }

    #[test]
    fn test_title_match_beats_subtitle() {
        // A title substring match (T5) must beat a subtitle-only match (T6).
        let title = tiered_match_item("essi", "New Session", "some host").unwrap();
        let sub = tiered_match_item("host", "New Session", "some host").unwrap();
        assert!(title.tier < sub.tier, "T5 must beat T6");
    }

    #[test]
    fn test_prefix_indices_are_leading() {
        let m = tiered_match_item("New", "New Session", "").unwrap();
        assert_eq!(m.tier, MatchTier::TitlePrefix);
        assert_eq!(m.matched_indices, vec![0, 1, 2]);
    }
}
