//! Port of packages/coding-agent/src/core/bash-executor.ts
//!
//! Bash command execution with streaming support and cancellation.
//!
//! This module provides a unified bash execution implementation used by:
//! - AgentSession.executeBash() for interactive and RPC modes
//! - Direct calls from modes that need bash execution

use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::core::tools::bash::{BashExecOptions, BashExecResult, BashOperations};
use crate::core::tools::output_accumulator::OutputSpill;
use crate::core::tools::truncate::{truncate_tail, TruncationOptions, DEFAULT_MAX_BYTES};
use crate::utils::shell::sanitize_binary_output;

/// Options for executing shell commands.
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

/// Result of executing a shell command.
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

/// Execute a bash command using custom [`BashOperations`].
/// Used for remote execution (SSH, containers, etc.).
pub async fn execute_bash_with_operations(
    command: &str,
    cwd: &str,
    operations: Arc<dyn BashOperations>,
    options: Option<BashExecutorOptions>,
) -> Result<BashResult, String> {
    let options = options.unwrap_or_default();
    let output_chunks: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let output_bytes = Arc::new(Mutex::new(0usize));
    let max_output_bytes = DEFAULT_MAX_BYTES * 2;

    let spill = Arc::new(Mutex::new(OutputSpill::new("pi-bash")));
    let total_bytes = Arc::new(Mutex::new(0usize));

    let on_chunk = options.on_chunk.clone();
    let chunks_for_data = Arc::clone(&output_chunks);
    let bytes_for_data = Arc::clone(&output_bytes);
    let spill_for_data = Arc::clone(&spill);
    let total_for_data = Arc::clone(&total_bytes);
    let on_data: Arc<dyn Fn(&[u8]) + Send + Sync> = Arc::new(move |data: &[u8]| {
        {
            let mut total = total_for_data.lock().expect("total bytes");
            *total += data.len();
        }
        // `sanitizeBinaryOutput(stripAnsi(decoder.decode(data, { stream: true })))`
        // plus the CR removal. A streaming TextDecoder buffers a partial UTF-8
        // sequence; this port decodes lossily per chunk with the same effect.
        let text = sanitize_binary_output(&pi_tui::utils::strip_ansi(&String::from_utf8_lossy(data)))
            .replace('\r', "");
        if *total_for_data.lock().expect("total bytes") > DEFAULT_MAX_BYTES {
            let replay: Vec<String> = chunks_for_data.lock().expect("chunks").clone();
            spill_for_data
                .lock()
                .expect("spill")
                .open(replay.iter().map(|chunk| chunk.as_bytes()));
        }

        spill_for_data.lock().expect("spill").write(text.as_bytes());
        {
            let mut chunks = chunks_for_data.lock().expect("chunks");
            chunks.push(text.clone());
            let mut bytes = bytes_for_data.lock().expect("output bytes");
            *bytes += text.len();
            while *bytes > max_output_bytes && chunks.len() > 1 {
                let removed = chunks.remove(0);
                *bytes -= removed.len();
            }
        }
        if let Some(on_chunk) = &on_chunk {
            on_chunk(&text);
        }
    });

    let exec_result = operations
        .exec(
            command,
            cwd,
            BashExecOptions {
                on_data,
                signal: options.signal.clone(),
                timeout: None,
                env: None,
            },
        )
        .await;

    let full_output = || output_chunks.lock().expect("chunks").join("");

    match exec_result {
        Ok(BashExecResult { exit_code }) => {
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
                exit_code: if cancelled {
                    None
                } else {
                    exit_code.map(i64::from)
                },
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
    use futures::future::BoxFuture;

    struct FakeOperations {
        chunks: Vec<Vec<u8>>,
        exit_code: Option<i32>,
        fail: bool,
    }

    impl BashOperations for FakeOperations {
        fn exec(
            &self,
            _command: &str,
            _cwd: &str,
            options: BashExecOptions,
        ) -> BoxFuture<'static, Result<BashExecResult, String>> {
            let chunks = self.chunks.clone();
            let exit_code = self.exit_code;
            let fail = self.fail;
            Box::pin(async move {
                for chunk in chunks {
                    (options.on_data)(&chunk);
                }
                if fail {
                    return Err("boom".to_string());
                }
                Ok(BashExecResult { exit_code })
            })
        }
    }

    #[tokio::test]
    async fn streams_and_returns_the_exit_code() {
        let operations = Arc::new(FakeOperations {
            chunks: vec![b"hello\n".to_vec()],
            exit_code: Some(0),
            fail: false,
        });
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let result = execute_bash_with_operations(
            "cmd",
            "/tmp",
            operations,
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
        assert!(result.full_output_path.is_none());
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn strips_carriage_returns() {
        let operations = Arc::new(FakeOperations {
            chunks: vec![b"a\r\nb".to_vec()],
            exit_code: Some(0),
            fail: false,
        });
        let result = execute_bash_with_operations("cmd", "/tmp", operations, None)
            .await
            .unwrap();
        assert_eq!(result.output, "a\nb");
    }

    #[tokio::test]
    async fn cancellation_reports_no_exit_code() {
        let token = CancellationToken::new();
        token.cancel();
        let operations = Arc::new(FakeOperations {
            chunks: vec![b"partial".to_vec()],
            exit_code: None,
            fail: true,
        });
        let result = execute_bash_with_operations(
            "cmd",
            "/tmp",
            operations,
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
        let operations = Arc::new(FakeOperations {
            chunks: Vec::new(),
            exit_code: None,
            fail: true,
        });
        let error = execute_bash_with_operations("cmd", "/tmp", operations, None)
            .await
            .unwrap_err();
        assert_eq!(error, "boom");
    }

    #[tokio::test]
    async fn large_output_is_truncated_and_spilled() {
        let big = "x".repeat(DEFAULT_MAX_BYTES + 1024);
        let operations = Arc::new(FakeOperations {
            chunks: vec![big.into_bytes()],
            exit_code: Some(0),
            fail: false,
        });
        let result = execute_bash_with_operations("cmd", "/tmp", operations, None)
            .await
            .unwrap();
        assert!(result.truncated);
        assert!(result.output.len() <= DEFAULT_MAX_BYTES);
        let path = result.full_output_path.expect("full output path");
        assert!(std::path::Path::new(&path).exists());
        let _ = std::fs::remove_file(path);
    }
}
