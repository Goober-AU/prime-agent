//! Port of packages/coding-agent/src/modes/session-worker/private-framing.ts

use std::sync::{Arc, Mutex};

const FRAME_PREFIX_BYTES: usize = 8;

/// `PrivateFrameLimits`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivateFrameLimits {
    pub max_header_bytes: usize,
    pub max_payload_bytes: usize,
}

/// `DEFAULT_PRIVATE_FRAME_LIMITS`.
pub const DEFAULT_PRIVATE_FRAME_LIMITS: PrivateFrameLimits = PrivateFrameLimits {
    max_header_bytes: 1024 * 1024,
    max_payload_bytes: 1024 * 1024 * 1024,
};

/// `PrivateFrame<THeader>`.
#[derive(Debug, Clone, PartialEq)]
pub struct PrivateFrame<THeader> {
    pub header: THeader,
    pub payload: Vec<u8>,
}

/// `PrivateFrameHeaderValidator<THeader>`.
pub type PrivateFrameHeaderValidator<THeader> = Arc<dyn Fn(&serde_json::Value) -> bool + Send + Sync>;

/// `assertFrameLength(name, value, maximum)`.
fn assert_frame_length(name: &str, value: u64, maximum: usize) -> Result<(), String> {
    if value > maximum as u64 {
        return Err(format!("Invalid private frame {name}: {value}"));
    }
    Ok(())
}

/// `isObjectHeader(value)`.
fn is_object_header(value: &serde_json::Value) -> bool {
    value.is_object()
}

/// `encodePrivateFrame(header, payload, limits)`.
pub fn encode_private_frame(
    header: &serde_json::Value,
    payload: &[u8],
    limits: PrivateFrameLimits,
) -> Result<Vec<u8>, String> {
    let header_buffer = serde_json::to_string(header)
        .map_err(|error| error.to_string())?
        .into_bytes();
    assert_frame_length("header length", header_buffer.len() as u64, limits.max_header_bytes)?;
    assert_frame_length("payload length", payload.len() as u64, limits.max_payload_bytes)?;
    if header_buffer.is_empty() {
        return Err("Private frame header cannot be empty".to_string());
    }

    let mut frame = Vec::with_capacity(FRAME_PREFIX_BYTES + header_buffer.len() + payload.len());
    frame.extend_from_slice(&(header_buffer.len() as u32).to_be_bytes());
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&header_buffer);
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// `class PrivateFrameDecoder<THeader>`.
pub struct PrivateFrameDecoder<THeader> {
    validate_header: PrivateFrameHeaderValidator<THeader>,
    limits: PrivateFrameLimits,
    buffered: Vec<u8>,
}

impl<THeader: serde::de::DeserializeOwned> PrivateFrameDecoder<THeader> {
    pub fn new(
        validate_header: PrivateFrameHeaderValidator<THeader>,
        limits: PrivateFrameLimits,
    ) -> Self {
        Self {
            validate_header,
            limits,
            buffered: Vec::new(),
        }
    }

    /// `get bufferedBytes()`.
    pub fn buffered_bytes(&self) -> usize {
        self.buffered.len()
    }

    /// `push(chunk)`.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<PrivateFrame<THeader>>, String> {
        if !chunk.is_empty() {
            self.buffered.extend_from_slice(chunk);
        }

        let mut frames: Vec<PrivateFrame<THeader>> = Vec::new();
        let mut offset = 0usize;
        while self.buffered.len() - offset >= FRAME_PREFIX_BYTES {
            let header_length = u32::from_be_bytes(
                self.buffered[offset..offset + 4]
                    .try_into()
                    .expect("four bytes"),
            ) as u64;
            let payload_length = u32::from_be_bytes(
                self.buffered[offset + 4..offset + 8]
                    .try_into()
                    .expect("four bytes"),
            ) as u64;
            assert_frame_length("header length", header_length, self.limits.max_header_bytes)?;
            assert_frame_length("payload length", payload_length, self.limits.max_payload_bytes)?;
            if header_length == 0 {
                return Err("Private frame header cannot be empty".to_string());
            }

            let frame_length = FRAME_PREFIX_BYTES + header_length as usize + payload_length as usize;
            if self.buffered.len() - offset < frame_length {
                break;
            }

            let header_start = offset + FRAME_PREFIX_BYTES;
            let payload_start = header_start + header_length as usize;
            let decoded: serde_json::Value =
                serde_json::from_slice(&self.buffered[header_start..payload_start]).map_err(|error| {
                    format!("Invalid private frame header JSON: {error}")
                })?;
            if !is_object_header(&decoded) || !(self.validate_header)(&decoded) {
                return Err("Invalid private frame routing header".to_string());
            }

            frames.push(PrivateFrame {
                header: decode_header(&decoded),
                payload: self.buffered[payload_start..payload_start + payload_length as usize].to_vec(),
            });
            offset += frame_length;
        }

