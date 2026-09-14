//! Port of packages/tui/src/terminal-image.ts.

use base64::Engine;
use rand::Rng;
use std::cell::RefCell;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    Kitty,
    Iterm2,
}

/// `null` in the TypeScript protocol union.
pub type ImageProtocolOption = Option<ImageProtocol>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    pub images: ImageProtocolOption,
    pub true_color: bool,
    pub hyperlinks: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDimensions {
    pub width_px: i64,
    pub height_px: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageDimensions {
    pub width_px: i64,
    pub height_px: i64,
}

#[derive(Debug, Clone, Default)]
pub struct ImageRenderOptions {
    pub max_width_cells: Option<i64>,
    pub max_height_cells: Option<i64>,
    pub preserve_aspect_ratio: Option<bool>,
    /// Kitty image ID. If provided, reuses/replaces existing image with this ID.
    pub image_id: Option<u32>,
    /// Whether Kitty should apply its default cursor movement after placement.
    pub move_cursor: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedImage {
    pub sequence: String,
    pub rows: usize,
    pub image_id: Option<u32>,
}

thread_local! {
    static CACHED_CAPABILITIES: RefCell<Option<TerminalCapabilities>> = const { RefCell::new(None) };
    static CELL_DIMENSIONS: RefCell<CellDimensions> = const {
        RefCell::new(CellDimensions { width_px: 9, height_px: 18 })
    };
}

pub fn get_cell_dimensions() -> CellDimensions {
    CELL_DIMENSIONS.with(|c| *c.borrow())
}

pub fn set_cell_dimensions(dims: CellDimensions) {
    CELL_DIMENSIONS.with(|c| *c.borrow_mut() = dims);
}

fn env_lower(name: &str) -> String {
    std::env::var(name).unwrap_or_default().to_lowercase()
}

pub fn detect_capabilities() -> TerminalCapabilities {
    let term_program = env_lower("TERM_PROGRAM");
    let term = env_lower("TERM");
    let color_term = env_lower("COLORTERM");

    // tmux and screen swallow OSC 8 by default (passthrough is opt-in and wraps
    // sequences differently). Force hyperlinks off whenever we detect them.
    let in_tmux_or_screen = std::env::var("TMUX").is_ok()
        || term.starts_with("tmux")
        || term.starts_with("screen");
    if in_tmux_or_screen {
        let true_color = color_term == "truecolor" || color_term == "24bit";
        return TerminalCapabilities {
            images: None,
            true_color,
            hyperlinks: false,
        };
    }

    if std::env::var("KITTY_WINDOW_ID").is_ok() || term_program == "kitty" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "ghostty" || term.contains("ghostty") || std::env::var("GHOSTTY_RESOURCES_DIR").is_ok() {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if std::env::var("WEZTERM_PANE").is_ok() || term_program == "wezterm" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Kitty),
            true_color: true,
            hyperlinks: true,
        };
    }

    if std::env::var("ITERM_SESSION_ID").is_ok() || term_program == "iterm.app" {
        return TerminalCapabilities {
            images: Some(ImageProtocol::Iterm2),
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "vscode" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    if term_program == "alacritty" {
        return TerminalCapabilities {
            images: None,
            true_color: true,
            hyperlinks: true,
        };
    }

    let true_color = color_term == "truecolor" || color_term == "24bit";
    TerminalCapabilities {
        images: None,
        true_color,
        hyperlinks: false,
    }
}

pub fn get_capabilities() -> TerminalCapabilities {
    CACHED_CAPABILITIES.with(|c| {
        let mut slot = c.borrow_mut();
        if slot.is_none() {
            *slot = Some(detect_capabilities());
        }
        slot.unwrap()
    })
}

pub fn reset_capabilities_cache() {
    CACHED_CAPABILITIES.with(|c| *c.borrow_mut() = None);
}

/// Override the cached capabilities. Useful in tests to exercise both code paths.
pub fn set_capabilities(caps: TerminalCapabilities) {
    CACHED_CAPABILITIES.with(|c| *c.borrow_mut() = Some(caps));
}

const KITTY_PREFIX: &str = "\x1b_G";
const ITERM2_PREFIX: &str = "\x1b]1337;File=";

pub fn is_image_line(line: &str) -> bool {
    // Fast path: sequence at line start (single-row images)
    if line.starts_with(KITTY_PREFIX) || line.starts_with(ITERM2_PREFIX) {
        return true;
    }
    // Slow path: sequence elsewhere (multi-row images have cursor-up prefix)
    line.contains(KITTY_PREFIX) || line.contains(ITERM2_PREFIX)
}

/// Generate a random image ID for Kitty graphics protocol.
pub fn allocate_image_id() -> u32 {
    // Use random ID in range [1, 0xffffffff] to avoid collisions
    rand::thread_rng().gen_range(1..=0xffff_fffeu32)
}

#[derive(Debug, Clone, Default)]
pub struct KittyEncodeOptions {
    pub columns: Option<i64>,
    pub rows: Option<usize>,
    pub image_id: Option<u32>,
    /// Whether Kitty should apply its default cursor movement after placement. Default: true.
    pub move_cursor: Option<bool>,
}

pub fn encode_kitty(base64_data: &str, options: &KittyEncodeOptions) -> String {
    const CHUNK_SIZE: usize = 4096;

    let mut params: Vec<String> = vec!["a=T".to_string(), "f=100".to_string(), "q=2".to_string()];

    if options.move_cursor == Some(false) {
        params.push("C=1".to_string());
    }
    if let Some(columns) = options.columns {
        if columns != 0 {
            params.push(format!("c={columns}"));
        }
    }
    if let Some(rows) = options.rows {
        if rows != 0 {
            params.push(format!("r={rows}"));
        }
    }
    if let Some(image_id) = options.image_id {
        if image_id != 0 {
            params.push(format!("i={image_id}"));
        }
    }

    if base64_data.len() <= CHUNK_SIZE {
        return format!("\x1b_G{};{}\x1b\\", params.join(","), base64_data);
    }

    let mut chunks: Vec<String> = Vec::new();
    let mut offset = 0usize;
    let mut is_first = true;

    while offset < base64_data.len() {
        let end = (offset + CHUNK_SIZE).min(base64_data.len());
        let chunk = &base64_data[offset..end];
        let is_last = offset + CHUNK_SIZE >= base64_data.len();

        if is_first {
            chunks.push(format!("\x1b_G{},m=1;{}\x1b\\", params.join(","), chunk));
            is_first = false;
        } else if is_last {
            chunks.push(format!("\x1b_Gm=0;{chunk}\x1b\\"));
        } else {
            chunks.push(format!("\x1b_Gm=1;{chunk}\x1b\\"));
        }

        offset += CHUNK_SIZE;
    }

    chunks.join("")
}

/// Delete a Kitty graphics image by ID.
pub fn delete_kitty_image(image_id: u32) -> String {
    format!("\x1b_Ga=d,d=I,i={image_id},q=2\x1b\\")
}

/// Delete all visible Kitty graphics images.
pub fn delete_all_kitty_images() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

#[derive(Debug, Clone, Default)]
pub struct Iterm2EncodeOptions {
    pub width: Option<String>,
    pub height: Option<String>,
    pub name: Option<String>,
    pub preserve_aspect_ratio: Option<bool>,
    pub inline: Option<bool>,
}

pub fn encode_iterm2(base64_data: &str, options: &Iterm2EncodeOptions) -> String {
    let mut params: Vec<String> = vec![format!("inline={}", if options.inline != Some(false) { 1 } else { 0 })];

    if let Some(width) = &options.width {
        params.push(format!("width={width}"));
    }
    if let Some(height) = &options.height {
        params.push(format!("height={height}"));
    }
    if let Some(name) = &options.name {
        if !name.is_empty() {
            let name_base64 = base64::engine::general_purpose::STANDARD.encode(name);
            params.push(format!("name={name_base64}"));
        }
    }
    if options.preserve_aspect_ratio == Some(false) {
        params.push("preserveAspectRatio=0".to_string());
    }

    format!("\x1b]1337;File={}:{}\x07", params.join(";"), base64_data)
}

pub fn calculate_image_rows(image_dimensions: &ImageDimensions, target_width_cells: i64) -> usize {
    let cell_dimensions = get_cell_dimensions();
    calculate_image_rows_with_cells(image_dimensions, target_width_cells, &cell_dimensions)
}

pub fn calculate_image_rows_with_cells(
    image_dimensions: &ImageDimensions,
    target_width_cells: i64,
    cell_dimensions: &CellDimensions,
) -> usize {
    let target_width_px = (target_width_cells * cell_dimensions.width_px) as f64;
    let scale = if image_dimensions.width_px == 0 {
        0.0
    } else {
        target_width_px / image_dimensions.width_px as f64
    };
    let scaled_height_px = image_dimensions.height_px as f64 * scale;
    let rows = if cell_dimensions.height_px == 0 {
        0.0
    } else {
        (scaled_height_px / cell_dimensions.height_px as f64).ceil()
    };
    (rows as i64).max(1) as usize
}

fn decode_base64(base64_data: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(base64_data)
        .ok()
}

pub fn get_png_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 24 {
        return None;
    }
    if buffer[0] != 0x89 || buffer[1] != 0x50 || buffer[2] != 0x4e || buffer[3] != 0x47 {
        return None;
    }
    let width = u32::from_be_bytes([buffer[16], buffer[17], buffer[18], buffer[19]]);
    let height = u32::from_be_bytes([buffer[20], buffer[21], buffer[22], buffer[23]]);
    Some(ImageDimensions {
        width_px: width as i64,
        height_px: height as i64,
    })
}

pub fn get_jpeg_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 2 {
        return None;
    }
    if buffer[0] != 0xff || buffer[1] != 0xd8 {
        return None;
    }

    let mut offset = 2usize;
    while offset + 9 < buffer.len() {
        if buffer[offset] != 0xff {
            offset += 1;
            continue;
        }
        let marker = buffer[offset + 1];
        if (0xc0..=0xc2).contains(&marker) {
            let height = u16::from_be_bytes([buffer[offset + 5], buffer[offset + 6]]);
            let width = u16::from_be_bytes([buffer[offset + 7], buffer[offset + 8]]);
            return Some(ImageDimensions {
                width_px: width as i64,
                height_px: height as i64,
            });
        }
        if offset + 3 >= buffer.len() {
            return None;
        }
        let length = u16::from_be_bytes([buffer[offset + 2], buffer[offset + 3]]) as usize;
        if length < 2 {
            return None;
        }
        offset += 2 + length;
    }
    None
}

