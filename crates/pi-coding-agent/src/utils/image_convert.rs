//! Port of packages/coding-agent/src/utils/image-convert.ts

use base64::Engine as _;

use super::exif_orientation::apply_exif_orientation;
use super::photon::load_photon;

pub struct ConvertedImage {
    pub data: String,
    pub mime_type: String,
}

/// Convert image to PNG format for terminal display.
/// Kitty graphics protocol requires PNG format (f=100).
pub async fn convert_to_png(base64_data: &str, mime_type: &str) -> Option<ConvertedImage> {
    // Already PNG, no conversion needed
    if mime_type == "image/png" {
        return Some(ConvertedImage {
            data: base64_data.to_string(),
            mime_type: mime_type.to_string(),
        });
    }

    let photon = load_photon().await?;

    let engine = base64::engine::general_purpose::STANDARD;
    let bytes = match engine.decode(base64_data) {
        Ok(bytes) => bytes,
        Err(_) => return None,
    };

    let mut raw_image = match photon.new_from_byteslice(&bytes) {
        Some(image) => image,
        None => return None,
    };
    let rotated = apply_exif_orientation(&mut raw_image, &bytes);
    let image = match rotated {
        Some(rotated) => rotated,
        None => raw_image,
    };

    let png_buffer = photon.encode_png(&image)?;
    Some(ConvertedImage {
        data: engine.encode(&png_buffer),
        mime_type: "image/png".to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::photon::encode_png;

    #[tokio::test]
    async fn png_input_is_returned_unchanged() {
        let result = convert_to_png("QUJD", "image/png").await.unwrap();
        assert_eq!(result.data, "QUJD");
        assert_eq!(result.mime_type, "image/png");
    }

    #[tokio::test]
    async fn converts_a_jpeg_to_png() {
        let image = crate::utils::exif_orientation::RgbaImage {
            pixels: vec![255, 0, 0, 255],
            width: 1,
            height: 1,
        };
        let jpeg = crate::utils::photon::encode_jpeg(&image).unwrap();
        let engine = base64::engine::general_purpose::STANDARD;
        let converted = convert_to_png(&engine.encode(&jpeg), "image/jpeg").await.unwrap();
        assert_eq!(converted.mime_type, "image/png");
        let decoded = engine.decode(&converted.data).unwrap();
        assert_eq!(decoded, encode_png(&image).unwrap());
    }

    #[tokio::test]
    async fn returns_none_for_undecodable_payloads() {
        assert!(convert_to_png("!!!not base64!!!", "image/jpeg").await.is_none());
        let engine = base64::engine::general_purpose::STANDARD;
        assert!(convert_to_png(&engine.encode(b"not an image"), "image/jpeg").await.is_none());
    }
}
