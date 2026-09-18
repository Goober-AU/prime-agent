//! Content-free client timings. No prompt, key, output or error text is recorded.
use std::sync::Arc;
use std::time::{Duration, Instant};
use pi_agent_core::performance_metrics::{
    safe_record_performance_metric, PerformanceMetricComponent, PerformanceMetricCorrelation,
    PerformanceMetricEvent, PerformanceMetricIdentity, PerformanceMetricMeasurement as M,
    PerformanceMetricOperation as Op, PerformanceMetricOutcome as Outcome, PerformanceMetricRecorder,
};
use crate::core::performance_monitor::PerformanceMonitor;

pub(super) struct UiMetrics {
    pub recorder: Arc<dyn PerformanceMetricRecorder>,
    session_id: String,
    input: Option<Instant>,
    menus: Vec<Instant>,
    frames: u64,
    render_ms: f64,
    max_render_ms: f64,
    window: Instant,
}

pub(super) fn duration_event(op: Op, elapsed: Duration, outcome: Outcome) -> PerformanceMetricEvent {
    let mut event = PerformanceMetricEvent::new(op);
    event.identity = Some(PerformanceMetricIdentity { component: Some(PerformanceMetricComponent::Session), ..Default::default() });
    event.outcome = Some(outcome);
    event.measurements = Some([(M::TotalMs, Some(elapsed.as_secs_f64() * 1000.0))].into_iter().collect());
    event
}

impl UiMetrics {
    pub fn new(session_id: &str) -> Self {
        Self::with_recorder(session_id, Arc::new(PerformanceMonitor::from_environment(crate::config::get_agent_dir()).recorder(session_id.into())))
    }
    fn with_recorder(session_id: &str, recorder: Arc<dyn PerformanceMetricRecorder>) -> Self {
        Self { recorder, session_id: session_id.into(), input: None, menus: Vec::new(), frames: 0, render_ms: 0.0, max_render_ms: 0.0, window: Instant::now() }
    }
    pub fn session(&mut self, session_id: &str) {
        if self.session_id != session_id {
            self.flush_render();
            *self = Self::new(session_id);
        }
    }
    pub fn input(&mut self, received: Instant) {
        self.input.get_or_insert(received);
    }
    pub fn menu(&mut self, started: Instant) {
        if self.menus.len() < 32 { self.menus.push(started); }
    }
    pub fn first_frame(&self, started: Instant) {
        safe_record_performance_metric(Some(&self.recorder), duration_event(Op::UiSessionOpen, started.elapsed(), Outcome::Success));
    }
    pub fn rendered(&mut self, elapsed: Duration) {
        self.frames += 1;
        let ms = elapsed.as_secs_f64() * 1000.0;
        self.render_ms += ms;
        self.max_render_ms = self.max_render_ms.max(ms);
        if let Some(started) = self.input.take() {
            safe_record_performance_metric(Some(&self.recorder), duration_event(Op::UiInput, started.elapsed(), Outcome::Success));
        }
        for started in self.menus.drain(..) {
            safe_record_performance_metric(Some(&self.recorder), duration_event(Op::UiMenuOpen, started.elapsed(), Outcome::Success));
        }
        if self.window.elapsed() >= Duration::from_secs(1) { self.flush_render(); }
    }
    pub fn flush_render(&mut self) {
        if self.frames == 0 { return; }
        let mut event = duration_event(Op::UiRender, Duration::from_secs_f64(self.render_ms / 1000.0), Outcome::Success);
        let measurements = event.measurements.as_mut().expect("duration measurements");
        measurements.insert(M::FrameCount, Some(self.frames as f64));
        measurements.insert(M::MaxMs, Some(self.max_render_ms));
        safe_record_performance_metric(Some(&self.recorder), event);
        self.frames = 0;
        self.render_ms = 0.0;
        self.max_render_ms = 0.0;
        self.window = Instant::now();
    }
}

pub(super) async fn acknowledged<F>(recorder: Arc<dyn PerformanceMetricRecorder>, future: F) -> Result<(), String>
where F: std::future::Future<Output = Result<(), String>> {
    let started = Instant::now();
    let id = format!("UiInput-{}", uuid::Uuid::new_v4());
    let result = future.await;
    let mut event = duration_event(Op::UiInputAck, started.elapsed(), if result.is_ok() { Outcome::Success } else { Outcome::Failure });
    event.correlation = Some(PerformanceMetricCorrelation { action_id: Some(id), ..Default::default() });
    safe_record_performance_metric(Some(&recorder), event);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[derive(Default)]
    struct Recorder(Mutex<Vec<PerformanceMetricEvent>>);
    impl PerformanceMetricRecorder for Recorder {
        fn session_id(&self) -> &str { "test" }
        fn monotonic_now(&self) -> f64 { 0.0 }
        fn next_id(&self, _: pi_agent_core::performance_metrics::PerformanceMetricIdScope) -> String { "test".into() }
        fn record(&self, event: PerformanceMetricEvent) { self.0.lock().unwrap().push(event); }
        fn flush(&self) {}
        fn close(&self) {}
    }
    #[test]
    fn input_and_menu_only_settle_after_render_and_frames_are_aggregated() {
        let recorder = Arc::new(Recorder::default());
        let mut metrics = UiMetrics::with_recorder("test", recorder.clone());
        metrics.input(Instant::now());
        metrics.menu(Instant::now());
        assert!(recorder.0.lock().unwrap().is_empty());
        metrics.rendered(Duration::from_millis(4));
        metrics.rendered(Duration::from_millis(6));
        metrics.flush_render();
        let events = recorder.0.lock().unwrap();
        assert_eq!(events.iter().map(|e| e.operation).collect::<Vec<_>>(), [Op::UiInput, Op::UiMenuOpen, Op::UiRender]);
        let values = events[2].measurements.as_ref().unwrap();
        assert_eq!(values[&M::TotalMs], Some(10.0));
        assert_eq!(values[&M::MaxMs], Some(6.0));
        assert_eq!(values[&M::FrameCount], Some(2.0));
    }
    #[tokio::test]
    async fn acknowledgement_waits_for_the_matching_reply_and_omits_error_contents() {
        let recorder = Arc::new(Recorder::default());
        let (reply, wait) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(acknowledged(recorder.clone(), async move { wait.await.unwrap() }));
        tokio::task::yield_now().await;
        assert!(recorder.0.lock().unwrap().is_empty());
        reply.send(Err("private prompt and credential".into())).unwrap();
        assert!(task.await.unwrap().is_err());
        let events = recorder.0.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].operation, Op::UiInputAck);
        assert_eq!(events[0].outcome, Some(Outcome::Failure));
        assert!(!serde_json::to_string(&*events).unwrap().contains("private"));
    }
}
