//! Port of packages/ai/src/utils/event-stream.ts
//!
//! The TypeScript `EventStream` is an async-iterable queue with waiter callbacks.
//! The port uses `Arc` + `tokio::sync::Mutex` + `Notify` so the same object can be
//! cloned into background tasks (the TS `new AssistantMessageEventStream(); (async () => {})()`
//! pattern) and consumed with `next()` / `into_stream()`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

use crate::types::{AssistantMessage, AssistantMessageEvent};

type IsComplete<T> = Box<dyn Fn(&T) -> bool + Send + Sync>;
type ExtractResult<T, R> = Box<dyn Fn(&T) -> R + Send + Sync>;

struct EventStreamState<T, R> {
    queue: VecDeque<T>,
    done: bool,
    result: Option<R>,
    is_complete: IsComplete<T>,
    extract_result: ExtractResult<T, R>,
}

// The state mutex is a std mutex on purpose: `push`/`end` are synchronous (the
// TypeScript calls them from plain callbacks) and the lock is never held across
// an await point.

/// `class EventStream<T, R = T>`.
pub struct EventStream<T, R = T> {
    state: Arc<Mutex<EventStreamState<T, R>>>,
    /// Wakes blocked readers when an event arrives or the stream ends.
    notify: Arc<Notify>,
    /// Wakes `result()` waiters when the final result is available.
    result_notify: Arc<Notify>,
}

impl<T, R> Clone for EventStream<T, R> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            notify: self.notify.clone(),
            result_notify: self.result_notify.clone(),
        }
    }
}

impl<T, R> EventStream<T, R> {
    pub fn new(is_complete: IsComplete<T>, extract_result: ExtractResult<T, R>) -> Self {
        Self {
            state: Arc::new(Mutex::new(EventStreamState {
                queue: VecDeque::new(),
                done: false,
                result: None,
                is_complete,
                extract_result,
            })),
            notify: Arc::new(Notify::new()),
            result_notify: Arc::new(Notify::new()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, EventStreamState<T, R>> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// `push(event: T): void`
    pub fn push(&self, event: T) {
        let complete;
        {
            let mut state = self.lock();
            if state.done {
                return;
            }

            complete = (state.is_complete)(&event);
            if complete {
                state.done = true;
                let result = (state.extract_result)(&event);
                state.result = Some(result);
            }
            state.queue.push_back(event);
        }

        self.notify.notify_waiters();
        if complete {
            self.result_notify.notify_waiters();
        }
    }

    /// `end(result?: R): void`
    pub fn end(&self, result: Option<R>) {
        {
            let mut state = self.lock();
            state.done = true;
            if result.is_some() {
                state.result = result;
            }
        }
        self.notify.notify_waiters();
        self.result_notify.notify_waiters();
    }

    /// `for await (const event of stream)` - `None` means the stream ended.
    pub async fn next(&self) -> Option<T> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            // Register interest before reading the state so a concurrent push
            // cannot be missed between the check and the await.
            notified.as_mut().enable();
            {
                let mut state = self.lock();
                if let Some(event) = state.queue.pop_front() {
                    return Some(event);
                }
                if state.done {
                    return None;
                }
            }
            notified.await;
        }
    }

    /// `result(): Promise<R>`
    pub async fn result(&self) -> R
    where
        R: Clone,
    {
        loop {
            let notified = self.result_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let state = self.lock();
                if let Some(result) = state.result.as_ref() {
                    return result.clone();
                }
            }
            notified.await;
        }
    }

    pub fn is_done(&self) -> bool {
        self.lock().done
    }

    /// The async-iterable form: `AsyncIterator<T>`.
    pub fn into_stream(self) -> impl futures::Stream<Item = T> {
        futures::stream::unfold(self, |stream| async move {
            let next = stream.next().await;
            next.map(|event| (event, stream))
        })
    }
}

/// `class AssistantMessageEventStream extends EventStream<AssistantMessageEvent, AssistantMessage>`.
#[derive(Clone)]
pub struct AssistantMessageEventStream(EventStream<AssistantMessageEvent, AssistantMessage>);

impl Default for AssistantMessageEventStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AssistantMessageEventStream {
    pub fn new() -> Self {
        Self(EventStream::new(
            Box::new(|event| {
                matches!(
                    event,
                    AssistantMessageEvent::Done { .. } | AssistantMessageEvent::Error { .. }
                )
            }),
            Box::new(|event| match event {
                AssistantMessageEvent::Done { message, .. } => message.clone(),
                AssistantMessageEvent::Error { error, .. } => error.clone(),
                _ => panic!("Unexpected event type for final result"),
            }),
        ))
    }

