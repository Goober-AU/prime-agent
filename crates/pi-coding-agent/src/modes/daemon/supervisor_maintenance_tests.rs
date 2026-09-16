//! Private-profile regressions for recovery, heartbeat invalidation and large attaches.
use super::*;
use super::daemon_supervisor_parity_tests::{add_descriptor_only_worker, SupervisorFixture};
use crate::core::cron_jobs::{AgentCronJobStore, CreateAgentCronJobInput, STATUS_CANCELLED, SESSION_SCHEDULED_JOBS_FILENAME};
use crate::modes::daemon::worker_recovery_journal::{WorkerRecoveryJournal, WorkerRecoveryRecordInput};

fn dead_worker(fixture: &SupervisorFixture, name: &str) -> Arc<Worker> {
    let worker = add_descriptor_only_worker(fixture, name, name, "token", DAEMON_WORKER_LIFECYCLE_FAILED);
    { let mut descriptor = worker.descriptor.lock().unwrap(); descriptor.pid = i32::MAX; descriptor.process_start_id = Some("dead-generation".into()); }
    fixture.supervisor.persist_worker(&worker.descriptor.lock().unwrap()).unwrap();
    worker
}

fn session(fixture: &SupervisorFixture, worker: &Arc<Worker>, name: &str) -> (String, String) {
    let path = fixture.root.join("sessions").join(format!("{name}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("{}\n", json!({"type":"session","id":name,"version":3,"timestamp":"2026-01-01T00:00:00Z","cwd":fixture.root.join("workspace")}))).unwrap();
    let file = path.to_string_lossy().into_owned();
    let artifacts = crate::core::session_manager::get_session_artifact_path_for_file(&file, Some(name));
    std::fs::create_dir_all(&artifacts).unwrap();
    { let mut descriptor = worker.descriptor.lock().unwrap(); descriptor.root_session_id = Some(name.into()); descriptor.session_file = Some(file.clone()); }
    fixture.supervisor.persist_worker(&worker.descriptor.lock().unwrap()).unwrap();
    (file, artifacts)
}

async fn catalog_recorder(fixture: &SupervisorFixture) -> PathBuf {
    let receipts = fixture.root.join("catalog-receipts.jsonl");
    let script = fixture.root.join("catalog.py");
    let code = format!("import json,sys\nf=open({},'a',encoding='utf8')\nprint(json.dumps({{'type':'ready'}}),flush=True)\nfor line in sys.stdin:\n r=json.loads(line)\n f.write(json.dumps(r)+'\\n'); f.flush()\n print(json.dumps({{'type':'response','id':r['id'],'success':True,'data':{{'archived':True}}}}),flush=True)\n", serde_json::to_string(&receipts.to_string_lossy()).unwrap());
    std::fs::write(&script, code).unwrap();
    fixture.supervisor.catalog.start(if cfg!(windows) { "python" } else { "python3" }, vec![script.to_string_lossy().into_owned()], Vec::new()).await.unwrap();
    receipts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_dead_worker_invalidates_only_its_input_fences() {
    let fixture = SupervisorFixture::new("nine-input-fence").await;
    let dead = dead_worker(&fixture, "dead");
    let other = dead_worker(&fixture, "other");
    fixture.supervisor.pauses.lock().unwrap().insert("old".into(), InputPause { connection_id: fixture.client.connection_id.clone(), worker: dead.clone(), active: "dead".into(), requested: "dead".into() });
    fixture.supervisor.pauses.lock().unwrap().insert("other".into(), InputPause { connection_id: "another-client".into(), worker: other, active: "other".into(), requested: "other".into() });
    let connection = Arc::new(DaemonWorkerClient::new("unused-private-test-socket"));
    *dead.client.lock().unwrap() = Some(connection.clone());
    dead.descriptor.lock().unwrap().lifecycle = DAEMON_WORKER_LIFECYCLE_STOPPING.into();
    fixture.supervisor.handle_worker_close(&dead, &connection, "simulated disconnect").await;
    assert!(fixture.client.stopped.is_cancelled());
    assert_eq!(fixture.client.pause_epoch.load(Ordering::SeqCst), 1);
    let pauses = fixture.supervisor.pauses.lock().unwrap();
    assert!(!pauses.contains_key("old"));
    assert!(pauses.contains_key("other"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_stale_reclaim_preserves_transcript_and_rejects_live_identity() {
    let fixture = SupervisorFixture::new("nine-reclaim").await;
    let worker = dead_worker(&fixture, "dead");
    let (file, _) = session(&fixture, &worker, "saved");
    let before = std::fs::read(&file).unwrap();
    assert!(fixture.supervisor.reclaim_stale_worker_registration(&worker).await.unwrap());
    assert_eq!(std::fs::read(&file).unwrap(), before);
    assert!(!fixture.supervisor.descriptor_dir.join("dead.json").exists());
    assert!(!fixture.supervisor.workers.lock().unwrap().contains_key("dead"));
    let live = add_descriptor_only_worker(&fixture, "live", "live", "token-live", DAEMON_WORKER_LIFECYCLE_FAILED);
    live.descriptor.lock().unwrap().process_start_id = None;
    assert!(!fixture.supervisor.reclaim_stale_worker_registration(&live).await.unwrap());
    assert!(fixture.supervisor.descriptor_dir.join("live.json").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_uncertain_work_is_marked_interrupted_not_replayed() {
    let fixture = SupervisorFixture::new("nine-uncertain").await;
    let worker = dead_worker(&fixture, "dead");
    let (file, _) = session(&fixture, &worker, "saved");
    let receipts = catalog_recorder(&fixture).await;
    let path = worker.descriptor.lock().unwrap().recovery_journal_path.clone();
    WorkerRecoveryJournal::new(&path).record(WorkerRecoveryRecordInput { active_session_id: "dead".into(), session_id: "saved".into(), session_file: Some(file.clone()), busy: true, operation: "tool_call".into() });
    fixture.supervisor.recover_uncertain_worker_operations(&worker).await.unwrap();
    let calls: Vec<Value> = std::fs::read_to_string(receipts).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["command"], "mark_interrupted");
    assert_eq!(calls[0]["operations"], json!(["tool_call"]));
    let records = WorkerRecoveryJournal::read_latest(&path);
    assert_eq!(records.len(), 1); assert!(!records[0].busy); assert_eq!(records[0].operation, "recovery_hold");
    fixture.supervisor.recover_uncertain_worker_operations(&worker).await.unwrap();
    fixture.supervisor.catalog.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_orphan_cleanup_never_signals_reused_pid() {
    let fixture = SupervisorFixture::new("nine-orphan-identity").await;
    let worker = dead_worker(&fixture, "dead");
    let journal = fixture.root.join("orphans.jsonl");
    std::fs::write(&journal, format!("{}\n", json!({"version":1,"ownerPid":i32::MAX,"pid":std::process::id(),"processStartId":"not-this-generation","active":true,"recordedAt":"now"}))).unwrap();
    worker.descriptor.lock().unwrap().orphan_process_journal_path = Some(journal.to_string_lossy().into_owned());
    fixture.supervisor.recover_uncertain_worker_operations(&worker).await.unwrap();
    assert!(!journal.exists());
    assert!(get_process_start_id(std::process::id() as i64).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_unknown_or_malformed_orphans_preserve_cleanup_evidence() {
    let fixture = SupervisorFixture::new("nine-orphan-evidence").await;
    let worker = dead_worker(&fixture, "dead");
    let journal = fixture.root.join("orphans.jsonl");
    worker.descriptor.lock().unwrap().orphan_process_journal_path = Some(journal.to_string_lossy().into_owned());
    let identity_free = json!({"version":1,"ownerPid":i32::MAX,"pid":std::process::id(),"active":true,"recordedAt":"now"});
    for bytes in [format!("{identity_free}\n"), "{\"version\":1,\"pid\":".into(), format!("{}\n", json!({"version":1,"ownerPid":i32::MAX,"pid":std::process::id(),"processStartId":17,"active":true,"recordedAt":"now"}))] {
        std::fs::write(&journal, &bytes).unwrap();
        let error = fixture.supervisor.recover_uncertain_worker_operations(&worker).await.unwrap_err();
        assert!(error.contains("journal retained"), "{error}");
        assert_eq!(std::fs::read_to_string(&journal).unwrap(), bytes);
        assert!(get_process_start_id(std::process::id() as i64).is_some());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_one_broken_recovery_does_not_abort_daemon_adoption() {
    let fixture = SupervisorFixture::new("nine-adoption-isolation").await;
    let broken = dead_worker(&fixture, "broken");
    let (file, _) = session(&fixture, &broken, "broken-chat");
    let before = std::fs::read(&file).unwrap();
    let journal = fixture.root.join("unrecoverable-orphan.jsonl");
    std::fs::write(&journal, b"crash-truncated-append").unwrap();
    broken.descriptor.lock().unwrap().orphan_process_journal_path = Some(journal.to_string_lossy().into_owned());
    fixture.supervisor.persist_worker(&broken.descriptor.lock().unwrap()).unwrap();
    let unrelated = dead_worker(&fixture, "unrelated");
    fixture.supervisor.workers.lock().unwrap().clear();
    fixture.supervisor.adopt_workers().await.expect("one corrupt chat must not reject supervisor startup");
    let workers = fixture.supervisor.workers.lock().unwrap();
    let parked = workers.get("broken").unwrap().descriptor.lock().unwrap();
    assert_eq!(parked.lifecycle, DAEMON_WORKER_LIFECYCLE_FAILED);
    assert!(parked.last_error.as_deref().unwrap().contains("Malformed orphan record"));
    assert!(workers.contains_key(&unrelated.descriptor.lock().unwrap().worker_id));
    assert_eq!(std::fs::read(&journal).unwrap(), b"crash-truncated-append");
    assert_eq!(std::fs::read(file).unwrap(), before);
    drop(parked); drop(workers);
    assert_eq!(fixture.hello()["type"], "daemon_hello");
    assert!(!fixture.supervisor.stopped.is_cancelled());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_cancel_failure_retains_tombstone_until_restart_retry() {
    let fixture = SupervisorFixture::new("nine-cancel-retry").await;
    let worker = dead_worker(&fixture, "owned");
    let (file, artifacts) = session(&fixture, &worker, "owned-session");
    { let mut descriptor = worker.descriptor.lock().unwrap(); descriptor.owner_client_id = Some("gone-client".into()); descriptor.stop_requested_at = Some("now".into()); }
    fixture.supervisor.persist_worker(&worker.descriptor.lock().unwrap()).unwrap();
    let jobs_path = Path::new(&artifacts).join(SESSION_SCHEDULED_JOBS_FILENAME);
    std::fs::write(&jobs_path, b"broken-json").unwrap();
    fixture.supervisor.workers.lock().unwrap().remove("owned");
    let excluded = fixture.supervisor.settle_ephemeral_cancel_intents().await;
    assert!(excluded.contains(&canonical_session_path(&file)));
    assert!(fixture.supervisor.descriptor_dir.join("owned.json").exists());
    assert_eq!(std::fs::read(&jobs_path).unwrap(), b"broken-json");
    std::fs::write(&jobs_path, b"{\"jobs\":[],\"dispatches\":[]}").unwrap();
    let store = AgentCronJobStore::for_session_artifacts();
    store.register_session_artifact("owned-session", &artifacts);
    let job = store.create(&CreateAgentCronJobInput { active_session_id: "owned".into(), session_id: "owned-session".into(), session_file: file.clone(), cwd: fixture.root.to_string_lossy().into_owned(), prompt: "must not wake".into(), schedule_text: "every 1m".into(), now: Some(supervisor_now_ms() as f64), ..Default::default() }).unwrap();
    fixture.supervisor.settle_ephemeral_cancel_intents().await;
    assert!(!fixture.supervisor.descriptor_dir.join("owned.json").exists());
    assert_eq!(store.list().iter().find(|item| item.id == job.id).unwrap().status, STATUS_CANCELLED);
    fixture.supervisor.stopped.cancel();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_cached_heartbeats_survive_disconnect_but_not_change() {
    let mut fixture = SupervisorFixture::new("nine-heartbeat-cache").await;
    let worker = add_descriptor_only_worker(&fixture, "worker", "active", "token", DAEMON_WORKER_LIFECYCLE_READY);
    worker.heartbeat_snapshot.lock().unwrap().store_if_current(0, vec![json!({"job":{"id":"heartbeat"}})]);
    let reply = fixture.send(json!({"type":"heartbeats_list","id":"cached"})).await;
    assert_eq!(reply["success"], true, "{reply}");
    assert_eq!(reply["data"]["heartbeats"][0]["job"]["id"], "heartbeat");
    fixture.supervisor.handle_worker_frame(&worker, &PrivateFrame { header: json!({"kind":"outbound","outboundType":"heartbeats_changed"}), payload: b"{}".to_vec() }, None);
    assert_eq!(fixture.last_frame()["type"], "heartbeats_changed");
    let reply = fixture.send(json!({"type":"heartbeats_list","id":"stale"})).await;
    assert_eq!(reply["success"], false, "stale cache must not conceal missing heartbeat truth: {reply}");
    fixture.supervisor.stopped.cancel();
}

#[test]
fn nine_supervisor_heartbeat_refresh_cannot_erase_concurrent_invalidation() {
    let mut cache = HeartbeatSnapshot::default();
    cache.store_if_current(0, vec![json!({"job":{"id":"old"}})]);
    let epoch = cache.epoch;
    cache.invalidate();
    cache.store_if_current(epoch, vec![json!({"job":{"id":"stale-result"}})]);
    assert!(cache.fresh().is_none());
    cache.store_if_current(cache.epoch, vec![json!({"job":{"id":"new"}})]);
    assert_eq!(cache.fresh().unwrap()[0]["job"]["id"], "new");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_large_attach_spills_and_preserves_all_messages() {
    let mut fixture = SupervisorFixture::new("nine-large-snapshot").await;
    let messages: Vec<Value> = (0..220).map(|index| json!({"role":"user","content":format!("{index}:{}", "x".repeat(24_000)),"timestamp":index})).collect();
    let expected = messages.clone();
    let summary = SessionSummary { id: "active".into(), session_id: "saved".into(), ..Default::default() };
    let state = crate::modes::agent_connection::types::AgentConnectionState::default();
    let mut response = DaemonResponse::success(Some("attach"), "attach", Some(json!({"activeSessionId":"active","snapshot":{"activeSessionId":"active","messages":messages,"summary":summary,"state":state,"lastEventSequence":7,"lastEventCursor":{"generation":"generation","sequence":7}}})));
    let supervisor = fixture.supervisor.clone(); let client = fixture.client.clone();
    let task = tokio::spawn(async move { supervisor.stream_cached_attach(&client, "active", &mut response).await });
    let first = fixture.next_frame(Duration::from_secs(10)).await.unwrap();
    assert_eq!(first["type"], "response");
    assert_eq!(first["data"]["snapshot"]["messages"], json!([]));
    let snapshot_id = first["data"]["snapshotStream"]["id"].as_str().unwrap().to_string();
    assert!(fixture.supervisor.descriptor_dir.join("snapshot-cache").join(&snapshot_id).exists(), "more than 4 MiB must be disk-backed while the reader is held");
    let mut frames = vec![first];
    let mut decoded = Vec::new(); let mut chunk_index = 0;
    loop {
        let frame = fixture.next_frame(Duration::from_secs(10)).await.unwrap();
        frames.push(frame.clone());
        match frame["type"].as_str().unwrap() {
            "session_snapshot_begin" => assert_eq!(frame["messageCount"], 220),
            "session_snapshot_chunk" => { assert_eq!(frame["index"], chunk_index); chunk_index += 1; decoded.extend(frame["messages"].as_array().unwrap().clone()); },
            "session_snapshot_end" => { assert_eq!(frame["chunkCount"], chunk_index); assert_eq!(frame["lastEventSequence"], 7); break; },
            other => panic!("unexpected snapshot record {other}"),
        }
    }
    task.await.unwrap().unwrap();
    assert_eq!(decoded, expected);
    let client_messages = crate::modes::agent_connection::daemon_agent_connection::test_decode_cached_attach_frames(&frames).await;
    let expected_messages: Vec<pi_agent_core::types::AgentMessage> = serde_json::from_value(json!(expected)).unwrap();
    assert_eq!(client_messages, expected_messages);
    assert!(!fixture.supervisor.descriptor_dir.join("snapshot-cache").join(snapshot_id).exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nine_supervisor_disconnected_snapshot_releases_disk_cache() {
    let fixture = SupervisorFixture::new("nine-disconnect-snapshot").await;
    fixture.client.stopped.cancel();
    let mut response = DaemonResponse::success(None, "attach", Some(json!({"snapshot":{"messages":[{"role":"user","content":"x".repeat(5 * 1024 * 1024)}],"lastEventSequence":0}})));
    fixture.supervisor.stream_cached_attach(&fixture.client, "active", &mut response).await.unwrap();
    assert_eq!(std::fs::read_dir(fixture.supervisor.descriptor_dir.join("snapshot-cache")).unwrap().count(), 0);
}
