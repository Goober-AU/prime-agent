//! Port of packages/tui/src/components/image.ts

use std::cell::Cell;

use crate::terminal_image::{
    allocate_image_id, get_capabilities, get_image_dimensions, image_fallback, render_image, ImageDimensions,
    ImageProtocol, ImageRenderOptions,
};
use crate::tui::Component;

thread_local! {
    /// Port of the module-level `fullscreenFallback` flag.
    static FULLSCREEN_FALLBACK: Cell<bool> = const { Cell::new(false) };
}

pub fn with_fullscreen_image_fallback<T>(render: impl FnOnce() -> T) -> T {
    let previous = FULLSCREEN_FALLBACK.with(|flag| flag.replace(true));
    let result = render();
    FULLSCREEN_FALLBACK.with(|flag| flag.set(previous));
    result
}

fn fullscreen_fallback() -> bool {
    FULLSCREEN_FALLBACK.with(|flag| flag.get())
}

/// Port of `ImageTheme`.
pub struct ImageTheme {
    pub fallback_color: Box<dyn Fn(&str) -> String>,
}

/// Port of `ImageOptions`.
#[derive(Clone, Default)]
pub struct ImageOptions {
    pub max_width_cells: Option<usize>,
    pub max_height_cells: Option<usize>,
    pub filename: Option<String>,
    /// Renders textual image metadata instead of terminal graphics.
    pub fallback_only: bool,
    /// Prefix prepended to textual fallback metadata.
    pub fallback_prefix: Option<String>,
    /// Kitty image ID to reuse across updates or animations.
    pub image_id: Option<u32>,
}

pub struct Image {
    base64_data: String,
    mime_type: String,
    dimensions: ImageDimensions,
    theme: ImageTheme,
    options: ImageOptions,
    image_id: Option<u32>,

    cached_lines: Option<Vec<String>>,
    cached_width: Option<usize>,
    cached_fullscreen_fallback: Option<bool>,
}

impl Image {
    pub fn new(
        base64_data: String,
        mime_type: String,
        theme: ImageTheme,
        options: ImageOptions,
        dimensions: Option<ImageDimensions>,
    ) -> Self {
        let dimensions = dimensions
            .or_else(|| get_image_dimensions(&base64_data, &mime_type))
            .unwrap_or(ImageDimensions {
                width_px: 800,
                height_px: 600,
            });
        let image_id = options.image_id;
        Self {
            base64_data,
            mime_type,
            dimensions,
            theme,
            options,
            image_id,
            cached_lines: None,
            cached_width: None,
            cached_fullscreen_fallback: None,
        }
    }

    /// Returns the Kitty image ID allocated or supplied for this image.
    pub fn get_image_id(&self) -> Option<u32> {
        self.image_id
    }

    pub fn dimensions(&self) -> &ImageDimensions {
        &self.dimensions
    }
}

impl Component for Image {
    fn render(&mut self, width: f64) -> Vec<String> {
        let width = width.max(0.0).floor() as usize;
        let fallback_flag = fullscreen_fallback();
        if let (Some(lines), Some(cached_width), Some(cached_fallback)) = (
            &self.cached_lines,
            self.cached_width,
            self.cached_fullscreen_fallback,
        ) {
            if cached_width == width && cached_fallback == fallback_flag {
                return lines.clone();
            }
        }

        // `Math.min(width - 2, maxWidthCells ?? 60)`; the subtraction stays in i64
        // because TypeScript allows a negative result for width < 2.
        let max_width = (width as i64 - 2).min(self.options.max_width_cells.unwrap_or(60) as i64);

        let caps = get_capabilities();
        let lines: Vec<String>;

        if fallback_flag || self.options.fallback_only {
            let mut parts: Vec<String> = vec![self.mime_type.clone()];
            parts.push(format!("{}×{}", self.dimensions.width_px, self.dimensions.height_px));
            if let Some(filename) = &self.options.filename {
                parts.insert(0, filename.clone());
            }
            lines = vec![(self.theme.fallback_color)(&format!(
                "{}[{}]",
                self.options.fallback_prefix.clone().unwrap_or_default(),
                parts.join(" · ")
            ))];
        } else if caps.images.is_some() {
            if caps.images == Some(ImageProtocol::Kitty) && self.image_id.is_none() {
                self.image_id = Some(allocate_image_id());
            }
            let result = render_image(
                &self.base64_data,
                &self.dimensions,
                &ImageRenderOptions {
                    max_width_cells: Some(max_width.max(0)),
                    max_height_cells: None,
                    preserve_aspect_ratio: Some(true),
                    image_id: self.image_id,
                    move_cursor: Some(false),
                },
            );

            match result {
                Some(result) => {
                    if let Some(image_id) = result.image_id {
                        self.image_id = Some(image_id);
                    }

                    // Return `rows` lines so TUI accounts for image height.
                    // First (rows-1) lines are empty and cleared before the image is drawn.
                    // Last line: move cursor back up, draw the image, then move back down
                    // for Kitty (this component disables Kitty's terminal-side cursor movement)
                    // so TUI cursor accounting stays inside the scroll area.
                    let mut rendered: Vec<String> = Vec::new();
                    for _ in 0..result.rows.saturating_sub(1) {
                        rendered.push(String::new());
                    }
                    let row_offset = result.rows.saturating_sub(1);
                    let move_up = if row_offset > 0 {
                        format!("\x1b[{row_offset}A")
                    } else {
                        String::new()
                    };
                    let move_down = if caps.images == Some(ImageProtocol::Kitty) && row_offset > 0 {
                        format!("\x1b[{row_offset}B")
                    } else {
                        String::new()
                    };
                    rendered.push(format!("{move_up}{}{move_down}", result.sequence));
                    lines = rendered;
                }
                None => {
                    let fallback = image_fallback(
                        &self.mime_type,
                        Some(&self.dimensions),
                        self.options.filename.as_deref(),
                    );
                    lines = vec![(self.theme.fallback_color)(&fallback)];
                }
            }
        } else {
            let fallback = image_fallback(
                &self.mime_type,
                Some(&self.dimensions),
                self.options.filename.as_deref(),
            );
            lines = vec![(self.theme.fallback_color)(&fallback)];
        }

        self.cached_lines = Some(lines.clone());
        self.cached_width = Some(width);
        self.cached_fullscreen_fallback = Some(fallback_flag);

        lines
    }

    fn invalidate(&mut self) {
        self.cached_lines = None;
        self.cached_width = None;
        self.cached_fullscreen_fallback = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_fallback_flag_is_restored() {
        assert!(!fullscreen_fallback());
        let inner = with_fullscreen_image_fallback(|| fullscreen_fallback());
        assert!(inner);
        assert!(!fullscreen_fallback());
    }

    #[test]
    fn fallback_line_uses_metadata_parts() {
        let image = Image::new(
            String::new(),
            "image/png".to_string(),
            ImageTheme {
                fallback_color: Box::new(|text: &str| text.to_string()),
            },
            ImageOptions {
                filename: Some("cat.png".to_string()),
                fallback_only: true,
                fallback_prefix: Some("> ".to_string()),
                ..ImageOptions::default()
            },
            Some(ImageDimensions {
                width_px: 10,
                height_px: 20,
            }),
        );
        let mut image = image;
        let lines = image.render(40.0);
        assert_eq!(lines, vec!["> [cat.png · image/png · 10×20]".to_string()]);
    }
}
