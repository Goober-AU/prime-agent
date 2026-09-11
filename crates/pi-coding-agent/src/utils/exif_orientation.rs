//! Port of packages/coding-agent/src/utils/exif-orientation.ts

#[derive(Debug, Clone)]
pub struct RgbaImage {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

fn read_orientation_from_tiff(bytes: &[u8], tiff_start: usize) -> u16 {
    if tiff_start + 8 > bytes.len() {
        return 1;
    }

    let byte_order = ((bytes[tiff_start] as u16) << 8) | bytes[tiff_start + 1] as u16;
    let le = byte_order == 0x4949;

    let read16 = |pos: usize| -> u16 {
        if le {
            bytes[pos] as u16 | ((bytes[pos + 1] as u16) << 8)
        } else {
            ((bytes[pos] as u16) << 8) | bytes[pos + 1] as u16
        }
    };

    let read32 = |pos: usize| -> u32 {
        if le {
            (bytes[pos] as u32
                | ((bytes[pos + 1] as u32) << 8)
                | ((bytes[pos + 2] as u32) << 16)
                | ((bytes[pos + 3] as u32) << 24))
                as u32
        } else {
            (((bytes[pos] as u32) << 24)
                | ((bytes[pos + 1] as u32) << 16)
                | ((bytes[pos + 2] as u32) << 8)
                | bytes[pos + 3] as u32) as u32
        }
    };

    let ifd_offset = read32(tiff_start + 4) as usize;
    let ifd_start = tiff_start + ifd_offset;
    if ifd_start + 2 > bytes.len() {
        return 1;
    }

    let entry_count = read16(ifd_start);
    for index in 0..entry_count as usize {
        let entry_pos = ifd_start + 2 + index * 12;
        if entry_pos + 12 > bytes.len() {
            return 1;
        }

        if read16(entry_pos) == 0x0112 {
            let value = read16(entry_pos + 8);
            return if (1..=8).contains(&value) { value } else { 1 };
        }
    }

    1
}

fn has_exif_header(bytes: &[u8], offset: usize) -> bool {
    offset + 5 < bytes.len()
        && bytes[offset] == 0x45
        && bytes[offset + 1] == 0x78
        && bytes[offset + 2] == 0x69
        && bytes[offset + 3] == 0x66
        && bytes[offset + 4] == 0x00
        && bytes[offset + 5] == 0x00
}

fn find_jpeg_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 2usize;
    while offset + 1 < bytes.len() {
        if bytes[offset] != 0xff {
            return None;
        }
        let marker = bytes[offset + 1];
        if marker == 0xff {
            offset += 1;
            continue;
        }

        if marker == 0xe1 {
            if offset + 4 >= bytes.len() {
                return None;
            }
            let segment_start = offset + 4;
            if segment_start + 6 > bytes.len() {
                return None;
            }
            if !has_exif_header(bytes, segment_start) {
                return None;
            }
            return Some(segment_start + 6);
        }

        if offset + 4 > bytes.len() {
            return None;
        }
        let length = ((bytes[offset + 2] as usize) << 8) | bytes[offset + 3] as usize;
        offset += 2 + length;
    }

    None
}

fn find_webp_tiff_offset(bytes: &[u8]) -> Option<usize> {
    let mut offset = 12usize;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        // Unsigned: a high-bit chunk size read as negative would walk the scan backward forever.
        let chunk_size = (bytes[offset + 4] as u32
            | ((bytes[offset + 5] as u32) << 8)
            | ((bytes[offset + 6] as u32) << 16)
            | ((bytes[offset + 7] as u32) << 24)) as u32;
        let data_start = offset + 8;

        if chunk_id == b"EXIF" {
            if data_start + chunk_size as usize > bytes.len() {
                return None;
            }
            // Some WebP files have "Exif\0\0" prefix before the TIFF header
            let tiff_start = if chunk_size >= 6 && has_exif_header(bytes, data_start) {
                data_start + 6
            } else {
                data_start
            };
            return Some(tiff_start);
        }

        // RIFF chunks are padded to even size
        offset = data_start + chunk_size as usize + (chunk_size % 2) as usize;
    }

    None
}

pub fn get_exif_orientation(bytes: &[u8]) -> u16 {
    let mut tiff_offset: Option<usize> = None;

    // JPEG: starts with FF D8
    if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 {
        tiff_offset = find_jpeg_tiff_offset(bytes);
    }
    // WebP: starts with RIFF....WEBP
    else if bytes.len() >= 12
        && bytes[0] == 0x52
        && bytes[1] == 0x49
        && bytes[2] == 0x46
        && bytes[3] == 0x46
        && bytes[8] == 0x57
        && bytes[9] == 0x45
        && bytes[10] == 0x42
        && bytes[11] == 0x50
    {
        tiff_offset = find_webp_tiff_offset(bytes);
    }

    match tiff_offset {
        None => 1,
        Some(tiff_offset) => read_orientation_from_tiff(bytes, tiff_offset),
    }
}

type DstIndexFn = fn(u32, u32, u32, u32) -> u32;

/// The photon `rotate90`: destination pixels come from a new image of swapped dimensions.
fn rotate90(image: &RgbaImage, dst_index: DstIndexFn) -> RgbaImage {
    let w = image.width;
    let h = image.height;
    let src = &image.pixels;
    let mut dst = vec![0u8; src.len()];

    for y in 0..h {
        for x in 0..w {
            let src_idx = ((y * w + x) * 4) as usize;
            let dst_idx = (dst_index(x, y, w, h) * 4) as usize;
            dst[dst_idx] = src[src_idx];
            dst[dst_idx + 1] = src[src_idx + 1];
            dst[dst_idx + 2] = src[src_idx + 2];
            dst[dst_idx + 3] = src[src_idx + 3];
        }
    }

    RgbaImage {
        pixels: dst,
        width: h,
        height: w,
    }
}

