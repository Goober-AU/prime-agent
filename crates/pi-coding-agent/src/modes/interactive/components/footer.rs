//! Port of packages/coding-agent/src/modes/interactive/components/footer.ts
//!
//! Footer component for the prime brand TUI.
//!
//! Renders nothing by default - token counters, cost, model name, cwd, and
//! context % are intentionally hidden. The setters and invalidate/dispose hooks
//! are kept so the existing call sites in interactive-mode keep working without
//! modification, and so `/usage` can expose telemetry without re-plumbing.

use pi_tui::tui::Component;

use crate::core::footer_data_provider::ReadonlyFooterDataProvider;

/// Port of `FooterComponent`.
pub struct FooterComponent {
    /// `private footerData: ReadonlyFooterDataProvider` - unused while the footer
    /// is empty (`void this.footerData`).
    footer_data: std::sync::Arc<dyn ReadonlyFooterDataProvider>,
}

impl FooterComponent {
    pub fn new(footer_data: std::sync::Arc<dyn ReadonlyFooterDataProvider>) -> Self {
        Self { footer_data }
    }

    /// The constructor's `void this.footerData;`.
    pub fn footer_data(&self) -> &std::sync::Arc<dyn ReadonlyFooterDataProvider> {
        &self.footer_data
    }

    /// Port of `setAutoCompactEnabled` - no-op while the footer is empty.
    pub fn set_auto_compact_enabled(&mut self, _enabled: bool) {
        // no-op while the footer is empty
    }

    /// Port of `dispose`. Git watcher cleanup is handled by the provider.
    pub fn dispose(&mut self) {
        // Git watcher cleanup handled by provider
    }
}

impl Component for FooterComponent {
    fn render(&mut self, _width: f64) -> Vec<String> {
        // Footer is intentionally empty in the prime brand TUI. Telemetry (cost,
        // tokens, model, cwd, context %) is hidden by default; bring it back via
        // /usage when needed.
        Vec::new()
    }

    /// Port of `invalidate` - no-op: git branch caching is handled by the provider.
    fn invalidate(&mut self) {
        // No-op: git branch is cached/invalidated by provider
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::footer_data_provider::FooterDataProvider;

    /// The TypeScript passes any `ReadonlyFooterDataProvider`; the port uses the
    /// real provider because `Unsubscribe` has no public constructor.
    fn provider() -> std::sync::Arc<FooterDataProvider> {
        std::sync::Arc::new(FooterDataProvider::new("."))
    }

    #[test]
    fn renders_nothing() {
        let mut footer = FooterComponent::new(provider());
        assert_eq!(footer.render(80.0), Vec::<String>::new());
    }

    #[test]
    fn setters_and_hooks_are_no_ops() {
        let mut footer = FooterComponent::new(provider());
        footer.set_auto_compact_enabled(true);
        footer.invalidate();
        footer.dispose();
        assert!(std::sync::Arc::ptr_eq(
            footer.footer_data(),
            footer.footer_data()
        ));
    }
}
