//! Private Python round trips for the kernel recorder and explicit legacy export.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use pi_coding_agent::core::kernel::performance_metrics::KernelPerformanceMetricAdapter;
use pi_coding_agent::core::kernel::repl_manager::{new_repl_kernel_manager, ReplKernelManager};
use pi_coding_agent::core::kernel::shared::{
    ExecuteOptions, ExecuteStatus, KernelClient, KernelManagerOptions, KernelRestoreOptions,
    KernelShutdownOptions, KernelSnapshotConfig,
};
use pi_coding_agent::core::kernel::state_snapshot::{
    cas_snapshot_root_in, manifest_path_in, snapshot_path_in, KernelRestoreSource,
    KernelSnapshotFormat, LegacyExportSource,
};
use pi_coding_agent::core::performance_monitor::{LivePerformanceMetricRecorder, PerformanceMonitor};

struct OwnedKernel(Arc<ReplKernelManager>);
impl Drop for OwnedKernel {
    fn drop(&mut self) { self.0.dispose_sync(); }
}

fn kernel(root: &Path, recorder: Option<Arc<LivePerformanceMetricRecorder>>) -> OwnedKernel {
    std::fs::create_dir_all(root).unwrap();
    let root_string = root.to_string_lossy();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../prime-agent-runtime/src");
    let mut env = HashMap::new();
    for key in ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "TEMP", "TMP", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"] {
        env.insert(key.into(), root_string.to_string());
    }
    env.insert("PYTHONPATH".into(), source.to_string_lossy().into_owned());
    env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
    env.insert("PYTHONNOUSERSITE".into(), "1".into());
    let python = std::env::var("PRIME_AGENT_KERNEL_PYTHON").expect("test requires an explicitly provisioned Python with dill; never bootstrap production");
    OwnedKernel(new_repl_kernel_manager(KernelManagerOptions {
        python: Some(python), cwd: Some(root_string.to_string()), env: Some(env),
        session_id: Some("private-kernel-metrics-export".into()),
        snapshot: Some(KernelSnapshotConfig {
            path: snapshot_path_in(&root_string), manifest_path: manifest_path_in(&root_string),
            cas_root_path: Some(cas_snapshot_root_in(&root_string)),
            format: Some(KernelSnapshotFormat::CasV2), debounce_ms: Some(60_000),
            ..Default::default()
        }),
        performance_metrics: recorder.map(|recorder| Arc::new(KernelPerformanceMetricAdapter::new(recorder)) as Arc<dyn pi_coding_agent::core::kernel::shared::PerformanceMetricRecorder>),
        stderr_log_path: Some(root.join("kernel.stderr.log").to_string_lossy().into_owned()),
        ..Default::default()
    }))
}

async fn execute(client: &ReplKernelManager, code: &str) -> String {
    let result = client.execute(code.into(), ExecuteOptions::default()).await.unwrap();
    assert_eq!(result.status, ExecuteStatus::Ok, "{} {:?}", result.stderr, result.error);
    result.stdout
}

async fn stop(client: &ReplKernelManager) {
    assert!(client.shutdown(KernelShutdownOptions { snapshot: false, drain_host_requests: true }).await.unwrap());
    assert!(!client.is_running());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_real_kernel_exports_current_previous_and_records_snapshot_metrics() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let root = tempfile::Builder::new().prefix("optimus-nine-kernel-").tempdir().unwrap();
        let state = root.path().join("state");
        let monitor = PerformanceMonitor::new(root.path().join("monitor"), true);
        let recorder = Arc::new(monitor.recorder("private-kernel-metrics-export".into()));
        let writer = kernel(&state, Some(recorder.clone()));
        assert!(writer.0.export_state_for_legacy_runtime(LegacyExportSource::Current).await.is_none());
        assert!(!Path::new(&snapshot_path_in(&state.to_string_lossy())).exists());
        execute(&writer.0, "legacy_probe_value = 'previous'\nsecret_namespace_marker = [1, 2, 3]").await;
        let first = writer.0.snapshot_state().await.expect("first CAS snapshot");
        assert_eq!(first.format, KernelSnapshotFormat::CasV2);
        execute(&writer.0, "legacy_probe_value = 'current'").await;
        let current = writer.0.snapshot_state().await.expect("second CAS snapshot");
        assert_ne!(first.generation, current.generation);
        execute(&writer.0, "legacy_probe_value = 'unsaved-must-not-export'").await;
        for (source, expected, generation) in [
            (LegacyExportSource::Current, "current", current.generation.as_ref().unwrap()),
            (LegacyExportSource::Previous, "previous", first.generation.as_ref().unwrap()),
        ] {
            let exported = writer.0.export_state_for_legacy_runtime(source).await.expect("supported export succeeds");
            assert!(exported.backward_readable);
            assert_eq!(&exported.source_generation, generation);
            assert!(exported.exported.contains(&"legacy_probe_value".into()));
            assert!(exported.bytes > 0);
            let legacy_root = root.path().join(expected);
            std::fs::create_dir_all(&legacy_root).unwrap();
            std::fs::copy(&exported.path, snapshot_path_in(&legacy_root.to_string_lossy())).unwrap();
            let reader = kernel(&legacy_root, None);
            let restored = reader.0.restore_state(KernelRestoreOptions { source: Some(KernelRestoreSource::Legacy) }).await.expect("legacy restore");
            assert!(restored.failed.is_empty(), "{:?}", restored.failed);
            assert!(restored.restored.contains(&"legacy_probe_value".into()));
            let value = execute(&reader.0, "print(legacy_probe_value)").await;
            assert_eq!(value.trim(), expected);
            stop(&reader.0).await;
        }
        stop(&writer.0).await;
        recorder.flush_pending().await;
        let mut records = Vec::new();
        for file in std::fs::read_dir(monitor.directory()).unwrap() {
            let contents = std::fs::read_to_string(file.unwrap().path()).unwrap();
            assert!(!contents.contains("secret_namespace_marker"));
            assert!(!contents.contains("unsaved-must-not-export"));
            records.extend(contents.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()));
        }
        let snapshots: Vec<_> = records.iter().filter(|event| event["operation"] == "snapshot").collect();
        assert_eq!(snapshots.len(), 2);
        for event in snapshots {
            assert_eq!(event["outcome"], "success");
            assert_eq!(event["correlation"]["sessionId"], "private-kernel-metrics-export");
            for key in ["total_ms", "queue_ms", "serialization_ms", "serialization_cpu_ms", "write_ms", "serialized_bytes", "written_bytes"] {
                assert!(event["measurements"][key].as_f64().is_some_and(|value| value >= 0.0), "missing {key}: {event}");
            }
            assert!(event["measurements"]["written_bytes"].as_f64().unwrap() > 0.0);
        }
    }).await.expect("private kernel round trips exceeded bounded deadline");
}
