//! Port of packages/ai/src/utils/json-parse.ts
//!
//! The TypeScript imports `parse as partialParse` from the third-party
//! `partial-json` package. That package is not in the workspace dependency set,
//! so the parser is ported here, in a private submodule, from the pinned
//! `partial-json@0.1.7` `dist/index.js` + `dist/options.js` shipped with the
//! reference checkout. Behaviour (including the `Allow.ALL` default and the
//! `PartialJSON` / `MalformedJSON` failure modes) is preserved.

use serde_json::Value;

const VALID_JSON_ESCAPES: [char; 9] = ['"', '\\', '/', 'b', 'f', 'n', 'r', 't', 'u'];

fn is_control_character(ch: char) -> bool {
    let code_point = ch as u32;
    (0x00..=0x1f).contains(&code_point)
}

fn escape_control_character(ch: char) -> String {
    match ch {
        '\u{8}' => "\\b".to_string(),
        '\u{c}' => "\\f".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        _ => format!("\\u{:04x}", ch as u32),
    }
}

/// Repairs malformed JSON string literals by:
/// - escaping raw control characters inside strings
/// - doubling backslashes before invalid escape characters
pub fn repair_json(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut repaired = String::new();
    let mut in_string = false;
    let mut index = 0usize;

    while index < chars.len() {
        let ch = chars[index];

        if !in_string {
            repaired.push(ch);
            if ch == '"' {
                in_string = true;
            }
            index += 1;
            continue;
        }

        if ch == '"' {
            repaired.push(ch);
            in_string = false;
            index += 1;
            continue;
        }

        if ch == '\\' {
            let next_char = chars.get(index + 1).copied();
            let Some(next_char) = next_char else {
                repaired.push_str("\\\\");
                index += 1;
                continue;
            };

            if next_char == 'u' {
                let unicode_digits: String = chars.iter().skip(index + 2).take(4).collect();
                if unicode_digits.len() == 4
                    && unicode_digits.chars().all(|c| c.is_ascii_hexdigit())
                {
                    repaired.push_str(&format!("\\u{}", unicode_digits));
                    index += 6;
                    continue;
                }
            }

            if VALID_JSON_ESCAPES.contains(&next_char) {
                repaired.push('\\');
                repaired.push(next_char);
                index += 2;
                continue;
            }

            repaired.push_str("\\\\");
            index += 1;
            continue;
        }

        if is_control_character(ch) {
            repaired.push_str(&escape_control_character(ch));
        } else {
            repaired.push(ch);
        }
        index += 1;
    }

    repaired
}

/// `parseJsonWithRepair<T>(json: string): T`
pub fn parse_json_with_repair(json: &str) -> Result<Value, String> {
    match serde_json::from_str::<Value>(json) {
        Ok(value) => Ok(value),
        Err(error) => {
            let repaired_json = repair_json(json);
            if repaired_json != json {
                return serde_json::from_str::<Value>(&repaired_json).map_err(|e| e.to_string());
            }
            Err(error.to_string())
        }
    }
}

/// Attempts to parse potentially incomplete JSON during streaming.
/// Always returns a valid object, even if the JSON is incomplete.
pub fn parse_streaming_json(partial_json: Option<&str>) -> Value {
    let Some(partial_json) = partial_json else {
        return Value::Object(serde_json::Map::new());
    };
    if partial_json.trim().is_empty() {
        return Value::Object(serde_json::Map::new());
    }

    if let Ok(value) = parse_json_with_repair(partial_json) {
        return value;
    }
    if let Ok(value) = partial_json_crate::parse(partial_json) {
        return value;
    }
    if let Ok(value) = partial_json_crate::parse(&repair_json(partial_json)) {
        return value;
    }
    Value::Object(serde_json::Map::new())
}

/// Port of the third-party `partial-json` parser (default `Allow.ALL`).
mod partial_json_crate {
    use serde_json::{Map, Number, Value};

    /// `Allow` bit flags from `partial-json/dist/options.js`.
    pub const STR: u32 = 0b000000001;
    pub const NUM: u32 = 0b000000010;
    pub const ARR: u32 = 0b000000100;
    pub const OBJ: u32 = 0b000001000;
    pub const NULL: u32 = 0b000010000;
    pub const BOOL: u32 = 0b000100000;
    pub const NAN: u32 = 0b001000000;
    pub const INFINITY: u32 = 0b010000000;
    pub const _INFINITY: u32 = 0b100000000;
    pub const ALL: u32 =
        STR | NUM | ARR | OBJ | NULL | BOOL | NAN | INFINITY | _INFINITY;

