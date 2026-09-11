//! Port of packages/coding-agent/src/cli/file-processor.ts
//!
//! TODO(slice): `resolveReadPath` (ca-tools slice, core/tools/path-utils.ts),
//! `resizeImage`/`formatDimensionNote` (ca-utils slice, utils/image-resize.ts)
//! and `detectSupportedImageMimeTypeFromFile` (ca-utils slice, utils/mime.ts)
//! are not landed. Private local stand-ins with the same behaviour live below
//! and are listed in the slice status file.

use std::path::{Path, PathBuf};

use base64::Engine;
use pi_ai::types::ImageContent;

use crate::utils::mime::detect_supported_image_mime_type_from_file;

pub struct ProcessedFiles {
    pub text: String,
    pub images: Vec<ImageContent>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessFileOptions {
    pub auto_resize_images: Option<bool>,
}

/// Callback surface for the TypeScript `console.error` / `process.exit(1)` pair.
pub struct FileProcessorIo<'a> {
    pub error: &'a dyn Fn(&str),
    pub exit: &'a dyn Fn(i32),
}

pub async fn process_file_arguments(
    file_args: &[String],
    options: Option<ProcessFileOptions>,
    io: &FileProcessorIo<'_>,
) -> ProcessedFiles {
    let auto_resize_images = options.and_then(|options| options.auto_resize_images).unwrap_or(true);
    let mut text = String::new();
    let mut images: Vec<ImageContent> = Vec::new();

    for file_arg in file_args {
        let absolute_path = resolve_read_path(file_arg, &current_cwd()).to_string_lossy().to_string();

        if !Path::new(&absolute_path).exists() {
            (io.error)(&format!("Error: File not found: {}", absolute_path));
            (io.exit)(1);
            return ProcessedFiles { text, images };
        }

        let size = match std::fs::metadata(&absolute_path) {
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        };
        if size == 0 {
            continue;
        }

        let mime_type = detect_supported_image_mime_type_from_file(&absolute_path).await.ok().flatten();

        if let Some(mime_type) = mime_type {
            let content = match std::fs::read(&absolute_path) {
                Ok(content) => content,
                Err(_) => Vec::new(),
            };
            let base64_content = base64::engine::general_purpose::STANDARD.encode(&content);

            let attachment: ImageContent;
            let dimension_note: Option<String>;

            if auto_resize_images {
                let resized = resize_image(&ImageContent::new(base64_content, &mime_type), None).await;
                match resized {
                    None => {
                        text += &format!(
                            "<file name=\"{}\">[Image omitted: could not be resized below the inline image size limit.]</file>\n",
                            absolute_path
                        );
                        continue;
                    }
                    Some(resized) => {
                        dimension_note = format_dimension_note(&resized);
                        attachment = ImageContent::new(resized.data, resized.mime_type);
                    }
                }
            } else {
                dimension_note = None;
                attachment = ImageContent::new(base64_content, mime_type);
            }

            images.push(attachment);

            match dimension_note {
                Some(note) => {
                    text += &format!("<file name=\"{}\">{}</file>\n", absolute_path, note);
                }
                None => {
                    text += &format!("<file name=\"{}\"></file>\n", absolute_path);
                }
            }
        } else {
            match std::fs::read_to_string(&absolute_path) {
                Ok(content) => {
                    text += &format!("<file name=\"{}\">\n{}\n</file>\n", absolute_path, content);
                }
                Err(error) => {
                    (io.error)(&format!(
                        "Error: Could not read file {}: {}",
                        absolute_path,
                        error.to_string()
                    ));
                    (io.exit)(1);
                    return ProcessedFiles { text, images };
                }
            }
        }
    }

    ProcessedFiles { text, images }
}

fn current_cwd() -> String {
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Private local stand-ins for not-yet-landed slices.
// ---------------------------------------------------------------------------

/// Local stand-in for `resolveReadPath(filePath, cwd)` from
/// ../core/tools/path-utils.js: tilde expansion plus cwd resolution. The
/// macOS/NFD/curly-quote fallbacks of the full implementation are not needed to
/// place a file argument, and the file-existence check above already reports a
/// missing path.
fn resolve_read_path(file_path: &str, cwd: &str) -> PathBuf {
    let stripped = file_path.strip_prefix('@').unwrap_or(file_path);
    let expanded = expand_tilde_path(stripped);
    let candidate = Path::new(&expanded);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        Path::new(cwd).join(candidate)
    }
}

