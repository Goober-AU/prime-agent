//! Port of packages/coding-agent/src/cli/list-models.ts
//!
//! TODO(slice): `ModelRegistry` (ca-models slice, core/model-registry.ts),
//! `formatNoModelsAvailableMessage` (ca-session-core slice, core/auth-guidance.ts)
//! and `fuzzyFilter` (tui-core slice, packages/tui/src/fuzzy.ts) are not landed.
//! Private local stand-ins below keep the same behaviour and are listed in the
//! slice status file.

use pi_ai::types::Model;

fn format_token_count(count: f64) -> String {
    if count >= 1_000_000.0 {
        let millions = count / 1_000_000.0;
        return if millions.fract() == 0.0 {
            format!("{}M", millions)
        } else {
            format!("{:.1}M", millions)
        };
    }
    if count >= 1_000.0 {
        let thousands = count / 1_000.0;
        return if thousands.fract() == 0.0 {
            format!("{}K", thousands)
        } else {
            format!("{:.1}K", thousands)
        };
    }
    format_number(count)
}

/// JavaScript `String(number)`: integral values print without a trailing `.0`.
fn format_number(count: f64) -> String {
    if count.fract() == 0.0 && count.is_finite() {
        format!("{}", count as i64)
    } else {
        format!("{}", count)
    }
}

/// Callback surface for `console.log` / `console.error`.
pub struct ListModelsIo<'a> {
    pub log: &'a dyn Fn(&str),
    pub error: &'a dyn Fn(&str),
}

/// Minimal `ModelRegistry` surface used by this module.
pub trait ModelRegistry {
    fn get_error(&self) -> Option<String>;
    fn refresh_available_models(&self) -> Vec<Model>;
}

pub fn list_models(registry: &dyn ModelRegistry, search_pattern: Option<&str>, io: &ListModelsIo<'_>) {
    if let Some(load_error) = registry.get_error() {
        (io.error)(&format!("Warning: errors loading models.json:\n{}", load_error));
    }

    let models = registry.refresh_available_models();

    if models.is_empty() {
        (io.log)(&format_no_models_available_message());
        return;
    }

    let mut filtered_models: Vec<Model> = models;
    if let Some(search_pattern) = search_pattern {
        filtered_models = fuzzy_filter(filtered_models, search_pattern, &|model: &Model| {
            format!("{} {}", model.provider, model.id)
        });
    }

    if filtered_models.is_empty() {
        (io.log)(&format!("No models matching \"{}\"", search_pattern.unwrap_or("")));
        return;
    }

    filtered_models.sort_by(|left, right| {
        let provider_cmp = left.provider.cmp(&right.provider);
        if provider_cmp != std::cmp::Ordering::Equal {
            return provider_cmp;
        }
        left.id.cmp(&right.id)
    });

    let rows: Vec<ModelRow> = filtered_models
        .iter()
        .map(|model| ModelRow {
            provider: model.provider.clone(),
            model: model.id.clone(),
            context: format_token_count(model.context_window),
            max_out: format_token_count(model.max_tokens),
            thinking: if model.reasoning { "yes".to_string() } else { "no".to_string() },
            images: if model.input.iter().any(|input| input.as_str() == "image") {
                "yes".to_string()
            } else {
                "no".to_string()
            },
        })
        .collect();

    let headers = ModelRow {
        provider: "provider".to_string(),
        model: "model".to_string(),
        context: "context".to_string(),
        max_out: "max-out".to_string(),
        thinking: "thinking".to_string(),
        images: "images".to_string(),
    };

    let widths = ColumnWidths {
        provider: max_width(&headers.provider, rows.iter().map(|row| &row.provider)),
        model: max_width(&headers.model, rows.iter().map(|row| &row.model)),
        context: max_width(&headers.context, rows.iter().map(|row| &row.context)),
        max_out: max_width(&headers.max_out, rows.iter().map(|row| &row.max_out)),
        thinking: max_width(&headers.thinking, rows.iter().map(|row| &row.thinking)),
        images: max_width(&headers.images, rows.iter().map(|row| &row.images)),
    };

    (io.log)(&format_row(&headers, &widths));
    for row in &rows {
        (io.log)(&format_row(row, &widths));
    }
}

struct ModelRow {
    provider: String,
    model: String,
    context: String,
    max_out: String,
    thinking: String,
    images: String,
}

struct ColumnWidths {
    provider: usize,
    model: usize,
    context: usize,
    max_out: usize,
    thinking: usize,
    images: usize,
}

fn max_width<'a>(header: &str, values: impl Iterator<Item = &'a String>) -> usize {
    values
        .map(|value| value.chars().count())
        .chain(std::iter::once(header.chars().count()))
        .max()
        .unwrap_or(0)
}

