//! Port of packages/coding-agent/src/modes/daemon/snapshot-transcript-cache.ts

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pi_agent_core::types::AgentMessage;
use tokio::sync::oneshot;

pub const SNAPSHOT_TARGET_CHUNK_BYTES: usize = 512 * 1024;
pub const SNAPSHOT_MEMORY_CACHE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
enum SnapshotTranscriptChunk {
    Buffer(Vec<u8>),
    Path(PathBuf),
}

#[derive(Debug, Clone)]
pub struct SnapshotTranscriptCacheOptions {
    pub active_session_id: String,
    pub snapshot_id: String,
    pub messages: Option<Vec<AgentMessage>>,
    pub cache_root: String,
    pub target_chunk_bytes: Option<usize>,
    pub memory_cache_bytes: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct CreateSnapshotTranscriptChunksOptions {
    pub active_session_id: String,
    pub snapshot_id: String,
    pub messages: Vec<AgentMessage>,
    pub target_chunk_bytes: Option<usize>,
    pub aborted: bool,
}

/// The `signal.throwIfAborted()` failure the generator raises.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("The snapshot transcript was aborted")]
pub struct SnapshotTranscriptAborted;

/// The lazy `Iterable<Buffer>` the TypeScript generator returns.
pub struct SnapshotTranscriptChunks {
    options: CreateSnapshotTranscriptChunksOptions,
    serialized_messages: Vec<String>,
    serialized_bytes: usize,
    index: usize,
    message_index: usize,
    finished: bool,
}

impl SnapshotTranscriptChunks {
    fn flush(&mut self) -> Option<Vec<u8>> {
        if self.serialized_messages.is_empty() {
            return None;
        }
        let prefix = format!(
            "{{\"type\":\"session_snapshot_chunk\",\"activeSessionId\":{},\"snapshotId\":{},\"index\":{},\"messages\":[",
            json_string(&self.options.active_session_id),
            json_string(&self.options.snapshot_id),
            self.index
        );
        let line = format!("{prefix}{}]}}\n", self.serialized_messages.join(","));
        self.serialized_messages = Vec::new();
        self.serialized_bytes = 0;
        self.index += 1;
        Some(line.into_bytes())
    }
}

impl Iterator for SnapshotTranscriptChunks {
    type Item = Result<Vec<u8>, SnapshotTranscriptAborted>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.options.aborted {
            return Some(Err(SnapshotTranscriptAborted));
        }
        if self.finished {
            return None;
        }
        let target = self
            .options
            .target_chunk_bytes
            .unwrap_or(SNAPSHOT_TARGET_CHUNK_BYTES);
        while self.message_index < self.options.messages.len() {
            if self.options.aborted {
                return Some(Err(SnapshotTranscriptAborted));
            }
            let serialized = serde_json::to_string(&self.options.messages[self.message_index]).ok();
            self.message_index += 1;
            let Some(serialized) = serialized else {
                continue;
            };
            let bytes = serialized.len() + usize::from(!self.serialized_messages.is_empty());
            if !self.serialized_messages.is_empty() && self.serialized_bytes + bytes > target {
                if let Some(chunk) = self.flush() {
                    return Some(Ok(chunk));
                }
            }
            self.serialized_messages.push(serialized);
            self.serialized_bytes += bytes;
        }
        self.finished = true;
        if self.options.aborted {
            return Some(Err(SnapshotTranscriptAborted));
        }
        self.flush().map(Ok)
    }
}

pub fn create_snapshot_transcript_chunks(
    options: CreateSnapshotTranscriptChunksOptions,
) -> SnapshotTranscriptChunks {
    SnapshotTranscriptChunks {
        options,
        serialized_messages: Vec::new(),
        serialized_bytes: 0,
        index: 0,
        message_index: 0,
        finished: false,
    }
}

struct ChunkWaiter {
    sender: oneshot::Sender<Result<Option<Vec<u8>>, String>>,
}

pub struct SnapshotTranscriptCache {
    options: SnapshotTranscriptCacheOptions,
    chunks: Mutex<Vec<SnapshotTranscriptChunk>>,
    cache_directory: Mutex<Option<PathBuf>>,
    total_bytes: Mutex<usize>,
    completed: Mutex<bool>,
    readers: Mutex<usize>,
    dispose_requested: Mutex<bool>,
    disposed: Mutex<bool>,
    failure: Mutex<Option<String>>,
    chunk_waiters: Mutex<HashMap<usize, Vec<ChunkWaiter>>>,
    pub target_chunk_bytes: usize,
    pub snapshot_id: String,
    pub active_session_id: String,
}