pub fn get_gif_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 10 {
        return None;
    }
    let sig = String::from_utf8_lossy(&buffer[0..6]).to_string();
    if sig != "GIF87a" && sig != "GIF89a" {
        return None;
    }
    let width = u16::from_le_bytes([buffer[6], buffer[7]]);
    let height = u16::from_le_bytes([buffer[8], buffer[9]]);
    Some(ImageDimensions {
        width_px: width as i64,
        height_px: height as i64,
    })
}

pub fn get_webp_dimensions(base64_data: &str) -> Option<ImageDimensions> {
    let buffer = decode_base64(base64_data)?;
    if buffer.len() < 30 {
        return None;
    }
    let riff = String::from_utf8_lossy(&buffer[0..4]).to_string();
    let webp = String::from_utf8_lossy(&buffer[8..12]).to_string();
    if riff != "RIFF" || webp != "WEBP" {
        return None;
    }

    let chunk = String::from_utf8_lossy(&buffer[12..16]).to_string();
    if chunk == "VP8 " {
        if buffer.len() < 30 {
            return None;
        }
        let width = u16::from_le_bytes([buffer[26], buffer[27]]) & 0x3fff;
        let height = u16::from_le_bytes([buffer[28], buffer[29]]) & 0x3fff;
        return Some(ImageDimensions {
            width_px: width as i64,
            height_px: height as i64,
        });
    } else if chunk == "VP8L" {
        if buffer.len() < 25 {
            return None;
        }
        let bits = u32::from_le_bytes([buffer[21], buffer[22], buffer[23], buffer[24]]);
        let width = (bits & 0x3fff) + 1;
        let height = ((bits >> 14) & 0x3fff) + 1;
        return Some(ImageDimensions {
            width_px: width as i64,
            height_px: height as i64,
        });
    } else if chunk == "VP8X" {
        if buffer.len() < 30 {
            return None;
        }
        let width = (buffer[24] as i64 | (buffer[25] as i64) << 8 | (buffer[26] as i64) << 16) + 1;
        let height = (buffer[27] as i64 | (buffer[28] as i64) << 8 | (buffer[29] as i64) << 16) + 1;
        return Some(ImageDimensions {
            width_px: width,
            height_px: height,
        });
    }

    None
}

