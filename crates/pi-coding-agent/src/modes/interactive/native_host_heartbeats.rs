//! Heartbeat catalog and native manager callbacks.
use super::*;
use crate::modes::interactive::components::heartbeat_manager::{
    HeartbeatManagerComponent, HeartbeatManagerOptions,
};

pub(super) fn scoped(
    mode: &InteractiveMode,
    catalog: &[wire::AgentConnectionHeartbeat],
) -> Vec<wire::AgentConnectionHeartbeat> {
    let ids: std::collections::HashSet<String> = mode
        .get_scoped_heartbeats()
        .into_iter()
        .map(|h| h.job.id)
        .collect();
    catalog
        .iter()
        .filter(|h| {
            h.job
                .get("id")
                .and_then(|id| id.as_str())
                .is_some_and(|id| ids.contains(id))
        })
        .cloned()
        .collect()
}

pub(super) fn apply_catalog(
    mode: &mut InteractiveMode,
    catalog: &[wire::AgentConnectionHeartbeat],
) {
    mode.heartbeat_catalog = catalog
        .iter()
        .filter_map(|h| {
            Some(local::AgentConnectionHeartbeat {
                job: project_heartbeat(h.job.clone())?,
                session_name: h.session_name.clone(),
                first_message: h.first_message.clone(),
            })
        })
        .collect();
}

pub(super) fn create(
    mode: Rc<RefCell<InteractiveMode>>,
    catalog: Rc<RefCell<Vec<wire::AgentConnectionHeartbeat>>>,
    rows: Rc<Cell<f64>>,
    send: mpsc::Sender<HostEvent>,
    connection: Arc<dyn wire::AgentConnection>,
) -> HeartbeatManagerComponent {
    let close = send.clone();
    let render = send.clone();
    HeartbeatManagerComponent::new(HeartbeatManagerOptions {
        get_heartbeats: Box::new(move || scoped(&mode.borrow(), &catalog.borrow())),
        get_rows: Rc::new(move || rows.get()),
        on_close: Box::new(move || {
            let _ = close.send(HostEvent::CloseHeartbeats);
        }),
        request_render: Box::new(move || {
            let _ = render.send(HostEvent::Render);
        }),
        on_action: Box::new(move |heartbeat, action| {
            let connection = connection.clone();
            let send = send.clone();
            Box::pin(async move {
                let job: crate::core::cron_jobs::AgentCronJob =
                    serde_json::from_value(heartbeat.job.clone()).map_err(|e| e.to_string())?;
                let updated = connection
                    .manage_heartbeat(
                        &job.active_session_id,
                        &job.id,
                        serde_json::Value::String(action),
                    )
                    .await?;
                let _ = send.send(HostEvent::HeartbeatUpdated(heartbeat, updated));
                if let Ok(catalog) = connection.list_heartbeats().await {
                    let _ = send.send(HostEvent::Heartbeats(catalog, false));
                }
                Ok(())
            })
        }),
    })
}

pub(super) fn refresh_delay(catalog: &[wire::AgentConnectionHeartbeat]) -> Option<Duration> {
    let next = catalog
        .iter()
        .filter(|h| h.job["status"] == "active")
        .filter_map(|h| {
            chrono::DateTime::parse_from_rfc3339(h.job["nextRunAt"].as_str()?)
                .ok()
                .map(|date| date.timestamp_millis())
        })
        .min()?;
    let until = next.saturating_sub(chrono::Utc::now().timestamp_millis());
    Some(Duration::from_millis(if until > 0 {
        until.saturating_add(250).min(60_000) as u64
    } else {
        5_000
    }))
}