impl SnapshotTranscriptCache {
    pub fn new(options: SnapshotTranscriptCacheOptions) -> Self {
        let cache = Self {
            target_chunk_bytes: options.target_chunk_bytes.unwrap_or(SNAPSHOT_TARGET_CHUNK_BYTES),
            snapshot_id: options.snapshot_id.clone(),
            active_session_id: options.active_session_id.clone(),
            options: options.clone(),
            chunks: Mutex::new(Vec::new()),
            cache_directory: Mutex::new(None),
            total_bytes: Mutex::new(0),
            completed: Mutex::new(false),
            readers: Mutex::new(0),
            dispose_requested: Mutex::new(false),
            disposed: Mutex::new(false),
            failure: Mutex::new(None),
            chunk_waiters: Mutex::new(HashMap::new()),
        };
        if let Some(messages) = options.messages.as_ref() {
            cache.encode_messages(messages);
            *cache.completed.lock().expect("completed poisoned") = true;
        }
        cache
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.lock().expect("chunks poisoned").len()
    }

    pub fn complete(&self) -> bool {
        *self.completed.lock().expect("completed poisoned")
            && self.failure.lock().expect("failure poisoned").is_none()
            && !*self.disposed.lock().expect("disposed poisoned")
    }

    pub fn bytes(&self) -> usize {
        *self.total_bytes.lock().expect("bytes poisoned")
    }

    pub fn file_backed(&self) -> bool {
        self.cache_directory
            .lock()
            .expect("cache directory poisoned")
            .is_some()
    }

    pub fn read_chunk(&self, index: usize) -> Result<Vec<u8>, String> {
        let chunk = self
            .chunks
            .lock()
            .expect("chunks poisoned")
            .get(index)
            .cloned();
        let Some(chunk) = chunk else {
            return Err(format!("Unknown snapshot transcript chunk: {index}"));
        };
        match chunk {
            SnapshotTranscriptChunk::Buffer(buffer) => Ok(buffer),
            SnapshotTranscriptChunk::Path(path) => std::fs::read(&path).map_err(|error| error.to_string()),
        }
    }

    pub fn iter_chunks(&self) -> Result<Vec<Vec<u8>>, String> {
        let mut chunks = Vec::with_capacity(self.chunk_count());
        for index in 0..self.chunk_count() {
            chunks.push(self.read_chunk(index)?);
        }
        Ok(chunks)
    }

    pub fn append_encoded_chunk(&self, buffer: Vec<u8>) -> Result<(), String> {
        if *self.completed.lock().expect("completed poisoned")
            || self.failure.lock().expect("failure poisoned").is_some()
            || *self.disposed.lock().expect("disposed poisoned")
        {
            return Err(format!("Snapshot transcript {} is not writable", self.snapshot_id));
        }
        self.store_chunk(buffer)
    }

    pub fn mark_complete(&self) -> Result<(), String> {
        if *self.completed.lock().expect("completed poisoned") {
            return Ok(());
        }
        if self.failure.lock().expect("failure poisoned").is_some()
            || *self.disposed.lock().expect("disposed poisoned")
        {
            return Err(format!(
                "Snapshot transcript {} cannot be completed",
                self.snapshot_id
            ));
        }
        *self.completed.lock().expect("completed poisoned") = true;
        let chunk_count = self.chunk_count();
        let mut waiters = self.chunk_waiters.lock().expect("waiters poisoned");
        let indexes: Vec<usize> = waiters.keys().copied().collect();
        for index in indexes {
            if index < chunk_count {
                continue;
            }
            if let Some(entries) = waiters.remove(&index) {
                for entry in entries {
                    let _ = entry.sender.send(Ok(None));
                }
            }
        }
        Ok(())
    }

    pub fn mark_failed(&self, error: &str) {
        {
            let mut failure = self.failure.lock().expect("failure poisoned");
            if failure.is_some() {
                return;
            }
            *failure = Some(error.to_string());
        }
        let mut waiters = self.chunk_waiters.lock().expect("waiters poisoned");
        for (_, entries) in waiters.drain() {
            for entry in entries {
                let _ = entry.sender.send(Err(error.to_string()));
            }
        }
    }