        if offset > 0 {
            self.buffered.drain(..offset);
        }
        Ok(frames)
    }

    /// `finish()`.
    pub fn finish(&self) -> Result<(), String> {
        if !self.buffered.is_empty() {
            return Err(format!(
                "Private frame channel ended with {} incomplete bytes",
                self.buffered.len()
            ));
        }
        Ok(())
    }
}

/// The generic decoder hands back the validated JSON header; a caller that
/// declared a typed header deserializes it from this value.
fn decode_header<THeader>(value: &serde_json::Value) -> THeader
where
    THeader: serde::de::DeserializeOwned,
{
    serde_json::from_value(value.clone()).expect("validated header must deserialize")
}

/// `PrivateFrameListener<THeader>`.
pub type PrivateFrameListener<THeader> =
    Arc<dyn Fn(&PrivateFrame<THeader>) + Send + Sync>;

/// The `Duplex` surface `PrivateFramedChannel` uses.
///
/// blocked_on: a Rust library cannot own a Node `Duplex`; the port drives the
/// same three stream events through an explicit seam.
pub trait PrivateFrameStream: Send + Sync {
    /// `stream.on("data")`.
    fn on_data(&self, handler: Arc<dyn Fn(&[u8]) + Send + Sync>);
    /// `stream.on("end")`.
    fn on_end(&self, handler: Arc<dyn Fn() + Send + Sync>);
    /// `stream.on("close")`.
    fn on_close(&self, handler: Arc<dyn Fn() + Send + Sync>);
    /// `stream.off(...)` for all three handlers.
    fn detach(&self);
    /// `stream.write(frame, callback)`.
    fn write(&self, frame: Vec<u8>) -> pi_ai::types::BoxFuture<Result<(), String>>;
    /// `stream.end()`.
    fn end(&self);
    /// `stream.destroy(error)`.
    fn destroy(&self, error: String);
    /// `stream.destroyed`.
    fn is_destroyed(&self) -> bool;
}

/// `class PrivateFramedChannel<THeader>`.
pub struct PrivateFramedChannel<THeader> {
    decoder: Mutex<PrivateFrameDecoder<THeader>>,
    listeners: Mutex<Vec<PrivateFrameListener<THeader>>>,
    closed: Mutex<bool>,
    stream: Arc<dyn PrivateFrameStream>,
    limits: PrivateFrameLimits,
}

