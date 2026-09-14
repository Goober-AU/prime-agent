//! Port of packages/coding-agent/src/core/tools/render-utils.ts

use pi_ai::types::ImageContent;
use pi_tui::utils::strip_ansi;

/// Local port of `pi-tui`'s `getImageDimensions` / `imageFallback`.
///
/// `pi_tui::terminal_image` is ported by another slice and is still empty in this
/// workspace, so the two helpers this file needs are implemented here. The
/// observable behaviour is the same: only the four supported image MIME types
/// produce dimensions, and failures return `None`.
pub mod image_dims {
    use base64::Engine as _;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ImageDimensions {
        pub width_px: u32,
        pub height_px: u32,
    }

    pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
        if !matches!(mime_type, "image/png" | "image/jpeg" | "image/gif" | "image/webp") {
            return None;
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(base64_data).ok()?;
        let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let (width_px, height_px) = reader.into_dimensions().ok()?;
        Some(ImageDimensions { width_px, height_px })
    }

    pub fn image_fallback(mime_type: &str, dimensions: Option<&ImageDimensions>, filename: Option<&str>) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(filename) = filename {
            parts.push(filename.to_string());
        }
        parts.push(format!("[{mime_type}]"));
        if let Some(dimensions) = dimensions {
            parts.push(format!("{}x{}", dimensions.width_px, dimensions.height_px));
        }
        format!("[Image: {}]", parts.join(" "))
    }
}

use image_dims::{get_image_dimensions, image_fallback};

/// Minimal stand-in for the interactive theme used by the render helpers.
///
/// The TypeScript code calls `theme.fg(name, text)` / `theme.bold(text)` /
/// `theme.bg(name, text)`. The real theme lives in another slice, so this trait
/// keeps the call sites identical without depending on that module.
pub trait ToolTheme {
    fn fg(&self, name: &str, text: &str) -> String;
    fn bold(&self, text: &str) -> String;
    fn bg(&self, name: &str, text: &str) -> String;
}

/// Identity theme: colours resolve to the plain text.
pub struct PlainTheme;

impl ToolTheme for PlainTheme {
    fn fg(&self, _name: &str, text: &str) -> String {
        text.to_string()
    }

    fn bold(&self, text: &str) -> String {
        text.to_string()
    }

    fn bg(&self, _name: &str, text: &str) -> String {
        text.to_string()
    }
}

/// `sanitizeBinaryOutput` from utils/shell.ts, needed before any width math.
///
/// Filter out characters that crash width measurement: control characters
/// except tab/newline/carriage return, and the Unicode format range FFF9-FFFB.
pub fn sanitize_binary_output(text: &str) -> String {
    text.chars()
        .filter(|&character| {
            let code = character as u32;
            // Allow tab, newline, carriage return
            if code == 0x09 || code == 0x0a || code == 0x0d {
                return true;
            }
            // Filter out control characters (0x00-0x1F, except 0x09, 0x0a, 0x0d)
            if code <= 0x1f {
                return false;
            }
            // Filter out Unicode format characters
            if (0xfff9..=0xfffb).contains(&code) {
                return false;
            }
            true
        })
        .collect()
}

fn home_dir() -> String {
    match dirs::home_dir() {
        Some(home) => home.to_string_lossy().into_owned(),
        None => std::env::var("HOME").unwrap_or_default(),
    }
}

pub fn shorten_path(path: &serde_json::Value) -> String {
    let Some(text) = path.as_str() else {
        return String::new();
    };
    let home = home_dir();
    if text.starts_with(&home) {
        return format!("~{}", &text[home.len()..]);
    }
    text.to_string()
}

/// TypeScript `str(value: unknown): string | null`.
///
/// `None` is the `null` result (invalid type); `Some("")` is the `""` result
/// (`undefined`/`null` input). The distinction is observable in `formatBashCall`.
pub fn str_value(value: Option<&serde_json::Value>) -> Option<String> {
    match value {
        Some(serde_json::Value::String(text)) => Some(text.clone()),
        Some(serde_json::Value::Null) | None => Some(String::new()),
        Some(_) => None,
    }
}

pub fn replace_tabs(text: &str) -> String {
    text.replace('\t', "   ")
}

/// TypeScript `interface TextOutputOptions`.
#[derive(Debug, Clone, Copy)]
pub struct TextOutputOptions {
    /// Whether image fallbacks should parse image dimensions from base64 data.
    pub include_image_dimensions: Option<bool>,
}

impl Default for TextOutputOptions {
    fn default() -> Self {
        Self {
            include_image_dimensions: None,
        }
    }
}

/// One `{ type, text?, data?, mimeType? }` content entry as rendered by the tools.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RenderContentBlock {
    pub r#type: String,
    pub text: Option<String>,
    pub data: Option<String>,
    pub mime_type: Option<String>,
}

