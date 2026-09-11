//! Port of packages/coding-agent/src/core/bash-executor.ts
//!
//! Bash command execution with streaming support and cancellation.
//!
//! This module provides a unified bash execution implementation used by:
//! - AgentSession.executeBash() for interactive and RPC modes
//! - Direct calls from modes that need bash execution

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::core::extensions::types::BashOperations;
use crate::core::tools::output_accumulator::OutputSpill;
use crate::core::tools::truncate::{truncate_tail, TruncationOptions, DEFAULT_MAX_BYTES};
use crate::utils::shell::sanitize_binary_output;

pub struct BashExecutorOptions {
    /// Callback for streaming output chunks (already sanitized)
    pub on_chunk: Option<Arc<dyn Fn(&str) + Send + Sync>>,
    /// AbortSignal for cancellation
    pub signal: Option<CancellationToken>,
}

impl Default for BashExecutorOptions {
    fn default() -> Self {
        Self {
            on_chunk: None,
            signal: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BashResult {
    /// Combined stdout + stderr output (sanitized, possibly truncated)
    pub output: String,
    /// Process exit code (None if killed/cancelled)
    pub exit_code: Option<i64>,
    /// Whether the command was cancelled via signal
    pub cancelled: bool,
    /// Whether the output was truncated
    pub truncated: bool,
    /// Path to temp file containing full output (if output exceeded truncation threshold)
    pub full_output_path: Option<String>,
}

/// Execute a bash command using custom BashOperations.
/// Used for remote execution (SSH, containers, etc.).
pub async fn execute_bash_with_operations(
    command: &str,
    cwd: &str,
    operations: &dyn BashOperations,
    options: Option<BashExecutorOptions>,
) -> Result<BashResult, String> {
    let options = options.unwrap_or_default();
    let output_chunks: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let output_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let max_output_bytes = DEFAULT_MAX_BYTES * 2;

    let spill = Arc::new(std::sync::Mutex::new(OutputSpill::new("pi-bash")));
    let total_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let on_chunk = options.on_chunk.clone();
    let chunks_for_data = Arc::clone(&output_chunks);
    let bytes_for_data = Arc::clone(&output_bytes);
    let spill_for_data = Arc::clone(&spill);
    let total_for_data = Arc::clone(&total_bytes);
    let on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync> = Arc::new(move |data: Vec<u8>| {
        total_for_data.fetch_add(data.len(), std::sync::atomic::Ordering::SeqCst);
        // The port of `stripAnsi(decoder.decode(data, { stream: true }))` plus the
        // CR removal. A streaming TextDecoder buffers a partial UTF-8 sequence; the
        // port keeps the same effect by decoding lossily per chunk.
        let text = sanitize_binary_output(&pi_tui::utils::strip_ansi(&String::from_utf8_lossy(&data)))
            .replace('\r', "");
        if total_for_data.load(std::sync::atomic::Ordering::SeqCst) > DEFAULT_MAX_BYTES {
            let replay: Vec<String> = chunks_for_data.lock().expect("chunks").clone();
            spill_for_data
                .lock()
                .expect("spill")
                .open(replay.iter().map(|chunk| chunk.as_bytes()));
        }

        {
            let mut spill = spill_for_data.lock().expect("spill");
            spill.write(text.as_bytes());
        }
        {
            let mut chunks = chunks_for_data.lock().expect("chunks");
            chunks.push(text.clone());
            bytes_for_data.fetch_add(text.len(), std::sync::atomic::Ordering::SeqCst);
            while bytes_for_data.load(std::sync::atomic::Ordering::SeqCst) > max_output_bytes && chunks.len() > 1 {
                let removed = chunks.remove(0);
                bytes_for_data.fetch_sub(removed.len(), std::sync::atomic::Ordering::SeqCst);
            }
        }
        if let Some(on_chunk) = &on_chunk {
            on_chunk(&text);
        }
    });

    let exec_result = operations
        .exec(
            command.to_string(),
            cwd.to_string(),
            on_data,
            options.signal.clone(),
            None,
            None,
        )
        .await;

    let full_output = || output_chunks.lock().expect("chunks").join("");

    match exec_result {
        Ok(exit_code) => {
            let full = full_output();
            let truncation_result = truncate_tail(&full, TruncationOptions::default());
            if truncation_result.truncated {
                let replay: Vec<String> = output_chunks.lock().expect("chunks").clone();
                spill
                    .lock()
                    .expect("spill")
                    .open(replay.iter().map(|chunk| chunk.as_bytes()));
            }
            // Settled before advertising: the path refers to the COMPLETE file or is undefined.
            let full_output_path = spill.lock().expect("spill").finalize();
            let cancelled = options
                .signal
                .as_ref()
                .map(CancellationToken::is_cancelled)
                .unwrap_or(false);

            Ok(BashResult {
                output: if truncation_result.truncated {
                    truncation_result.content
                } else {
                    full
                },
                exit_code: if cancelled { None } else { exit_code },
                cancelled,
                truncated: truncation_result.truncated,
                full_output_path,
            })
        }
        Err(error) => {
            if options
                .signal
                .as_ref()
                .map(CancellationToken::is_cancelled)
                .unwrap_or(false)
            {
                let full = full_output();
                let truncation_result = truncate_tail(&full, TruncationOptions::default());
                if truncation_result.truncated {
                    let replay: Vec<String> = output_chunks.lock().expect("chunks").clone();
                    spill
                        .lock()
                        .expect("spill")
                        .open(replay.iter().map(|chunk| chunk.as_bytes()));
                }
                let full_output_path = spill.lock().expect("spill").finalize();
                return Ok(BashResult {
                    output: if truncation_result.truncated {
                        truncation_result.content
                    } else {
                        full
                    },
                    exit_code: None,
                    cancelled: true,
                    truncated: truncation_result.truncated,
                    full_output_path,
                });
            }

            let _ = spill.lock().expect("spill").finalize();

            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extensions::types::BashOperations;
    use std::future::Future;
    use std::pin::Pin;

    struct FakeOperations {
        chunks: Vec<Vec<u8>>,
        exit_code: Option<i64>,
        fail: bool,
    }

    impl BashOperations for FakeOperations {
        fn exec(
            &self,
            _command: String,
            _cwd: String,
            on_data: Arc<dyn Fn(Vec<u8>) + Send + Sync>,
            _signal: Option<CancellationToken>,
            _timeout: Option<f64>,
            _env: Option<serde_json::Map<String, serde_json::Value>>,
        ) -> Pin<Box<dyn Future<Output = Result<Option<i64>, String>> + Send>> {
            let chunks = self.chunks.clone();
            let exit_code = self.exit_code;
            let fail = self.fail;
            Box::pin(async move {
                for chunk in chunks {
                    on_data(chunk);
                }
                if fail {
                    return Err("boom".to_string());
                }
                Ok(exit_code)
            })
        }
    }

    #[tokio::test]
    async fn streams_and_returns_the_exit_code() {
        let operations = FakeOperations {
            chunks: vec![b"hello\n".to_vec()],
            exit_code: Some(0),
            fail: false,
        };
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let result = execute_bash_with_operations(
            "cmd",
            "/tmp",
            &operations,
            Some(BashExecutorOptions {
                on_chunk: Some(Arc::new(move |chunk: &str| {
                    seen_clone.lock().unwrap().push(chunk.to_string());
                })),
                signal: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.output, "hello\n");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.cancelled);
        assert!(!result.truncated);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn strips_carriage_returns() {
        let operations = FakeOperations {
            chunks: vec![b"a\r\nb".to_vec()],
            exit_code: Some(0),
            fail: false,
        };
        let result = execute_bash_with_operations("cmd", "/tmp", &operations, None)
            .await
            .unwrap();
        assert_eq!(result.output, "a\nb");
    }

    #[tokio::test]
    async fn cancellation_reports_no_exit_code() {
        let token = CancellationToken::new();
        token.cancel();
        let operations = FakeOperations {
            chunks: vec![b"partial".to_vec()],
            exit_code: None,
            fail: true,
        };
        let result = execute_bash_with_operations(
            "cmd",
            "/tmp",
            &operations,
            Some(BashExecutorOptions {
                on_chunk: None,
                signal: Some(token),
            }),
        )
        .await
        .unwrap();
        assert!(result.cancelled);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.output, "partial");
    }

    #[tokio::test]
    async fn errors_propagate_without_cancellation() {
        let operations = FakeOperations {
            chunks: Vec::new(),
            exit_code: None,
            fail: true,
        };
        let error = execute_bash_with_operations("cmd", "/tmp", &operations, None)
            .await
            .unwrap_err();
        assert_eq!(error, "boom");
    }
}