impl<THeader> PrivateFramedChannel<THeader>
where
    THeader: serde::de::DeserializeOwned + Send + Sync + 'static,
{
    pub fn new(
        stream: Arc<dyn PrivateFrameStream>,
        validate_header: PrivateFrameHeaderValidator<THeader>,
        limits: PrivateFrameLimits,
    ) -> Arc<Self> {
        let channel = Arc::new(Self {
            decoder: Mutex::new(PrivateFrameDecoder::new(validate_header, limits)),
            listeners: Mutex::new(Vec::new()),
            closed: Mutex::new(false),
            stream: stream.clone(),
            limits,
        });

        let handle_data = {
            let channel = channel.clone();
            Arc::new(move |chunk: &[u8]| {
                let frames = channel.decoder.lock().expect("decoder poisoned").push(chunk);
                match frames {
                    Ok(frames) => {
                        let listeners = channel.listeners.lock().expect("listeners poisoned").clone();
                        for frame in frames {
                            for listener in &listeners {
                                listener(&frame);
                            }
                        }
                    }
                    Err(error) => channel.stream.destroy(error),
                }
            })
        };
        let handle_end = {
            let channel = channel.clone();
            Arc::new(move || {
                let result = channel.decoder.lock().expect("decoder poisoned").finish();
                if let Err(error) = result {
                    channel.stream.destroy(error);
                }
            })
        };
        let handle_close = {
            let channel = channel.clone();
            Arc::new(move || {
                *channel.closed.lock().expect("closed poisoned") = true;
                channel.detach();
            })
        };
        stream.on_data(handle_data);
        stream.on_end(handle_end);
        stream.on_close(handle_close);
        channel
    }

    /// `onFrame(listener)`.
    pub fn on_frame(&self, listener: PrivateFrameListener<THeader>) -> Arc<dyn Fn() + Send + Sync> {
        self.listeners
            .lock()
            .expect("listeners poisoned")
            .push(listener.clone());
        let listeners = self.listeners.clone();
        Arc::new(move || {
            let mut guard = listeners.lock().expect("listeners poisoned");
            if let Some(index) = guard.iter().position(|entry| Arc::ptr_eq(entry, &listener)) {
                guard.remove(index);
            }
        })
    }

    /// `send(header, payload)`.
    pub async fn send(
        &self,
        header: &serde_json::Value,
        payload: &[u8],
    ) -> Result<(), String> {
        if *self.closed.lock().expect("closed poisoned") || self.stream.is_destroyed() {
            return Err("Private frame channel is closed".to_string());
        }
        let frame = encode_private_frame(header, payload, self.limits)?;
        self.stream.write(frame).await
    }

    /// `close()`.
    pub fn close(&self) {
        if *self.closed.lock().expect("closed poisoned") {
            return;
        }
        *self.closed.lock().expect("closed poisoned") = true;
        self.detach();
        self.stream.end();
    }

    fn detach(&self) {
        self.stream.detach();
        self.listeners.lock().expect("listeners poisoned").clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    #[derive(Debug, Clone, PartialEq, serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct TestHeader {
        session_id: String,
    }

    fn validator() -> PrivateFrameHeaderValidator<TestHeader> {
        Arc::new(|value: &serde_json::Value| {
            value
                .get("sessionId")
                .and_then(serde_json::Value::as_str)
                .is_some()
        })
    }

    fn header(session_id: &str) -> serde_json::Value {
        serde_json::json!({ "sessionId": session_id })
    }

    #[test]
    fn frames_round_trip_through_the_decoder() {
        let frame = encode_private_frame(
            &header("s1"),
            b"payload",
            DEFAULT_PRIVATE_FRAME_LIMITS,
        )
        .unwrap();
        assert_eq!(&frame[0..4], &(14u32).to_be_bytes());
        assert_eq!(&frame[4..8], &(7u32).to_be_bytes());

        let mut decoder = PrivateFrameDecoder::new(validator(), DEFAULT_PRIVATE_FRAME_LIMITS);
        let frames = decoder.push(&frame).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].header,
            TestHeader {
                session_id: "s1".to_string()
            }
        );
        assert_eq!(frames[0].payload, b"payload");
        assert_eq!(decoder.buffered_bytes(), 0);
        decoder.finish().unwrap();
    }

    #[test]
    fn a_partial_frame_stays_buffered_until_it_completes() {
        let frame = encode_private_frame(&header("s1"), b"ab", DEFAULT_PRIVATE_FRAME_LIMITS).unwrap();
        let mut decoder = PrivateFrameDecoder::new(validator(), DEFAULT_PRIVATE_FRAME_LIMITS);
        assert!(decoder.push(&frame[..5]).unwrap().is_empty());
        assert_eq!(decoder.buffered_bytes(), 5);
        let frames = decoder.push(&frame[5..]).unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].payload, b"ab");
    }

    #[test]
    fn two_frames_in_one_chunk_decode_in_order() {
        let mut bytes = encode_private_frame(&header("a"), b"", DEFAULT_PRIVATE_FRAME_LIMITS).unwrap();
        bytes.extend(encode_private_frame(&header("b"), b"x", DEFAULT_PRIVATE_FRAME_LIMITS).unwrap());
        let mut decoder = PrivateFrameDecoder::new(validator(), DEFAULT_PRIVATE_FRAME_LIMITS);
        let frames = decoder.push(&bytes).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].header.session_id, "a");
        assert_eq!(frames[1].header.session_id, "b");
    }

    #[test]
    fn an_empty_header_is_rejected_on_encode_and_decode() {
        let error = encode_private_frame(
            &serde_json::json!({}),
            b"",
            PrivateFrameLimits {
                max_header_bytes: 0,
                max_payload_bytes: 16,
            },
        )
        .unwrap_err();
        assert_eq!(error, "Invalid private frame header length: 2");

        let mut decoder = PrivateFrameDecoder::new(validator(), DEFAULT_PRIVATE_FRAME_LIMITS);
        let mut bytes = 0u32.to_be_bytes().to_vec();
        bytes.extend(4u32.to_be_bytes());
        bytes.extend(b"null");
        assert_eq!(
            decoder.push(&bytes).unwrap_err(),
            "Private frame header cannot be empty"
        );
    }

    #[test]
    fn an_oversized_length_is_rejected() {
        let mut decoder = PrivateFrameDecoder::new(
            validator(),
            PrivateFrameLimits {
                max_header_bytes: 8,
                max_payload_bytes: 8,
            },
        );
        let mut bytes = 9u32.to_be_bytes().to_vec();
        bytes.extend(0u32.to_be_bytes());
        assert_eq!(
            decoder.push(&bytes).unwrap_err(),
            "Invalid private frame header length: 9"
        );
    }

    #[test]
    fn a_bad_header_payload_is_rejected() {
        let mut decoder = PrivateFrameDecoder::new(validator(), DEFAULT_PRIVATE_FRAME_LIMITS);
        let mut bytes = 3u32.to_be_bytes().to_vec();
        bytes.extend(0u32.to_be_bytes());
        bytes.extend(b"not");
        let error = decoder.push(&bytes).unwrap_err();
        assert!(error.starts_with("Invalid private frame header JSON:"));
    }

    #[test]
    fn an_invalid_routing_header_is_rejected() {
        let mut decoder = PrivateFrameDecoder::new(validator(), DEFAULT_PRIVATE_FRAME_LIMITS);
        let payload = b"{}";
        let mut bytes = (payload.len() as u32).to_be_bytes().to_vec();
        bytes.extend(0u32.to_be_bytes());
        bytes.extend(payload);
        assert_eq!(
            decoder.push(&bytes).unwrap_err(),
            "Invalid private frame routing header"
        );
    }

    #[test]
    fn finish_reports_incomplete_bytes() {
        let mut decoder: PrivateFrameDecoder<TestHeader> =
            PrivateFrameDecoder::new(validator(), DEFAULT_PRIVATE_FRAME_LIMITS);
        decoder.push(&[1, 2, 3]).unwrap();
        assert_eq!(
            decoder.finish().unwrap_err(),
            "Private frame channel ended with 3 incomplete bytes"
        );
    }

    struct TestStream {
        data: StdMutex<Option<Arc<dyn Fn(&[u8]) + Send + Sync>>>,
        end: StdMutex<Option<Arc<dyn Fn() + Send + Sync>>>,
        close: StdMutex<Option<Arc<dyn Fn() + Send + Sync>>>,
        written: StdMutex<Vec<Vec<u8>>>,
        destroyed: StdMutex<Option<String>>,
        ended: StdMutex<bool>,
    }

    impl TestStream {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                data: StdMutex::new(None),
                end: StdMutex::new(None),
                close: StdMutex::new(None),
                written: StdMutex::new(Vec::new()),
                destroyed: StdMutex::new(None),
                ended: StdMutex::new(false),
            })
        }
    }

    impl PrivateFrameStream for TestStream {
        fn on_data(&self, handler: Arc<dyn Fn(&[u8]) + Send + Sync>) {
            *self.data.lock().unwrap() = Some(handler);
        }
        fn on_end(&self, handler: Arc<dyn Fn() + Send + Sync>) {
            *self.end.lock().unwrap() = Some(handler);
        }
        fn on_close(&self, handler: Arc<dyn Fn() + Send + Sync>) {
            *self.close.lock().unwrap() = Some(handler);
        }
        fn detach(&self) {
            *self.data.lock().unwrap() = None;
            *self.end.lock().unwrap() = None;
            *self.close.lock().unwrap() = None;
        }
        fn write(&self, frame: Vec<u8>) -> pi_ai::types::BoxFuture<Result<(), String>> {
            self.written.lock().unwrap().push(frame);
            Box::pin(async { Ok(()) })
        }
        fn end(&self) {
            *self.ended.lock().unwrap() = true;
        }
        fn destroy(&self, error: String) {
            *self.destroyed.lock().unwrap() = Some(error);
        }
        fn is_destroyed(&self) -> bool {
            self.destroyed.lock().unwrap().is_some()
        }
    }

    #[tokio::test]
    async fn the_channel_delivers_frames_and_closes() {
        let stream = TestStream::new();
        let channel: Arc<PrivateFramedChannel<TestHeader>> = PrivateFramedChannel::new(
            stream.clone(),
            validator(),
            DEFAULT_PRIVATE_FRAME_LIMITS,
        );
        let received = Arc::new(StdMutex::new(Vec::new()));
        let sink = received.clone();
        let _unsubscribe = channel.on_frame(Arc::new(move |frame: &PrivateFrame<TestHeader>| {
            sink.lock().unwrap().push(frame.header.session_id.clone());
        }));

        channel.send(&header("s1"), b"p").await.unwrap();
        assert_eq!(stream.written.lock().unwrap().len(), 1);
        let frame = stream.written.lock().unwrap()[0].clone();
        (stream.data.lock().unwrap().as_ref().unwrap())(&frame);
        assert_eq!(received.lock().unwrap().clone(), vec!["s1".to_string()]);

        channel.close();
        assert!(stream.ended.lock().unwrap().to_owned());
        assert!(channel.send(&header("s1"), b"").await.is_err());
    }

    #[tokio::test]
    async fn a_decode_failure_destroys_the_stream() {
        let stream = TestStream::new();
        let channel: Arc<PrivateFramedChannel<TestHeader>> = PrivateFramedChannel::new(
            stream.clone(),
            validator(),
            DEFAULT_PRIVATE_FRAME_LIMITS,
        );
        (stream.data.lock().unwrap().as_ref().unwrap())(&[0, 0, 0, 0, 0, 0, 0, 1]);
        assert_eq!(
            stream.destroyed.lock().unwrap().clone(),
            Some("Private frame header cannot be empty".to_string())
        );
        assert!(!channel.closed.lock().unwrap().to_owned());
    }
}