    pub fn push(&self, event: AssistantMessageEvent) {
        self.0.push(event);
    }

    pub fn end(&self, result: Option<AssistantMessage>) {
        self.0.end(result);
    }

    pub async fn next(&self) -> Option<AssistantMessageEvent> {
        self.0.next().await
    }

    pub async fn result(&self) -> AssistantMessage {
        self.0.result().await
    }

    pub fn is_done(&self) -> bool {
        self.0.is_done()
    }

    pub fn into_stream(self) -> impl futures::Stream<Item = AssistantMessageEvent> {
        self.0.into_stream()
    }

    /// Runs the producer body in the background, mirroring the TypeScript
    /// `(async () => { ... })()` pattern.
    pub fn spawn<F>(&self, future: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        tokio::spawn(future);
    }
}

/// Factory function for AssistantMessageEventStream (for use in extensions)
pub fn create_assistant_message_event_stream() -> AssistantMessageEventStream {
    AssistantMessageEventStream::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::STOP_REASON_STOP;

    fn message(text: &str) -> AssistantMessage {
        let mut message = AssistantMessage::default();
        message.content = vec![crate::types::ContentBlock::Text(crate::types::TextContent::new(text))];
        message
    }

    #[tokio::test]
    async fn queued_events_are_delivered_in_order() {
        let stream = AssistantMessageEventStream::new();
        let partial = message("a");
        stream.push(AssistantMessageEvent::Start { partial: partial.clone() });
        stream.push(AssistantMessageEvent::TextDelta {
            content_index: 0,
            delta: "a".to_string(),
            partial: partial.clone(),
        });
        stream.push(AssistantMessageEvent::Done {
            reason: STOP_REASON_STOP.to_string(),
            message: partial,
        });

        assert_eq!(stream.next().await.unwrap().event_type(), "start");
        assert_eq!(stream.next().await.unwrap().event_type(), "text_delta");
        assert_eq!(stream.next().await.unwrap().event_type(), "done");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn done_event_resolves_the_final_result() {
        let stream = AssistantMessageEventStream::new();
        let final_message = message("done");
        stream.push(AssistantMessageEvent::Done {
            reason: STOP_REASON_STOP.to_string(),
            message: final_message.clone(),
        });
        assert_eq!(stream.result().await, final_message);
        assert!(stream.is_done());
    }

    #[tokio::test]
    async fn events_after_completion_are_dropped() {
        let stream = AssistantMessageEventStream::new();
        stream.push(AssistantMessageEvent::Error {
            reason: "error".to_string(),
            error: message("boom"),
        });
        stream.push(AssistantMessageEvent::Start {
            partial: message("late"),
        });
        assert_eq!(stream.next().await.unwrap().event_type(), "error");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn waiting_reader_is_woken_by_a_later_push() {
        let stream = AssistantMessageEventStream::new();
        let producer = stream.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            producer.push(AssistantMessageEvent::Start {
                partial: message("later"),
            });
            producer.push(AssistantMessageEvent::Done {
                reason: STOP_REASON_STOP.to_string(),
                message: message("later"),
            });
        });

        let first = stream.next().await.unwrap();
        assert_eq!(first.event_type(), "start");
        let second = stream.next().await.unwrap();
        assert_eq!(second.event_type(), "done");
    }

    #[tokio::test]
    async fn end_without_result_terminates_the_iterator() {
        let stream = AssistantMessageEventStream::new();
        stream.end(None);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn error_event_reason_is_preserved() {
        let stream = AssistantMessageEventStream::new();
        let mut error_message = message("");
        error_message.stop_reason = "aborted".to_string();
        stream.push(AssistantMessageEvent::Error {
            reason: "aborted".to_string(),
            error: error_message.clone(),
        });
        let event = stream.next().await.unwrap();
        match event {
            AssistantMessageEvent::Error { reason, error } => {
                assert_eq!(reason, "aborted");
                assert_eq!(error.stop_reason, "aborted");
            }
            other => panic!("unexpected event {}", other.event_type()),
        }
        assert_eq!(stream.result().await, error_message);
    }

    #[test]
    fn factory_matches_constructor() {
        let stream = create_assistant_message_event_stream();
        assert!(!stream.is_done());
    }
}
