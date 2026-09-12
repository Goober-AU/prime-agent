//! Port of packages/coding-agent/src/themes/optimus-logo.ts

use std::collections::HashMap;
use std::sync::OnceLock;

use pi_tui::terminal_colors::{rgb_to_256, Rgb};

use crate::modes::interactive::theme::theme;
use crate::themes::optimus_logo_data::OPTIMUS_ART;

/// `export const OPTIMUS_ROBOT_LOGO = OPTIMUS_ART.hero.join("\n")`.
///
/// The TypeScript constant is computed once at module load; the Rust port keeps
/// the same once-only join behind an accessor because `join` is not const.
pub fn optimus_robot_logo() -> &'static str {
    static LOGO: OnceLock<String> = OnceLock::new();
    LOGO.get_or_init(|| OPTIMUS_ART.hero.join("\n")).as_str()
}

/// `const tonesByRow = new Map<string, readonly number[]>()` filled from every variant.
fn tones_by_row() -> &'static HashMap<&'static str, &'static [i32]> {
    static TONES: OnceLock<HashMap<&'static str, &'static [i32]>> = OnceLock::new();
    TONES.get_or_init(|| {
        let mut map: HashMap<&'static str, &'static [i32]> = HashMap::new();
        for variant in OPTIMUS_ART.variants {
            for (row, line) in variant.lines.iter().enumerate() {
                if !map.contains_key(*line) {
                    map.insert(*line, variant.tones[row]);
                }
            }
        }
        map
    })
}

/// Select a complete portrait instead of averaging its edges into dense shading.
pub fn get_optimus_logo(max_width: f64, max_rows: f64) -> Vec<String> {
    let width = if max_width.is_finite() {
        max_width.floor().max(1.0) as usize
    } else {
        50
    };
    let rows = if max_rows.is_finite() {
        max_rows.floor().max(1.0) as usize
    } else {
        25
    };
    let selected = OPTIMUS_ART
        .variants
        .iter()
        .find(|variant| {
            variant.lines.len() <= rows
                && variant
                    .lines
                    .iter()
                    .all(|line| line.chars().count() <= width)
        });
    match selected {
        Some(variant) => variant.lines.iter().map(|line| line.to_string()).collect(),
        None => vec!["*".to_string()],
    }
}

pub fn colorize_optimus_logo(text: &str) -> String {
    if std::env::var_os("NO_COLOR").is_some() {
        return text.to_string();
    }
    let theme = theme::theme();
    let palette = if theme.name.as_deref() == Some("light") {
        OPTIMUS_ART.palettes.light
    } else {
        OPTIMUS_ART.palettes.dark
    };
    let colors: Vec<String> = palette
        .iter()
        .map(|hex| {
            let rgb = Rgb {
                r: i64::from_str_radix(&hex[1..3], 16).unwrap_or(0),
                g: i64::from_str_radix(&hex[3..5], 16).unwrap_or(0),
                b: i64::from_str_radix(&hex[5..7], 16).unwrap_or(0),
            };
            if theme.get_color_mode() == theme::ColorMode::Truecolor {
                format!("\u{1b}[38;2;{};{};{}m", rgb.r, rgb.g, rgb.b)
            } else {
                format!("\u{1b}[38;5;{}m", rgb_to_256(&rgb))
            }
        })
        .collect();

    let tones = tones_by_row();
    text.split('\n')
        .map(|line| {
            let line_tones = tones.get(line).copied();
            let mut output = String::new();
            let mut previous: i32 = -1;
            for (index, character) in line.chars().enumerate() {
                if character != ' ' {
                    let tone = match line_tones {
                        Some(tones) if tones[index] >= 0 => tones[index],
                        _ => 3,
                    };
                    if tone != previous {
                        output.push_str(&colors[tone as usize]);
                    }
                    previous = tone;
                }
                output.push(character);
            }
            if previous < 0 {
                output
            } else {
                format!("{output}\u{1b}[39m")
            }
        })
        .collect::<Vec<String>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn robot_logo_joins_the_hero_rows() {
        assert_eq!(optimus_robot_logo().split('\n').count(), OPTIMUS_ART.hero.len());
    }

    #[test]
    fn picks_the_largest_variant_that_fits() {
        let wide = get_optimus_logo(200.0, 200.0);
        assert_eq!(wide.len(), 25);
        let narrow = get_optimus_logo(10.0, 200.0);
        assert_eq!(narrow, vec!["*".to_string()]);
    }

    #[test]
    fn non_finite_limits_use_the_defaults() {
        assert_eq!(get_optimus_logo(f64::NAN, f64::NAN).len(), 25);
        assert_eq!(get_optimus_logo(f64::INFINITY, f64::INFINITY).len(), 25);
    }

    #[test]
    fn tones_cover_every_variant_line() {
        assert_eq!(tones_by_row().len() >= 25, true);
    }
}
