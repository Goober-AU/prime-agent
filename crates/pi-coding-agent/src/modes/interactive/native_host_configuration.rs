//! Native configuration-menu mounting with the persisted provider registry.
use super::*;
use crate::core::{auth_storage::AuthStorage, model_registry::ModelRegistry};
use crate::modes::interactive::components::configuration_menu::{
    AuthSelectorProvider, ConfigurationMenuComponent, ConfigurationMenuOptions,
    ConfigurationMenuScopedModel,
};

pub(super) fn create(
    mode: &InteractiveMode,
    ui: Rc<RefCell<TUI>>,
    tab: &'static str,
    models: &[wire::AgentConnectionModel],
    configured: &std::collections::HashSet<String>,
    search: Option<String>,
    rows: Rc<Cell<f64>>,
    selection: mpsc::Sender<Option<String>>,
    send: mpsc::Sender<HostEvent>,
) -> Result<ConfigurationMenuComponent, String> {
    let auth = Rc::new(RefCell::new(AuthStorage::create(None, None)));
    let registry = Rc::new(RefCell::new(ModelRegistry::create(
        AuthStorage::create(None, None),
        None,
    )));
    let providers = provider_options(&auth.borrow(), &registry.borrow(), models);
    let selected_provider = selection.clone();
    let selected_service = selection.clone();
    let selected_model = selection.clone();
    let settings = mode.settings_manager().lock().map_err(|e| e.to_string())?;
    Ok(ConfigurationMenuComponent::new(ConfigurationMenuOptions {
        initial_tab: tab,
        tui: ui,
        auth_storage: auth,
        model_registry: registry,
        provider_options: providers,
        current_model: mode.get_current_model().cloned(),
        scoped_models: mode
            .get_scoped_model_state()
            .into_iter()
            .map(|s| ConfigurationMenuScopedModel {
                model: s.model,
                thinking_level: None,
            })
            .collect(),
        available_models: models.to_vec(),
        configured_providers: configured.clone(),
        recent_models: Some(settings.get_recent_models()),
        initial_model_search: search,
        get_rows: Some(Box::new(move || rows.get())),
        request_render: Box::new(move || {
            let _ = send.send(HostEvent::Render);
        }),
        on_select_provider: Box::new(move |provider| {
            let _ = selected_provider.send(Some(login_key(provider)));
        }),
        on_select_mcp_connection: Box::new(move |provider| {
            let _ = selected_service.send(Some(login_key(provider)));
        }),
        on_select_model: Box::new(move |model| {
            let _ = selected_model.send(Some(format!("{}/{}", model.provider, model.id)));
        }),
        on_cancel: Box::new(move || {
            let _ = selection.send(None);
        }),
    }))
}

fn login_key(provider: &AuthSelectorProvider) -> String {
    format!(
        "login:{}:{}",
        if provider.auth_type == "oauth" {
            "oauth"
        } else {
            "api"
        },
        provider.id
    )
}

fn provider_options(
    auth: &AuthStorage,
    registry: &ModelRegistry,
    models: &[wire::AgentConnectionModel],
) -> Vec<AuthSelectorProvider> {
    let oauth = auth.get_oauth_providers();
    let ids = oauth.iter().map(|p| p.id.clone()).collect();
    let mut options: Vec<_> = oauth
        .into_iter()
        .map(|p| AuthSelectorProvider {
            category: p.id.starts_with("mcp:").then(|| "service".into()),
            id: p.id,
            name: p.name,
            auth_type: "oauth".into(),
        })
        .collect();
    let providers: std::collections::BTreeSet<_> = models
        .iter()
        .map(|m| m.provider.clone())
        .chain(registry.get_all().into_iter().map(|m| m.provider))
        .collect();
    for provider in providers {
        if crate::modes::interactive::auth_flows::is_api_key_login_provider(&provider, &ids, None) {
            options.push(AuthSelectorProvider {
                name: registry.get_provider_display_name(&provider),
                id: provider,
                auth_type: "api_key".into(),
                category: None,
            });
        }
    }
    options.push(AuthSelectorProvider {
        id: crate::core::websearch_credential::SERPER_CREDENTIAL_ID.into(),
        name: crate::core::websearch_credential::SERPER_CREDENTIAL_NAME.into(),
        auth_type: "api_key".into(),
        category: Some("service".into()),
    });
    options
}
