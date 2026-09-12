//! Port of packages/coding-agent/src/modes/interactive/components/earendil-announcement.ts

use std::sync::OnceLock;

use pi_tui::components::image::{Image, ImageOptions, ImageTheme};
use pi_tui::components::spacer::Spacer;
use pi_tui::components::text::Text;
use pi_tui::tui::Component;

use crate::config::get_bundled_interactive_asset_path;
use crate::modes::interactive::theme::theme::theme;

use super::dynamic_border::DynamicBorder;

/// `BLOG_URL`
pub const BLOG_URL: &str = "https://mariozechner.at/posts/2026-04-08-ive-sold-out/";
/// `IMAGE_FILENAME`
pub const IMAGE_FILENAME: &str = "clankolas.png";

/// `cachedImageBase64` / `attemptedImageLoad` module state.
fn image_state() -> &'static std::sync::Mutex<(bool, Option<String>)> {
    static STATE: OnceLock<std::sync::Mutex<(bool, Option<String>)>> = OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new((false, None)))
}

/// Port of `loadImageBase64`.
pub fn load_image_base64() -> Option<String> {
    let state = image_state();
    let mut guard = state.lock().expect("image state");
    if guard.0 {
        return guard.1.clone();
    }

    guard.0 = true;
    guard.1 = std::fs::read(get_bundled_interactive_asset_path(IMAGE_FILENAME))
        .ok()
        .map(|bytes| {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        });
    guard.1.clone()
}

pub struct EarendilAnnouncementComponent {
    children: Vec<Box<dyn Component>>,
}

impl EarendilAnnouncementComponent {
    pub fn new() -> Self {
        let mut children: Vec<Box<dyn Component>> = Vec::new();

        children.push(Box::new(DynamicBorder::new(Box::new(|text: &str| {
            theme().fg("accent", text)
        }))));
        children.push(Box::new(Text::new(
            theme().bold(&theme().fg("accent", "pi has joined Earendil")),
            1,
            0,
            None,
        )));
        children.push(Box::new(Spacer::new(1)));
        children.push(Box::new(Text::new(
            theme().fg("muted", "Read the blog post:"),
            1,
            0,
            None,
        )));
        children.push(Box::new(Text::new(
            theme().fg("mdLink", BLOG_URL),
            1,
            0,
            None,
        )));
        children.push(Box::new(Spacer::new(1)));

        if let Some(image_base64) = load_image_base64() {
            children.push(Box::new(Image::new(
                image_base64,
                "image/png".to_string(),
                ImageTheme {
                    fallback_color: Box::new(|text: &str| theme().fg("muted", text)),
                },
                ImageOptions {
                    max_width_cells: Some(56),
                    filename: Some(IMAGE_FILENAME.to_string()),
                    fallback_only: true,
                    ..Default::default()
                },
                None,
            )));
            children.push(Box::new(Spacer::new(1)));
        }

        children.push(Box::new(DynamicBorder::new(Box::new(|text: &str| {
            theme().fg("accent", text)
        }))));

        Self { children }
    }

    pub fn children_len(&self) -> usize {
        self.children.len()
    }
}

impl Default for EarendilAnnouncementComponent {
    fn default() -> Self {
        Self::new()
    }
}

impl Component for EarendilAnnouncementComponent {
    fn invalidate(&mut self) {
        for child in self.children.iter_mut() {
            child.invalidate();
        }
    }

    fn render(&mut self, width: f64) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for child in self.children.iter_mut() {
            lines.extend(child.render(width));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_tui::utils::strip_ansi;

    #[test]
    fn renders_the_announcement_text_between_two_borders() {
        let mut component = EarendilAnnouncementComponent::new();
        let lines = component.render(40.0);
        let plain: Vec<String> = lines.iter().map(|line| strip_ansi(line)).collect();
        assert!(plain[0].starts_with('\u{2500}'));
        assert!(plain
            .iter()
            .any(|line| line.contains("pi has joined Earendil")));
        assert!(plain
            .iter()
            .any(|line| line.contains("Read the blog post:")));
        assert!(plain.iter().any(|line| line.contains(BLOG_URL)));
        assert!(plain.last().unwrap().starts_with('\u{2500}'));
    }

    #[test]
    fn image_is_loaded_once_and_cached() {
        let first = load_image_base64();
        let second = load_image_base64();
        assert_eq!(first, second);
    }
}