    pub async fn wait_for_chunk(&self, index: usize) -> Result<Option<Vec<u8>>, String> {
        if let Some(failure) = self.failure.lock().expect("failure poisoned").clone() {
            return Err(failure);
        }
        if index < self.chunk_count() {
            return self.read_chunk(index).map(Some);
        }
        if *self.completed.lock().expect("completed poisoned") {
            return Ok(None);
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut waiters = self.chunk_waiters.lock().expect("waiters poisoned");
            waiters.entry(index).or_default().push(ChunkWaiter { sender });
        }
        receiver
            .await
            .unwrap_or_else(|_| Err("Snapshot transcript cache was dropped".to_string()))
    }

    pub fn retain(self: &Arc<Self>) -> SnapshotTranscriptRetain {
        if *self.disposed.lock().expect("disposed poisoned") {
            panic!("Snapshot transcript {} was disposed", self.snapshot_id);
        }
        *self.readers.lock().expect("readers poisoned") += 1;
        SnapshotTranscriptRetain {
            cache: Arc::clone(self),
            released: false,
        }
    }

    pub fn dispose(&self) {
        {
            let disposed = self.disposed.lock().expect("disposed poisoned");
            let mut requested = self.dispose_requested.lock().expect("dispose poisoned");
            if *disposed || *requested {
                return;
            }
            *requested = true;
        }
        if *self.readers.lock().expect("readers poisoned") > 0 {
            return;
        }
        self.dispose_now();
    }

    fn dispose_now(&self) {
        {
            let mut disposed = self.disposed.lock().expect("disposed poisoned");
            if *disposed {
                return;
            }
            *disposed = true;
        }
        self.mark_failed(&format!("Snapshot transcript {} was disposed", self.snapshot_id));
        if let Some(directory) = self
            .cache_directory
            .lock()
            .expect("cache directory poisoned")
            .take()
        {
            let _ = std::fs::remove_dir_all(directory);
        }
        self.chunks.lock().expect("chunks poisoned").clear();
    }

    fn encode_messages(&self, messages: &[AgentMessage]) {
        let mut serialized_messages: Vec<String> = Vec::new();
        let mut serialized_bytes = 0usize;
        for message in messages {
            let serialized = match serde_json::to_string(message) {
                Ok(serialized) => serialized,
                Err(_) => continue,
            };
            let bytes = serialized.len() + usize::from(!serialized_messages.is_empty());
            if !serialized_messages.is_empty() && serialized_bytes + bytes > self.target_chunk_bytes {
                self.flush_messages(&mut serialized_messages, &mut serialized_bytes);
            }
            serialized_messages.push(serialized);
            serialized_bytes += bytes;
        }
        self.flush_messages(&mut serialized_messages, &mut serialized_bytes);
    }

    fn flush_messages(&self, serialized_messages: &mut Vec<String>, serialized_bytes: &mut usize) {
        if serialized_messages.is_empty() {
            return;
        }
        let index = self.chunk_count();
        let prefix = format!(
            "{{\"type\":\"session_snapshot_chunk\",\"activeSessionId\":{},\"snapshotId\":{},\"index\":{},\"messages\":[",
            json_string(&self.options.active_session_id),
            json_string(&self.options.snapshot_id),
            index
        );
        let line = format!("{prefix}{}]}}\n", serialized_messages.join(","));
        let _ = self.store_chunk(line.into_bytes());
        serialized_messages.clear();
        *serialized_bytes = 0;
    }

