//! Connect the shared roster store to the interactive connection's accessory bar.

use super::*;
use crate::modes::agents_view::native_host::NativeTransport;
use crate::modes::agents_view::roster_store::{
    AgentsViewRosterStore, DaemonTransport, DaemonTransportClient as RosterClient,
};

#[derive(Clone)]
pub(super) struct RosterSubscription {
    store: AgentsViewRosterStore,
    summaries: Arc<Mutex<Vec<Value>>>,
    listeners: Arc<Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>>,
    transport: Arc<Mutex<Option<Arc<dyn DaemonTransport>>>>,
    attach_lock: Arc<tokio::sync::Mutex<()>>,
    cancelled: tokio_util::sync::CancellationToken,
    initialized: Arc<Mutex<bool>>,
}

impl RosterSubscription {
    pub(super) fn new() -> Self {
        Self {
            store: AgentsViewRosterStore::new(),
            summaries: Arc::new(Mutex::new(Vec::new())),
            listeners: Arc::new(Mutex::new(Vec::new())),
            transport: Arc::new(Mutex::new(None)),
            attach_lock: Arc::new(tokio::sync::Mutex::new(())),
            cancelled: tokio_util::sync::CancellationToken::new(),
            initialized: Arc::new(Mutex::new(false)),
        }
    }

    async fn refresh(&self) {
        let rows = self
            .store
            .summaries()
            .await
            .into_iter()
            .map(|row| serde_json::to_value(row).expect("roster row serializes"))
            .collect();
        *self.summaries.lock().unwrap() = rows;
        let listeners = self.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener();
        }
    }
}

impl AgentConnectionRosterStore for RosterSubscription {
    fn attach(&self, client: Arc<dyn DaemonTransportClient>) -> BoxFuture<Result<bool, String>> {
        let this = self.clone();
        Box::pin(async move {
            let _guard = this.attach_lock.lock().await;
            if this.cancelled.is_cancelled() {
                return Ok(false);
            }
            let control = client.control_plane_transport();
            if !control.supports_server_capability("agent_roster") {
                return Ok(false);
            }
            let socket = control
                .hello_socket_path()
                .ok_or("Roster daemon endpoint is unavailable")?;
            let existing = this.transport.lock().unwrap().clone();
            let transport = match existing {
                Some(transport) if transport.socket_path() == socket => transport,
                Some(old) => {
                    old.close();
                    Arc::new(NativeTransport::new(&socket)) as Arc<dyn DaemonTransport>
                }
                None => Arc::new(NativeTransport::new(&socket)) as Arc<dyn DaemonTransport>,
            };
            if !transport.is_connected() {
                transport.connect(3000).await?;
            }
            transport.wait_for_hello(3000).await?;
            *this.transport.lock().unwrap() = Some(transport.clone());
            if !this
                .store
                .attach(RosterClient::new(transport.clone()))
                .await?
            {
                return Ok(false);
            }
            this.refresh().await;
            let start = {
                let mut initialized = this.initialized.lock().unwrap();
                let start = !*initialized;
                *initialized = true;
                start
            };
            if start {
                let store = this.clone();
                this.store
                    .on_update(Arc::new(move || {
                        let store = store.clone();
                        tokio::spawn(async move {
                            if !store.cancelled.is_cancelled() {
                                store.refresh().await;
                            }
                        });
                    }))
                    .await;
                let store = this.clone();
                tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            _ = store.cancelled.cancelled() => break,
                            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                        }
                        let _guard = store.attach_lock.lock().await;
                        if store.cancelled.is_cancelled() {
                            break;
                        }
                        let transport = store.transport.lock().unwrap().clone();
                        if let Some(transport) = transport {
                            if !transport.is_connected() {
                                tokio::select! {
                                    _ = store.cancelled.cancelled() => break,
                                    _ = async {
                                        if transport.reconnect(3000).await.is_ok() {
                                            let _ = store.store.attach(RosterClient::new(transport)).await;
                                            store.refresh().await;
                                        }
                                    } => {}
                                }
                            }
                        }
                    }
                });
            }
            this.refresh().await;
            Ok(true)
        })
    }

    fn on_update(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Box<dyn Fn() + Send + Sync> {
        self.listeners.lock().unwrap().push(listener.clone());
        let listeners = self.listeners.clone();
        Box::new(move || {
            listeners
                .lock()
                .unwrap()
                .retain(|candidate| !Arc::ptr_eq(candidate, &listener));
        })
    }
    fn summaries(&self) -> Vec<Value> {
        self.summaries.lock().unwrap().clone()
    }
    fn dispose(&self) -> BoxFuture<()> {
        let this = self.clone();
        this.cancelled.cancel();
        Box::pin(async move {
            let _guard = this.attach_lock.lock().await;
            this.store.dispose().await;
            let transport = this.transport.lock().unwrap().take();
            if let Some(transport) = transport {
                transport.close();
            }
            this.listeners.lock().unwrap().clear();
        })
    }
}
