//! Port of packages/ai/src/utils/headers.ts

use indexmap::IndexMap;

/// `headersToRecord(headers: Headers): Record<string, string>`.
///
/// The TypeScript iterates `headers.entries()` and assigns into a plain object,
/// so later duplicates win and the insertion order of first appearance is kept.
/// The generic form accepts any `(name, value)` iterator; `header_map_to_record`
/// is the `reqwest::header::HeaderMap` adapter.
pub fn headers_to_record<I>(headers: I) -> IndexMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut result: IndexMap<String, String> = IndexMap::new();
    for (key, value) in headers {
        result.insert(key, value);
    }
    result
}

/// `headersToRecord` for a `reqwest::header::HeaderMap`.
pub fn header_map_to_record(headers: &reqwest::header::HeaderMap) -> IndexMap<String, String> {
    headers_to_record(
        headers
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn later_values_win_like_js_object_assignment() {
        let headers = vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "3".to_string()),
        ];
        let record = headers_to_record(headers);
        assert_eq!(record.get("a").map(String::as_str), Some("3"));
        assert_eq!(record.get("b").map(String::as_str), Some("2"));
        // First-insertion position is kept, matching JS object key order.
        assert_eq!(record.keys().collect::<Vec<_>>(), vec!["a", "b"]);
    }

    #[test]
    fn header_map_adapter_reads_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-test", "1".parse().unwrap());
        let record = header_map_to_record(&headers);
        assert_eq!(record.get("x-test").map(String::as_str), Some("1"));
    }
}
