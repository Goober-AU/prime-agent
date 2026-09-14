//! Port of packages/tui/src/terminal-colors.ts.

use once_cell::sync::Lazy;
use regex::Regex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Rgb {
    pub r: i64,
    pub g: i64,
    pub b: i64,
}

/// Terminal default foreground/background colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultTerminalColors {
    pub foreground: Rgb,
    pub background: Rgb,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalBackgroundKind {
    Dark,
    Light,
}

impl TerminalBackgroundKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            TerminalBackgroundKind::Dark => "dark",
            TerminalBackgroundKind::Light => "light",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalColorMode {
    Truecolor,
    Color256,
    Ansi16,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OscColorKind {
    Foreground,
    Background,
}

impl OscColorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            OscColorKind::Foreground => "foreground",
            OscColorKind::Background => "background",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OscColorResponse {
    pub kind: OscColorKind,
    pub rgb: Rgb,
}

pub const QUERY_DEFAULT_FOREGROUND: &str = "\x1b]10;?\x1b\\";
pub const QUERY_DEFAULT_BACKGROUND: &str = "\x1b]11;?\x1b\\";

const CUBE_VALUES: [i64; 6] = [0, 95, 135, 175, 215, 255];

fn gray_values() -> [i64; 24] {
    let mut out = [0i64; 24];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = 8 + i as i64 * 10;
    }
    out
}

/// `let defaultTerminalColors` (terminal-colors.ts:28): ONE cell for the whole
/// process. `getDefaultTerminalColors`/`setDefaultTerminalColors`
/// (terminal-colors.ts:180-187) and the OSC 10/11 probe that writes it
/// (terminal.rs:382,535) must observe the same value, exactly like the
/// module-level variable in the TypeScript module.
static DEFAULT_TERMINAL_COLORS: Mutex<Option<DefaultTerminalColors>> = Mutex::new(None);

/// `const defaultColorListeners = new Set<() => void>()` (terminal-colors.ts:26):
/// also process-wide, in insertion order. A listener must be callable from the
/// thread that finishes the probe.
static DEFAULT_COLOR_LISTENERS: Mutex<Vec<(u64, Arc<dyn Fn() + Send + Sync>)>> =
    Mutex::new(Vec::new());
static DEFAULT_COLOR_LISTENER_IDS: AtomicU64 = AtomicU64::new(0);

fn color_listeners() -> std::sync::MutexGuard<'static, Vec<(u64, Arc<dyn Fn() + Send + Sync>)>> {
    DEFAULT_COLOR_LISTENERS
        .lock()
        .expect("terminal color listeners")
}

fn clamp_channel(value: f64) -> i64 {
    value.round().clamp(0.0, 255.0) as i64
}

fn normalize_hex_channel(value: &str) -> Option<i64> {
    if value.is_empty() || value.len() < 2 || value.len() > 4 || !value.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let parsed = i64::from_str_radix(value, 16).ok()?;
    if value.len() <= 2 {
        return Some(parsed);
    }
    let max = 16f64.powi(value.len() as i32) - 1.0;
    Some((parsed as f64 / max * 255.0).round() as i64)
}

static RGB_PAYLOAD_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^rgb:([0-9a-fA-F]{2,4})/([0-9a-fA-F]{2,4})/([0-9a-fA-F]{2,4})$").unwrap()
});
static HEX_PAYLOAD_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^#?([0-9a-fA-F]{6})$").unwrap());
static OSC_COLOR_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\x1b\](10|11);([^\x07\x1b]+)(?:\x07|\x1b\\)$").unwrap());

fn parse_rgb_payload(payload: &str) -> Option<Rgb> {
    if let Some(caps) = RGB_PAYLOAD_PATTERN.captures(payload) {
        let r = normalize_hex_channel(caps.get(1)?.as_str());
        let g = normalize_hex_channel(caps.get(2)?.as_str());
        let b = normalize_hex_channel(caps.get(3)?.as_str());
        return match (r, g, b) {
            (Some(r), Some(g), Some(b)) => Some(Rgb { r, g, b }),
            _ => None,
        };
    }

    let caps = HEX_PAYLOAD_PATTERN.captures(payload)?;
    let hex = caps.get(1)?.as_str();
    Some(Rgb {
        r: i64::from_str_radix(&hex[0..2], 16).ok()?,
        g: i64::from_str_radix(&hex[2..4], 16).ok()?,
        b: i64::from_str_radix(&hex[4..6], 16).ok()?,
    })
}

fn color_distance(a: &Rgb, b: &Rgb) -> f64 {
    let dr = (a.r - b.r) as f64;
    let dg = (a.g - b.g) as f64;
    let db = (a.b - b.b) as f64;
    dr * dr * 0.299 + dg * dg * 0.587 + db * db * 0.114
}

fn find_closest_index(value: i64, values: &[i64]) -> usize {
    let mut min_dist = f64::INFINITY;
    let mut min_idx = 0usize;
    for (i, v) in values.iter().enumerate() {
        let dist = (value - *v).abs() as f64;
        if dist < min_dist {
            min_dist = dist;
            min_idx = i;
        }
    }
    min_idx
}

