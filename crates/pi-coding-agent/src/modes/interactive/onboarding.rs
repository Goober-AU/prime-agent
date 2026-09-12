//! Port of packages/coding-agent/src/modes/interactive/onboarding.ts

use super::interactive_mode_services::{AgentConnectionModel, AuthStatus, ModelRegistry, SettingsManager};

/// `PRIME_INFERENCE_PROVIDER_ID`
pub const PRIME_INFERENCE_PROVIDER_ID: &str = "prime-inference";

/// `OnboardingSettingsReader`
pub trait OnboardingSettingsReader {
    fn get_onboarding_shown(&self) -> bool;
}

impl OnboardingSettingsReader for SettingsManager {
    fn get_onboarding_shown(&self) -> bool {
        SettingsManager::get_onboarding_shown(self)
    }
}

/// `OnboardingModelRegistryReader`
pub trait OnboardingModelRegistryReader {
    fn refresh(&self);
    fn has_configured_auth(&self, model: &AgentConnectionModel) -> bool;
    fn get_provider_auth_status(&self, provider: &str) -> AuthStatus;
}

/// The TypeScript hands the live `ModelRegistry` object to the reader and calls
/// `refresh(): void` on it, so the reader borrows the registry shared. The port's
/// registry mutates on `refresh` (`&mut self`), so the reader owns it behind a
/// `RefCell` and borrows mutably only for the reload - the same adapter shape
/// `InteractiveMode` and `main_entry` use for their shared registries.
impl OnboardingModelRegistryReader for std::cell::RefCell<ModelRegistry> {
    fn refresh(&self) {
        self.borrow_mut().refresh();
    }

    fn has_configured_auth(&self, model: &AgentConnectionModel) -> bool {
        self.borrow().has_configured_auth(model)
    }

    fn get_provider_auth_status(&self, provider: &str) -> AuthStatus {
        self.borrow().get_provider_auth_status(provider)
    }
}

/// `OnboardingStartupState`
pub struct OnboardingStartupState<'a> {
    pub settings_manager: &'a dyn OnboardingSettingsReader,
    pub model_registry: &'a dyn OnboardingModelRegistryReader,
    pub model: Option<&'a AgentConnectionModel>,
}

/// Port of `shouldRunPrimeCliOnboardingSplash`.
pub fn should_run_prime_cli_onboarding_splash(state: &OnboardingStartupState<'_>) -> bool {
    if state.settings_manager.get_onboarding_shown() {
        return false;
    }
    match state.model {
        Some(model) if model.provider == PRIME_INFERENCE_PROVIDER_ID => {}
        _ => return false,
    }
    let auth_status = state.model_registry.get_provider_auth_status(PRIME_INFERENCE_PROVIDER_ID);
    auth_status.source == "prime_cli"
}

/// Port of `isOnboardingModelReady`.
pub fn is_onboarding_model_ready(state: &OnboardingStartupState<'_>) -> bool {
    match state.model {
        Some(model) => state.model_registry.has_configured_auth(model),
        None => false,
    }
}

/// Port of `shouldRunOnboarding`.
pub fn should_run_onboarding(state: &OnboardingStartupState<'_>) -> bool {
    if state.settings_manager.get_onboarding_shown() {
        return false;
    }
    state.model_registry.refresh();
    if should_run_prime_cli_onboarding_splash(state) {
        return true;
    }
    !is_onboarding_model_ready(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_ai::types::Model;

    #[derive(Default)]
    struct Settings {
        shown: bool,
    }

    impl OnboardingSettingsReader for Settings {
        fn get_onboarding_shown(&self) -> bool {
            self.shown
        }
    }

    struct Registry {
        refreshes: std::cell::Cell<usize>,
        configured: bool,
        auth_source: String,
    }

    impl OnboardingModelRegistryReader for Registry {
        fn refresh(&self) {
            self.refreshes.set(self.refreshes.get() + 1);
        }

        fn has_configured_auth(&self, _model: &AgentConnectionModel) -> bool {
            self.configured
        }

        fn get_provider_auth_status(&self, _provider: &str) -> AuthStatus {
            AuthStatus { source: self.auth_source.clone() }
        }
    }

    fn model(provider: &str) -> AgentConnectionModel {
        Model::new("glm-5.3", "GLM 5.3", "openai-completions", provider, "https://example.invalid")
    }

    #[test]
    fn shown_onboarding_short_circuits() {
        let settings = Settings { shown: true };
        let registry = Registry { refreshes: 0.into(), configured: false, auth_source: "prime_cli".into() };
        let model = model(PRIME_INFERENCE_PROVIDER_ID);
        let state = OnboardingStartupState { settings_manager: &settings, model_registry: &registry, model: Some(&model) };
        assert!(!should_run_prime_cli_onboarding_splash(&state));
        assert!(!should_run_onboarding(&state));
        assert_eq!(registry.refreshes.get(), 0);
    }

    #[test]
    fn prime_cli_splash_requires_prime_inference_and_prime_cli_source() {
        let settings = Settings::default();
        let registry = Registry { refreshes: 0.into(), configured: true, auth_source: "prime_cli".into() };
        let prime = model(PRIME_INFERENCE_PROVIDER_ID);
        let other = model("anthropic");
        let state = OnboardingStartupState { settings_manager: &settings, model_registry: &registry, model: Some(&prime) };
        assert!(should_run_prime_cli_onboarding_splash(&state));
        let state = OnboardingStartupState { settings_manager: &settings, model_registry: &registry, model: Some(&other) };
        assert!(!should_run_prime_cli_onboarding_splash(&state));
        let state = OnboardingStartupState { settings_manager: &settings, model_registry: &registry, model: None };
        assert!(!should_run_prime_cli_onboarding_splash(&state));
    }

    #[test]
    fn onboarding_runs_when_no_model_has_auth() {
        let settings = Settings::default();
        let registry = Registry { refreshes: 0.into(), configured: false, auth_source: "env".into() };
        let prime = model(PRIME_INFERENCE_PROVIDER_ID);
        let state = OnboardingStartupState { settings_manager: &settings, model_registry: &registry, model: Some(&prime) };
        assert!(should_run_onboarding(&state));
        assert_eq!(registry.refreshes.get(), 1);

        let registry = Registry { refreshes: 0.into(), configured: true, auth_source: "env".into() };
        let state = OnboardingStartupState { settings_manager: &settings, model_registry: &registry, model: Some(&prime) };
        assert!(!should_run_onboarding(&state));
    }

    #[test]
    fn model_ready_requires_a_model() {
        let settings = Settings::default();
        let registry = Registry { refreshes: 0.into(), configured: true, auth_source: "env".into() };
        let state = OnboardingStartupState { settings_manager: &settings, model_registry: &registry, model: None };
        assert!(!is_onboarding_model_ready(&state));
    }
}
