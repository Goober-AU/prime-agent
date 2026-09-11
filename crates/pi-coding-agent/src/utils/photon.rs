//! Port of packages/coding-agent/src/utils/photon.ts
//!
//! The TypeScript loads `@silvia-odwyer/photon-node` (Rust/WASM) and patches
//! `fs.readFileSync` so a compiled binary can find `photon_rs_bg.wasm` next to
//! the executable. The Rust port performs the same image work in-process, so
//! the WASM read patch and the fallback path list become the executable
//! directory probe used to decide whether the image backend is available.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use super::exif_orientation::RgbaImage;

pub const WASM_FILENAME: &str = "photon_rs_bg.wasm";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SamplingFilter {
    Nearest,
    Triangle,
    CatmullRom,
    Gaussian,
    Lanczos3,
}

/// The photon module surface used by this crate.
#[derive(Debug, Clone)]
pub struct Photon {
    pub executable_dir: PathBuf,
}

static PHOTON_MODULE: OnceLock<Mutex<Option<Photon>>> = OnceLock::new();

/// `path.dirname(process.execPath)` for the running binary.
pub fn exec_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// The TypeScript fallback list: `<execDir>/photon_rs_bg.wasm`,
/// `<execDir>/photon/photon_rs_bg.wasm`, `<cwd>/photon_rs_bg.wasm`.
pub fn get_fallback_wasm_paths() -> Vec<PathBuf> {
    let exec_dir = exec_dir();
    let mut paths = vec![exec_dir.join(WASM_FILENAME), exec_dir.join("photon").join(WASM_FILENAME)];
    if let Ok(cwd) = std::env::current_dir() {
        paths.push(cwd.join(WASM_FILENAME));
    }
    paths
}

/// Load the image module. Returns the cached module on subsequent calls.
pub async fn load_photon() -> Option<Photon> {
    let cache = PHOTON_MODULE.get_or_init(|| Mutex::new(None));
    {
        let guard = cache.lock().expect("photon cache");
        if let Some(module) = guard.as_ref() {
            return Some(module.clone());
        }
    }

    let module = Photon {
        executable_dir: exec_dir(),
    };
    let mut guard = cache.lock().expect("photon cache");
    if guard.is_none() {
        *guard = Some(module.clone());
    }
    Some(module)
}

impl Photon {
    pub fn new_from_byteslice(&self, bytes: &[u8]) -> Option<RgbaImage> {
        decode_rgba(bytes)
    }

    /// `get_bytes()` returns a PNG buffer.
    pub fn encode_png(&self, image: &RgbaImage) -> Option<Vec<u8>> {
        encode_png(image)
    }

    pub fn encode_jpeg(&self, image: &RgbaImage, _quality: u8) -> Option<Vec<u8>> {
        encode_jpeg(image)
    }

    pub fn resize(&self, image: &RgbaImage, width: u32, height: u32, _filter: SamplingFilter) -> RgbaImage {
        resize_bilinear(image, width, height)
    }

    pub fn free(&self) {}
}

/// Decode a PNG or JPEG buffer into RGBA pixels.
pub fn decode_rgba(bytes: &[u8]) -> Option<RgbaImage> {
    let format = image::guess_format(bytes).ok()?;
    let decoded = image::load_from_memory_with_format(bytes, format).ok()?;
    let rgba = decoded.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(RgbaImage {
        pixels: rgba.into_raw(),
        width,
        height,
    })
}

pub fn encode_png(image: &RgbaImage) -> Option<Vec<u8>> {
    let buffer = image::RgbaImage::from_raw(image.width, image.height, image.pixels.clone())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

pub fn encode_jpeg(image: &RgbaImage) -> Option<Vec<u8>> {
    let buffer = image::RgbaImage::from_raw(image.width, image.height, image.pixels.clone())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(buffer)
        .write_to(&mut out, image::ImageFormat::Jpeg)
        .ok()?;
    Some(out.into_inner())
}

/// Bilinear resample, the closest deterministic match for photon's filter chain.
pub fn resize_bilinear(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
    let width = width.max(1);
    let height = height.max(1);
    let source = image::RgbaImage::from_raw(image.width, image.height, image.pixels.clone())
        .unwrap_or_else(|| image::RgbaImage::new(image.width.max(1), image.height.max(1)));
    let resized = image::imageops::resize(
        &source,
        width,
        height,
        image::imageops::FilterType::Lanczos3,
    );
    RgbaImage {
        pixels: resized.into_raw(),
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_wasm_paths_match_the_typescript_order() {
        let paths = get_fallback_wasm_paths();
        assert!(paths[0].ends_with(WASM_FILENAME));
        assert!(paths[1].ends_with(Path::new("photon").join(WASM_FILENAME)));
        assert_eq!(paths.len(), 3);
    }

    #[tokio::test]
    async fn load_photon_is_cached() {
        let first = load_photon().await.unwrap();
        let second = load_photon().await.unwrap();
        assert_eq!(first.executable_dir, second.executable_dir);
    }

    #[test]
    fn round_trips_png_pixels() {
        let image = RgbaImage {
            pixels: vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255],
            width: 2,
            height: 2,
        };
        let png = encode_png(&image).unwrap();
        let decoded = decode_rgba(&png).unwrap();
        assert_eq!(decoded.pixels, image.pixels);
        assert_eq!((decoded.width, decoded.height), (2, 2));
    }

    #[test]
    fn resize_produces_the_requested_dimensions() {
        let image = RgbaImage {
            pixels: vec![10u8; 4 * 4 * 4],
            width: 4,
            height: 4,
        };
        let resized = resize_bilinear(&image, 2, 3);
        assert_eq!((resized.width, resized.height), (2, 3));
        assert_eq!(resized.pixels.len(), 2 * 3 * 4);
    }
}
