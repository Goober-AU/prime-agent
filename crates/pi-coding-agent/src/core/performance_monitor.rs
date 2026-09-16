//! Live, profile-scoped control of the bounded local performance recorder.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pi_agent_core::performance_metrics::{
    PerformanceMetricEvent, PerformanceMetricIdScope, PerformanceMetricRecorder,
};
use serde::{Deserialize, Serialize};

use super::performance_metrics::{
    DefaultPerformanceMetricFileIo, LocalPerformanceMetricRecorder,
    LocalPerformanceMetricRecorderOptions, PerformanceMetricFileIo, PerformanceMetricIoError,
};

#[derive(Clone)]
pub struct PerformanceMonitor {
    agent_dir: PathBuf,
    environment_enabled: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    enabled: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum MonitorCommand {
    Select,
    Status,
    Set(bool),
}

pub fn parse_monitor_command(args: &str) -> Result<MonitorCommand, String> {
    match args.trim().to_ascii_lowercase().as_str() {
        "" => Ok(MonitorCommand::Select),
        "status" => Ok(MonitorCommand::Status),
        "on" => Ok(MonitorCommand::Set(true)),
        "off" => Ok(MonitorCommand::Set(false)),
        _ => Err("Usage: /monitor [status|on|off]".into()),
    }
}

impl PerformanceMonitor {
    pub fn from_environment(agent_dir: impl Into<PathBuf>) -> Self {
        let enabled = std::env::var("PRIME_AGENT_PERFORMANCE_METRICS")
            .ok()
            .is_some_and(|value| {
                matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on")
            });
        Self::new(agent_dir, enabled)
    }

    pub fn new(agent_dir: impl Into<PathBuf>, environment_enabled: bool) -> Self {
        Self { agent_dir: agent_dir.into(), environment_enabled }
    }

    pub fn preference_path(&self) -> PathBuf {
        self.agent_dir.join("performance-monitor.json")
    }

    pub fn directory(&self) -> PathBuf {
        self.agent_dir.join("performance-metrics")
    }

    pub fn enabled(&self) -> Result<bool, String> {
        let file = match std::fs::File::open(self.preference_path()) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(self.environment_enabled);
            }
            Err(error) => return Err(format!("Cannot read monitoring preference: {error}")),
        };
        let mut contents = Vec::new();
        file.take(1025).read_to_end(&mut contents)
            .map_err(|error| format!("Cannot read monitoring preference: {error}"))?;
        if contents.len() > 1024 {
            return Err("Monitoring preference is too large; monitoring is OFF.".into());
        }
        serde_json::from_slice::<Preference>(&contents)
            .map(|preference| preference.enabled)
            .map_err(|_| "Invalid monitoring preference; monitoring is OFF.".into())
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        std::fs::create_dir_all(&self.agent_dir)
            .map_err(|error| format!("Cannot save monitoring preference: {error}"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(&self.agent_dir)
            .map_err(|error| format!("Cannot save monitoring preference: {error}"))?;
        serde_json::to_writer(&mut temporary, &Preference { enabled })
            .map_err(|error| format!("Cannot save monitoring preference: {error}"))?;
        temporary.write_all(b"\n").and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| format!("Cannot save monitoring preference: {error}"))?;
        temporary.persist(self.preference_path())
            .map_err(|error| format!("Cannot save monitoring preference: {error}"))?;
        if self.enabled()? != enabled {
            return Err("Monitoring preference changed concurrently; run /monitor status.".into());
        }
        Ok(())
    }

    pub fn status_text(&self) -> Result<String, String> {
        Ok(format!(
            "Performance monitoring: {}. Local timing and token metrics. Saved for this profile; applies to existing and future sessions.\nLogs: {}\nCommands: /monitor on, /monitor off, /monitor status",
            if self.enabled()? { "ON" } else { "OFF" },
            self.directory().display(),
        ))
    }

    pub fn recorder(&self, session_id: String) -> LivePerformanceMetricRecorder {
        LivePerformanceMetricRecorder {
            monitor: self.clone(), session_id, started: Instant::now(),
            recorder: Mutex::new(None),
        }
    }
}

