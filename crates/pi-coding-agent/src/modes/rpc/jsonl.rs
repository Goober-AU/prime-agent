//! Port of packages/coding-agent/src/modes/rpc/jsonl.ts
//!
//! LF-only JSONL framing. Payload strings may contain other Unicode separators
//! such as U+2028 and U+2029, so readers must split records on `\n` only.

use std::sync::{Arc, Mutex};

use serde_json::Value;

/// Serialize a single strict JSONL record.
pub fn serialize_json_line(value: &Value) -> String {
    format!("{value}\n")
}

/// `JsonlLineReaderOptions`.
#[derive(Clone, Default)]
pub struct JsonlLineReaderOptions {
    pub max_line_length: Option<usize>,
    pub on_line_overflow: Option<Arc<dyn Fn(String) + Send + Sync>>,
}

/// State of one LF-only line reader.
///
/// The TypeScript attaches listeners to a Node stream. The port keeps the same
/// segmentation state machine and exposes `push`/`end` so a caller can drive it
/// from any byte source.
pub struct JsonlLineReader {
    on_line: Arc<dyn Fn(String) + Send + Sync>,
    options: JsonlLineReaderOptions,
    pending: Vec<String>,
    pending_length: usize,
    discarding_overflow: bool,
}

impl JsonlLineReader {
    pub fn new(on_line: Arc<dyn Fn(String) + Send + Sync>, options: JsonlLineReaderOptions) -> Self {
        Self {
            on_line,
            options,
            pending: Vec::new(),
            pending_length: 0,
            discarding_overflow: false,
        }
    }

    fn emit_line(&self, line: &str) {
        let trimmed = line.strip_suffix('\r').unwrap_or(line);
        (self.on_line)(trimmed.to_string());
    }

    fn reset_pending(&mut self) {
        self.pending.clear();
        self.pending_length = 0;
    }

    fn append_pending(&mut self, segment: &str) {
        if self.discarding_overflow || segment.is_empty() {
            return;
        }
        if let Some(max_line_length) = self.options.max_line_length {
            if self.pending_length + segment.chars().count() > max_line_length {
                let remaining = max_line_length.saturating_sub(self.pending_length);
                if remaining > 0 {
                    self.pending.push(segment.chars().take(remaining).collect());
                }
                if let Some(on_line_overflow) = &self.options.on_line_overflow {
                    on_line_overflow(self.pending.concat());
                }
                self.reset_pending();
                self.discarding_overflow = true;
                return;
            }
        }
        self.pending.push(segment.to_string());
        self.pending_length += segment.chars().count();
    }

    fn emit_from(&mut self, segment: &str) {
        if self.discarding_overflow {
            self.discarding_overflow = false;
            self.reset_pending();
            return;
        }
        self.append_pending(segment);
        if self.discarding_overflow {
            self.discarding_overflow = false;
            return;
        }
        let line = self.pending.concat();
        self.emit_line(&line);
        self.reset_pending();
    }

    /// Feed one already-decoded chunk.
    pub fn push(&mut self, text: &str) {
        let mut start = 0usize;
        let mut newline_index = text.find('\n');
        while let Some(index) = newline_index {
            self.emit_from(&text[start..index]);
            start = index + 1;
            newline_index = text[start..].find('\n').map(|offset| start + offset);
        }
        if start < text.len() {
            self.append_pending(&text[start..]);
        }
    }

    /// Signal end of input: flush the trailing partial line.
    pub fn end(&mut self) {
        if !self.discarding_overflow && !self.pending.is_empty() {
            let line = self.pending.concat();
            self.emit_line(&line);
        }
        self.reset_pending();
        self.discarding_overflow = false;
    }
}

/// Incremental UTF-8 decoder that mirrors Node's `StringDecoder`.
///
/// Node's decoder buffers an incomplete trailing multi-byte sequence until the
/// next chunk. The port exposes the same behaviour over raw bytes.
#[derive(Default)]
pub struct StringDecoder {
    pending: Vec<u8>,
}

