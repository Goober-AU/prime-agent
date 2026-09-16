use std::{cell::{Cell, RefCell}, rc::Rc};
use pi_coding_agent::{core::{auth_storage::{AuthStorage, AuthStorageOptions}, model_registry::ModelRegistry}, modes::interactive::components::custom_editor::{CustomEditor, CustomEditorOptions}};
use pi_tui::{components::{editor::EditorTheme, select_list::SelectListTheme}, terminal::ProcessTerminal, tui::{TUI, Component}};

use pi_coding_agent::modes::agents_view::native_wire;

#[test]
fn browser_accepts_real_catalogue_dates() {
    use pi_coding_agent::modes::{agents_view::agents_view_mode::deserialize_saved_session_info,
        daemon::saved_session_info::serialize_saved_session_info};
    let session=pi_coding_agent::core::session_manager::SessionInfo {
        id:"copied-chat".into(),path:"C:/isolated/sessions/copied-chat.jsonl".into(),
        cwd:"C:/isolated".into(),name:Some("Copied chat".into()),state:None,
        parent_session_path:None,rlm_depth:0,created:1_700_000_000_000.0,
        modified:1_700_000_001_000.0,message_count:2,first_message:"hello".into(),
        all_messages_text:"hello".into(),agent_status:None,usage:None,
    };
    let wire=serde_json::to_value(serialize_saved_session_info(&session)).unwrap();
    let parsed=deserialize_saved_session_info(&native_wire::normalize_browser_numbers(wire.clone())).expect("real catalogue row must not disappear");
    assert_eq!(parsed.id,session.id);
    assert_eq!(parsed.created.timestamp_millis() as f64,session.created);
    assert_eq!(parsed.modified.timestamp_millis() as f64,session.modified);
    let local=serde_json::to_value(&parsed).unwrap();
    assert_eq!(deserialize_saved_session_info(&local).unwrap(),parsed);
    let mut invalid=wire;
    invalid["created"]=serde_json::json!("not a date");
    assert!(deserialize_saved_session_info(&invalid).is_err());
    let numeric=serde_json::json!({"count":2.0,"cost":0.125,"text":"0.0","large":9007199254740993_u64});
    let normalized=native_wire::normalize_browser_numbers(numeric.clone());
    assert_eq!(normalized["count"].as_i64(),Some(2));
    assert_eq!(normalized["cost"],numeric["cost"]);
    assert_eq!(normalized["text"],numeric["text"]);
    assert_eq!(normalized["large"],numeric["large"]);
}

#[cfg(windows)]
#[tokio::test]
async fn busy_pipe_waits_for_an_instance_within_the_callers_deadline() {
    use pi_coding_agent::modes::daemon::daemon_client::DaemonClient;
    use tokio::net::windows::named_pipe::{ClientOptions,ServerOptions};
    use std::time::Duration;
    let path=format!(r"\\.\pipe\optimus-busy-test-{}",uuid::Uuid::new_v4());
    let server=ServerOptions::new().first_pipe_instance(true).create(&path).unwrap();
    let occupied=ClientOptions::new().open(&path).unwrap();
    server.connect().await.unwrap();
    let next_path=path.clone();
    let ready=tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(120)).await;
        let next=ServerOptions::new().create(next_path).unwrap();
        next.connect().await.unwrap();
        next
    });
    let client=DaemonClient::create(&path);
    let result=client.connect(1500).await;
    if result.is_err() { ready.abort(); }
    result.expect("busy is a transient pipe condition, not a connection failure");
    let accepted=ready.await.unwrap();
    client.close().await;
    drop((accepted,occupied,server));
}

#[cfg(windows)]
#[tokio::test]
async fn permanently_busy_pipe_respects_deadline_and_missing_pipe_fails() {
    use pi_coding_agent::modes::daemon::daemon_client::DaemonClient;
    use tokio::net::windows::named_pipe::{ClientOptions,ServerOptions};
    let path=format!(r"\\.\pipe\optimus-busy-test-{}",uuid::Uuid::new_v4());
    let server=ServerOptions::new().first_pipe_instance(true).create(&path).unwrap();
    let occupied=ClientOptions::new().open(&path).unwrap();
    server.connect().await.unwrap();
    let client=DaemonClient::create(&path);
    let start=std::time::Instant::now();
    assert!(client.connect(100).await.unwrap_err().message().contains("Timed out"));
    assert!(start.elapsed().as_secs()<2);
    drop((occupied,server));
    let missing=DaemonClient::create(&format!("{path}-missing"));
    assert!(missing.connect(1000).await.unwrap_err().message().contains("Failed to connect"));
}

#[test]
fn left_edits_text_and_only_leaves_an_empty_prompt() {
    pi_coding_agent::core::keybindings::KeybindingsManager::new(Default::default(), None).install();
    let identity = || Box::new(|s: &str| s.to_string()) as Box<dyn Fn(&str)->String>;
    let theme = EditorTheme { border_color:Rc::new(str::to_string), background_color:None,
        autocomplete_background_color:None, command_color:None, select_list:SelectListTheme {
            selected_prefix:identity(), selected_text:identity(), description:identity(),
            argument_hint:None, source_tag:None, scroll_info:identity(), no_match:identity() }};
    let tui=Rc::new(RefCell::new(TUI::new(Box::new(ProcessTerminal::new()), None)));
    let mut editor=CustomEditor::new(tui,theme,CustomEditorOptions::default());
    let back=Rc::new(Cell::new(false));
    let called=back.clone();
    editor.on_agents_back=Some(Box::new(move || { called.set(true); true }));
    editor.editor_mut().set_text("/hep");
    for key in ["\x1b[1;1D", "l", "\x1b[1;1C"] { editor.handle_input(key); }
    assert_eq!(editor.editor().get_text(), "/help");
    assert!(!back.get());
    editor.editor_mut().set_text("");
    editor.handle_input("\x1b[1;1D");
    assert!(back.get());
}

#[test]
fn model_registry_respects_the_private_agent_directory() {
    let temp=tempfile::tempdir().unwrap();
    let previous=std::env::var_os("PRIME_AGENT_CODING_AGENT_DIR");
    struct Restore(Option<std::ffi::OsString>);
    impl Drop for Restore { fn drop(&mut self) { match &self.0 {
        Some(v)=>std::env::set_var("PRIME_AGENT_CODING_AGENT_DIR",v),
        None=>std::env::remove_var("PRIME_AGENT_CODING_AGENT_DIR") }} }
    let _restore=Restore(previous);
    std::env::set_var("PRIME_AGENT_CODING_AGENT_DIR",temp.path());
    std::fs::write(temp.path().join("models.json"),r#"{"providers":{"isolated-test":{"apiKey":"dummy","baseUrl":"http://127.0.0.1:1/v1","api":"openai-completions","models":[{"id":"private-marker"}]}}}"#).unwrap();
    let auth=AuthStorage::in_memory(Default::default(),Some(AuthStorageOptions { prime_cli_config_path:None,use_prime_cli_config:false }));
    let registry=ModelRegistry::create(auth,None);
    assert!(registry.get_error().is_none());
    assert!(registry.find("isolated-test","private-marker").is_some());
    assert!(!registry.get_provider_auth_status("azure-openai-managed").configured);
}
