//! Port of packages/ai/src/utils/headers.ts

use indexmap::IndexMap;

/// `headersToRecord(headers: Headers): Record<string, string>`.
///
/// The TypeScript iterates `headers.entries()` (headers.ts:3-4) and assigns into a plain
/// object. The iteration order is the first-appearance order, and the VALUES it yields are
/// the Fetch "combined value": `Headers` joins repeated names with `", "` (the one exception
/// is `set-cookie`, whose values are yielded as separate entries, so the last one wins).
/// A `reqwest::header::HeaderMap` keeps repeats as separate entries, so this adapter must
/// apply the same combination rule instead of silently keeping only the last value.
/// The generic form accepts any `(name, value)` iterator; `header_map_to_record`
/// is the `reqwest::header::HeaderMap` adapter.
pub fn headers_to_record<I>(headers: I) -> IndexMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut result: IndexMap<String, String> = IndexMap::new();
    for (key, value) in headers {
        // `set-cookie` is never combined by `Headers.entries()`; every other repeat is.
        if !key.eq_ignore_ascii_case("set-cookie") {
            if let Some(existing) = result.get_mut(&key) {
                existing.push_str(", ");
                existing.push_str(&value);
                continue;
            }
        }
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
    fn repeated_values_are_combined_like_fetch_headers_entries() {
        // headers.ts:3-4 consumes `headers.entries()`, whose "sort and combine" step joins a
        // repeated name with ", " - it does not keep only the last value.
        let headers = vec![
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
            ("a".to_string(), "3".to_string()),
        ];
        let record = headers_to_record(headers);
        assert_eq!(record.get("a").map(String::as_str), Some("1, 3"));
        assert_eq!(record.get("b").map(String::as_str), Some("2"));
        // First-insertion position is kept, matching JS object key order.
        assert_eq!(record.keys().collect::<Vec<_>>(), vec!["a", "b"]);

        // The Fetch iterator's one exception: `set-cookie` keeps the last value.
        let set_cookie = vec![
            ("set-cookie".to_string(), "a=1".to_string()),
            ("set-cookie".to_string(), "b=2".to_string()),
        ];
        assert_eq!(
            headers_to_record(set_cookie).get("set-cookie").map(String::as_str),
            Some("b=2")
        );
    }

    #[test]
    fn header_map_adapter_reads_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-test", "1".parse().unwrap());
        let record = header_map_to_record(&headers);
        assert_eq!(record.get("x-test").map(String::as_str), Some("1"));
    }

    #[test]
    fn header_map_adapter_combines_repeats() {
        // `reqwest::header::HeaderMap` keeps repeats as separate entries, while the Fetch
        // `Headers` the TypeScript reads combines them, so `onResponse.headers` must match.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append("x-multi", "1".parse().unwrap());
        headers.append("x-multi", "2".parse().unwrap());
        let record = header_map_to_record(&headers);
        assert_eq!(record.get("x-multi").map(String::as_str), Some("1, 2"));
    }
}