fn flip_horizontal(image: &mut RgbaImage) {
    let w = image.width as usize;
    let h = image.height as usize;
    for y in 0..h {
        for x in 0..w / 2 {
            let left = (y * w + x) * 4;
            let right = (y * w + (w - 1 - x)) * 4;
            for channel in 0..4 {
                image.pixels.swap(left + channel, right + channel);
            }
        }
    }
}

fn flip_vertical(image: &mut RgbaImage) {
    let w = image.width as usize;
    let h = image.height as usize;
    for y in 0..h / 2 {
        for x in 0..w {
            let top = (y * w + x) * 4;
            let bottom = ((h - 1 - y) * w + x) * 4;
            for channel in 0..4 {
                image.pixels.swap(top + channel, bottom + channel);
            }
        }
    }
}

/// Flip orientations mutate in place. Rotations return a new image (caller must
/// drop the old one when the returned image is a different allocation).
pub fn apply_exif_orientation(image: &mut RgbaImage, original_bytes: &[u8]) -> Option<RgbaImage> {
    let orientation = get_exif_orientation(original_bytes);
    if orientation == 1 {
        return None;
    }

    match orientation {
        2 => {
            flip_horizontal(image);
            None
        }
        3 => {
            flip_horizontal(image);
            flip_vertical(image);
            None
        }
        4 => {
            flip_vertical(image);
            None
        }
        5 => {
            let mut rotated = rotate90(image, |x, y, _w, h| x * h + (h - 1 - y));
            flip_horizontal(&mut rotated);
            Some(rotated)
        }
        6 => Some(rotate90(image, |x, y, _w, h| x * h + (h - 1 - y))),
        7 => {
            let mut rotated = rotate90(image, |x, y, w, h| (w - 1 - x) * h + y);
            flip_horizontal(&mut rotated);
            Some(rotated)
        }
        8 => Some(rotate90(image, |x, y, w, h| (w - 1 - x) * h + y)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal little-endian TIFF header with an orientation tag.
    fn tiff_with_orientation(orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes()); // IFD offset
        tiff.extend_from_slice(&1u16.to_le_bytes()); // entry count
        tiff.extend_from_slice(&0x0112u16.to_le_bytes()); // orientation tag
        tiff.extend_from_slice(&3u16.to_le_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_le_bytes()); // count
        tiff.extend_from_slice(&orientation.to_le_bytes());
        tiff.extend_from_slice(&0u16.to_le_bytes()); // padding to 12 bytes
        tiff.extend_from_slice(&0u32.to_le_bytes()); // next IFD
        tiff
    }

    fn jpeg_with_exif(orientation: u16) -> Vec<u8> {
        let tiff = tiff_with_orientation(orientation);
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe1];
        let length = (tiff.len() + 6 + 2) as u16;
        jpeg.extend_from_slice(&length.to_be_bytes());
        jpeg.extend_from_slice(b"Exif\0\0");
        jpeg.extend_from_slice(&tiff);
        jpeg
    }

    fn webp_with_exif(orientation: u16) -> Vec<u8> {
        let tiff = tiff_with_orientation(orientation);
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[0, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        webp.extend_from_slice(b"EXIF");
        webp.extend_from_slice(&(tiff.len() as u32).to_le_bytes());
        webp.extend_from_slice(b"Exif\0\0");
        webp.extend_from_slice(&tiff);
        webp
    }

    #[test]
    fn reads_orientation_from_jpeg_and_webp() {
        assert_eq!(get_exif_orientation(&jpeg_with_exif(6)), 6);
        assert_eq!(get_exif_orientation(&webp_with_exif(8)), 8);
        assert_eq!(get_exif_orientation(&jpeg_with_exif(1)), 1);
    }

    #[test]
    fn defaults_to_orientation_one() {
        assert_eq!(get_exif_orientation(b""), 1);
        assert_eq!(get_exif_orientation(b"not an image"), 1);
        assert_eq!(get_exif_orientation(&[0xff, 0xd8, 0xff, 0xe0, 0x00]), 1);
    }

    #[test]
    fn rejects_out_of_range_orientation_values() {
        assert_eq!(get_exif_orientation(&jpeg_with_exif(9)), 1);
    }

    fn image(width: u32, height: u32) -> RgbaImage {
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for y in 0..height {
            for x in 0..width {
                let index = ((y * width + x) * 4) as usize;
                pixels[index] = x as u8;
                pixels[index + 1] = y as u8;
                pixels[index + 2] = 0;
                pixels[index + 3] = 255;
            }
        }
        RgbaImage {
            pixels,
            width,
            height,
        }
    }

    #[test]
    fn orientation_one_returns_the_same_image() {
        let mut img = image(2, 2);
        assert!(apply_exif_orientation(&mut img, &jpeg_with_exif(1)).is_none());
    }

    #[test]
    fn rotation_six_swaps_dimensions() {
        let mut img = image(2, 3);
        let rotated = apply_exif_orientation(&mut img, &jpeg_with_exif(6)).unwrap();
        assert_eq!((rotated.width, rotated.height), (3, 2));
    }

    #[test]
    fn flip_orientations_mutate_in_place() {
        let mut img = image(2, 1);
        let before = img.pixels.clone();
        assert!(apply_exif_orientation(&mut img, &jpeg_with_exif(2)).is_none());
        assert_ne!(img.pixels, before);
        assert_eq!((img.width, img.height), (2, 1));
    }
}
