//! Port of packages/coding-agent/src/utils/mime.ts

use std::io::Read;
use std::path::Path;

/// The TypeScript `file-type` sniff window.
const FILE_TYPE_SNIFF_BYTES: usize = 4100;

pub fn image_mime_types() -> [&'static str; 4] {
    ["image/jpeg", "image/png", "image/gif", "image/webp"]
}

pub fn is_image_mime_type(mime_type: &str) -> bool {
    image_mime_types().contains(&mime_type)
}

pub async fn detect_supported_image_mime_type_from_file(file_path: &str) -> std::io::Result<Option<String>> {
    let path = file_path.to_string();
    tokio::task::spawn_blocking(move || detect_supported_image_mime_type_from_file_sync(&path))
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))?
}

pub fn detect_supported_image_mime_type_from_file_sync(file_path: &str) -> std::io::Result<Option<String>> {
    let mut file = std::fs::File::open(Path::new(file_path))?;
    let mut buffer = vec![0u8; FILE_TYPE_SNIFF_BYTES];
    let bytes_read = file.read(&mut buffer)?;
    if bytes_read == 0 {
        return Ok(None);
    }

    let file_type = match file_type_from_buffer(&buffer[..bytes_read]) {
        Some(file_type) => file_type,
        None => return Ok(None),
    };

    if !is_image_mime_type(file_type) {
        return Ok(None);
    }

    Ok(Some(file_type.to_string()))
}

/// Magic-number sniff for the four supported image formats.
///
/// `file-type` also detects many non-image formats; only the supported image
/// magic numbers are needed here, so anything else returns `None` exactly like a
/// sniff miss does in the TypeScript.
pub fn file_type_from_buffer(buffer: &[u8]) -> Option<&'static str> {
    if buffer.len() >= 8 && buffer.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        return Some("image/png");
    }
    if buffer.len() >= 3 && buffer.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some("image/jpeg");
    }
    if buffer.len() >= 6 && (buffer.starts_with(b"GIF87a") || buffer.starts_with(b"GIF89a")) {
        return Some("image/gif");
    }
    if buffer.len() >= 12 && buffer.starts_with(b"RIFF") && &buffer[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_the_supported_image_formats() {
        assert_eq!(
            file_type_from_buffer(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0]),
            Some("image/png")
        );
        assert_eq!(file_type_from_buffer(&[0xff, 0xd8, 0xff, 0xe0]), Some("image/jpeg"));
        assert_eq!(file_type_from_buffer(b"GIF89a____"), Some("image/gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(file_type_from_buffer(&webp), Some("image/webp"));
    }

    #[test]
    fn returns_none_for_unknown_or_empty_buffers() {
        assert_eq!(file_type_from_buffer(b""), None);
        assert_eq!(file_type_from_buffer(b"hello world"), None);
    }

    #[test]
    fn detects_from_a_real_file_and_ignores_empty_files() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("empty.png");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(
            detect_supported_image_mime_type_from_file_sync(empty.to_str().unwrap()).unwrap(),
            None
        );

        let png = dir.path().join("image.png");
        std::fs::write(&png, [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).unwrap();
        assert_eq!(
            detect_supported_image_mime_type_from_file_sync(png.to_str().unwrap()).unwrap(),
            Some("image/png".to_string())
        );

        let text = dir.path().join("notes.txt");
        std::fs::write(&text, b"plain text").unwrap();
        assert_eq!(
            detect_supported_image_mime_type_from_file_sync(text.to_str().unwrap()).unwrap(),
            None
        );
    }
}
