//! Port of packages/ai/src/utils/headers.ts

use std::collections::BTreeMap;

/// `headersToRecord(headers: Headers): Record<string, string>`.
///
/// The Rust equivalent keeps the header name/value pairs in the order they are
/// supplied; callers that need JS object ordering semantics pass the entries in
/// iteration order.
pub fn headers_to_record(headers: &[(String, String)]) -> BTreeMap<String, String> {
    let mut result = BTreeMap::new();
    for (key, value) in headers {
        result.insert(key.clone(), value.clone());
    }
    result
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
        let record = headers_to_record(&headers);
        assert_eq!(record.get("a").map(String::as_str), Some("3"));
        assert_eq!(record.get("b").map(String::as_str), Some("2"));
    }
}