/// `String.prototype.padEnd`, which pads with spaces on the right.
fn pad_end(value: &str, width: usize) -> String {
    let length = value.chars().count();
    if length >= width {
        return value.to_string();
    }
    format!("{}{}", value, " ".repeat(width - length))
}

fn format_row(row: &ModelRow, widths: &ColumnWidths) -> String {
    [
        pad_end(&row.provider, widths.provider),
        pad_end(&row.model, widths.model),
        pad_end(&row.context, widths.context),
        pad_end(&row.max_out, widths.max_out),
        pad_end(&row.thinking, widths.thinking),
        pad_end(&row.images, widths.images),
    ]
    .join("  ")
}

// ---------------------------------------------------------------------------
// Private local stand-ins for not-yet-landed slices.
// ---------------------------------------------------------------------------

/// Local stand-in for `formatNoModelsAvailableMessage` from
/// ../core/auth-guidance.js.
fn format_no_models_available_message() -> String {
    "No models available. Log in with `/login` or set an API key environment variable.".to_string()
}

/// Local stand-in for `fuzzyFilter(items, query, getText)` from pi-tui.
fn fuzzy_filter<T: Clone>(items: Vec<T>, query: &str, get_text: &dyn Fn(&T) -> String) -> Vec<T> {
    if query.trim().is_empty() {
        return items;
    }
    fuzzy_filter_scored(items, query, get_text)
        .into_iter()
        .map(|scored| scored.item)
        .collect()
}

struct ScoredItem<T> {
    item: T,
    score: f64,
}

