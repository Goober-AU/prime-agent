use super::*;
use super::daemon_parity_tests::{ReportingSession, ScriptedDaemonFixture};

fn fixture(case: &str) -> ScriptedDaemonFixture {
    let root = daemon_parity_tests::state_root(case);
    let file = root.join("sessions/scripted.jsonl").to_string_lossy().into_owned();
    ScriptedDaemonFixture::new(case, ReportingSession::arc("active", "saved", &file, None, None))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn backlog_snapshot_capture_and_stream_keep_the_published_cursor() {
    let mut fixture = fixture("backlog-snapshot-capture");
    let entry = fixture.daemon.session_entry_for_state(&fixture.state);
    let saved: AgentMessage = serde_json::from_value(serde_json::json!({"role":"user", "content":"published", "timestamp":1})).unwrap();
    *entry.snapshot_boundary.lock().unwrap() = Some(PublishedTranscript {
        messages: Arc::new(vec![saved.clone()]), streaming_message: None, state_flags: (false, false, false), sequence: 5, generation: "captured".into(),
    });
    fixture.state.lock().unwrap().last_event_sequence = 99;
    let snapshot = fixture.daemon.create_session_snapshot(&fixture.state, false).await.unwrap();
    assert_eq!(snapshot["lastEventSequence"], 5);
    assert_eq!(snapshot["messages"][0]["content"], "published");
    let transcript = create_snapshot_transcript_chunks(CreateSnapshotTranscriptChunksOptions {
        active_session_id: fixture.active_session_id.clone(), snapshot_id: "captured-transfer".into(), messages: vec![saved], target_chunk_bytes: Some(SNAPSHOT_TARGET_CHUNK_BYTES), aborted: false,
    });
    let signal = mark_client_snapshot_streaming(&fixture.client, &fixture.active_session_id);
    fixture.daemon.stream_worker_snapshot(&fixture.client, &fixture.state, "captured-transfer", snapshot, 1, transcript, "attach", signal, true).await.unwrap();
    let mut end = None;
    while let Ok(bytes) = fixture.outbound.try_recv() {
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if value["type"] == "session_snapshot_end" { end = Some(value); }
    }
    let end = end.expect("successful terminal frame");
    assert_eq!(end["lastEventSequence"], 5, "streamer must not substitute the newer live counter");
    assert_eq!(end["lastEventCursor"]["generation"], "captured");
}

#[tokio::test]
async fn backlog_snapshot_replays_bounded_worker_frames_without_a_full_catchup() {
    let mut fixture = fixture("backlog-snapshot-frames");
    let signal = mark_client_snapshot_streaming(&fixture.client, &fixture.active_session_id);
    for sequence in [5, 6, 7] {
        let frame = DaemonOutbound::Raw(serde_json::json!({
            "type":"session_event", "activeSessionId":fixture.active_session_id,
            "event":{"type":"message_end", "message":{"role":"user", "content":sequence.to_string(), "timestamp":sequence}},
            "meta":{"sequence":sequence, "cursor":{"generation":"generation", "sequence":sequence}}
        }));
        assert!(fixture.daemon.defer_snapshot_frame(&fixture.client, &fixture.active_session_id, &frame));
    }
    assert!(!signal.is_cancelled());
    fixture.daemon.finish_snapshot_and_replay(&fixture.client, &fixture.active_session_id, 5, "generation", false);
    let mut sequences = Vec::new();
    while let Ok(bytes) = fixture.outbound.try_recv() {
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        sequences.push(value["meta"]["sequence"].as_u64().unwrap());
    }
    assert_eq!(sequences, vec![6, 7]);
    assert!(!fixture.client.snapshot_streaming());
    assert!(fixture.client.state.lock().unwrap().catchup_active_session_ids.as_ref().is_none_or(|ids| ids.is_empty()));
    assert!(fixture.client.deferred_snapshot_frames.lock().unwrap().is_empty());
}