    fn store_chunk(&self, buffer: Vec<u8>) -> Result<(), String> {
        let mut total_bytes = self.total_bytes.lock().expect("bytes poisoned");
        *total_bytes += buffer.len();
        let memory_limit = self
            .options
            .memory_cache_bytes
            .unwrap_or(SNAPSHOT_MEMORY_CACHE_BYTES);
        if self
            .cache_directory
            .lock()
            .expect("cache directory poisoned")
            .is_none()
            && *total_bytes > memory_limit
        {
            let directory = Path::new(&self.options.cache_root).join(sanitize_snapshot_id(&self.options.snapshot_id));
            create_private_directory(&directory).map_err(|error| error.to_string())?;
            *self.cache_directory.lock().expect("cache directory poisoned") = Some(directory.clone());
            let mut chunks = self.chunks.lock().expect("chunks poisoned");
            for index in 0..chunks.len() {
                let existing = chunks[index].clone();
                let SnapshotTranscriptChunk::Buffer(existing_buffer) = existing else {
                    continue;
                };
                let path = directory.join(format!("{index}.jsonl"));
                write_private_file(&path, &existing_buffer).map_err(|error| error.to_string())?;
                chunks[index] = SnapshotTranscriptChunk::Path(path);
            }
        }

        let index = {
            let directory = self
                .cache_directory
                .lock()
                .expect("cache directory poisoned")
                .clone();
            let mut chunks = self.chunks.lock().expect("chunks poisoned");
            if let Some(directory) = directory {
                let path = directory.join(format!("{}.jsonl", chunks.len()));
                write_private_file(&path, &buffer).map_err(|error| error.to_string())?;
                chunks.push(SnapshotTranscriptChunk::Path(path));
            } else {
                chunks.push(SnapshotTranscriptChunk::Buffer(buffer));
            }
            chunks.len() - 1
        };

        let waiters = {
            let mut waiters = self.chunk_waiters.lock().expect("waiters poisoned");
            waiters.remove(&index)
        };
        if let Some(waiters) = waiters {
            let stored = self.read_chunk(index);
            for waiter in waiters {
                let _ = waiter.sender.send(stored.clone().map(Some));
            }
        }
        Ok(())
    }
}

pub struct SnapshotTranscriptRetain {
    cache: Arc<SnapshotTranscriptCache>,
    released: bool,
}

impl SnapshotTranscriptRetain {
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let remaining = {
            let mut readers = self.cache.readers.lock().expect("readers poisoned");
            *readers = readers.saturating_sub(1);
            *readers
        };
        if remaining == 0
            && *self
                .cache
                .dispose_requested
                .lock()
                .expect("dispose poisoned")
        {
            self.cache.dispose_now();
        }
    }
}

