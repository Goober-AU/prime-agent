use pi_coding_agent::core::session_manager::{SessionManager, SessionState, SessionStateStatus};
use pi_agent_core::types::AgentMessage;
use pi_ai::types::{Message, UserContent, UserMessage};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct CountingAllocator;
thread_local! {
    static COUNT: Cell<Option<usize>> = const { Cell::new(None) };
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNT.try_with(|n| { if let Some(bytes) = n.get() { n.set(Some(bytes + layout.size())); } });
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { System.dealloc(ptr, layout); }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let _ = COUNT.try_with(|n| { if let Some(bytes) = n.get() { n.set(Some(bytes + size)); } });
        System.realloc(ptr, layout, size)
    }
}

fn metadata_bytes(manager: &SessionManager) -> (Option<String>, Option<SessionState>, usize) {
    COUNT.with(|n| n.set(Some(0)));
    let name = manager.get_session_name();
    let state = manager.get_session_state();
    let bytes = COUNT.with(|n| n.replace(None).unwrap());
    (name, state, bytes)
}

#[test]
fn streamed_metadata_does_not_allocate_transcript_copies() {
    let mut manager = SessionManager::in_memory(Some("/isolated"), Some("")).unwrap();
    manager.append_message(AgentMessage::Message(Message::User(UserMessage::new(
        UserContent::Text("x".repeat(8 * 1024 * 1024)), 1,
    )))).unwrap();
    let (name, state, bytes) = metadata_bytes(&manager);
    assert!(name.is_none() && state.is_none());
    assert!(bytes < 1024, "unnamed metadata allocated {bytes} bytes");
    manager.append_session_info(" first ").unwrap();
    manager.append_session_state(&SessionState { status: SessionStateStatus::Archived }).unwrap();
    manager.append_session_info(" renamed ").unwrap();
    manager.append_message(AgentMessage::Message(Message::User(UserMessage::new(
        UserContent::Text("y".repeat(8 * 1024 * 1024)), 2,
    )))).unwrap();
    for _ in 0..100 {
        let (name, state, bytes) = metadata_bytes(&manager);
        assert_eq!(name.as_deref(), Some("renamed"));
        assert_eq!(state, Some(SessionState { status: SessionStateStatus::Archived }));
        assert!(bytes < 1024, "named metadata allocated {bytes} bytes");
    }
}
