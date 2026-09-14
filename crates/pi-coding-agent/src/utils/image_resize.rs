//! Port of packages/coding-agent/src/utils/image-resize.ts

use base64::Engine as _;

use super::exif_orientation::apply_exif_orientation;
use super::photon::{load_photon, SamplingFilter};

/// The `ImageContent` shape from pi-ai's `types.ts`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ImageContent {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ImageResizeOptions {
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_bytes: Option<f64>,
    pub jpeg_quality: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct ResizedImage {
    pub data: String,
    pub mime_type: String,
    pub original_width: u32,
    pub original_height: u32,
    pub width: u32,
    pub height: u32,
    pub was_resized: bool,
}

// 4.5MB of base64 payload. Provides headroom below Anthropic's 5MB limit.
const DEFAULT_MAX_BYTES: f64 = 4.5 * 1024.0 * 1024.0;

pub struct ResolvedImageResizeOptions {
    pub max_width: u32,
    pub max_height: u32,
    pub max_bytes: f64,
    pub jpeg_quality: u8,
}

impl Default for ResolvedImageResizeOptions {
    fn default() -> Self {
        Self {
            max_width: 2000,
            max_height: 2000,
            max_bytes: DEFAULT_MAX_BYTES,
            jpeg_quality: 80,
        }
    }
}

impl ResolvedImageResizeOptions {
    pub fn from_options(options: Option<ImageResizeOptions>) -> Self {
        let defaults = Self::default();
        let Some(options) = options else {
            return defaults;
        };
        Self {
            max_width: options.max_width.unwrap_or(defaults.max_width),
            max_height: options.max_height.unwrap_or(defaults.max_height),
            max_bytes: options.max_bytes.unwrap_or(defaults.max_bytes),
            jpeg_quality: options.jpeg_quality.unwrap_or(defaults.jpeg_quality),
        }
    }
}

struct EncodedCandidate {
    data: String,
    encoded_size: usize,
    mime_type: String,
}

fn encode_candidate(buffer: Vec<u8>, mime_type: &str) -> EncodedCandidate {
    let data = base64::engine::general_purpose::STANDARD.encode(&buffer);
    EncodedCandidate {
        encoded_size: data.len(),
        data,
        mime_type: mime_type.to_string(),
    }
}