pub fn get_image_dimensions(base64_data: &str, mime_type: &str) -> Option<ImageDimensions> {
    match mime_type {
        "image/png" => get_png_dimensions(base64_data),
        "image/jpeg" => get_jpeg_dimensions(base64_data),
        "image/gif" => get_gif_dimensions(base64_data),
        "image/webp" => get_webp_dimensions(base64_data),
        _ => None,
    }
}

pub fn render_image(
    base64_data: &str,
    image_dimensions: &ImageDimensions,
    options: &ImageRenderOptions,
) -> Option<RenderedImage> {
    let caps = get_capabilities();
    let protocol = caps.images?;

    let max_width = options.max_width_cells.unwrap_or(80);
    let rows = calculate_image_rows(image_dimensions, max_width);

    match protocol {
        ImageProtocol::Kitty => {
            let sequence = encode_kitty(
                base64_data,
                &KittyEncodeOptions {
                    columns: Some(max_width),
                    rows: Some(rows),
                    image_id: options.image_id,
                    move_cursor: options.move_cursor,
                },
            );
            Some(RenderedImage {
                sequence,
                rows,
                image_id: options.image_id,
            })
        }
        ImageProtocol::Iterm2 => {
            let sequence = encode_iterm2(
                base64_data,
                &Iterm2EncodeOptions {
                    width: Some(max_width.to_string()),
                    height: Some("auto".to_string()),
                    name: None,
                    preserve_aspect_ratio: Some(options.preserve_aspect_ratio.unwrap_or(true)),
                    inline: None,
                },
            );
            Some(RenderedImage {
                sequence,
                rows,
                image_id: None,
            })
        }
    }
}

