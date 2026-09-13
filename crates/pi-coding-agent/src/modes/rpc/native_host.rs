//! Native process adapter for rpc-mode.ts.

use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::modes::agent_connection::types::AgentConnection;
use super::rpc_mode::{run_rpc_mode, RpcModeHost};

pub(crate) async fn run_native_rpc_mode(connection: Arc<dyn AgentConnection>) -> Result<(), String> {
    run_rpc_mode(connection, Arc::new(NativeRpcModeHost { input_cancelled: CancellationToken::new() })).await
}

struct NativeRpcModeHost {
    input_cancelled: CancellationToken,
}

impl RpcModeHost for NativeRpcModeHost {
    fn attach_stdin(
        &self,
        on_data: Arc<dyn Fn(&[u8]) + Send + Sync>,
        on_end: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        let cancelled = self.input_cancelled.clone();
        let task_cancelled = cancelled.clone();
        tokio::spawn(async move {
            let mut input = tokio::io::stdin();
            let mut buffer = vec![0; 8192];
            loop {
                let read = tokio::select! {
                    _ = task_cancelled.cancelled() => return,
                    read = input.read(&mut buffer) => read,
                };
                match read {
                    Ok(0) | Err(_) => { on_end(); return; }
                    Ok(size) => on_data(&buffer[..size]),
                }
            }
        });
        Arc::new(move || cancelled.cancel())
    }

    fn pause_stdin(&self) { self.input_cancelled.cancel(); }

    fn on_signal(&self, signal: &str, handler: Arc<dyn Fn() + Send + Sync>) -> Arc<dyn Fn() + Send + Sync> {
        let cancelled = CancellationToken::new();
        let task_cancelled = cancelled.clone();
        #[cfg(unix)]
        {
            let kind = match signal {
                "SIGHUP" => tokio::signal::unix::SignalKind::hangup(),
                "SIGINT" => tokio::signal::unix::SignalKind::interrupt(),
                _ => tokio::signal::unix::SignalKind::terminate(),
            };
            match tokio::signal::unix::signal(kind) {
                Ok(mut stream) => { tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = task_cancelled.cancelled() => break,
                            next = stream.recv() => {
                                if next.is_none() { break; }
                                handler();
                            }
                        }
                    }
                }); }
                Err(error) => eprintln!("Could not install {signal} handler: {error}"),
            }
        }
        #[cfg(windows)]
        {
            let _ = signal;
            tokio::spawn(async move {
                tokio::select! {
                    _ = task_cancelled.cancelled() => {},
                    result = tokio::signal::ctrl_c() => { if result.is_ok() { handler(); } }
                }
            });
        }
        Arc::new(move || cancelled.cancel())
    }

    fn exit(&self, code: i32) { std::process::exit(code); }
    fn platform(&self) -> String { crate::utils::pi_user_agent::process_platform().to_string() }
    fn kill_tracked_detached_children(&self) { crate::utils::shell::kill_tracked_detached_children(); }
}