/// Local stand-in for `fuzzyFilterScored` from pi-tui.
fn fuzzy_filter_scored<T: Clone>(items: Vec<T>, query: &str, get_text: &dyn Fn(&T) -> String) -> Vec<ScoredItem<T>> {
    let tokens: Vec<&str> = query.split_whitespace().collect();
    if tokens.is_empty() {
        return items.into_iter().map(|item| ScoredItem { item, score: 0.0 }).collect();
    }

    let mut results: Vec<ScoredItem<T>> = Vec::new();
    for item in items {
        let text = get_text(&item);
        let mut total_score = 0.0;
        let mut all_match = true;
        for token in &tokens {
            match fuzzy_match(token, &text) {
                Some(score) => total_score += score,
                None => {
                    all_match = false;
                    break;
                }
            }
        }
        if all_match {
            results.push(ScoredItem { item, score: total_score });
        }
    }
    results.sort_by(|left, right| left.score.partial_cmp(&right.score).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// Local stand-in for `fuzzyMatch` from pi-tui: returns the score when the query
/// matches, `None` otherwise.
fn fuzzy_match(query: &str, text: &str) -> Option<f64> {
    let query_lower = query.to_lowercase();
    let text_lower = text.to_lowercase();
    let match_query = |normalized_query: &str| -> Option<f64> {
        if normalized_query.is_empty() {
            return Some(0.0);
        }
        if normalized_query.chars().count() > text_lower.chars().count() {
            return None;
        }
        let query_chars: Vec<char> = normalized_query.chars().collect();
        let text_chars: Vec<char> = text_lower.chars().collect();
        let mut query_index = 0usize;
        let mut score = 0.0f64;
        let mut last_match_index: i64 = -1;
        let mut consecutive_matches = 0i64;

        for (i, ch) in text_chars.iter().enumerate() {
            if query_index >= query_chars.len() {
                break;
            }
            if *ch == query_chars[query_index] {
                let is_word_boundary = i == 0
                    || matches!(text_chars[i - 1], ' ' | '\t' | '\n' | '\r' | '-' | '_' | '.' | '/' | ':');
                if last_match_index == i as i64 - 1 {
                    consecutive_matches += 1;
                    score -= consecutive_matches as f64 * 5.0;
                } else {
                    consecutive_matches = 0;
                    if last_match_index >= 0 {
                        // TS: `score += (i - lastMatchIndex - 1) * 2` - an integer
                        // difference, so compute it as one and widen afterwards.
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
        }

        if query_index < query_chars.len() {
            return None;
        }
        if normalized_query == text_lower {
            score -= 100.0;
        }
        Some(score)
    };

    if let Some(score) = match_query(&query_lower) {
        return Some(score);
    }

    let swapped = swap_letters_and_digits(&query_lower);
    let swapped_query = match swapped {
        Some(swapped) => swapped,
        None => return None,
    };
    match_query(&swapped_query).map(|score| score + 5.0)
}

/// `^(?<letters>[a-z]+)(?<digits>[0-9]+)$` / `^(?<digits>[0-9]+)(?<letters>[a-z]+)$`.
fn swap_letters_and_digits(query: &str) -> Option<String> {
    let split = query
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_lowercase())
        .map(|(index, _)| index)
        .unwrap_or(query.len());
    let (letters, rest) = query.split_at(split);
    if !letters.is_empty() && !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(format!("{}{}", rest, letters));
    }
    let split = query
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_digit())
        .map(|(index, _)| index)
        .unwrap_or(query.len());
    let (digits, rest) = query.split_at(split);
    if !digits.is_empty() && !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return Some(format!("{}{}", rest, digits));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeRegistry {
        error: Option<String>,
        models: Vec<Model>,
    }

    impl ModelRegistry for FakeRegistry {
        fn get_error(&self) -> Option<String> {
            self.error.clone()
        }
        fn refresh_available_models(&self) -> Vec<Model> {
            self.models.clone()
        }
    }

    fn model(provider: &str, id: &str, context: f64, max_tokens: f64, reasoning: bool, image: bool) -> Model {
        let mut model = Model::new(id, id, "openai-completions", provider, "https://example.invalid");
        model.context_window = context;
        model.max_tokens = max_tokens;
        model.reasoning = reasoning;
        model.input = if image {
            vec![pi_ai::types::InputModality::Image]
        } else {
            vec![pi_ai::types::InputModality::Text]
        };
        model
    }

    fn capture(registry: &dyn ModelRegistry, search: Option<&str>) -> (Vec<String>, Vec<String>) {
        let log = RefCell::new(Vec::new());
        let error = RefCell::new(Vec::new());
        let log_fn = |message: &str| log.borrow_mut().push(message.to_string());
        let error_fn = |message: &str| error.borrow_mut().push(message.to_string());
        list_models(registry, search, &ListModelsIo { log: &log_fn, error: &error_fn });
        (log.into_inner(), error.into_inner())
    }

    #[test]
    fn formats_token_counts_like_the_typescript() {
        assert_eq!(format_token_count(999.0), "999");
        assert_eq!(format_token_count(1_000.0), "1K");
        assert_eq!(format_token_count(1_500.0), "1.5K");
        assert_eq!(format_token_count(1_000_000.0), "1M");
        assert_eq!(format_token_count(2_500_000.0), "2.5M");
    }

    #[test]
    fn prints_an_aligned_table_sorted_by_provider_then_model() {
        let registry = FakeRegistry {
            error: None,
            models: vec![
                model("zeta", "b", 1_000.0, 1_000.0, false, false),
                model("alpha", "z", 2_000.0, 3_000.0, true, true),
                model("alpha", "a", 10.0, 20.0, false, false),
            ],
        };
        let (log, error) = capture(&registry, None);
        assert!(error.is_empty());
        assert_eq!(log[0], "provider  model  context  max-out  thinking  images");
        assert_eq!(log[1], "alpha     a      10       20       no        no    ");
        assert_eq!(log[2], "alpha     z      2K       3K       yes       yes   ");
        assert_eq!(log[3], "zeta      b      1K       1K       no        no    ");
    }

    #[test]
    fn reports_the_load_error_before_the_table() {
        let registry = FakeRegistry { error: Some("bad json".to_string()), models: Vec::new() };
        let (log, error) = capture(&registry, None);
        assert_eq!(error[0], "Warning: errors loading models.json:\nbad json");
        assert_eq!(log.len(), 1);
        assert!(log[0].starts_with("No models available."));
    }

    #[test]
    fn filters_by_fuzzy_search_and_reports_no_matches() {
        let registry = FakeRegistry {
            error: None,
            models: vec![
                model("alpha", "claude-3", 1.0, 1.0, false, false),
                model("beta", "gpt-4", 1.0, 1.0, false, false),
            ],
        };
        let (log, _) = capture(&registry, Some("claude"));
        assert_eq!(log.len(), 2);
        assert!(log[1].starts_with("alpha"));

        let (log, _) = capture(&registry, Some("zzzzzzzz"));
        assert_eq!(log, vec!["No models matching \"zzzzzzzz\"".to_string()]);
    }

    #[test]
    fn fuzzy_match_returns_none_when_characters_are_missing() {
        assert!(fuzzy_match("abc", "axbxc").is_some());
        assert!(fuzzy_match("abcd", "abc").is_none());
        assert_eq!(fuzzy_match("", "anything"), Some(0.0));
    }

    #[test]
    fn fuzzy_match_swaps_letter_and_digit_runs() {
        let swapped = fuzzy_match("4gpt", "gpt4");
        assert!(swapped.is_some());
        assert!(swapped.unwrap() > fuzzy_match("gpt4", "gpt4").unwrap());
        assert_eq!(swap_letters_and_digits("gpt4"), Some("4gpt".to_string()));
        assert_eq!(swap_letters_and_digits("4gpt"), Some("gpt4".to_string()));
        assert_eq!(swap_letters_and_digits("gpt"), None);
    }
}
