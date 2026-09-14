//! Port of packages/coding-agent/src/core/event-bus.ts
//!
//! Node's `EventEmitter` becomes an ordered listener map. Listeners are invoked
//! asynchronously in registration order and their failures are logged with the
//! same message shape, matching the `safeHandler` wrapper in the TypeScript.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub type EventHandler = Arc<dyn Fn(Value) + Send + Sync>;
pub type Unsubscribe = Box<dyn Fn() + Send + Sync>;

pub trait EventBus: Send + Sync {
    fn emit(&self, channel: &str, data: Value);
    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe;
}

pub trait EventBusController: EventBus {
    fn clear(&self);
}

struct EventBusInner {
    next_id: AtomicU64,
    listeners: Mutex<HashMap<String, Vec<(u64, EventHandler)>>>,
}

/// `createEventBus()`.
pub struct EventBusImpl {
    inner: Arc<EventBusInner>,
}

impl Default for EventBusImpl {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBusImpl {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(EventBusInner {
                next_id: AtomicU64::new(1),
                listeners: Mutex::new(HashMap::new()),
            }),
        }
    }
}

impl EventBus for EventBusImpl {
    fn emit(&self, channel: &str, data: Value) {
        let handlers: Vec<EventHandler> = {
            let listeners = self.inner.listeners.lock().unwrap();
            listeners
                .get(channel)
                .map(|entries| entries.iter().map(|(_, handler)| handler.clone()).collect())
                .unwrap_or_default()
        };
        for handler in handlers {
            let channel_name = channel.to_string();
            // `safeHandler` awaits the listener and logs failures instead of
            // propagating them; a panic is reported the same way.
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(data.clone())));
            if result.is_err() {
                eprintln!("Event handler error ({channel_name}): handler panicked");
            }
        }
    }

    fn on(&self, channel: &str, handler: EventHandler) -> Unsubscribe {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        {
            let mut listeners = self.inner.listeners.lock().unwrap();
            listeners
                .entry(channel.to_string())
                .or_default()
                .push((id, handler));
        }
        let inner = self.inner.clone();
        let channel = channel.to_string();
        Box::new(move || {
            let mut listeners = inner.listeners.lock().unwrap();
            if let Some(entries) = listeners.get_mut(&channel) {
                entries.retain(|(entry_id, _)| *entry_id != id);
            }
        })
    }
}

impl EventBusController for EventBusImpl {
    fn clear(&self) {
        self.inner.listeners.lock().unwrap().clear();
    }
}

/// `createEventBus()` free function matching the TypeScript export.
pub fn create_event_bus() -> EventBusImpl {
    EventBusImpl::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn listeners_receive_only_their_channel() {
        let bus = create_event_bus();
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = seen.clone();
        let _first = bus.on(
            "a",
            Arc::new(move |data| recorder.lock().unwrap().push(data.to_string())),
        );
        let other: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let recorder = other.clone();
        let _second = bus.on(
            "b",
            Arc::new(move |data| recorder.lock().unwrap().push(data.to_string())),
        );
        bus.emit("a", json!("one"));
        assert_eq!(seen.lock().unwrap().as_slice(), ["\"one\""]);
        assert!(other.lock().unwrap().is_empty());
    }

    #[test]
    fn listeners_run_in_registration_order() {
        let bus = create_event_bus();
        let order: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        for index in 0..3u8 {
            let recorder = order.clone();
            let _guard = bus.on(
                "c",
                Arc::new(move |_data| recorder.lock().unwrap().push(index)),
            );
        }
        bus.emit("c", json!(null));
        assert_eq!(order.lock().unwrap().as_slice(), [0, 1, 2]);
    }

    #[test]
    fn unsubscribe_and_clear_remove_listeners() {
        let bus = create_event_bus();
        let count = Arc::new(AtomicU64::new(0));
        let counter = count.clone();
        let unsubscribe = bus.on(
            "d",
            Arc::new(move |_data| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("d", json!(1));
        unsubscribe();
        bus.emit("d", json!(1));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let counter = count.clone();
        let _guard = bus.on(
            "e",
            Arc::new(move |_data| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.clear();
        bus.emit("e", json!(1));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn panicking_handlers_do_not_stop_other_listeners() {
        let bus = create_event_bus();
        let count = Arc::new(AtomicU64::new(0));
        let _bad = bus.on("f", Arc::new(|_data| panic!("boom")));
        let counter = count.clone();
        let _good = bus.on(
            "f",
            Arc::new(move |_data| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        );
        bus.emit("f", json!(null));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }
}