pub struct LivePerformanceMetricRecorder {
    monitor: PerformanceMonitor,
    session_id: String,
    started: Instant,
    recorder: Mutex<Option<LocalPerformanceMetricRecorder>>,
}

impl LivePerformanceMetricRecorder {
    pub async fn flush_pending(&self) {
        let flush = self.recorder.lock().unwrap_or_else(|e| e.into_inner())
            .as_ref().map(LocalPerformanceMetricRecorder::flush);
        if let Some(flush) = flush {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), flush).await;
        }
    }
}

impl PerformanceMetricRecorder for LivePerformanceMetricRecorder {
    fn session_id(&self) -> &str { &self.session_id }
    fn monotonic_now(&self) -> f64 { self.started.elapsed().as_secs_f64() * 1000.0 }
    fn next_id(&self, scope: PerformanceMetricIdScope) -> String {
        format!("{scope:?}-{}", uuid::Uuid::new_v4())
    }
    fn record(&self, event: PerformanceMetricEvent) {
        if !self.monitor.enabled().unwrap_or(false) { return; }
        let mut recorder = self.recorder.lock().unwrap_or_else(|e| e.into_inner());
        let recorder = recorder.get_or_insert_with(|| LocalPerformanceMetricRecorder::new(
            LocalPerformanceMetricRecorderOptions {
                directory: self.monitor.directory().to_string_lossy().into_owned(),
                session_id: self.session_id.clone(),
                file_io: Some(Arc::new(MonitorFileIo(self.monitor.clone()))),
                ..Default::default()
            },
        ));
        recorder.record(event);
    }
    fn flush(&self) {
        if let Some(recorder) = self.recorder.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            PerformanceMetricRecorder::flush(recorder);
        }
    }
    fn close(&self) {
        if let Some(recorder) = self.recorder.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            PerformanceMetricRecorder::close(recorder);
        }
    }
}

impl Drop for LivePerformanceMetricRecorder {
    fn drop(&mut self) { self.close(); }
}

// Recheck the shared preference when the timer drains previously buffered records.
// An append already in progress can finish after OFF; subsequent appends are suppressed.
struct MonitorFileIo(PerformanceMonitor);
impl PerformanceMetricFileIo for MonitorFileIo {
    fn mkdir(&self, path: &str) -> pi_ai::types::BoxFuture<Result<(), PerformanceMetricIoError>> {
        if !self.0.enabled().unwrap_or(false) { return Box::pin(async { Ok(()) }); }
        DefaultPerformanceMetricFileIo.mkdir(path)
    }
    fn append(&self, path: &str, data: &str) -> pi_ai::types::BoxFuture<Result<(), PerformanceMetricIoError>> {
        let monitor = self.0.clone();
        let path = path.to_string();
        let data = data.to_string();
        Box::pin(async move {
            if !monitor.enabled().unwrap_or(false) { return Ok(()); }
            DefaultPerformanceMetricFileIo.append(&path, &data).await
        })
    }
    fn size(&self, path: &str) -> pi_ai::types::BoxFuture<Result<Option<u64>, PerformanceMetricIoError>> {
        DefaultPerformanceMetricFileIo.size(path)
    }
    fn rename(&self, source: &str, destination: &str) -> pi_ai::types::BoxFuture<Result<(), PerformanceMetricIoError>> {
        if !self.0.enabled().unwrap_or(false) { return Box::pin(async { Ok(()) }); }
        DefaultPerformanceMetricFileIo.rename(source, destination)
    }
    fn remove(&self, path: &str) -> pi_ai::types::BoxFuture<Result<(), PerformanceMetricIoError>> {
        if !self.0.enabled().unwrap_or(false) { return Box::pin(async { Ok(()) }); }
        DefaultPerformanceMetricFileIo.remove(path)
    }
}

pub fn attach_performance_monitor(
    agent: &dyn super::agent_session::AgentHandle,
    agent_dir: &Path,
    session_id: String,
) {
    if agent.performance_metrics().is_none() {
        let recorder = PerformanceMonitor::from_environment(agent_dir).recorder(session_id);
        agent.set_performance_metrics(Some(
            pi_agent_core::performance_metrics::AgentLoopPerformanceMetrics::new(Arc::new(recorder)),
        ));
    }
}