/// `notifyDefaultColorListeners` (terminal-colors.ts:90-94): call every
/// registered listener. The listener set is copied out first so a listener that
/// (un)subscribes during the notification cannot deadlock or skip an entry.
fn notify_default_color_listeners() {
    let listeners: Vec<Arc<dyn Fn() + Send + Sync>> = color_listeners()
        .iter()
        .map(|(_, listener)| listener.clone())
        .collect();
    for listener in listeners {
        listener();
    }
}

pub fn rgb_to_hex(rgb: &Rgb) -> String {
    let to_hex = |value: i64| format!("{:02x}", clamp_channel(value as f64));
    format!("#{}{}{}", to_hex(rgb.r), to_hex(rgb.g), to_hex(rgb.b))
}

pub fn is_light_color(rgb: &Rgb) -> bool {
    0.299 * rgb.r as f64 + 0.587 * rgb.g as f64 + 0.114 * rgb.b as f64 > 128.0
}

pub fn blend_color(top: &Rgb, bottom: &Rgb, alpha: f64) -> Rgb {
    let clamped_alpha = alpha.clamp(0.0, 1.0);
    Rgb {
        r: clamp_channel(top.r as f64 * clamped_alpha + bottom.r as f64 * (1.0 - clamped_alpha)),
        g: clamp_channel(top.g as f64 * clamped_alpha + bottom.g as f64 * (1.0 - clamped_alpha)),
        b: clamp_channel(top.b as f64 * clamped_alpha + bottom.b as f64 * (1.0 - clamped_alpha)),
    }
}

pub fn rgb_to_256(rgb: &Rgb) -> i64 {
    let r_idx = find_closest_index(rgb.r, &CUBE_VALUES);
    let g_idx = find_closest_index(rgb.g, &CUBE_VALUES);
    let b_idx = find_closest_index(rgb.b, &CUBE_VALUES);
    let cube_rgb = Rgb {
        r: CUBE_VALUES[r_idx],
        g: CUBE_VALUES[g_idx],
        b: CUBE_VALUES[b_idx],
    };
    let cube_index = 16 + 36 * r_idx as i64 + 6 * g_idx as i64 + b_idx as i64;
    let cube_dist = color_distance(rgb, &cube_rgb);

    let gray = (0.299 * rgb.r as f64 + 0.587 * rgb.g as f64 + 0.114 * rgb.b as f64).round() as i64;
    let grays = gray_values();
    let gray_idx = find_closest_index(gray, &grays);
    let gray_value = grays[gray_idx];
    let gray_rgb = Rgb {
        r: gray_value,
        g: gray_value,
        b: gray_value,
    };
    let gray_index = 232 + gray_idx as i64;
    let gray_dist = color_distance(rgb, &gray_rgb);

    let max_channel = rgb.r.max(rgb.g).max(rgb.b);
    let min_channel = rgb.r.min(rgb.g).min(rgb.b);
    if max_channel - min_channel < 10 && gray_dist < cube_dist {
        return gray_index;
    }

    cube_index
}

/// `bestAnsiColor` returns either a hex string, a 256-colour index, or "".
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum BestAnsiColor {
    Hex(String),
    Index(i64),
}

pub fn best_ansi_color(rgb: &Rgb, mode: TerminalColorMode) -> BestAnsiColor {
    match mode {
        TerminalColorMode::Truecolor => BestAnsiColor::Hex(rgb_to_hex(rgb)),
        TerminalColorMode::Color256 => BestAnsiColor::Index(rgb_to_256(rgb)),
        _ => BestAnsiColor::Hex(String::new()),
    }
}

pub fn parse_osc_color_response(sequence: &str) -> Option<OscColorResponse> {
    let caps = OSC_COLOR_PATTERN.captures(sequence)?;
    let rgb = parse_rgb_payload(caps.get(2)?.as_str())?;
    Some(OscColorResponse {
        kind: if caps.get(1)?.as_str() == "10" {
            OscColorKind::Foreground
        } else {
            OscColorKind::Background
        },
        rgb,
    })
}

pub fn detect_background_from_color_fg_bg(value: Option<&str>) -> Option<TerminalBackgroundKind> {
    let owned;
    let value = match value {
        Some(v) => v,
        None => {
            owned = std::env::var("COLORFGBG").ok();
            match owned.as_deref() {
                Some(v) if !v.is_empty() => v,
                _ => return None,
            }
        }
    };
    if value.is_empty() {
        return None;
    }
    let parts: Vec<&str> = value.split(';').collect();
    if parts.len() < 2 {
        return None;
    }
    let bg: i64 = parts[1].parse().ok()?;
    Some(if bg < 8 {
        TerminalBackgroundKind::Dark
    } else {
        TerminalBackgroundKind::Light
    })
}

pub fn get_default_terminal_colors() -> Option<DefaultTerminalColors> {
    *DEFAULT_TERMINAL_COLORS
        .lock()
        .expect("default terminal colors")
}

