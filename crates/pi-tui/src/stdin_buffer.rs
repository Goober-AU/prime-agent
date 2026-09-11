//! Port of packages/tui/src/stdin-buffer.ts.

use once_cell::sync::Lazy;
use regex::Regex;

const ESC: char = '\x1b';
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceStatus {
    Complete,
    Incomplete,
    NotEscape,
}

fn is_complete_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with(ESC) {
        return SequenceStatus::NotEscape;
    }

    if data.chars().count() == 1 {
        return SequenceStatus::Incomplete;
    }

    let after_esc = &data[1..];

    if after_esc.starts_with('[') {
        if after_esc.starts_with("[M") {
            return if data.len() >= 6 {
                SequenceStatus::Complete
            } else {
                SequenceStatus::Incomplete
            };
        }
        return is_complete_csi_sequence(data);
    }

    if after_esc.starts_with(']') {
        return is_complete_osc_sequence(data);
    }

    if after_esc.starts_with('P') {
        return is_complete_dcs_sequence(data);
    }

    if after_esc.starts_with('_') {
        return is_complete_apc_sequence(data);
    }

    if after_esc.starts_with('O') {
        return if after_esc.len() >= 2 {
            SequenceStatus::Complete
        } else {
            SequenceStatus::Incomplete
        };
    }

    if after_esc.chars().count() == 1 {
        return SequenceStatus::Complete;
    }

    SequenceStatus::Complete
}

static SGR_MOUSE_PAYLOAD_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^<\d+;\d+;\d+[Mm]$").unwrap());
static DIGITS_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d+$").unwrap());

fn is_complete_csi_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b[") {
        return SequenceStatus::Complete;
    }

    if data.chars().count() < 3 {
        return SequenceStatus::Incomplete;
    }

    let payload = &data[2..];

    let last_char = payload.chars().last().unwrap();
    let last_char_code = last_char as u32;

    if (0x40..=0x7e).contains(&last_char_code) {
        if payload.starts_with('<') {
            if SGR_MOUSE_PAYLOAD_PATTERN.is_match(payload) {
                return SequenceStatus::Complete;
            }
            if last_char == 'M' || last_char == 'm' {
                let inner: String = payload.chars().skip(1).take(payload.chars().count() - 2).collect();
                let parts: Vec<&str> = inner.split(';').collect();
                if parts.len() == 3 && parts.iter().all(|p| DIGITS_PATTERN.is_match(p)) {
                    return SequenceStatus::Complete;
                }
            }

            return SequenceStatus::Incomplete;
        }

        return SequenceStatus::Complete;
    }

    SequenceStatus::Incomplete
}

