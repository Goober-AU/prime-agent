//! Offline regressions for CORE-003 and CORE-005. No real worker is launched.
use super::*;
use super::daemon_supervisor_parity_tests::{add_descriptor_only_worker, SupervisorFixture};
use crate::modes::daemon::worker_recovery_journal::{WorkerRecoveryJournal, WorkerRecoveryRecordInput};

fn stopped_worker(fixture: &SupervisorFixture, id: &str) -> Arc<Worker> {
    let worker = add_descriptor_only_worker(fixture, id, id, "isolated-token", DAEMON_WORKER_LIFECYCLE_STOPPING);
    {
        let mut descriptor = worker.descriptor.lock().unwrap();
        descriptor.pid = i32::MAX;
        descriptor.process_start_id = Some("missing-process".into());
        descriptor.stop_requested_at = Some("2026-09-17T00:00:00Z".into());
        descriptor.archive_on_stop = Some(false);
        fixture.supervisor.persist_worker(&descriptor).unwrap();
    }
    worker
}

#[tokio::test]
async fn core_supervisor_unrelated_opens_progress_and_same_session_create_retry_join() {
    let fixture = SupervisorFixture::new("core-independent-opens").await;
    let worker = stopped_worker(&fixture, "same-chat");
    let path = fixture.root.join("saved.jsonl").to_string_lossy().into_owned();
    {
        let mut descriptor = worker.descriptor.lock().unwrap();
        descriptor.session_file = Some(path.clone());
        descriptor.stop_requested_at = None;
    }
    let key = canonical_session_path(&path);
    let entered = CancellationToken::new();
    let released = CancellationToken::new();
    let opener = {
        let supervisor = fixture.supervisor.clone();
        let entered = entered.clone(); let released = released.clone();
        tokio::spawn(async move {
            supervisor.with_worker_open(key, async {
                entered.cancel(); released.cancelled().await;
                Ok(json!({"activeSessionId":"shared-open"}))
            }).await
        })
    };
    entered.cancelled().await;
    let create_body = json!({"type":"create", "sessionPath":path}).as_object().unwrap().clone();
    let mut joined_create = Box::pin(fixture.supervisor.create_for_owner("owner".into(), &create_body));
    let mut joined_retry = Box::pin(fixture.supervisor.retry_worker("owner".into(), "same-chat"));
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut joined_create).await.is_err());
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut joined_retry).await.is_err());

    // This real create reaches config validation, not process launch. It must not
    // wait behind the unrelated worker's deliberately stalled create operation.
    let unrelated = json!({"type":"create", "config":"invalid-config"}).as_object().unwrap().clone();
    let error = tokio::time::timeout(Duration::from_secs(1), fixture.supervisor.create_for_owner("owner".into(), &unrelated)).await.expect("unrelated open blocked").unwrap_err();
    assert!(error.contains("invalid type"), "{error}");
    assert!(fixture.supervisor.opening.try_write().is_err(), "eviction must still exclude an open");
    released.cancel();
    let expected = opener.await.unwrap().unwrap();
    assert_eq!(joined_create.await.unwrap(), expected);
    assert_eq!(joined_retry.await.unwrap(), expected);
    assert!(fixture.supervisor.opening_workers.lock().unwrap().is_empty());
    assert!(fixture.supervisor.opening.try_write().is_ok());
    fixture.supervisor.stopped.cancel();
}

#[tokio::test]
async fn core_supervisor_cancelled_open_releases_key_and_joiners() {
    let fixture = SupervisorFixture::new("core-cancelled-open").await;
    let entered = CancellationToken::new();
    let opener = {
        let supervisor = fixture.supervisor.clone(); let entered = entered.clone();
        tokio::spawn(async move {
            supervisor.with_worker_open("same".into(), async {
                entered.cancel(); std::future::pending::<Result<Value, String>>().await
            }).await
        })
    };
    entered.cancelled().await;
    let mut joiner = Box::pin(fixture.supervisor.with_worker_open("same".into(), async { panic!("joiner must not launch") }));
    assert!(tokio::time::timeout(Duration::from_millis(20), &mut joiner).await.is_err());
    opener.abort(); assert!(opener.await.unwrap_err().is_cancelled());
    assert!(tokio::time::timeout(Duration::from_secs(1), joiner).await.unwrap().unwrap_err().contains("interrupted"));
    assert!(fixture.supervisor.opening_workers.lock().unwrap().is_empty());
    assert_eq!(fixture.supervisor.with_worker_open("same".into(), async { Ok(json!("retry")) }).await.unwrap(), json!("retry"));
    fixture.supervisor.stopped.cancel();
}

#[tokio::test]
async fn core_supervisor_late_create_never_revives_stopped_or_replaced_generation() {
    let fixture = SupervisorFixture::new("core-late-create").await;
    let worker = stopped_worker(&fixture, "generation");
    let expected = worker.descriptor.lock().unwrap().clone();
    let descriptor_file = fixture.supervisor.descriptor_dir.join("generation.json");
    let stopped_bytes = std::fs::read(&descriptor_file).unwrap();
    assert!(fixture.supervisor.mark_created_worker_ready(&worker, &expected, &json!({"sessionId":"late"})).is_err());
    fixture.supervisor.fail_created_worker(&worker, &expected, "late reply");
    assert_eq!(worker.descriptor.lock().unwrap().lifecycle, DAEMON_WORKER_LIFECYCLE_STOPPING);
    assert_eq!(std::fs::read(&descriptor_file).unwrap(), stopped_bytes);
    let replacement = stopped_worker(&fixture, "generation");
    let replacement_bytes = std::fs::read(&descriptor_file).unwrap();
    assert!(fixture.supervisor.mark_created_worker_ready(&worker, &expected, &json!({"sessionId":"late"})).is_err());
    fixture.supervisor.fail_created_worker(&worker, &expected, "retired open");
    assert_eq!(replacement.descriptor.lock().unwrap().lifecycle, DAEMON_WORKER_LIFECYCLE_STOPPING);
    assert_eq!(std::fs::read(&descriptor_file).unwrap(), replacement_bytes);
    assert!(Arc::ptr_eq(fixture.supervisor.workers.lock().unwrap().get("generation").unwrap(), &replacement));
    fixture.supervisor.stopped.cancel();
}