pub fn set_default_terminal_colors(colors: Option<DefaultTerminalColors>) {
    *DEFAULT_TERMINAL_COLORS
        .lock()
        .expect("default terminal colors") = colors;
    notify_default_color_listeners();
}

pub fn clear_default_terminal_colors() {
    set_default_terminal_colors(None);
}

pub fn get_terminal_background_kind() -> Option<TerminalBackgroundKind> {
    let colors = get_default_terminal_colors();
    if let Some(bg) = colors.map(|c| c.background) {
        return Some(if is_light_color(&bg) {
            TerminalBackgroundKind::Light
        } else {
            TerminalBackgroundKind::Dark
        });
    }
    detect_background_from_color_fg_bg(None)
}

/// Register a listener; the returned id removes it again (port of the
/// unsubscribe closure returned by `onDefaultTerminalColorsChange`,
/// terminal-colors.ts:201-206).
pub fn on_default_terminal_colors_change(listener: Arc<dyn Fn() + Send + Sync>) -> u64 {
    let id = DEFAULT_COLOR_LISTENER_IDS.fetch_add(1, Ordering::SeqCst) + 1;
    color_listeners().push((id, listener));
    id
}

pub fn remove_default_terminal_colors_listener(id: u64) {
    color_listeners().retain(|(listener_id, _)| *listener_id != id);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_lightness() {
        assert_eq!(rgb_to_hex(&Rgb { r: 1, g: 2, b: 3 }), "#010203");
        assert!(is_light_color(&Rgb {
            r: 255,
            g: 255,
            b: 255
        }));
        assert!(!is_light_color(&Rgb { r: 0, g: 0, b: 0 }));
    }

    #[test]
    fn blend_clamps_alpha() {
        let top = Rgb {
            r: 255,
            g: 0,
            b: 0,
        };
        let bottom = Rgb {
            r: 0,
            g: 0,
            b: 255,
        };
        assert_eq!(blend_color(&top, &bottom, 1.0), top);
        assert_eq!(blend_color(&top, &bottom, 0.0), bottom);
    }

    #[test]
    fn rgb_to_256_maps_grays_and_cube() {
        assert_eq!(
            rgb_to_256(&Rgb {
                r: 128,
                g: 128,
                b: 128
            }),
            244
        );
        assert_eq!(rgb_to_256(&Rgb { r: 0, g: 0, b: 0 }), 16);
    }

    #[test]
    fn parses_osc_color_responses() {
        let fg = parse_osc_color_response("\x1b]10;rgb:ffff/ffff/ffff\x1b\\").unwrap();
        assert_eq!(fg.kind, OscColorKind::Foreground);
        assert_eq!(fg.rgb, Rgb { r: 255, g: 255, b: 255 });
        let bg = parse_osc_color_response("\x1b]11;#000000\x07").unwrap();
        assert_eq!(bg.kind, OscColorKind::Background);
        assert_eq!(parse_osc_color_response("nope"), None);
    }

    #[test]
    fn set_and_get_share_one_store_and_notify_listeners() {
        // terminal-colors.ts:28 is ONE module-level variable that
        // `getDefaultTerminalColors` reads (terminal-colors.ts:180-182) and
        // `setDefaultTerminalColors` writes (terminal-colors.ts:184-187). This is
        // exactly the value the OSC 10/11 probe publishes
        // (packages/tui/src/terminal.ts:348) and the theme subscribes to
        // (packages/coding-agent/.../theme.ts:847).
        clear_default_terminal_colors();
        let hits = Arc::new(AtomicU64::new(0));
        let seen = hits.clone();
        let id = on_default_terminal_colors_change(Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        }));
        let colors = DefaultTerminalColors {
            foreground: Rgb { r: 1, g: 2, b: 3 },
            background: Rgb {
                r: 240,
                g: 240,
                b: 240,
            },
        };
        set_default_terminal_colors(Some(colors));
        assert_eq!(
            get_default_terminal_colors(),
            Some(colors),
            "the setter must be observable through the getter"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 1, "a set notifies once");
        assert_eq!(
            get_terminal_background_kind(),
            Some(TerminalBackgroundKind::Light)
        );
        clear_default_terminal_colors();
        assert_eq!(get_default_terminal_colors(), None);
        assert_eq!(hits.load(Ordering::SeqCst), 2, "a clear also notifies");
        remove_default_terminal_colors_listener(id);
        set_default_terminal_colors(Some(colors));
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "a removed listener stays removed"
        );
        clear_default_terminal_colors();
    }

    #[test]
    fn colorfgbg_detection() {
        assert_eq!(
            detect_background_from_color_fg_bg(Some("15;0")),
            Some(TerminalBackgroundKind::Dark)
        );
        assert_eq!(
            detect_background_from_color_fg_bg(Some("0;15")),
            Some(TerminalBackgroundKind::Light)
        );
        assert_eq!(detect_background_from_color_fg_bg(Some("15")), None);
    }
}
