//! Byte-preserving SSE line framing. JSON and provider error policy belong to callers.

#[derive(Default)]
pub(crate) struct SseFrames {
    line: Vec<u8>,
    data: Vec<String>,
    skip_lf: bool,
}

impl SseFrames {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if self.skip_lf {
                self.skip_lf = false;
                if byte == b'\n' {
                    continue;
                }
            }
            if byte != b'\r' && byte != b'\n' {
                self.line.push(byte);
                continue;
            }
            self.skip_lf = byte == b'\r';
            if self.line.is_empty() {
                if let Some(data) = self.take_data() {
                    frames.push(data);
                }
            } else {
                self.finish_line();
            }
        }
        frames
    }

    // Bedrock historically decodes a final undelimited payload; Codex does not.
    pub(crate) fn finish(&mut self) -> Vec<String> {
        if !self.line.is_empty() {
            self.finish_line();
        }
        self.take_data().into_iter().collect()
    }

    fn finish_line(&mut self) {
        let line = String::from_utf8_lossy(&self.line);
        if let Some(data) = line.strip_prefix("data:") {
            self.data.push(data.strip_prefix(' ').unwrap_or(data).to_string());
        }
        self.line.clear();
    }

    fn take_data(&mut self) -> Option<String> {
        let data = self.data.join("\n");
        self.data.clear();
        if data.trim().is_empty() || data.trim() == "[DONE]" {
            None
        } else {
            Some(data)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_comments_mixed_terminators_and_done_are_preserved() {
        let wire = b":comment\r\ndata: {\rdata: \"delta\":\"ok\"}\n\rdata: [DONE]\r\r";
        let mut frames = SseFrames::default();
        let result: Vec<_> = wire.iter().flat_map(|byte| frames.push(&[*byte])).collect();
        assert_eq!(result, vec!["{\n\"delta\":\"ok\"}"]);
    }

    #[test]
    fn finish_decodes_only_the_remaining_tail_once() {
        let mut frames = SseFrames::default();
        assert!(frames.push("data: \"日本\"".as_bytes()).is_empty());
        assert_eq!(frames.finish(), vec!["\"日本\""]);
        assert!(frames.finish().is_empty());
    }
}
