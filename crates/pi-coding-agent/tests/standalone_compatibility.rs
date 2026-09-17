use pi_coding_agent::{core::{performance_monitor::{PerformanceMonitor, MonitorCommand, parse_monitor_command}, model_reasoning_policy::apply_azure_reasoning_levels}, modes::{agents_view::{native_wire::{normalize_browser_numbers, is_background_only}, agents_view_state::{SessionSummary as ViewSummary, classify_agents_view_session, AgentsViewSection}}, daemon::daemon_session_list::SessionSummary, interactive::components::subagent_summary_line::{count_roster_subagent_statuses, count_direct_subagent_statuses, RosterParent}}};
use pi_agent_core::performance_metrics::{PerformanceMetricRecorder, PerformanceMetricEvent, PerformanceMetricOperation};
use pi_ai::types::Model;
use serde_json::json;

#[tokio::test]
async fn monitor_controls_actual_recording_and_existing_recorders() {
    let dir=tempfile::tempdir().unwrap();
    let monitor=PerformanceMonitor::new(dir.path(), false);
    let existing=monitor.recorder("test-session".into());
    let event=|| PerformanceMetricEvent::new(PerformanceMetricOperation::LogicalRequest);
    existing.record(event()); existing.flush_pending().await;
    assert!(!monitor.directory().exists());
    monitor.set_enabled(true).unwrap();
    assert!(monitor.status_text().unwrap().contains("ON"));
    existing.record(event()); existing.flush_pending().await;
    let files:Vec<_>=std::fs::read_dir(monitor.directory()).unwrap().map(|e|e.unwrap().path()).collect();
    assert_eq!(files.len(),1);
    let before=std::fs::read_to_string(&files[0]).unwrap();
    assert_eq!(before.lines().count(),1);
    assert_eq!(serde_json::from_str::<serde_json::Value>(before.lines().next().unwrap()).unwrap()["operation"],"logical_request");
    monitor.set_enabled(false).unwrap();
    existing.record(event()); existing.flush_pending().await;
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap(),before);
    assert!(!PerformanceMonitor::new(dir.path(),true).enabled().unwrap(),"explicit OFF overrides environment ON");
    monitor.set_enabled(true).unwrap();
    existing.record(event()); existing.flush_pending().await;
    assert_eq!(std::fs::read_to_string(&files[0]).unwrap().lines().count(),2);
    assert!(PerformanceMonitor::new(dir.path(),false).enabled().unwrap());
    existing.close();
}

#[test]
fn monitor_commands_and_invalid_preference_are_explicit() {
    assert_eq!(parse_monitor_command("").unwrap(),MonitorCommand::Select);
    assert_eq!(parse_monitor_command("on").unwrap(),MonitorCommand::Set(true));
    assert_eq!(parse_monitor_command("OFF").unwrap(),MonitorCommand::Set(false));
    assert_eq!(parse_monitor_command("status").unwrap(),MonitorCommand::Status);
    assert!(parse_monitor_command("unexpected").is_err());
    let dir=tempfile::tempdir().unwrap();
    let monitor=PerformanceMonitor::new(dir.path(),true);
    assert!(monitor.enabled().unwrap());
    std::fs::write(monitor.preference_path(),"broken").unwrap();
    assert!(monitor.enabled().is_err());
}

#[test]
fn azure_reasoning_levels_do_not_change_other_routes_or_context() {
    for provider in ["azure-openai-managed","azure-foundry-managed","azure-openai-responses"] {
        for id in ["gpt-6-astra","gpt-5.6-sol","FW-Kimi-K3","FW-GLM-5.3"] {
            let mut model=Model::new(id,id,"openai-responses",provider,"https://unused.invalid");
            model.context_window=1_050_000.0; model.max_tokens=128_000.0;
            apply_azure_reasoning_levels(&mut model);
            for level in ["low","high","max"] {assert_eq!(model.thinking_level_map_get(level),Some(Some(level.into())));}
            for level in ["medium","xhigh"] {assert_eq!(model.thinking_level_map_get(level),Some(id.starts_with("gpt-").then(|| level.into())));}
            assert_eq!(model.thinking_level_map_get("off"),Some(None));
            assert_eq!(model.context_window,1_050_000.0); assert_eq!(model.max_tokens,128_000.0);
        }
    }
    for provider in ["openai-codex","github-copilot","ollama"] {
        let mut model=Model::new("gpt-6-astra","astra","openai-responses",provider,"");
        let before=model.clone(); apply_azure_reasoning_levels(&mut model); assert_eq!(model,before);
    }
}

