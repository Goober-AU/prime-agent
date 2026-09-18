//! Privacy-safe acceptance-to-model-delivery timing, separate from provider latency.
use super::*;
use pi_agent_core::performance_metrics::{
    PerformanceMetricComponent, PerformanceMetricCorrelation, PerformanceMetricEvent,
    PerformanceMetricIdentity, PerformanceMetricMeasurement, PerformanceMetricOperation,
    PerformanceMetricOutcome,
};

impl AgentSession {
    pub(super) fn start_action_queue_metric(&self, action: &QueuedSessionAction) {
        if !matches!(action.payload, QueuedActionPayload::Turn(_)) {
            return;
        }
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some(metrics) = self.agent.performance_metrics() else {
                return;
            };
            let mut pending = self.action_queue_metrics.lock().unwrap();
            // Telemetry must remain bounded independently of a backed-up queue.
            if pending.len() < 512 {
                pending
                    .entry(action.id.clone())
                    .or_insert((std::time::Instant::now(), metrics.recorder));
            }
        }));
    }

    pub(super) fn finish_action_queue_metric(&self, action: &QueuedSessionAction, delivered: bool) {
        let action_id = &action.id;
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some((accepted, recorder)) =
                self.action_queue_metrics.lock().unwrap().remove(action_id)
            else {
                return;
            };
            let mut event = PerformanceMetricEvent::new(PerformanceMetricOperation::SessionInput);
            event.correlation = Some(PerformanceMetricCorrelation {
                action_id: Some(action_id.into()),
                ..Default::default()
            });
            event.identity = Some(PerformanceMetricIdentity {
                component: Some(PerformanceMetricComponent::Session),
                ..Default::default()
            });
            event.outcome = Some(if delivered {
                PerformanceMetricOutcome::Success
            } else {
                PerformanceMetricOutcome::Cancelled
            });
            event.measurements = Some(
                [(
                    PerformanceMetricMeasurement::QueueMs,
                    Some(accepted.elapsed().as_secs_f64() * 1000.0),
                ), (
                    PerformanceMetricMeasurement::InputAgentMessage,
                    Some(if action.agent_message_id.is_some() || primary_delivery_record(action).ok().is_some_and(|record| {
                        matches!(record.message, DeliveryMessage::Custom(custom) if custom.custom_type == "agent_message")
                    }) { 1.0 } else { 0.0 }),
                )]
                .into_iter()
                .collect(),
            );
            recorder.record(event);
        }));
    }
}