/// Local stand-in for `expandTildePath` from ../config.js.
fn expand_tilde_path(path: &str) -> String {
    if path == "~" {
        return home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return Path::new(&home_dir()).join(rest).to_string_lossy().to_string();
    }
    if cfg!(target_os = "windows") {
        if let Some(rest) = path.strip_prefix("~\\") {
            return Path::new(&home_dir()).join(rest).to_string_lossy().to_string();
        }
    }
    path.to_string()
}

fn home_dir() -> String {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default()
}

/// Local stand-in for `ResizedImage` from ../utils/image-resize.js.
struct ResizedImage {
    data: String,
    mime_type: String,
    original_width: u32,
    original_height: u32,
    width: u32,
    height: u32,
    was_resized: bool,
}

/// Local stand-in for `formatDimensionNote` from ../utils/image-resize.js.
fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized {
        return None;
    }
    let scale = result.original_width as f64 / result.width as f64;
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {:.2} to map to original image.]",
        result.original_width, result.original_height, result.width, result.height, scale
    ))
}

/// Local stand-in for `resizeImage` from ../utils/image-resize.js: decodes with
/// the `image` crate and re-encodes until the base64 payload fits the limit.
async fn resize_image(img: &ImageContent, options: Option<ResizeOptions>) -> Option<ResizedImage> {
    let options = options.unwrap_or_default();
    let input = base64::engine::general_purpose::STANDARD.decode(img.data.as_bytes()).ok()?;
    let input_base64_size = img.data.len() as f64;

    let decoded = image::load_from_memory(&input).ok()?;
    let original_width = decoded.width();
    let original_height = decoded.height();
    let format = img.mime_type.split('/').nth(1).unwrap_or("png").to_string();

    if (original_width as f64) <= options.max_width
        && (original_height as f64) <= options.max_height
        && input_base64_size < options.max_bytes
    {
        return Some(ResizedImage {
            data: img.data.clone(),
            mime_type: if img.mime_type.is_empty() {
                format!("image/{}", format)
            } else {
                img.mime_type.clone()
            },
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    let mut target_width = original_width as f64;
    let mut target_height = original_height as f64;
    if target_width > options.max_width {
        target_height = (target_height * options.max_width / target_width).round();
        target_width = options.max_width;
    }
    if target_height > options.max_height {
        target_width = (target_width * options.max_height / target_height).round();
        target_height = options.max_height;
    }

    let mut current_width = target_width.max(1.0) as u32;
    let mut current_height = target_height.max(1.0) as u32;
    let qualities: Vec<u8> = dedupe_qualities(options.jpeg_quality);

    loop {
        for candidate in try_encodings(&decoded, current_width, current_height, &qualities) {
            if (candidate.encoded_size as f64) < options.max_bytes {
                return Some(ResizedImage {
                    data: candidate.data,
                    mime_type: candidate.mime_type,
                    original_width,
                    original_height,
                    width: current_width,
                    height: current_height,
                    was_resized: true,
                });
            }
        }

        if current_width == 1 && current_height == 1 {
            break;
        }
        let next_width = if current_width == 1 {
            1
        } else {
            ((current_width as f64 * 0.75).floor() as u32).max(1)
        };
        let next_height = if current_height == 1 {
            1
        } else {
            ((current_height as f64 * 0.75).floor() as u32).max(1)
        };
        if next_width == current_width && next_height == current_height {
            break;
        }
        current_width = next_width;
        current_height = next_height;
    }

    None
}

#[derive(Clone, Copy)]
struct ResizeOptions {
    max_width: f64,
    max_height: f64,
    max_bytes: f64,
    jpeg_quality: u8,
}

impl Default for ResizeOptions {
    fn default() -> Self {
        Self {
            max_width: 2000.0,
            max_height: 2000.0,
            // 4.5MB of base64 payload, below Anthropic's 5MB limit.
            max_bytes: 4.5 * 1024.0 * 1024.0,
            jpeg_quality: 80,
        }
    }
}

fn dedupe_qualities(quality: u8) -> Vec<u8> {
    let mut qualities: Vec<u8> = Vec::new();
    for candidate in [quality, 85, 70, 55, 40] {
        if !qualities.contains(&candidate) {
            qualities.push(candidate);
        }
    }
    qualities
}

struct EncodedCandidate {
    data: String,
    encoded_size: usize,
    mime_type: String,
}

fn try_encodings(
    decoded: &image::DynamicImage,
    width: u32,
    height: u32,
    qualities: &[u8],
) -> Vec<EncodedCandidate> {
    let resized = decoded.resize_exact(width, height, image::imageops::FilterType::Lanczos3);
    let mut candidates: Vec<EncodedCandidate> = Vec::new();

    let mut png_bytes: Vec<u8> = Vec::new();
    if resized
        .write_to(&mut std::io::Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .is_ok()
    {
        candidates.push(encode_candidate(&png_bytes, "image/png"));
    }

    for quality in qualities {
        let mut jpeg_bytes: Vec<u8> = Vec::new();
        let mut cursor = std::io::Cursor::new(&mut jpeg_bytes);
        if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, *quality)
            .encode_image(&resized)
            .is_ok()
        {
            candidates.push(encode_candidate(&jpeg_bytes, "image/jpeg"));
        }
    }

    candidates
}

fn encode_candidate(buffer: &[u8], mime_type: &str) -> EncodedCandidate {
    let data = base64::engine::general_purpose::STANDARD.encode(buffer);
    EncodedCandidate { encoded_size: data.len(), data, mime_type: mime_type.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_note_is_absent_when_not_resized() {
        let result = ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_string(),
            original_width: 100,
            original_height: 50,
            width: 100,
            height: 50,
            was_resized: false,
        };
        assert_eq!(format_dimension_note(&result), None);
    }

    #[test]
    fn dimension_note_reports_the_scale_factor() {
        let result = ResizedImage {
            data: String::new(),
            mime_type: "image/png".to_string(),
            original_width: 4000,
            original_height: 2000,
            width: 2000,
            height: 1000,
            was_resized: true,
        };
        assert_eq!(
            format_dimension_note(&result).unwrap(),
            "[Image: original 4000x2000, displayed at 2000x1000. Multiply coordinates by 2.00 to map to original image.]"
        );
    }

    #[test]
    fn tilde_expansion_matches_the_config_helper() {
        let home = home_dir();
        assert_eq!(expand_tilde_path("~"), home);
        if !home.is_empty() {
            assert!(expand_tilde_path("~/x").starts_with(&home));
            assert!(expand_tilde_path("~/x").ends_with("x"));
        }
        assert_eq!(expand_tilde_path("plain.txt"), "plain.txt");
    }

    #[test]
    fn resolve_read_path_strips_the_at_prefix_and_joins_cwd() {
        let resolved = resolve_read_path("@notes.txt", "/base");
        assert_eq!(resolved.to_string_lossy(), Path::new("/base").join("notes.txt").to_string_lossy());
    }

    #[tokio::test]
    async fn skips_empty_files_and_reports_missing_ones() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.txt");
        std::fs::write(&empty, b"").unwrap();

        let errors: std::cell::RefCell<Vec<String>> = std::cell::RefCell::new(Vec::new());
        let exits: std::cell::RefCell<Vec<i32>> = std::cell::RefCell::new(Vec::new());
        let error_fn = |message: &str| errors.borrow_mut().push(message.to_string());
        let exit_fn = |code: i32| exits.borrow_mut().push(code);
        let io = FileProcessorIo { error: &error_fn, exit: &exit_fn };

        let processed = process_file_arguments(&[empty.to_string_lossy().to_string()], None, &io).await;
        assert_eq!(processed.text, "");
        assert!(processed.images.is_empty());

        let missing = dir.path().join("missing.txt");
        let processed = process_file_arguments(&[missing.to_string_lossy().to_string()], None, &io).await;
        assert!(processed.text.is_empty());
        assert_eq!(exits.borrow().as_slice(), &[1]);
        assert!(errors.borrow()[0].starts_with("Error: File not found: "));
    }

    #[tokio::test]
    async fn wraps_text_files_in_a_file_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "hello").unwrap();

        let error_fn = |_: &str| {};
        let exit_fn = |_: i32| {};
        let io = FileProcessorIo { error: &error_fn, exit: &exit_fn };

        let processed = process_file_arguments(&[path.to_string_lossy().to_string()], None, &io).await;
        assert_eq!(
            processed.text,
            format!("<file name=\"{}\">\nhello\n</file>\n", path.to_string_lossy())
        );
        assert!(processed.images.is_empty());
    }
}