impl RenderContentBlock {
    pub fn from_text(text: impl Into<String>) -> Self {
        Self {
            r#type: "text".to_string(),
            text: Some(text.into()),
            data: None,
            mime_type: None,
        }
    }

    pub fn from_image(image: &ImageContent) -> Self {
        Self {
            r#type: "image".to_string(),
            text: None,
            data: Some(image.data.clone()),
            mime_type: Some(image.mime_type.clone()),
        }
    }
}

pub struct RenderResultLike {
    pub content: Vec<RenderContentBlock>,
}

pub fn get_text_output(
    result: Option<&RenderResultLike>,
    show_images: bool,
    options: TextOutputOptions,
) -> String {
    let Some(result) = result else {
        return String::new();
    };

    let text_blocks: Vec<&RenderContentBlock> = result
        .content
        .iter()
        .filter(|block| block.r#type == "text")
        .collect();
    let image_blocks: Vec<&RenderContentBlock> = result
        .content
        .iter()
        .filter(|block| block.r#type == "image")
        .collect();

    let mut output = text_blocks
        .iter()
        .map(|block| {
            sanitize_binary_output(&strip_ansi(block.text.as_deref().unwrap_or(""))).replace('\r', "")
        })
        .collect::<Vec<String>>()
        .join("\n");

    let include_image_dimensions = options.include_image_dimensions.unwrap_or(true);
    if !image_blocks.is_empty() && !show_images {
        let image_indicators = image_blocks
            .iter()
            .map(|image| {
                let mime_type = image
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "image/unknown".to_string());
                let dims = if include_image_dimensions {
                    match (image.data.as_deref(), image.mime_type.as_deref()) {
                        (Some(data), Some(mime)) if !data.is_empty() && !mime.is_empty() => {
                            get_image_dimensions(data, mime)
                        }
                        _ => None,
                    }
                } else {
                    None
                };
                image_fallback(&mime_type, dims.as_ref(), None)
            })
            .collect::<Vec<String>>()
            .join("\n");
        output = if output.is_empty() {
            image_indicator_join(image_indicators)
        } else {
            format!("{output}\n{image_indicators}")
        };
    }

    output
}

fn image_indicator_join(indicators: String) -> String {
    indicators
}

pub fn invalid_arg_text(theme: &dyn ToolTheme) -> String {
    theme.fg("error", "[invalid arg]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shorten_path_replaces_home_prefix() {
        let home = home_dir();
        let value = serde_json::Value::String(format!("{home}/x"));
        assert_eq!(shorten_path(&value), "~/x");
        assert_eq!(shorten_path(&serde_json::json!(42)), "");
        assert_eq!(shorten_path(&serde_json::json!("relative/path")), "relative/path");
    }

    #[test]
    fn str_value_matches_typescript_null_vs_empty() {
        assert_eq!(str_value(Some(&serde_json::json!("abc"))), Some("abc".to_string()));
        assert_eq!(str_value(Some(&serde_json::Value::Null)), Some(String::new()));
        assert_eq!(str_value(None), Some(String::new()));
        assert_eq!(str_value(Some(&serde_json::json!(5))), None);
    }

    #[test]
    fn replace_tabs_uses_three_spaces() {
        assert_eq!(replace_tabs("a\tb"), "a   b");
    }

    #[test]
    fn sanitize_binary_output_keeps_tabs_and_drops_controls() {
        assert_eq!(sanitize_binary_output("a\tb\nc"), "a\tb\nc");
        assert_eq!(sanitize_binary_output("a\u{0}b"), "ab");
        assert_eq!(sanitize_binary_output("a\u{fff9}b"), "ab");
    }

    #[test]
    fn get_text_output_joins_text_blocks_and_strips_ansi() {
        let result = RenderResultLike {
            content: vec![
                RenderContentBlock::from_text("\u{1b}[1mbold\u{1b}[0m"),
                RenderContentBlock::from_text("second\r"),
            ],
        };
        assert_eq!(get_text_output(Some(&result), true, TextOutputOptions::default()), "bold\nsecond");
        assert_eq!(get_text_output(None, true, TextOutputOptions::default()), "");
    }

    #[test]
    fn get_text_output_appends_image_fallback_when_images_hidden() {
        let result = RenderResultLike {
            content: vec![
                RenderContentBlock::from_text("text"),
                RenderContentBlock {
                    r#type: "image".to_string(),
                    text: None,
                    data: Some("not-base64".to_string()),
                    mime_type: Some("image/png".to_string()),
                },
            ],
        };
        assert_eq!(
            get_text_output(Some(&result), false, TextOutputOptions::default()),
            "text\n[Image: [image/png]]"
        );
        // showImages=true keeps only the text blocks
        assert_eq!(get_text_output(Some(&result), true, TextOutputOptions::default()), "text");
    }

    #[test]
    fn invalid_arg_text_uses_error_colour() {
        assert_eq!(invalid_arg_text(&PlainTheme), "[invalid arg]");
    }
}