/// Wrap text in an OSC 8 hyperlink sequence.
pub fn hyperlink(text: &str, url: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
}

pub fn image_fallback(
    mime_type: &str,
    dimensions: Option<&ImageDimensions>,
    filename: Option<&str>,
) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_1X1: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    #[test]
    fn detects_png_dimensions() {
        let dims = get_png_dimensions(PNG_1X1).unwrap();
        assert_eq!(dims.width_px, 1);
        assert_eq!(dims.height_px, 1);
        assert!(get_png_dimensions("not-base64!!").is_none());
    }

    #[test]
    fn kitty_encoding_chunks_and_params() {
        let single = encode_kitty(
            "AAAA",
            &KittyEncodeOptions {
                columns: Some(4),
                rows: Some(2),
                image_id: Some(7),
                move_cursor: None,
            },
        );
        assert_eq!(single, "\x1b_Ga=T,f=100,q=2,c=4,r=2,i=7;AAAA\x1b\\");

        let big = "A".repeat(5000);
        let chunked = encode_kitty(&big, &KittyEncodeOptions::default());
        assert!(chunked.contains(",m=1;"));
        assert!(chunked.contains("\x1b_Gm=0;"));
    }

    #[test]
    fn iterm2_encoding_includes_name() {
        let encoded = encode_iterm2("AAAA", &Iterm2EncodeOptions {
            width: Some("10".to_string()),
            height: Some("auto".to_string()),
            name: Some("x".to_string()),
            preserve_aspect_ratio: Some(false),
            inline: None,
        });
        assert_eq!(
            encoded,
            "\x1b]1337;File=inline=1;width=10;height=auto;name=eA==;preserveAspectRatio=0:AAAA\x07"
        );
    }

    #[test]
    fn image_rows_use_cell_dimensions() {
        let dims = ImageDimensions {
            width_px: 100,
            height_px: 50,
        };
        assert_eq!(
            calculate_image_rows_with_cells(&dims, 10, &CellDimensions { width_px: 10, height_px: 10 }),
            5
        );
    }

    #[test]
    fn image_line_detection_and_fallback() {
        assert!(is_image_line("\x1b_Ga=T;AAAA\x1b\\"));
        assert!(is_image_line("\x1b]1337;File=inline=1:AAAA\x07"));
        assert!(!is_image_line("plain"));
        assert_eq!(
            image_fallback("image/png", Some(&ImageDimensions { width_px: 2, height_px: 3 }), Some("a.png")),
            "[Image: a.png [image/png] 2x3]"
        );
    }

    #[test]
    fn hyperlink_wraps_text() {
        assert_eq!(
            hyperlink("x", "https://e.test"),
            "\x1b]8;;https://e.test\x1b\\x\x1b]8;;\x1b\\"
        );
    }
}