fn is_complete_osc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b]") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") || data.ends_with('\x07') {
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

fn is_complete_dcs_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1bP") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") {
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

fn is_complete_apc_sequence(data: &str) -> SequenceStatus {
    if !data.starts_with("\x1b_") {
        return SequenceStatus::Complete;
    }
    if data.ends_with("\x1b\\") {
        return SequenceStatus::Complete;
    }
    SequenceStatus::Incomplete
}

static KITTY_PRINTABLE_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\x1b\[(\d+)(?::\d*)?(?::\d+)?u$").unwrap());

fn parse_unmodified_kitty_printable_codepoint(sequence: &str) -> Option<u32> {
    let caps = KITTY_PRINTABLE_PATTERN.captures(sequence)?;
    let codepoint: u32 = caps.get(1)?.as_str().parse().ok()?;
    if codepoint >= 32 {
        Some(codepoint)
    } else {
        None
    }
}

static RAW_MULTILINE_PASTE_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[^\r\n][\r\n]+[^\r\n]").unwrap());

fn is_raw_multiline_paste(data: &str) -> bool {
    if data.contains(ESC) {
        return false;
    }
    // A leading or trailing Enter alone is ordinary key input, not evidence of a multiline paste.
    RAW_MULTILINE_PASTE_PATTERN.is_match(data)
}

fn extract_complete_sequences(buffer: &str) -> (Vec<String>, String) {
    let mut sequences: Vec<String> = Vec::new();
    let mut pos = 0usize;

    while pos < buffer.len() {
        let remaining = &buffer[pos..];

        if remaining.starts_with(ESC) {
            let mut seq_end = 1usize;
            let mut consumed = false;
            while seq_end <= remaining.len() {
                let candidate = &remaining[..seq_end];
                let status = is_complete_sequence(candidate);

                if status == SequenceStatus::Complete {
                    sequences.push(candidate.to_string());
                    pos += seq_end;
                    consumed = true;
                    break;
                } else if status == SequenceStatus::Incomplete {
                    seq_end += next_char_len(remaining, seq_end);
                } else {
                    sequences.push(candidate.to_string());
                    pos += seq_end;
                    consumed = true;
                    break;
                }
            }

            if !consumed {
                return (sequences, remaining.to_string());
            }
        } else {
            let ch = remaining.chars().next().unwrap();
            sequences.push(ch.to_string());
            pos += ch.len_utf8();
        }
    }

    (sequences, String::new())
}

fn next_char_len(s: &str, at: usize) -> usize {
    match s[at..].chars().next() {
        Some(c) => c.len_utf8(),
        None => 1,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StdinBufferOptions {
    /// Maximum time to wait for sequence completion (default: 10ms)
    pub timeout: Option<u64>,
}

impl Default for StdinBufferOptions {
    fn default() -> Self {
        Self { timeout: None }
    }
}

/// Events emitted by the buffer (the TypeScript `StdinBufferEventMap`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdinBufferEvent {
    Data(String),
    Paste(String),
}

/// Port of the `StdinBufferEventMap` type: the buffer emits either a complete
/// `data` sequence or a `paste` payload.
pub type StdinBufferEventMap = StdinBufferEvent;

/// Buffers stdin input and emits complete sequences.
///
/// The TypeScript class is an EventEmitter with a `setTimeout` flush timer; here the
/// caller drives the flush by calling [`StdinBuffer::flush`] after `timeout_ms`,
/// which keeps the same sequence/ordering semantics without a timer thread.
pub struct StdinBuffer {
    buffer: String,
    timeout_ms: u64,
    paste_mode: bool,
    paste_buffer: String,
    pending_kitty_printable_codepoint: Option<u32>,
    pending_events: Vec<StdinBufferEvent>,
}

impl StdinBuffer {
    pub fn new(options: StdinBufferOptions) -> Self {
        Self {
            buffer: String::new(),
            timeout_ms: options.timeout.unwrap_or(10),
            paste_mode: false,
            paste_buffer: String::new(),
            pending_kitty_printable_codepoint: None,
            pending_events: Vec::new(),
        }
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    /// Process raw input (string or a single byte buffer) and emit complete sequences.
    pub fn process(&mut self, data: &[u8]) {
        let str_data = self.decode_input(data);

        if str_data.is_empty() && self.buffer.is_empty() {
            self.emit_data_sequence("");
            return;
        }

        self.buffer.push_str(&str_data);

        if self.paste_mode {
            self.paste_buffer.push_str(&self.buffer);
            self.buffer.clear();

            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                let pasted_content = self.paste_buffer[..end_index].to_string();
                let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

                self.paste_mode = false;
                self.paste_buffer.clear();
                self.pending_kitty_printable_codepoint = None;

                self.pending_events.push(StdinBufferEvent::Paste(pasted_content));

                if !remaining.is_empty() {
                    self.process(remaining.as_bytes());
                }
            }
            return;
        }

        if let Some(start_index) = self.buffer.find(BRACKETED_PASTE_START) {
            if start_index > 0 {
                let before_paste = self.buffer[..start_index].to_string();
                let (sequences, _) = extract_complete_sequences(&before_paste);
                for sequence in sequences {
                    self.emit_data_sequence(&sequence);
                }
            }

            self.pending_kitty_printable_codepoint = None;
            self.buffer = self.buffer[start_index + BRACKETED_PASTE_START.len()..].to_string();
            self.paste_mode = true;
            self.paste_buffer = std::mem::take(&mut self.buffer);

            if let Some(end_index) = self.paste_buffer.find(BRACKETED_PASTE_END) {
                let pasted_content = self.paste_buffer[..end_index].to_string();
                let remaining = self.paste_buffer[end_index + BRACKETED_PASTE_END.len()..].to_string();

                self.paste_mode = false;
                self.paste_buffer.clear();
                self.pending_kitty_printable_codepoint = None;

                self.pending_events.push(StdinBufferEvent::Paste(pasted_content));

                if !remaining.is_empty() {
                    self.process(remaining.as_bytes());
                }
            }
            return;
        }

        if is_raw_multiline_paste(&self.buffer) {
            let pasted_content = std::mem::take(&mut self.buffer);
            self.pending_kitty_printable_codepoint = None;
            self.pending_events.push(StdinBufferEvent::Paste(pasted_content));
            return;
        }

        let (sequences, remainder) = extract_complete_sequences(&self.buffer);
        self.buffer = remainder;

        for sequence in sequences {
            self.emit_data_sequence(&sequence);
        }
    }

    fn decode_input(&self, data: &[u8]) -> String {
        // Handle high-byte conversion (for compatibility with parseKeypress).
        // If buffer has single byte > 127, convert to ESC + (byte - 128).
        if data.len() == 1 && data[0] > 127 {
            let byte = data[0] - 128;
            return format!("\x1b{}", byte as char);
        }
        String::from_utf8_lossy(data).to_string()
    }

    fn emit_data_sequence(&mut self, sequence: &str) {
        let raw_codepoint = if sequence.chars().count() == 1 {
            sequence.chars().next().map(|c| c as u32)
        } else {
            None
        };
        if let (Some(raw), Some(pending)) = (raw_codepoint, self.pending_kitty_printable_codepoint) {
            if raw == pending {
                self.pending_kitty_printable_codepoint = None;
                return;
            }
        }

        self.pending_kitty_printable_codepoint = parse_unmodified_kitty_printable_codepoint(sequence);
        self.pending_events
            .push(StdinBufferEvent::Data(sequence.to_string()));
    }

    /// Drain the events emitted since the last drain (the TypeScript `emit` calls).
    pub fn take_events(&mut self) -> Vec<StdinBufferEvent> {
        std::mem::take(&mut self.pending_events)
    }

    pub fn flush(&mut self) -> Vec<String> {
        if self.buffer.is_empty() {
            return Vec::new();
        }
        let sequences = vec![std::mem::take(&mut self.buffer)];
        self.pending_kitty_printable_codepoint = None;
        sequences
    }

    /// Flush the buffer and push the flushed sequences as `data` events.
    pub fn flush_events(&mut self) -> Vec<String> {
        let flushed = self.flush();
        for sequence in &flushed {
            self.emit_data_sequence(sequence);
        }
        flushed
    }

    pub fn clear(&mut self) {
        self.buffer.clear();
        self.paste_mode = false;
        self.paste_buffer.clear();
        self.pending_kitty_printable_codepoint = None;
    }

    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    pub fn destroy(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(buffer: &mut StdinBuffer, data: &str) -> Vec<StdinBufferEvent> {
        buffer.process(data.as_bytes());
        buffer.take_events()
    }

    #[test]
    fn splits_batched_sequences() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = feed(&mut buffer, "\x1b[A\x1b[B");
        assert_eq!(
            events,
            vec![
                StdinBufferEvent::Data("\x1b[A".to_string()),
                StdinBufferEvent::Data("\x1b[B".to_string())
            ]
        );
    }

    #[test]
    fn buffers_incomplete_escape_sequences() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = feed(&mut buffer, "\x1b[");
        assert!(events.is_empty());
        assert_eq!(buffer.get_buffer(), "\x1b[");
        let events = feed(&mut buffer, "A");
        assert_eq!(events, vec![StdinBufferEvent::Data("\x1b[A".to_string())]);
    }

    #[test]
    fn bracketed_paste_is_emitted_as_paste() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = feed(&mut buffer, "\x1b[200~line1\nline2\x1b[201~");
        assert_eq!(
            events,
            vec![StdinBufferEvent::Paste("line1\nline2".to_string())]
        );
    }

    #[test]
    fn raw_multiline_paste_detected() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = feed(&mut buffer, "a\nb");
        assert_eq!(events, vec![StdinBufferEvent::Paste("a\nb".to_string())]);
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = feed(&mut buffer, "\r");
        assert_eq!(events, vec![StdinBufferEvent::Data("\r".to_string())]);
    }

    #[test]
    fn kitty_printable_duplicate_is_suppressed() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        let events = feed(&mut buffer, "\x1b[97u");
        assert_eq!(events, vec![StdinBufferEvent::Data("\x1b[97u".to_string())]);
        let events = feed(&mut buffer, "a");
        assert!(events.is_empty());
    }

    #[test]
    fn flush_returns_pending_buffer() {
        let mut buffer = StdinBuffer::new(StdinBufferOptions::default());
        feed(&mut buffer, "\x1b[");
        assert_eq!(buffer.flush(), vec!["\x1b[".to_string()]);
        assert_eq!(buffer.flush(), Vec::<String>::new());
    }

    #[test]
    fn sgr_mouse_completeness() {
        assert_eq!(is_complete_sequence("\x1b[<0;1;1M"), SequenceStatus::Complete);
        assert_eq!(is_complete_sequence("\x1b[<0;1"), SequenceStatus::Incomplete);
    }
}