    #[derive(Debug)]
    pub enum PartialJsonError {
        /// `PartialJSON`: the JSON is incomplete for the given `allow` flags.
        Partial(String),
        /// `MalformedJSON`: the JSON is malformed.
        Malformed(String),
    }

    impl std::fmt::Display for PartialJsonError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                PartialJsonError::Partial(msg) => write!(f, "{}", msg),
                PartialJsonError::Malformed(msg) => write!(f, "{}", msg),
            }
        }
    }

    struct Parser<'a> {
        chars: Vec<char>,
        index: usize,
        allow: u32,
        _source: &'a str,
    }

    impl<'a> Parser<'a> {
        fn length(&self) -> usize {
            self.chars.len()
        }

        fn char_at(&self, index: usize) -> Option<char> {
            self.chars.get(index).copied()
        }

        fn substring(&self, start: usize, end: usize) -> String {
            self.chars
                .iter()
                .skip(start)
                .take(end.saturating_sub(start))
                .collect()
        }

        fn mark_partial_json<T>(&self, msg: &str) -> Result<T, PartialJsonError> {
            Err(PartialJsonError::Partial(format!(
                "{} at position {}",
                msg, self.index
            )))
        }

        fn throw_malformed_error<T>(&self, msg: &str) -> Result<T, PartialJsonError> {
            Err(PartialJsonError::Malformed(format!(
                "{} at position {}",
                msg, self.index
            )))
        }

        fn skip_blank(&mut self) {
            while self.index < self.length() {
                match self.char_at(self.index) {
                    Some(' ') | Some('\n') | Some('\r') | Some('\t') => self.index += 1,
                    _ => break,
                }
            }
        }

        fn parse_any(&mut self) -> Result<Value, PartialJsonError> {
            self.skip_blank();
            if self.index >= self.length() {
                return self.mark_partial_json("Unexpected end of input");
            }
            match self.char_at(self.index) {
                Some('"') => return self.parse_str(),
                Some('{') => return self.parse_obj(),
                Some('[') => return self.parse_arr(),
                _ => {}
            }
            let remaining = self.length() - self.index;
            if self.substring(self.index, self.index + 4) == "null"
                || (NULL & self.allow != 0
                    && remaining < 4
                    && "null".starts_with(&self.substring(self.index, self.length())))
            {
                self.index += 4;
                return Ok(Value::Null);
            }
            if self.substring(self.index, self.index + 4) == "true"
                || (BOOL & self.allow != 0
                    && remaining < 4
                    && "true".starts_with(&self.substring(self.index, self.length())))
            {
                self.index += 4;
                return Ok(Value::Bool(true));
            }
            if self.substring(self.index, self.index + 5) == "false"
                || (BOOL & self.allow != 0
                    && remaining < 5
                    && "false".starts_with(&self.substring(self.index, self.length())))
            {
                self.index += 5;
                return Ok(Value::Bool(false));
            }
            // JSON has no representation for NaN/Infinity; the TypeScript returns
            // the JS number, the port returns null (see module comment).
            if self.substring(self.index, self.index + 8) == "Infinity"
                || (INFINITY & self.allow != 0
                    && remaining < 8
                    && "Infinity".starts_with(&self.substring(self.index, self.length())))
            {
                self.index += 8;
                return Ok(Value::Null);
            }
            if self.substring(self.index, self.index + 9) == "-Infinity"
                || (_INFINITY & self.allow != 0
                    && 1 < remaining
                    && remaining < 9
                    && "-Infinity".starts_with(&self.substring(self.index, self.length())))
            {
                self.index += 9;
                return Ok(Value::Null);
            }
            if self.substring(self.index, self.index + 3) == "NaN"
                || (NAN & self.allow != 0
                    && remaining < 3
                    && "NaN".starts_with(&self.substring(self.index, self.length())))
            {
                self.index += 3;
                return Ok(Value::Null);
            }
            self.parse_num()
        }

        fn parse_str(&mut self) -> Result<Value, PartialJsonError> {
            let start = self.index;
            let mut escape = false;
            self.index += 1; // skip initial quote
            while self.index < self.length()
                && (self.char_at(self.index) != Some('"')
                    || (escape && self.char_at(self.index.wrapping_sub(1)) == Some('\\')))
            {
                escape = if self.char_at(self.index) == Some('\\') {
                    !escape
                } else {
                    false
                };
                self.index += 1;
            }
            if self.char_at(self.index) == Some('"') {
                self.index += 1;
                let end = self.index - if escape { 1 } else { 0 };
                match serde_json::from_str::<Value>(&self.substring(start, end)) {
                    Ok(Value::String(s)) => return Ok(Value::String(s)),
                    Ok(other) => return Ok(other),
                    Err(e) => return self.throw_malformed_error(&e.to_string()),
                }
            } else if STR & self.allow != 0 {
                let end = self.index - if escape { 1 } else { 0 };
                let candidate = format!("{}\"", self.substring(start, end));
                if let Ok(Value::String(s)) = serde_json::from_str::<Value>(&candidate) {
                    return Ok(Value::String(s));
                }
                // SyntaxError: Invalid escape sequence
                let last_backslash = self.chars[..self.index]
                    .iter()
                    .rposition(|c| *c == '\\')
                    .map(|p| p)
                    .unwrap_or(0);
                let candidate = format!("{}\"", self.substring(start, last_backslash));
                if let Ok(Value::String(s)) = serde_json::from_str::<Value>(&candidate) {
                    return Ok(Value::String(s));
                }
                return self.throw_malformed_error("Invalid escape sequence");
            }
            self.mark_partial_json("Unterminated string literal")
        }

        fn parse_obj(&mut self) -> Result<Value, PartialJsonError> {
            self.index += 1; // skip initial brace
            self.skip_blank();
            let mut obj = Map::new();
            loop {
                if self.char_at(self.index) == Some('}') {
                    break;
                }
                self.skip_blank();
                if self.index >= self.length() && OBJ & self.allow != 0 {
                    return Ok(Value::Object(obj));
                }
                let key = match self.parse_str() {
                    Ok(Value::String(key)) => key,
                    Ok(other) => other.to_string(),
                    Err(PartialJsonError::Partial(msg)) => {
                        if OBJ & self.allow != 0 {
                            return Ok(Value::Object(obj));
                        }
                        return Err(PartialJsonError::Partial(msg));
                    }
                    Err(e) => return Err(e),
                };
                self.skip_blank();
                self.index += 1; // skip colon
                match self.parse_any() {
                    Ok(value) => {
                        obj.insert(key, value);
                    }
                    Err(PartialJsonError::Partial(msg)) => {
                        if OBJ & self.allow != 0 {
                            return Ok(Value::Object(obj));
                        }
                        return Err(PartialJsonError::Partial(msg));
                    }
                    Err(e) => return Err(e),
                }
                self.skip_blank();
                if self.char_at(self.index) == Some(',') {
                    self.index += 1; // skip comma
                }
                if self.index >= self.length() && OBJ & self.allow != 0 {
                    return Ok(Value::Object(obj));
                }
            }
            self.index += 1; // skip final brace
            Ok(Value::Object(obj))
        }

        fn parse_arr(&mut self) -> Result<Value, PartialJsonError> {
            self.index += 1; // skip initial bracket
            let mut arr: Vec<Value> = Vec::new();
            loop {
                if self.char_at(self.index) == Some(']') {
                    break;
                }
                if self.index >= self.length() {
                    if ARR & self.allow != 0 {
                        return Ok(Value::Array(arr));
                    }
                    return self.mark_partial_json("Expected ']' at end of array");
                }
                match self.parse_any() {
                    Ok(value) => arr.push(value),
                    Err(PartialJsonError::Partial(msg)) => {
                        if ARR & self.allow != 0 {
                            return Ok(Value::Array(arr));
                        }
                        return Err(PartialJsonError::Partial(msg));
                    }
                    Err(e) => return Err(e),
                }
                self.skip_blank();
                if self.char_at(self.index) == Some(',') {
                    self.index += 1; // skip comma
                }
            }
            self.index += 1; // skip final bracket
            Ok(Value::Array(arr))
        }

        fn parse_num(&mut self) -> Result<Value, PartialJsonError> {
            if self.index == 0 {
                let whole: String = self.chars.iter().collect();
                if whole == "-" {
                    return self.throw_malformed_error("Not sure what '-' is");
                }
                match serde_json::from_str::<Value>(&whole) {
                    Ok(value) => return Ok(value),
                    Err(e) => {
                        if NUM & self.allow != 0 {
                            let last_e = whole.rfind('e');
                            if let Some(last_e) = last_e {
                                if let Ok(value) = serde_json::from_str::<Value>(&whole[..last_e]) {
                                    return Ok(value);
                                }
                            }
                        }
                        return self.throw_malformed_error(&e.to_string());
                    }
                }
            }
            let start = self.index;
            if self.char_at(self.index) == Some('-') {
                self.index += 1;
            }
            while let Some(ch) = self.char_at(self.index) {
                if ",]}".contains(ch) {
                    break;
                }
                self.index += 1;
            }
            if self.index == self.length() && NUM & self.allow == 0 {
                return self.mark_partial_json("Unterminated number literal");
            }
            let slice = self.substring(start, self.index);
            match serde_json::from_str::<Value>(&slice) {
                Ok(value) => Ok(value),
                Err(e) => {
                    if slice == "-" {
                        return self.mark_partial_json("Not sure what '-' is");
                    }
                    let last_e = self.chars[..self.index]
                        .iter()
                        .rposition(|c| *c == 'e')
                        .unwrap_or(0);
                    if last_e > start {
                        if let Ok(value) = serde_json::from_str::<Value>(&self.substring(start, last_e)) {
                            return Ok(value);
                        }
                    }
                    self.throw_malformed_error(&e.to_string())
                }
            }
        }
    }

    /// `parseJSON(jsonString, allowPartial = Allow.ALL)`.
    pub fn parse(json_string: &str) -> Result<Value, PartialJsonError> {
        if json_string.trim().is_empty() {
            return Err(PartialJsonError::Malformed(format!(
                "{} is empty",
                json_string
            )));
        }
        let trimmed = json_string.trim();
        let mut parser = Parser {
            chars: trimmed.chars().collect(),
            index: 0,
            allow: ALL,
            _source: json_string,
        };
        parser.parse_any()
    }

    /// Helper for `Number` round-tripping in tests.
    #[allow(dead_code)]
    pub fn number(value: f64) -> Option<Number> {
        Number::from_f64(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repair_json_escapes_raw_control_characters() {
        assert_eq!(repair_json("{\"a\":\"x\ny\"}"), "{\"a\":\"x\\ny\"}");
        assert_eq!(repair_json("{\"a\":\"x\ty\"}"), "{\"a\":\"x\\ty\"}");
    }

    #[test]
    fn repair_json_doubles_invalid_escapes() {
        assert_eq!(repair_json("\"a\\qb\""), "\"a\\\\qb\"");
        assert_eq!(repair_json("\"a\\\"b\""), "\"a\\\"b\"");
    }

    #[test]
    fn repair_json_keeps_valid_unicode_escape() {
        assert_eq!(repair_json("\"\\u00e9\""), "\"\\u00e9\"");
    }

    #[test]
    fn repair_json_preserves_valid_json() {
        let json = "{\"a\":[1,2,{\"b\":null}]}";
        assert_eq!(repair_json(json), json);
    }

    #[test]
    fn parse_json_with_repair_round_trips() {
        assert_eq!(parse_json_with_repair("{\"a\":1}").unwrap(), json!({"a": 1}));
        assert!(parse_json_with_repair("{").is_err());
    }

    #[test]
    fn parse_streaming_json_empty_input() {
        assert_eq!(parse_streaming_json(None), json!({}));
        assert_eq!(parse_streaming_json(Some("")), json!({}));
        assert_eq!(parse_streaming_json(Some("   ")), json!({}));
    }

    #[test]
    fn parse_streaming_json_complete_document() {
        assert_eq!(parse_streaming_json(Some("{\"a\":1}")), json!({"a": 1}));
    }

    #[test]
    fn parse_streaming_json_partial_document() {
        assert_eq!(parse_streaming_json(Some("{\"a\":1,\"b\":")), json!({"a": 1}));
        assert_eq!(parse_streaming_json(Some("[1,2,")), json!([1, 2]));
        assert_eq!(
            parse_streaming_json(Some("{\"cmd\":\"ls\",\"args\":[\"-l\"")),
            json!({"cmd": "ls", "args": ["-l"]})
        );
    }

    #[test]
    fn parse_streaming_json_garbage_returns_empty_object() {
        assert_eq!(parse_streaming_json(Some("not json at all")), json!({}));
    }
}