impl Drop for SnapshotTranscriptRetain {
    fn drop(&mut self) {
        self.release();
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn sanitize_snapshot_id(snapshot_id: &str) -> String {
    snapshot_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        return builder.create(path);
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        return file.write_all(bytes);
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::{AssistantMessage, ContentBlock, Message, TextContent, Usage};

    fn text_message(text: &str) -> AgentMessage {
        AgentMessage::Message(Message::Assistant(AssistantMessage {
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text(TextContent {
                type_: pi_ai::types::TEXT_CONTENT_TYPE.to_string(),
                text: text.to_string(),
                text_signature: None,
            })],
            api: "openai-completions".to_string(),
            provider: "openai".to_string(),
            model: "gpt".to_string(),
            response_model: None,
            response_id: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: "stop".to_string(),
            stop_reason_raw: None,
            error_message: None,
            timestamp: 0,
        }))
    }

    fn options(messages: Option<Vec<AgentMessage>>) -> SnapshotTranscriptCacheOptions {
        SnapshotTranscriptCacheOptions {
            active_session_id: "active-1".to_string(),
            snapshot_id: "snap/1".to_string(),
            messages,
            cache_root: std::env::temp_dir().to_string_lossy().to_string(),
            target_chunk_bytes: None,
            memory_cache_bytes: None,
        }
    }

    #[test]
    fn chunks_serialize_the_session_snapshot_envelope() {
        let chunks = create_snapshot_transcript_chunks(CreateSnapshotTranscriptChunksOptions {
            active_session_id: "active-1".to_string(),
            snapshot_id: "snap-1".to_string(),
            messages: vec![text_message("hello")],
            target_chunk_bytes: None,
            aborted: false,
        })
        .collect::<Result<Vec<_>, _>>()
        .expect("chunks");
        assert_eq!(chunks.len(), 1);
        let line = String::from_utf8(chunks[0].clone()).expect("utf8");
        assert!(line.ends_with("]}\n"));
        assert!(line.starts_with(
            "{\"type\":\"session_snapshot_chunk\",\"activeSessionId\":\"active-1\",\"snapshotId\":\"snap-1\",\"index\":0,\"messages\":["
        ));
    }

    #[test]
    fn small_target_bytes_split_the_chunks() {
        let chunks = create_snapshot_transcript_chunks(CreateSnapshotTranscriptChunksOptions {
            active_session_id: "a".to_string(),
            snapshot_id: "s".to_string(),
            messages: vec![text_message("one"), text_message("two")],
            target_chunk_bytes: Some(1),
            aborted: false,
        })
        .collect::<Result<Vec<_>, _>>()
        .expect("chunks");
        assert_eq!(chunks.len(), 2);
        assert!(String::from_utf8(chunks[1].clone())
            .expect("utf8")
            .contains("\"index\":1"));
    }

    #[test]
    fn an_aborted_signal_fails_the_iterator() {
        let mut chunks = create_snapshot_transcript_chunks(CreateSnapshotTranscriptChunksOptions {
            active_session_id: "a".to_string(),
            snapshot_id: "s".to_string(),
            messages: vec![text_message("one")],
            target_chunk_bytes: None,
            aborted: true,
        });
        assert_eq!(chunks.next(), Some(Err(SnapshotTranscriptAborted)));
    }

    #[test]
    fn the_cache_keeps_chunks_in_memory_and_reads_them() {
        let cache = SnapshotTranscriptCache::new(options(Some(vec![text_message("hello")])));
        assert_eq!(cache.chunk_count(), 1);
        assert!(cache.complete());
        assert!(!cache.file_backed());
        assert!(cache.bytes() > 0);
        assert!(String::from_utf8(cache.read_chunk(0).expect("chunk"))
            .expect("utf8")
            .contains("hello"));
        assert_eq!(
            cache.read_chunk(9).expect_err("unknown"),
            "Unknown snapshot transcript chunk: 9"
        );
        assert_eq!(cache.iter_chunks().expect("chunks").len(), 1);
    }

    #[test]
    fn the_cache_spills_to_disk_after_the_memory_limit() {
        let root = std::env::temp_dir().join(format!("snapshot-cache-{}", std::process::id()));
        let cache = SnapshotTranscriptCache::new(SnapshotTranscriptCacheOptions {
            active_session_id: "a".to_string(),
            snapshot_id: "snap/1".to_string(),
            messages: Some(vec![text_message("hello")]),
            cache_root: root.to_string_lossy().to_string(),
            target_chunk_bytes: None,
            memory_cache_bytes: Some(1),
        });
        assert!(cache.file_backed());
        assert!(root.join("snap_1").join("0.jsonl").exists());
        cache.dispose();
        assert!(!root.join("snap_1").exists());
    }

    #[test]
    fn dispose_waits_for_readers() {
        let cache = Arc::new(SnapshotTranscriptCache::new(options(Some(vec![text_message("hello")]))));
        let retain = cache.retain();
        cache.dispose();
        assert!(cache.complete());
        drop(retain);
        assert!(!cache.complete());
        assert_eq!(cache.chunk_count(), 0);
        assert_eq!(
            cache.read_chunk(0).expect_err("gone"),
            "Unknown snapshot transcript chunk: 0"
        );
    }

    #[tokio::test]
    async fn waiters_are_released_by_new_chunks_completion_and_failure() {
        let cache = Arc::new(SnapshotTranscriptCache::new(SnapshotTranscriptCacheOptions {
            active_session_id: "a".to_string(),
            snapshot_id: "s".to_string(),
            messages: None,
            cache_root: std::env::temp_dir().to_string_lossy().to_string(),
            target_chunk_bytes: None,
            memory_cache_bytes: None,
        }));
        let waiter = {
            let cache = Arc::clone(&cache);
            tokio::spawn(async move { cache.wait_for_chunk(0).await })
        };
        cache.append_encoded_chunk(b"line\n".to_vec()).expect("append");
        let stored = waiter.await.expect("join").expect("ok").expect("chunk");
        assert_eq!(stored, b"line\n".to_vec());

        let waiter = {
            let cache = Arc::clone(&cache);
            tokio::spawn(async move { cache.wait_for_chunk(5).await })
        };
        cache.mark_complete().expect("completes");
        assert_eq!(waiter.await.expect("join").expect("ok"), None);

        let waiter = {
            let cache = Arc::clone(&cache);
            tokio::spawn(async move { cache.wait_for_chunk(6).await })
        };
        cache.mark_failed("boom");
        assert_eq!(waiter.await.expect("join").expect_err("fails"), "boom");
        assert_eq!(cache.wait_for_chunk(0).await.expect_err("fails"), "boom");
        assert!(!cache.complete());
        assert!(cache.append_encoded_chunk(b"x".to_vec()).is_err());
    }
}
