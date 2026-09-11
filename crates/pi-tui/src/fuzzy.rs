//! Port of packages/tui/src/fuzzy.ts.

/// Result of matching one query token against one text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FuzzyMatch {
    pub matches: bool,
    pub score: f64,
}

pub fn fuzzy_match(query: &str, text: &str) -> FuzzyMatch {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();

    let match_query = |normalized_query: &str| -> FuzzyMatch {
        if normalized_query.is_empty() {
            return FuzzyMatch {
                matches: true,
                score: 0.0,
            };
        }

        let query_chars: Vec<char> = normalized_query.chars().collect();
        let text_chars: Vec<char> = text_lower.chars().collect();

        if query_chars.len() > text_chars.len() {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        }

        let mut query_index = 0usize;
        let mut score = 0.0f64;
        let mut last_match_index: i64 = -1;
        let mut consecutive_matches = 0i64;

        let mut i = 0usize;
        while i < text_chars.len() && query_index < query_chars.len() {
            if text_chars[i] == query_chars[query_index] {
                let is_word_boundary = i == 0
                    || matches!(
                        text_chars[i - 1],
                        ' ' | '\t' | '\n' | '\r' | '-' | '_' | '.' | '/' | ':'
                    );

                if last_match_index == i as i64 - 1 {
                    consecutive_matches += 1;
                    score -= consecutive_matches as f64 * 5.0;
                } else {
                    consecutive_matches = 0;
                    if last_match_index >= 0 {
                        score += (i as i64 - last_match_index - 1) as f64 * 2.0;
                    }
                }

                if is_word_boundary {
                    score -= 10.0;
                }

                score += i as f64 * 0.1;

                last_match_index = i as i64;
                query_index += 1;
            }
            i += 1;
        }

        if query_index < query_chars.len() {
            return FuzzyMatch {
                matches: false,
                score: 0.0,
            };
        }

        if normalized_query == text_lower {
            score -= 100.0;
        }

        FuzzyMatch {
            matches: true,
            score,
        }
    };

    let primary_match = match_query(&query_lower);
    if primary_match.matches {
        return primary_match;
    }

    // Numbers and letters are often typed in either order ("3d" vs "d3").
    let swapped_query = swap_alpha_numeric(&query_lower);
    let swapped_query = match swapped_query {
        Some(q) => q,
        None => return primary_match,
    };

    let swapped_match = match_query(&swapped_query);
    if !swapped_match.matches {
        return primary_match;
    }

    FuzzyMatch {
        matches: true,
        score: swapped_match.score + 5.0,
    }
}

/// Port of the `/^(?<letters>[a-z]+)(?<digits>[0-9]+)$/` and
/// `/^(?<digits>[0-9]+)(?<letters>[a-z]+)$/` swap.
fn swap_alpha_numeric(query_lower: &str) -> Option<String> {
    let digits = query_lower.chars().take_while(|c| c.is_ascii_digit()).count();
    let letters = query_lower.chars().take_while(|c| c.is_ascii_lowercase()).count();

    if letters > 0 && letters == query_lower.len() {
        return None;
    }
    if letters > 0 && query_lower[letters..].chars().all(|c| c.is_ascii_digit()) {
        let (l, d) = query_lower.split_at(letters);
        return Some(format!("{d}{l}"));
    }
    if digits > 0 && query_lower[digits..].chars().all(|c| c.is_ascii_lowercase()) {
        let (d, l) = query_lower.split_at(digits);
        return Some(format!("{l}{d}"));
    }
    None
}

/// An item paired with its match score.
#[derive(Debug, Clone)]
pub struct ScoredItem<T> {
    pub item: T,
    pub score: f64,
}

pub fn fuzzy_filter_scored<T: Clone>(
    items: &[T],
    query: &str,
    get_text: &dyn Fn(&T) -> String,
) -> Vec<ScoredItem<T>> {
    let tokens: Vec<&str> = query
        .trim()
        .split_whitespace()
        .filter(|t| !t.is_empty())
        .collect();

    if tokens.is_empty() {
        return items
            .iter()
            .map(|item| ScoredItem {
                item: item.clone(),
                score: 0.0,
            })
            .collect();
    }

    let mut results: Vec<ScoredItem<T>> = Vec::new();

    for item in items {
        let text = get_text(item);
        let mut total_score = 0.0f64;
        let mut all_match = true;

        for token in &tokens {
            let m = fuzzy_match(token, &text);
            if m.matches {
                total_score += m.score;
            } else {
                all_match = false;
                break;
            }
        }

        if all_match {
            results.push(ScoredItem {
                item: item.clone(),
                score: total_score,
            });
        }
    }

    results.sort_by(|a, b| a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal));
    results
}

pub fn fuzzy_filter<T: Clone>(items: &[T], query: &str, get_text: &dyn Fn(&T) -> String) -> Vec<T> {
    if query.trim().is_empty() {
        return items.to_vec();
    }
    fuzzy_filter_scored(items, query, get_text)
        .into_iter()
        .map(|r| r.item)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_matches_everything() {
        let m = fuzzy_match("", "anything");
        assert!(m.matches);
        assert_eq!(m.score, 0.0);
    }

    #[test]
    fn subsequence_matches_and_non_subsequence_does_not() {
        assert!(fuzzy_match("abc", "a-b-c").matches);
        assert!(!fuzzy_match("abc", "acb").matches);
        assert!(!fuzzy_match("abcd", "abc").matches);
    }

    #[test]
    fn consecutive_matches_score_better() {
        let tight = fuzzy_match("ab", "ab");
        let loose = fuzzy_match("ab", "a-b");
        assert!(tight.score < loose.score);
    }

    #[test]
    fn filter_sorts_by_score() {
        let items = vec!["alpha".to_string(), "beta".to_string(), "alp".to_string()];
        let out = fuzzy_filter(&items, "alp", &|s: &String| s.clone());
        assert_eq!(out.first().map(|s| s.as_str()), Some("alp"));
    }
}