/// Resize an image to fit within the specified max dimensions and encoded file size.
/// Returns None if the image cannot be resized below maxBytes.
///
/// Strategy for staying under maxBytes:
/// 1. First resize to maxWidth/maxHeight
/// 2. Try both PNG and JPEG formats, pick the smaller one
/// 3. If still too large, try JPEG with decreasing quality
/// 4. If still too large, progressively reduce dimensions until 1x1
pub async fn resize_image(img: &ImageContent, options: Option<ImageResizeOptions>) -> Option<ResizedImage> {
    let opts = ResolvedImageResizeOptions::from_options(options);
    let engine = base64::engine::general_purpose::STANDARD;
    let input_buffer = engine.decode(&img.data).ok()?;
    let input_base64_size = img.data.len();

    let photon = load_photon().await?;

    let mut raw_image = photon.new_from_byteslice(&input_buffer)?;
    let rotated = apply_exif_orientation(&mut raw_image, &input_buffer);
    let image = match rotated {
        Some(rotated) => rotated,
        None => raw_image,
    };

    let original_width = image.width;
    let original_height = image.height;
    let format = img
        .mime_type
        .split('/')
        .nth(1)
        .unwrap_or("png")
        .to_string();
    let source_mime_type = if img.mime_type.is_empty() {
        format!("image/{}", format)
    } else {
        img.mime_type.clone()
    };

    // Check if already within all limits (dimensions AND encoded size)
    if original_width <= opts.max_width
        && original_height <= opts.max_height
        && (input_base64_size as f64) < opts.max_bytes
    {
        return Some(ResizedImage {
            data: img.data.clone(),
            mime_type: source_mime_type,
            original_width,
            original_height,
            width: original_width,
            height: original_height,
            was_resized: false,
        });
    }

    // Calculate initial dimensions respecting max limits
    let mut target_width = original_width;
    let mut target_height = original_height;

    if target_width > opts.max_width {
        target_height = ((target_height as f64 * opts.max_width as f64) / target_width as f64).round() as u32;
        target_width = opts.max_width;
    }
    if target_height > opts.max_height {
        target_width = ((target_width as f64 * opts.max_height as f64) / target_height as f64).round() as u32;
        target_height = opts.max_height;
    }

    let try_encodings = |width: u32, height: u32, jpeg_qualities: &[u8]| -> Vec<EncodedCandidate> {
        let resized = photon.resize(&image, width, height, SamplingFilter::Lanczos3);
        let mut candidates: Vec<EncodedCandidate> = Vec::new();
        if let Some(png) = photon.encode_png(&resized) {
            candidates.push(encode_candidate(png, "image/png"));
        }
        for quality in jpeg_qualities {
            if let Some(jpeg) = photon.encode_jpeg(&resized, *quality) {
                candidates.push(encode_candidate(jpeg, "image/jpeg"));
            }
        }
        candidates
    };

    let mut quality_steps: Vec<u8> = vec![opts.jpeg_quality, 85, 70, 55, 40];
    let mut seen: Vec<u8> = Vec::new();
    quality_steps.retain(|quality| {
        if seen.contains(quality) {
            false
        } else {
            seen.push(*quality);
            true
        }
    });

    let mut current_width = target_width;
    let mut current_height = target_height;

    loop {
        let candidates = try_encodings(current_width, current_height, &quality_steps);
        for candidate in candidates {
            if (candidate.encoded_size as f64) < opts.max_bytes {
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

/// Format a dimension note for resized images.
/// This helps the model understand the coordinate mapping.
pub fn format_dimension_note(result: &ResizedImage) -> Option<String> {
    if !result.was_resized {
        return None;
    }

    let scale = result.original_width as f64 / result.width as f64;
    Some(format!(
        "[Image: original {}x{}, displayed at {}x{}. Multiply coordinates by {:.2} to map to original image.]",
        result.original_width,
        result.original_height,
        result.width,
        result.height,
        scale
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::exif_orientation::RgbaImage;

    fn png_image_content(width: u32, height: u32) -> ImageContent {
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for (index, byte) in pixels.iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let image = RgbaImage {
            pixels,
            width,
            height,
        };
        ImageContent {
            kind: "image".to_string(),
            data: base64::engine::general_purpose::STANDARD
                .encode(crate::utils::photon::encode_png(&image).unwrap()),
            mime_type: "image/png".to_string(),
        }
    }

    #[tokio::test]
    async fn small_images_are_not_resized() {
        let img = png_image_content(8, 8);
        let result = resize_image(&img, None).await.unwrap();
        assert!(!result.was_resized);
        assert_eq!(result.data, img.data);
        assert_eq!((result.width, result.height), (8, 8));
        assert_eq!(format_dimension_note(&result), None);
    }

    #[tokio::test]
    async fn oversized_images_are_resized_to_the_limit() {
        let img = png_image_content(400, 200);
        let result = resize_image(
            &img,
            Some(ImageResizeOptions {
                max_width: Some(100),
                max_height: Some(100),
                ..Default::default()
            }),
        )
        .await
        .unwrap();
        assert!(result.was_resized);
        assert!(result.width <= 100);
        assert!(result.height <= 100);
        assert_eq!((result.original_width, result.original_height), (400, 200));
        let note = format_dimension_note(&result).unwrap();
        assert!(note.starts_with("[Image: original 400x200, displayed at "));
    }

    #[tokio::test]
    async fn gives_up_when_the_byte_budget_is_impossible() {
        let img = png_image_content(64, 64);
        let result = resize_image(
            &img,
            Some(ImageResizeOptions {
                max_bytes: Some(1.0),
                ..Default::default()
            }),
        )
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn returns_none_for_undecodable_data() {
        let img = ImageContent {
            kind: "image".to_string(),
            data: base64::engine::general_purpose::STANDARD.encode(b"not an image"),
            mime_type: "image/png".to_string(),
        };
        assert!(resize_image(&img, None).await.is_none());
    }

    #[test]
    fn default_options_match_the_typescript() {
        let opts = ResolvedImageResizeOptions::from_options(None);
        assert_eq!(opts.max_width, 2000);
        assert_eq!(opts.max_height, 2000);
        assert_eq!(opts.max_bytes, 4.5 * 1024.0 * 1024.0);
        assert_eq!(opts.jpeg_quality, 80);
    }
}