#[tokio::test]
async fn core_supervisor_permanent_stop_failure_parks_and_survives_adoption() {
    let fixture = SupervisorFixture::new("core-permanent-cleanup").await;
    let worker = stopped_worker(&fixture, "broken");
    let journal = worker.descriptor.lock().unwrap().recovery_journal_path.clone();
    WorkerRecoveryJournal::new(&journal).record(WorkerRecoveryRecordInput {
        active_session_id: "missing-child".into(), session_id: "missing-child".into(),
        session_file: None, busy: true, operation: "tool_call".into(),
    });
    let before = std::fs::read(&journal).unwrap();
    let descriptor = worker.descriptor.lock().unwrap().clone();
    stop_finalizations().lock().unwrap().insert(descriptor.worker_id.clone());
    tokio::time::timeout(Duration::from_secs(2), finalize_timed_out_worker_stop(fixture.supervisor.clone(), worker.clone(), descriptor)).await.expect("permanent cleanup spun");
    assert!(stop_cleanup_is_parked(&worker.descriptor.lock().unwrap()));
    assert!(!stop_finalizations().lock().unwrap().contains("broken"));
    assert_eq!(std::fs::read(&journal).unwrap(), before);
    let persisted: DaemonWorkerDescriptor = serde_json::from_slice(&std::fs::read(fixture.supervisor.descriptor_dir.join("broken.json")).unwrap()).unwrap();
    assert!(stop_cleanup_is_parked(&persisted));
    assert!(fixture.supervisor.reclaim_stale_worker_registration(&worker).await.unwrap_err().contains("parked"));
    fixture.supervisor.workers.lock().unwrap().clear();
    fixture.supervisor.adopt_workers().await.unwrap();
    let adopted = fixture.supervisor.workers.lock().unwrap().get("broken").unwrap().clone();
    assert!(stop_cleanup_is_parked(&adopted.descriptor.lock().unwrap()));
    assert!(!stop_finalizations().lock().unwrap().contains("broken"));
    assert_eq!(std::fs::read(&journal).unwrap(), before);
    fixture.supervisor.stopped.cancel();
}

#[tokio::test]
async fn core_supervisor_transient_stop_failure_has_finite_retry_budget() {
    let fixture = SupervisorFixture::new("core-transient-cleanup").await;
    let worker = stopped_worker(&fixture, "transient");
    let unreadable = fixture.root.join("orphan-journal-is-directory");
    std::fs::create_dir(&unreadable).unwrap();
    worker.descriptor.lock().unwrap().orphan_process_journal_path = Some(unreadable.to_string_lossy().into_owned());
    let descriptor = worker.descriptor.lock().unwrap().clone();
    stop_finalizations().lock().unwrap().insert(descriptor.worker_id.clone());
    tokio::time::timeout(Duration::from_secs(20), finalize_timed_out_worker_stop(fixture.supervisor.clone(), worker.clone(), descriptor)).await.expect("transient cleanup retries were unbounded");
    assert!(stop_cleanup_is_parked(&worker.descriptor.lock().unwrap()));
    assert!(fixture.supervisor.descriptor_dir.join("transient.json").exists());
    assert!(unreadable.exists());
    assert!(!stop_finalizations().lock().unwrap().contains("transient"));
    fixture.supervisor.stopped.cancel();
}

#[test]
fn core_supervisor_stop_retry_policy_distinguishes_invariants_from_io_errors() {
    for error in ["Uncertain operation has no saved transcript; recovery journal retained", "Malformed orphan record; recovery journal retained", "Unverified orphan record; recovery journal retained"] {
        assert!(!stop_cleanup_should_retry(error, 1));
    }
    for attempts in 1..STOP_CLEANUP_MAX_ATTEMPTS { assert!(stop_cleanup_should_retry("sharing violation", attempts)); }
    assert!(!stop_cleanup_should_retry("sharing violation", STOP_CLEANUP_MAX_ATTEMPTS));
}

#[tokio::test]
async fn core_supervisor_recovery_parking_preserves_replacement_descriptor() {
    let fixture = SupervisorFixture::new("core-recovery-park-generation").await;
    let retired = stopped_worker(&fixture, "park-generation");
    let replacement = stopped_worker(&fixture, "park-generation");
    let path = fixture.supervisor.descriptor_dir.join("park-generation.json");
    let before = std::fs::read(&path).unwrap();
    fixture.supervisor.park_worker_recovery_failure(&retired, "late recovery");
    fixture.supervisor.park_worker_stop_cleanup_failure(&retired, "late stop");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(Arc::ptr_eq(fixture.supervisor.workers.lock().unwrap().get("park-generation").unwrap(), &replacement));
    fixture.supervisor.stopped.cancel();
}