impl StringDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn write(&mut self, chunk: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.pending);
        bytes.extend_from_slice(chunk);
        match std::str::from_utf8(&bytes) {
            Ok(text) => text.to_string(),
            Err(error) => {
                let valid = error.valid_up_to();
                let decoded = String::from_utf8_lossy(&bytes[..valid]).to_string();
                // An incomplete trailing sequence is held for the next chunk; any
                // other invalid byte is replaced, matching StringDecoder.
                if error.error_len().is_none() && bytes.len() - valid < 4 {
                    self.pending = bytes[valid..].to_vec();
                } else {
                    let mut rest = String::from_utf8_lossy(&bytes[valid..]).to_string();
                    self.pending = Vec::new();
                    rest = format!("{decoded}{rest}");
                    return rest;
                }
                decoded
            }
        }
    }

    pub fn end(&mut self) -> String {
        let bytes = std::mem::take(&mut self.pending);
        if bytes.is_empty() {
            return String::new();
        }
        String::from_utf8_lossy(&bytes).to_string()
    }
}

/// Shared reader handle so a caller can keep the state machine alive.
pub type SharedJsonlLineReader = Arc<Mutex<JsonlLineReader>>;

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(lines: Arc<Mutex<Vec<String>>>) -> JsonlLineReader {
        JsonlLineReader::new(
            Arc::new(move |line: String| {
                lines.lock().unwrap().push(line);
            }),
            JsonlLineReaderOptions::default(),
        )
    }

    #[test]
    fn serializes_lf_terminated_records() {
        assert_eq!(serialize_json_line(&serde_json::json!({"a": 1})), "{\"a\":1}\n");
    }

    #[test]
    fn splits_on_lf_only_and_keeps_u2028_in_payloads() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = collect(lines.clone());
        reader.push("{\"a\":\"x\u{2028}y\"}\n{\"b\":2}");
        reader.end();
        let captured = lines.lock().unwrap().clone();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0], "{\"a\":\"x\u{2028}y\"}");
        assert_eq!(captured[1], "{\"b\":2}");
    }

    #[test]
    fn strips_a_trailing_carriage_return() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = collect(lines.clone());
        reader.push("one\r\ntwo\n");
        assert_eq!(lines.lock().unwrap().clone(), vec!["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn reassembles_a_record_split_across_chunks() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let mut reader = collect(lines.clone());
        reader.push("{\"long\":");
        reader.push("\"value\"}");
        reader.push("\n");
        assert_eq!(lines.lock().unwrap().clone(), vec!["{\"long\":\"value\"}".to_string()]);
    }

    #[test]
    fn overflow_reports_the_prefix_and_discards_the_rest() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let overflows = Arc::new(Mutex::new(Vec::new()));
        let overflow_sink = overflows.clone();
        let mut reader = JsonlLineReader::new(
            Arc::new(move |line: String| {
                lines.lock().unwrap().push(line);
            }),
            JsonlLineReaderOptions {
                max_line_length: Some(4),
                on_line_overflow: Some(Arc::new(move |prefix: String| {
                    overflow_sink.lock().unwrap().push(prefix);
                })),
            },
        );
        reader.push("abcdef\nnext\n");
        assert_eq!(overflows.lock().unwrap().clone(), vec!["abcd".to_string()]);
        assert_eq!(lines.lock().unwrap().clone(), vec!["next".to_string()]);
    }

    #[test]
    fn decoder_holds_incomplete_sequences_until_the_next_chunk() {
        let mut decoder = StringDecoder::new();
        let first = decoder.write(&[0xe2, 0x82]);
        assert_eq!(first, "");
        let second = decoder.write(&[0xac]);
        assert_eq!(second, "\u{20ac}");
        assert_eq!(decoder.end(), "");
    }
}