#[test]
fn background_helper_is_not_active_agent_work_and_backend_is_unchanged() {
    // Older summaries have no explicit helper label; normalize only the UI copy.
    let raw=json!({"id":"child","sessionId":"child-session","cwd":"C:/isolated","lifecycle":"live","runtimeKind":"subagent","activeSessionId":"child-active","parentSessionId":"parent","isSessionActive":true,"activity":"idle","isStreaming":false,"isCompacting":false,"isRunningTools":false,"attachedClients":0.0,"messageCount":5.0,"rosterStatus":"running"});
    assert!(is_background_only(&raw));
    let view=normalize_browser_numbers(raw.clone());
    assert_eq!(raw["rosterStatus"],"running"); assert_eq!(raw["isSessionActive"],true);
    assert_eq!(view["statusLabel"],"background helper");
    let browser:ViewSummary=serde_json::from_value(view.clone()).unwrap();
    assert_eq!(classify_agents_view_session(&browser),AgentsViewSection::Idle);
    // Current daemon summaries already carry the authoritative classification.
    // The legacy heuristic intentionally leaves any explicit status label alone.
    let mut authoritative=raw.clone(); authoritative["statusLabel"]=json!("background helper"); authoritative["rosterStatus"]=json!("idle");
    assert!(!is_background_only(&authoritative));
    let authoritative_view=normalize_browser_numbers(authoritative.clone());
    assert_eq!(authoritative["isSessionActive"],true);
    assert_eq!(authoritative_view["statusLabel"],"background helper");
    assert_eq!(classify_agents_view_session(&serde_json::from_value::<ViewSummary>(authoritative_view.clone()).unwrap()),AgentsViewSection::Idle);
    use pi_coding_agent::modes::agents_view::roster_store::{AgentRosterEntry,session_summary_from_roster_entry};
    let entry:AgentRosterEntry=serde_json::from_value(normalize_browser_numbers(json!({"agentId":"child-active","status":"running","summary":raw.clone()}))).unwrap();
    assert_eq!(classify_agents_view_session(&session_summary_from_roster_entry(&entry)),AgentsViewSection::Idle);
    let recovering:AgentRosterEntry=serde_json::from_value(normalize_browser_numbers(json!({"agentId":"child-active","status":"running","statusLabel":"recovering","summary":raw.clone()}))).unwrap();
    assert_eq!(classify_agents_view_session(&session_summary_from_roster_entry(&recovering)),AgentsViewSection::Running);
    assert_eq!(serde_json::to_value(&recovering.summary).unwrap()["statusLabel"],"recovering");
    let child:SessionSummary=serde_json::from_value(view).unwrap();
    let counts=count_roster_subagent_statuses([&child],&RosterParent {session_id:Some("parent".into()),..Default::default()});
    assert_eq!(counts.running,0); assert_eq!(counts.background,1); assert_eq!(counts.total,1);
    let authoritative_child:SessionSummary=serde_json::from_value(authoritative_view).unwrap();
    let authoritative_counts=count_roster_subagent_statuses([&authoritative_child],&RosterParent {session_id:Some("parent".into()),..Default::default()});
    assert_eq!(authoritative_counts.running,0); assert_eq!(authoritative_counts.background,1); assert_eq!(authoritative_counts.total,1);
    let mut working=raw.clone(); working["activity"]=json!("working"); working.as_object_mut().unwrap().remove("statusLabel");
    assert!(!is_background_only(&working),"foreground work must not be mistaken for a background helper");
    let working_view:ViewSummary=serde_json::from_value(normalize_browser_numbers(working)).unwrap();
    assert_eq!(classify_agents_view_session(&working_view),AgentsViewSection::Running);
    for (key,value) in [("isStreaming",json!(true)),("isCompacting",json!(true)),("isRunningTools",json!(true)),("isBashRunning",json!(true)),("hasRunningRlmChildren",json!(true)),("workerState",json!("recovering")),("sessionActions",json!({"active":{"id":"a"}})),("sessionActions",json!({"queuedCount":1}))] {
        let mut busy=raw.clone(); busy[key]=value; assert!(!is_background_only(&busy),"{key}");
        assert_eq!(normalize_browser_numbers(busy)["rosterStatus"],"running");
    }
}

#[test]
fn terminal_child_waiting_is_not_claimed_running() {
    use pi_coding_agent::modes::agent_connection::types::{AgentConnectionRlmChildAgentSnapshot,AgentConnectionRlmChildAgentActivity};
    let child=AgentConnectionRlmChildAgentSnapshot {id:"child".into(), parent_id:Some("parent".into()), status:"done".into(),activity:Some(AgentConnectionRlmChildAgentActivity {kind:"waiting".into(),..Default::default()}),..Default::default()};
    let counts=count_direct_subagent_statuses([&child],Some("parent"));
    assert_eq!(counts.running,0); assert_eq!(counts.waiting,1);
}
