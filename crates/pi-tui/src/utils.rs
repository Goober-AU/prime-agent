//! Port of packages/tui/src/utils.ts.

use std::collections::HashMap;

use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_segmentation::UnicodeSegmentation;

// ---------------------------------------------------------------------------
// Grapheme segmentation (Intl.Segmenter replacement)
// ---------------------------------------------------------------------------

/// Grapheme clusters via UAX #29 (the port of `Intl.Segmenter({granularity:"grapheme"})`).
pub fn graphemes(s: &str) -> Vec<String> {
    unicode_segmentation::UnicodeSegmentation::graphemes(s, true)
        .map(|g| g.to_string())
        .collect()
}

#[allow(dead_code)] // Ported for fidelity; only the grapheme path above uses it today.
fn is_regional_indicator(c: char) -> bool {
    (0x1f1e6..=0x1f1ff).contains(&(c as u32))
}

#[allow(dead_code)] // Ported for fidelity; only the grapheme path above uses it today.
fn is_variation_selector(c: char) -> bool {
    let cp = c as u32;
    (0xfe00..=0xfe0f).contains(&cp) || (0xe0100..=0xe01ef).contains(&cp)
}

#[allow(dead_code)] // Ported for fidelity; only the grapheme path above uses it today.
fn is_emoji_modifier(c: char) -> bool {
    let cp = c as u32;
    (0x1f3fb..=0x1f3ff).contains(&cp) || cp == 0x20e3
}

/// Shared grapheme segmenter handle (the TypeScript module keeps one instance).
pub fn get_segmenter() -> Segmenter {
    Segmenter
}

/// Port of the shared `Intl.Segmenter` instance.
pub struct Segmenter;

impl Segmenter {
    pub fn segment(&self, s: &str) -> Vec<String> {
        graphemes(s)
    }
}

// ---------------------------------------------------------------------------
// East Asian width (port of the get-east-asian-width package data)
// ---------------------------------------------------------------------------

#[allow(dead_code)] // The generated tables keep every class from get-east-asian-width.
mod east_asian_width {
    include!("east_asian_width_tables.rs");

    fn is_in_range(ranges: &[u32], code_point: u32) -> bool {
        let mut low = 0usize;
        let mut high = (ranges.len() / 2).wrapping_sub(1);
        if ranges.is_empty() {
            return false;
        }
        loop {
            let mid = (low + high) / 2;
            let i = mid * 2;
            if code_point < ranges[i] {
                if mid == 0 {
                    return false;
                }
                high = mid - 1;
            } else if code_point > ranges[i + 1] {
                low = mid + 1;
            } else {
                return true;
            }
            if low > high {
                return false;
            }
        }
    }

    pub fn is_wide(code_point: u32) -> bool {
        if code_point >= WIDE_FAST_PATH_START && code_point <= WIDE_FAST_PATH_END {
            return true;
        }
        if code_point < WIDE_MINIMAL_CODE_POINT || code_point > WIDE_MAXIMUM_CODE_POINT {
            return false;
        }
        is_in_range(WIDE_RANGES, code_point)
    }

    fn is_full_width(code_point: u32) -> bool {
        if code_point < FULLWIDTH_MINIMAL_CODE_POINT || code_point > FULLWIDTH_MAXIMUM_CODE_POINT {
            return false;
        }
        is_in_range(FULLWIDTH_RANGES, code_point)
    }

    pub fn east_asian_width(code_point: u32) -> usize {
        if is_full_width(code_point) || is_wide(code_point) {
            2
        } else {
            1
        }
    }
}

use east_asian_width::east_asian_width;

// ---------------------------------------------------------------------------
// Grapheme width
// ---------------------------------------------------------------------------

/// `\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Mark}|\p{Surrogate}`
fn is_zero_width_class(c: char) -> bool {
    if is_default_ignorable(c) || is_control(c) || is_surrogate(c) {
        return true;
    }
    matches!(
        get_general_category(c),
        GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::NonspacingMark
    )
}

/// `\p{Default_Ignorable_Code_Point}|\p{Control}|\p{Format}|\p{Mark}|\p{Surrogate}`
fn is_leading_non_printing(c: char) -> bool {
    if is_default_ignorable(c) || is_control(c) || is_surrogate(c) {
        return true;
    }
    matches!(
        get_general_category(c),
        GeneralCategory::Format
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::NonspacingMark
    )
}

/// `\p{Default_Ignorable_Code_Point}` (main ranges).
fn is_default_ignorable(c: char) -> bool {
    let cp = c as u32;
    matches!(
        get_general_category(c),
        GeneralCategory::Control | GeneralCategory::Format
    ) || cp == 0x00ad
        || cp == 0x034f
        || cp == 0x061c
        || cp == 0x115f
        || cp == 0x1160
        || cp == 0x17b4
        || cp == 0x17b5
        || cp == 0x180b
        || cp == 0x180c
        || cp == 0x180d
        || cp == 0x180e
        || cp == 0x180f
        || (0x200b..=0x200f).contains(&cp)
        || (0x202a..=0x202e).contains(&cp)
        || (0x2060..=0x206f).contains(&cp)
        || cp == 0x3164
        || (0xfe00..=0xfe0f).contains(&cp)
        || cp == 0xfeff
        || cp == 0xffa0
        || (0xfff0..=0xfff8).contains(&cp)
        || (0x1bca0..=0x1bca3).contains(&cp)
        || (0x1d173..=0x1d17a).contains(&cp)
        || (0xe0000..=0xe0fff).contains(&cp)
}

/// `\p{Control}`.
fn is_control(c: char) -> bool {
    get_general_category(c) == GeneralCategory::Control
}

/// `\p{Surrogate}` - Rust `char` cannot hold lone surrogates, so this is never true.
fn is_surrogate(_c: char) -> bool {
    false
}

/// Check if a grapheme cluster could possibly be an RGI emoji (fast pre-filter).
fn could_be_emoji(segment: &str) -> bool {
    let cp = segment.chars().next().map(|c| c as u32).unwrap_or(0);
    (0x1f000..=0x1fbff).contains(&cp)
        || (0x2300..=0x23ff).contains(&cp)
        || (0x2600..=0x27bf).contains(&cp)
        || (0x2b50..=0x2b55).contains(&cp)
        || segment.contains('\u{fe0f}')
        || segment.chars().count() > 2
}

/// Match the fully qualified Unicode 17 RGI set used by the TypeScript regex.
fn is_rgi_emoji(segment: &str) -> bool {
    // Lookup also accepts unqualified variants; only the canonical spelling is RGI.
    emojis::get(segment).is_some_and(|emoji| emoji.as_str() == segment)
}

const WIDTH_CACHE_SIZE: usize = 512;

thread_local! {
    static WIDTH_CACHE: std::cell::RefCell<Vec<(String, usize)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn is_printable_ascii(s: &str) -> bool {
    s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

/// Calculate the terminal width of a single grapheme cluster.
fn grapheme_width(segment: &str) -> usize {
    if segment.is_empty() {
        return 0;
    }
    if segment.chars().all(is_zero_width_class) {
        return 0;
    }

    if could_be_emoji(segment) && is_rgi_emoji(segment) {
        return 2;
    }

    // Get base visible codepoint
    let base = {
        let mut start = 0usize;
        for (i, c) in segment.char_indices() {
            if is_leading_non_printing(c) {
                start = i + c.len_utf8();
            } else {
                break;
            }
        }
        &segment[start..]
    };
    let cp = match base.chars().next() {
        Some(c) => c as u32,
        None => return 0,
    };

    if (0x1f1e6..=0x1f1ff).contains(&cp) {
        return 2;
    }

    let mut width = east_asian_width(cp);

    if segment.chars().count() > 1 {
        for c in segment.chars().skip(1) {
            let c = c as u32;
            if (0xff00..=0xffef).contains(&c) {
                width += east_asian_width(c);
            } else if c == 0x0e33 || c == 0x0eb3 {
                width += 1;
            }
        }
    }

    width
}

// ---------------------------------------------------------------------------
// Visible width and content spans
// ---------------------------------------------------------------------------

/// Calculate the visible width of a string in terminal columns.
pub fn visible_width(s: &str) -> usize {
    if s.is_empty() {
        return 0;
    }
    if is_printable_ascii(s) {
        return s.len();
    }

    if let Some(cached) =
        WIDTH_CACHE.with(|c| c.borrow().iter().find(|(k, _)| k == s).map(|(_, v)| *v))
    {
        return cached;
    }

    let mut clean = s.to_string();
    if s.contains('\t') {
        clean = clean.replace('\t', "   ");
    }
    if clean.contains('\x1b') {
        let mut stripped = String::new();
        let mut i = 0usize;
        let extractor = AnsiCodeExtractor::new(&clean);
        while i < clean.len() {
            if let Some(ansi) = extractor.get(&clean, i) {
                i += ansi.length;
                continue;
            }
            let ch = clean[i..].chars().next().unwrap();
            stripped.push(ch);
            i += ch.len_utf8();
        }
        clean = stripped;
    }

    let mut width = 0usize;
    for segment in UnicodeSegmentation::graphemes(clean.as_str(), true) {
        width += grapheme_width(&segment);
    }

    WIDTH_CACHE.with(|c| {
        let mut cache = c.borrow_mut();
        if cache.len() >= WIDTH_CACHE_SIZE {
            cache.remove(0);
        }
        cache.push((s.to_string(), width));
    });

    width
}

/// Find the terminal-column span containing visible, non-whitespace content.
pub fn visible_content_span(line: &str, max_width: f64) -> Option<(usize, usize)> {
    let limit = max_width.floor();
    if line.is_empty() || !limit.is_finite() || limit <= 0.0 {
        return None;
    }
    let limit = limit as usize;

    let mut from: i64 = -1;
    let mut to: i64 = -1;
    let mut current_col = 0usize;
    let mut i = 0usize;
    let extractor = AnsiCodeExtractor::new(line);

    while i < line.len() && current_col < limit {
        if let Some(ansi) = extractor.get(line, i) {
            i += ansi.length;
            continue;
        }

        if line[i..].starts_with('\t') {
            current_col += 3;
            i += 1;
            continue;
        }

        let mut text_end = i;
        while text_end < line.len()
            && !line[text_end..].starts_with('\t')
            && extractor.get(line, text_end).is_none()
        {
            text_end += line[text_end..].chars().next().unwrap().len_utf8();
        }

        for segment in UnicodeSegmentation::graphemes(&line[i..text_end], true) {
            let width = grapheme_width(&segment);
            let segment_start = current_col;
            let segment_end = current_col + width;
            if width > 0 && !segment.trim().is_empty() && segment_start < limit {
                if from == -1 {
                    from = segment_start as i64;
                }
                to = segment_end.min(limit) as i64;
            }
            current_col = segment_end;
            if current_col >= limit {
                break;
            }
        }
        i = text_end;
    }

    if from == -1 {
        None
    } else {
        Some((from as usize, to as usize))
    }
}

// ---------------------------------------------------------------------------
// Terminal output normalization
// ---------------------------------------------------------------------------

/// Normalize text for terminal output without changing logical editor content.
pub fn normalize_terminal_output(s: &str) -> String {
    let has_thai_lao_am = s.contains('\u{0e33}') || s.contains('\u{0eb3}');
    let has_tab = s.contains('\t');
    if !has_thai_lao_am && !has_tab {
        return s.to_string();
    }

    let mut normalized = s.to_string();
    if has_thai_lao_am {
        let mut out = String::new();
        for c in normalized.chars() {
            match c {
                '\u{0e33}' => out.push_str("\u{0e4d}\u{0e32}"),
                '\u{0eb3}' => out.push_str("\u{0ecd}\u{0eb2}"),
                other => out.push(other),
            }
        }
        normalized = out;
    }
    if has_tab {
        normalized = normalized.replace('\t', "   ");
    }
    normalized
}

// ---------------------------------------------------------------------------
// ANSI scanning
// ---------------------------------------------------------------------------

/// A single ANSI/OSC/APC escape sequence found in a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnsiCode {
    pub code: String,
    pub length: usize,
}

type ControlStringEnds = HashMap<usize, Option<usize>>;

/// Scanner whose malformed-control-string cache is released after this string scan.
pub struct AnsiCodeExtractor {
    control_string_ends: std::cell::RefCell<ControlStringEnds>,
}

impl AnsiCodeExtractor {
    pub fn new(_s: &str) -> Self {
        Self {
            control_string_ends: std::cell::RefCell::new(HashMap::new()),
        }
    }

    pub fn get(&self, s: &str, pos: usize) -> Option<AnsiCode> {
        let end = self.extract_end_at(s, pos)?;
        Some(AnsiCode {
            code: s[pos..end].to_string(),
            length: end - pos,
        })
    }

    fn extract_end_at(&self, s: &str, pos: usize) -> Option<usize> {
        let bytes = s.as_bytes();
        if pos >= bytes.len() || bytes[pos] != 0x1b {
            return None;
        }
        let next = *bytes.get(pos + 1)?;

        if next == b'[' {
            let mut j = pos + 2;
            let mut has_intermediate = false;
            while j < bytes.len() {
                let byte = bytes[j];
                if (0x30..=0x3f).contains(&byte) && !has_intermediate {
                    j += 1;
                    continue;
                }
                if (0x20..=0x2f).contains(&byte) {
                    has_intermediate = true;
                    j += 1;
                    continue;
                }
                if (0x40..=0x7e).contains(&byte) {
                    return Some(j + 1);
                }
                return None;
            }
            return None;
        }

        if next == b']' || next == b'_' {
            return self.extract_control_string_end(s, pos, true);
        }

        if next == b'P' || next == b'^' || next == b'X' {
            return self.extract_control_string_end(s, pos, false);
        }

        None
    }

    fn extract_control_string_end(&self, s: &str, pos: usize, allow_bel: bool) -> Option<usize> {
        if let Some(cached) = self.control_string_ends.borrow().get(&pos) {
            return *cached;
        }

        let bytes = s.as_bytes();
        let mut j = pos + 2;
        while j < bytes.len() {
            if allow_bel && bytes[j] == 0x07 {
                return Some(j + 1);
            }
            if bytes[j] == 0x1b && bytes.get(j + 1) == Some(&b'\\') {
                return Some(j + 2);
            }
            j += 1;
        }

        let mut ends: ControlStringEnds = HashMap::new();
        cache_control_string_ends(s, pos, &mut ends);
        *self.control_string_ends.borrow_mut() = ends;
        None
    }
}

fn cache_control_string_ends(s: &str, from: usize, ends: &mut ControlStringEnds) {
    let bytes = s.as_bytes();
    let mut next_bel: i64 = -1;
    let mut next_st: i64 = -1;

    let mut i = bytes.len() as i64 - 1;
    while i >= from as i64 {
        let idx = i as usize;
        if bytes[idx] == 0x07 {
            next_bel = i;
        }
        if bytes[idx] != 0x1b {
            i -= 1;
            continue;
        }
        let next = bytes.get(idx + 1).copied();
        if next == Some(b'\\') {
            next_st = i;
        } else if next == Some(b']') || next == Some(b'_') {
            if next_bel != -1 && (next_st == -1 || next_bel < next_st) {
                ends.insert(idx, Some(next_bel as usize + 1));
            } else {
                ends.insert(
                    idx,
                    if next_st == -1 {
                        None
                    } else {
                        Some(next_st as usize + 2)
                    },
                );
            }
        } else if next == Some(b'P') || next == Some(b'^') || next == Some(b'X') {
            ends.insert(
                idx,
                if next_st == -1 {
                    None
                } else {
                    Some(next_st as usize + 2)
                },
            );
        }
        i -= 1;
    }
}

/// Extract an ANSI escape sequence from a string at the given position.
pub fn extract_ansi_code(s: &str, pos: usize) -> Option<AnsiCode> {
    AnsiCodeExtractor::new(s).get(s, pos)
}

/// Create a scanner whose malformed-control-string cache is released after this string scan.
pub fn create_ansi_code_extractor(s: &str) -> AnsiCodeExtractor {
    AnsiCodeExtractor::new(s)
}

// ---------------------------------------------------------------------------
// OSC 8 hyperlinks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Osc8Terminator {
    Bel,
    St,
}

impl Osc8Terminator {
    pub fn as_str(&self) -> &'static str {
        match self {
            Osc8Terminator::Bel => "\x07",
            Osc8Terminator::St => "\x1b\\",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveHyperlink {
    pub params: String,
    pub url: String,
    pub terminator: Osc8Terminator,
}

/// Parse an OSC 8 sequence. `None` means "not an OSC 8 sequence",
/// `Some(None)` means "hyperlink close", `Some(Some(_))` is an open link.
pub fn parse_osc8_hyperlink(ansi_code: &str) -> Option<Option<ActiveHyperlink>> {
    if !ansi_code.starts_with("\x1b]8;") {
        return None;
    }

    let terminator = if ansi_code.ends_with('\x07') {
        Osc8Terminator::Bel
    } else {
        Osc8Terminator::St
    };
    let body_end = if terminator == Osc8Terminator::Bel {
        ansi_code.len() - 1
    } else {
        ansi_code.len().saturating_sub(2)
    };
    let body = &ansi_code[4..body_end];
    let separator_index = match body.find(';') {
        Some(i) => i,
        None => return None,
    };

    let params = body[..separator_index].to_string();
    let url = body[separator_index + 1..].to_string();
    if url.is_empty() {
        return Some(None);
    }
    Some(Some(ActiveHyperlink {
        params,
        url,
        terminator,
    }))
}

fn format_osc8_hyperlink(hyperlink: &ActiveHyperlink) -> String {
    format!(
        "\x1b]8;{};{}{}",
        hyperlink.params,
        hyperlink.url,
        hyperlink.terminator.as_str()
    )
}

fn format_osc8_close(terminator: &Osc8Terminator) -> String {
    format!("\x1b]8;;{}", terminator.as_str())
}

// ---------------------------------------------------------------------------
// ANSI state tracker
// ---------------------------------------------------------------------------

/// Track active ANSI SGR codes to preserve styling across line breaks.
pub struct AnsiCodeTracker {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    inverse: bool,
    hidden: bool,
    strikethrough: bool,
    fg_color: Option<String>,
    bg_color: Option<String>,
    active_hyperlink: Option<ActiveHyperlink>,
}

impl Default for AnsiCodeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl AnsiCodeTracker {
    pub fn new() -> Self {
        Self {
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            blink: false,
            inverse: false,
            hidden: false,
            strikethrough: false,
            fg_color: None,
            bg_color: None,
            active_hyperlink: None,
        }
    }

    pub fn process(&mut self, ansi_code: &str) {
        if let Some(hyperlink) = parse_osc8_hyperlink(ansi_code) {
            self.active_hyperlink = hyperlink;
            return;
        }

        if !ansi_code.ends_with('m') {
            return;
        }

        let params = match extract_sgr_params(ansi_code) {
            Some(p) => p,
            None => return,
        };
        if params.is_empty() || params == "0" {
            self.reset();
            return;
        }

        let parts: Vec<&str> = params.split(';').collect();
        let mut i = 0usize;
        while i < parts.len() {
            let code: i64 = match parts[i].parse::<i64>() {
                Ok(v) => v,
                Err(_) => {
                    i += 1;
                    continue;
                }
            };

            if code == 38 || code == 48 {
                if parts.get(i + 1) == Some(&"5") && parts.get(i + 2).is_some() {
                    let color_code = format!("{};{};{}", parts[i], parts[i + 1], parts[i + 2]);
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 3;
                    continue;
                } else if parts.get(i + 1) == Some(&"2") && parts.get(i + 4).is_some() {
                    let color_code = format!(
                        "{};{};{};{};{}",
                        parts[i],
                        parts[i + 1],
                        parts[i + 2],
                        parts[i + 3],
                        parts[i + 4]
                    );
                    if code == 38 {
                        self.fg_color = Some(color_code);
                    } else {
                        self.bg_color = Some(color_code);
                    }
                    i += 5;
                    continue;
                }
            }

            match code {
                0 => self.reset(),
                1 => self.bold = true,
                2 => self.dim = true,
                3 => self.italic = true,
                4 => self.underline = true,
                5 => self.blink = true,
                7 => self.inverse = true,
                8 => self.hidden = true,
                9 => self.strikethrough = true,
                21 => self.bold = false,
                22 => {
                    self.bold = false;
                    self.dim = false;
                }
                23 => self.italic = false,
                24 => self.underline = false,
                25 => self.blink = false,
                27 => self.inverse = false,
                28 => self.hidden = false,
                29 => self.strikethrough = false,
                39 => self.fg_color = None,
                49 => self.bg_color = None,
                _ => {
                    if (30..=37).contains(&code) || (90..=97).contains(&code) {
                        self.fg_color = Some(code.to_string());
                    } else if (40..=47).contains(&code) || (100..=107).contains(&code) {
                        self.bg_color = Some(code.to_string());
                    }
                }
            }
            i += 1;
        }
    }

    fn reset(&mut self) {
        self.bold = false;
        self.dim = false;
        self.italic = false;
        self.underline = false;
        self.blink = false;
        self.inverse = false;
        self.hidden = false;
        self.strikethrough = false;
        self.fg_color = None;
        self.bg_color = None;
        // SGR reset does not affect OSC 8 hyperlink state
    }

    /// Clear all state for reuse.
    pub fn clear(&mut self) {
        self.reset();
        self.active_hyperlink = None;
    }

    pub fn get_active_codes(&self) -> String {
        let mut codes: Vec<String> = Vec::new();
        if self.bold {
            codes.push("1".to_string());
        }
        if self.dim {
            codes.push("2".to_string());
        }
        if self.italic {
            codes.push("3".to_string());
        }
        if self.underline {
            codes.push("4".to_string());
        }
        if self.blink {
            codes.push("5".to_string());
        }
        if self.inverse {
            codes.push("7".to_string());
        }
        if self.hidden {
            codes.push("8".to_string());
        }
        if self.strikethrough {
            codes.push("9".to_string());
        }
        if let Some(fg) = &self.fg_color {
            codes.push(fg.clone());
        }
        if let Some(bg) = &self.bg_color {
            codes.push(bg.clone());
        }

        let mut result = if !codes.is_empty() {
            format!("\x1b[{}m", codes.join(";"))
        } else {
            String::new()
        };
        if let Some(hyperlink) = &self.active_hyperlink {
            result.push_str(&format_osc8_hyperlink(hyperlink));
        }
        result
    }

    pub fn has_active_codes(&self) -> bool {
        self.bold
            || self.dim
            || self.italic
            || self.underline
            || self.blink
            || self.inverse
            || self.hidden
            || self.strikethrough
            || self.fg_color.is_some()
            || self.bg_color.is_some()
            || self.active_hyperlink.is_some()
    }

    /// Get reset codes for attributes that need to be turned off at line end.
    pub fn get_line_end_reset(&self) -> String {
        let mut result = String::new();
        if self.underline {
            result.push_str("\x1b[24m");
        }
        if let Some(hyperlink) = &self.active_hyperlink {
            result.push_str(&format_osc8_close(&hyperlink.terminator));
        }
        result
    }
}

/// Port of `ansiCode.match(/\x1b\[([\d;]*)m/)` - first match only.
fn extract_sgr_params(ansi_code: &str) -> Option<String> {
    let bytes = ansi_code.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                j += 1;
            }
            if j < bytes.len() && bytes[j] == b'm' {
                return Some(ansi_code[i + 2..j].to_string());
            }
        }
        i += 1;
    }
    None
}

fn update_tracker_from_text(text: &str, tracker: &mut AnsiCodeTracker) {
    let mut i = 0usize;
    let extractor = AnsiCodeExtractor::new(text);
    while i < text.len() {
        if let Some(ansi) = extractor.get(text, i) {
            tracker.process(&ansi.code);
            i += ansi.length;
        } else {
            i += text[i..].chars().next().unwrap().len_utf8();
        }
    }
}

// ---------------------------------------------------------------------------
// Word wrapping
// ---------------------------------------------------------------------------

/// Split text into words while keeping ANSI codes attached.
fn split_into_tokens_with_ansi(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut pending_ansi = String::new();
    let mut in_whitespace = false;
    let mut i = 0usize;
    let extractor = AnsiCodeExtractor::new(text);

    while i < text.len() {
        if let Some(ansi) = extractor.get(text, i) {
            pending_ansi.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }

        let ch = text[i..].chars().next().unwrap();
        let char_is_space = ch == ' ';

        if char_is_space != in_whitespace && !current.is_empty() {
            tokens.push(std::mem::take(&mut current));
        }

        if !pending_ansi.is_empty() {
            current.push_str(&pending_ansi);
            pending_ansi.clear();
        }

        in_whitespace = char_is_space;
        current.push(ch);
        i += ch.len_utf8();
    }

    if !pending_ansi.is_empty() {
        current.push_str(&pending_ansi);
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

/// Wrap text with ANSI codes preserved. Word wrapping only - no padding.
pub fn wrap_text_with_ansi(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }

    let input_lines: Vec<&str> = text.split('\n').collect();
    let mut result: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();

    for input_line in input_lines {
        let prefix = if !result.is_empty() {
            tracker.get_active_codes()
        } else {
            String::new()
        };
        let wrapped = wrap_single_line(&format!("{prefix}{input_line}"), width);
        result.extend(wrapped);
        update_tracker_from_text(input_line, &mut tracker);
    }

    if result.is_empty() {
        vec![String::new()]
    } else {
        result
    }
}

fn wrap_single_line(line: &str, width: usize) -> Vec<String> {
    if line.is_empty() {
        return vec![String::new()];
    }

    let visible_length = visible_width(line);
    if visible_length <= width {
        return vec![line.to_string()];
    }

    let mut wrapped: Vec<String> = Vec::new();
    let mut tracker = AnsiCodeTracker::new();
    let tokens = split_into_tokens_with_ansi(line);

    let mut current_line = String::new();
    let mut current_visible_length = 0usize;

    for token in tokens {
        let token_visible_length = visible_width(&token);
        let is_whitespace = token.trim().is_empty();

        if token_visible_length > width && !is_whitespace {
            if !current_line.is_empty() {
                let line_end_reset = tracker.get_line_end_reset();
                if !line_end_reset.is_empty() {
                    current_line.push_str(&line_end_reset);
                }
                wrapped.push(std::mem::take(&mut current_line));
            }

            let broken = break_long_word(&token, width, &mut tracker);
            if broken.len() > 1 {
                wrapped.extend(broken[..broken.len() - 1].iter().cloned());
            }
            current_line = broken.last().cloned().unwrap_or_default();
            current_visible_length = visible_width(&current_line);
            continue;
        }

        let total_needed = current_visible_length + token_visible_length;

        if total_needed > width && current_visible_length > 0 {
            let mut line_to_wrap = current_line.trim_end().to_string();
            let line_end_reset = tracker.get_line_end_reset();
            if !line_end_reset.is_empty() {
                line_to_wrap.push_str(&line_end_reset);
            }
            wrapped.push(line_to_wrap);
            if is_whitespace {
                current_line = tracker.get_active_codes();
                current_visible_length = 0;
            } else {
                current_line = format!("{}{}", tracker.get_active_codes(), token);
                current_visible_length = token_visible_length;
            }
        } else {
            current_line.push_str(&token);
            current_visible_length += token_visible_length;
        }

        update_tracker_from_text(&token, &mut tracker);
    }

    if !current_line.is_empty() {
        wrapped.push(current_line);
    }

    if wrapped.is_empty() {
        vec![String::new()]
    } else {
        wrapped
            .into_iter()
            .map(|line| line.trim_end().to_string())
            .collect()
    }
}

fn is_punctuation_char_byte(ch: char) -> bool {
    matches!(
        ch,
        '(' | ')'
            | '{'
            | '}'
            | '['
            | ']'
            | '<'
            | '>'
            | '.'
            | ','
            | ';'
            | ':'
            | '\''
            | '"'
            | '!'
            | '?'
            | '+'
            | '-'
            | '='
            | '*'
            | '/'
            | '\\'
            | '|'
            | '&'
            | '%'
            | '^'
            | '$'
            | '#'
            | '@'
            | '~'
            | '`'
    )
}

/// Remove all escape sequences (CSI, OSC, DCS/APC, two-char) leaving plain text.
pub fn strip_ansi(s: &str) -> String {
    if !s.contains('\x1b') {
        return s.to_string();
    }

    let input = strip_common_csi(s);
    let bytes = input.as_bytes();
    let mut escape_index = match input.find('\x1b') {
        Some(i) => i as i64,
        None => return input,
    };

    let extractor = AnsiCodeExtractor::new(&input);
    let mut result: Vec<String> = Vec::new();
    let mut plain_start = 0usize;
    while escape_index != -1 {
        let idx = escape_index as usize;
        match extractor.extract_end_at(&input, idx) {
            Some(ansi_end) => {
                if plain_start < idx {
                    result.push(input[plain_start..idx].to_string());
                }
                plain_start = ansi_end;
            }
            None => {
                let next = bytes.get(idx + 1).copied();
                let is_line_sep = next.is_none()
                    || next == Some(0x0a)
                    || next == Some(0x0d)
                    || input[idx + 1..].starts_with('\u{2028}')
                    || input[idx + 1..].starts_with('\u{2029}');
                if idx + 1 < input.len() && !is_line_sep {
                    if plain_start < idx {
                        result.push(input[plain_start..idx].to_string());
                    }
                    // JS removes one UTF-16 code unit after ESC. A split surrogate
                    // becomes U+FFFD at the UTF-8 boundary; never slice a Rust char.
                    let next_char = input[idx + 1..].chars().next().unwrap();
                    if next_char.len_utf16() == 2 {
                        result.push("\u{fffd}".to_string());
                    }
                    plain_start = idx + 1 + next_char.len_utf8();
                }
            }
        }
        let from = (escape_index + 1).max(plain_start as i64) as usize;
        if from >= input.len() {
            break;
        }
        escape_index = match input[from..].find('\x1b') {
            Some(i) => (from + i) as i64,
            None => -1,
        };
    }
    if plain_start < input.len() {
        result.push(input[plain_start..].to_string());
    }
    result.join("")
}

fn strip_common_csi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit()
                    || bytes[j] == b';'
                    || bytes[j] == b':'
                    || bytes[j] == b'?'
                    || bytes[j] == b'<'
                    || bytes[j] == b'='
                    || bytes[j] == b'>')
            {
                j += 1;
            }
            if j < bytes.len() && (0x40..=0x7e).contains(&bytes[j]) {
                i = j + 1;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Check if a character is whitespace.
///
/// Ports `isWhitespaceChar(char)` (packages/tui/src/utils.ts:935-937), which is literally
/// `return /\s/.test(char)`. JS `\s` is not the same set as Rust's `char::is_whitespace`:
/// it INCLUDES U+FEFF (zero-width no-break space) and EXCLUDES U+0085 (NEL).
pub fn is_whitespace_char(ch: &str) -> bool {
    // JS `/\s/` is exactly this set: it INCLUDES U+FEFF (zero-width no-break space)
    // and EXCLUDES U+0085 (NEL), the reverse of Rust's `char::is_whitespace`.
    ch.chars().any(|c| {
        matches!(
            c,
            '\u{0009}'..='\u{000d}'
                | '\u{0020}'
                | '\u{00a0}'
                | '\u{1680}'
                | '\u{2000}'..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
        )
    })
}

/// Check if a character is punctuation.
pub fn is_punctuation_char(ch: &str) -> bool {
    ch.chars().any(is_punctuation_char_byte)
}

fn break_long_word(word: &str, width: usize, tracker: &mut AnsiCodeTracker) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current_line = tracker.get_active_codes();
    let mut current_width = 0usize;

    enum Segment {
        Ansi(String),
        Grapheme(String),
    }

    let mut segments: Vec<Segment> = Vec::new();
    let mut i = 0usize;
    let extractor = AnsiCodeExtractor::new(word);

    while i < word.len() {
        if let Some(ansi) = extractor.get(word, i) {
            segments.push(Segment::Ansi(ansi.code));
            i += ansi.length;
        } else {
            let mut end = i;
            while end < word.len() && extractor.get(word, end).is_none() {
                end += word[end..].chars().next().unwrap().len_utf8();
            }
            for g in graphemes(&word[i..end]) {
                segments.push(Segment::Grapheme(g));
            }
            i = end;
        }
    }

    for seg in segments {
        match seg {
            Segment::Ansi(value) => {
                current_line.push_str(&value);
                tracker.process(&value);
            }
            Segment::Grapheme(grapheme) => {
                if grapheme.is_empty() {
                    continue;
                }
                let grapheme_w = visible_width(&grapheme);
                if current_width + grapheme_w > width {
                    let line_end_reset = tracker.get_line_end_reset();
                    if !line_end_reset.is_empty() {
                        current_line.push_str(&line_end_reset);
                    }
                    lines.push(std::mem::take(&mut current_line));
                    current_line = tracker.get_active_codes();
                    current_width = 0;
                }
                current_line.push_str(&grapheme);
                current_width += grapheme_w;
            }
        }
    }

    if !current_line.is_empty() {
        lines.push(current_line);
    }

    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

/// Apply background color to a line, padding to full width.
pub fn apply_background_to_line(
    line: &str,
    width: usize,
    bg_fn: &dyn Fn(&str) -> String,
) -> String {
    let visible_len = visible_width(line);
    let padding_needed = width.saturating_sub(visible_len);
    let padding = " ".repeat(padding_needed);
    let with_padding = format!("{line}{padding}");
    bg_fn(&with_padding)
}

// ---------------------------------------------------------------------------
// Truncation
// ---------------------------------------------------------------------------

fn truncate_fragment_to_width(text: &str, max_width: f64) -> (String, usize) {
    if max_width <= 0.0 || text.is_empty() {
        return (String::new(), 0);
    }
    let max_width = max_width as usize;

    if is_printable_ascii(text) {
        let clipped: String = text.chars().take(max_width).collect();
        let width = clipped.chars().count();
        return (clipped, width);
    }

    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');
    if !has_ansi && !has_tabs {
        let mut result = String::new();
        let mut width = 0usize;
        for segment in UnicodeSegmentation::graphemes(text, true) {
            let w = grapheme_width(&segment);
            if width + w > max_width {
                break;
            }
            result.push_str(&segment);
            width += w;
        }
        return (result, width);
    }

    let mut result = String::new();
    let mut width = 0usize;
    let mut i = 0usize;
    let mut pending_ansi = String::new();
    let extractor = AnsiCodeExtractor::new(text);

    while i < text.len() {
        if let Some(ansi) = extractor.get(text, i) {
            pending_ansi.push_str(&ansi.code);
            i += ansi.length;
            continue;
        }

        if text[i..].starts_with('\t') {
            if width + 3 > max_width {
                break;
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push('\t');
            width += 3;
            i += 1;
            continue;
        }

        let mut end = i;
        while end < text.len() && !text[end..].starts_with('\t') {
            if extractor.get(text, end).is_some() {
                break;
            }
            end += text[end..].chars().next().unwrap().len_utf8();
        }

        for segment in UnicodeSegmentation::graphemes(&text[i..end], true) {
            let w = grapheme_width(&segment);
            if width + w > max_width {
                return (result, width);
            }
            if !pending_ansi.is_empty() {
                result.push_str(&pending_ansi);
                pending_ansi.clear();
            }
            result.push_str(&segment);
            width += w;
        }
        i = end;
    }

    (result, width)
}

fn finalize_truncated_result(
    prefix: &str,
    prefix_width: usize,
    ellipsis: &str,
    ellipsis_width: usize,
    max_width: usize,
    pad: bool,
) -> String {
    let reset = "\x1b[0m";
    let visible_width_total = prefix_width + ellipsis_width;
    let prefix_has_ansi = prefix.contains('\x1b');
    let ellipsis_has_ansi = ellipsis.contains('\x1b');
    let before_ellipsis = if prefix_has_ansi { reset } else { "" };
    let after_ellipsis = if prefix_has_ansi || ellipsis_has_ansi {
        reset
    } else {
        ""
    };
    let result = if !ellipsis.is_empty() {
        format!("{prefix}{before_ellipsis}{ellipsis}{after_ellipsis}")
    } else {
        format!("{prefix}{before_ellipsis}")
    };

    if pad {
        format!(
            "{}{}",
            result,
            " ".repeat(max_width.saturating_sub(visible_width_total))
        )
    } else {
        result
    }
}

/// Truncate text to fit within a maximum visible width, adding an ellipsis if needed.
pub fn truncate_to_width(text: &str, max_width: f64, ellipsis: &str, pad: bool) -> String {
    if max_width <= 0.0 {
        return String::new();
    }
    let max_width = max_width as usize;

    if text.is_empty() {
        return if pad {
            " ".repeat(max_width)
        } else {
            String::new()
        };
    }

    let ellipsis_width = visible_width(ellipsis);
    if ellipsis_width >= max_width {
        let text_width = visible_width(text);
        if text_width <= max_width {
            return if pad {
                format!("{}{}", text, " ".repeat(max_width - text_width))
            } else {
                text.to_string()
            };
        }

        let (clipped_text, clipped_width) = truncate_fragment_to_width(ellipsis, max_width as f64);
        if clipped_width == 0 {
            return if pad {
                " ".repeat(max_width)
            } else {
                String::new()
            };
        }
        return finalize_truncated_result("", 0, &clipped_text, clipped_width, max_width, pad);
    }

    if is_printable_ascii(text) {
        if text.len() <= max_width {
            return if pad {
                format!("{}{}", text, " ".repeat(max_width - text.len()))
            } else {
                text.to_string()
            };
        }
        let target_width = max_width - ellipsis_width;
        let prefix: String = text.chars().take(target_width).collect();
        return finalize_truncated_result(
            &prefix,
            target_width,
            ellipsis,
            ellipsis_width,
            max_width,
            pad,
        );
    }

    let target_width = max_width - ellipsis_width;
    let mut result = String::new();
    let mut pending_ansi = String::new();
    let mut visible_so_far = 0usize;
    let mut kept_width = 0usize;
    let mut keep_contiguous_prefix = true;
    let mut overflowed = false;
    let has_ansi = text.contains('\x1b');
    let has_tabs = text.contains('\t');

    if !has_ansi && !has_tabs {
        for segment in UnicodeSegmentation::graphemes(text, true) {
            let width = grapheme_width(&segment);
            if keep_contiguous_prefix && kept_width + width <= target_width {
                result.push_str(&segment);
                kept_width += width;
            } else {
                keep_contiguous_prefix = false;
            }
            visible_so_far += width;
            if visible_so_far > max_width {
                overflowed = true;
                break;
            }
        }
    } else {
        let mut i = 0usize;
        let extractor = AnsiCodeExtractor::new(text);
        while i < text.len() {
            if let Some(ansi) = extractor.get(text, i) {
                pending_ansi.push_str(&ansi.code);
                i += ansi.length;
                continue;
            }

            if text[i..].starts_with('\t') {
                if keep_contiguous_prefix && kept_width + 3 <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push('\t');
                    kept_width += 3;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }
                visible_so_far += 3;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
                i += 1;
                continue;
            }

            let mut end = i;
            while end < text.len() && !text[end..].starts_with('\t') {
                if extractor.get(text, end).is_some() {
                    break;
                }
                end += text[end..].chars().next().unwrap().len_utf8();
            }

            for segment in UnicodeSegmentation::graphemes(&text[i..end], true) {
                let width = grapheme_width(&segment);
                if keep_contiguous_prefix && kept_width + width <= target_width {
                    if !pending_ansi.is_empty() {
                        result.push_str(&pending_ansi);
                        pending_ansi.clear();
                    }
                    result.push_str(&segment);
                    kept_width += width;
                } else {
                    keep_contiguous_prefix = false;
                    pending_ansi.clear();
                }

                visible_so_far += width;
                if visible_so_far > max_width {
                    overflowed = true;
                    break;
                }
            }
            if overflowed {
                break;
            }
            i = end;
        }
    }

    let exhausted_input = !overflowed;
    if !overflowed && exhausted_input {
        return if pad {
            format!(
                "{}{}",
                text,
                " ".repeat(max_width.saturating_sub(visible_so_far))
            )
        } else {
            text.to_string()
        };
    }

    finalize_truncated_result(
        &result,
        kept_width,
        ellipsis,
        ellipsis_width,
        max_width,
        pad,
    )
}

// ---------------------------------------------------------------------------
// Column slicing
// ---------------------------------------------------------------------------

/// Result of a column slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlicedText {
    pub text: String,
    pub width: usize,
}

/// Extract a range of visible columns from a line.
pub fn slice_by_column(line: &str, start_col: usize, length: usize, strict: bool) -> String {
    slice_with_width(line, start_col, length, strict).text
}

/// Like slice_by_column but also returns the actual visible width of the result.
pub fn slice_with_width(line: &str, start_col: usize, length: usize, strict: bool) -> SlicedText {
    if length == 0 {
        return SlicedText {
            text: String::new(),
            width: 0,
        };
    }
    let end_col = start_col + length;
    let mut result = String::new();
    let mut result_width = 0usize;
    let mut current_col = 0usize;
    let mut i = 0usize;
    let mut pending_ansi = String::new();
    let extractor = AnsiCodeExtractor::new(line);

    while i < line.len() {
        if let Some(ansi) = extractor.get(line, i) {
            if current_col >= start_col && current_col < end_col {
                result.push_str(&ansi.code);
            } else if current_col < start_col {
                pending_ansi.push_str(&ansi.code);
            }
            i += ansi.length;
            continue;
        }

        let mut text_end = i;
        while text_end < line.len() && extractor.get(line, text_end).is_none() {
            text_end += line[text_end..].chars().next().unwrap().len_utf8();
        }

        for segment in UnicodeSegmentation::graphemes(&line[i..text_end], true) {
            let w = grapheme_width(&segment);
            let in_range = current_col >= start_col && current_col < end_col;
            let fits = !strict || current_col + w <= end_col;
            if in_range && fits {
                if !pending_ansi.is_empty() {
                    result.push_str(&pending_ansi);
                    pending_ansi.clear();
                }
                result.push_str(&segment);
                result_width += w;
            }
            current_col += w;
            if current_col >= end_col {
                break;
            }
        }
        i = text_end;
        if current_col >= end_col {
            break;
        }
    }
    SlicedText {
        text: result,
        width: result_width,
    }
}

/// Return the OSC 8 hyperlink URL covering the given visible column.
pub fn hyperlink_at_column(line: &str, column: i64) -> Option<String> {
    if column < 0 {
        return None;
    }
    let extractor = AnsiCodeExtractor::new(line);
    let mut current_col = 0usize;
    let mut active_url: Option<String> = None;
    let mut i = 0usize;
    while i < line.len() {
        if let Some(ansi) = extractor.get(line, i) {
            if let Some(hyperlink) = parse_osc8_hyperlink(&ansi.code) {
                active_url = hyperlink.map(|h| h.url);
            }
            i += ansi.length;
            continue;
        }
        let mut text_end = i;
        while text_end < line.len() && extractor.get(line, text_end).is_none() {
            text_end += line[text_end..].chars().next().unwrap().len_utf8();
        }
        for segment in UnicodeSegmentation::graphemes(&line[i..text_end], true) {
            let w = grapheme_width(&segment);
            if (column as usize) < current_col + w {
                return active_url;
            }
            current_col += w;
        }
        i = text_end;
    }
    None
}

/// Return an explicit OSC 8 URL or terminal-style bare HTTP(S) URL covering a column.
pub fn url_at_column(line: &str, column: i64) -> Option<String> {
    let explicit_url = hyperlink_at_column(line, column);
    if explicit_url.is_some() || column < 0 {
        return explicit_url;
    }

    let plain_text = strip_ansi(line).replace('\t', "   ");
    for (start_byte, candidate) in find_urls(&plain_text) {
        let mut url = candidate.clone();
        // Trim trailing punctuation.
        while url
            .chars()
            .last()
            .map(|c| matches!(c, '.' | ',' | ';' | ':' | '!' | '?'))
            .unwrap_or(false)
        {
            url.pop();
        }
        for (open, close) in [('(', ')'), ('[', ']'), ('{', '}')] {
            while url.ends_with(close) && url.matches(close).count() > url.matches(open).count() {
                url.pop();
            }
        }
        let start = visible_width(&plain_text[..start_byte]);
        let end = start + visible_width(&url);
        if column >= start as i64 && column < end as i64 {
            return Some(url);
        }
    }
    None
}

/// Find `https?://[^\s<>"'`]+` matches as (byte offset, text).
fn find_urls(text: &str) -> Vec<(usize, String)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        let rest = &text[i..];
        let lower = rest.to_ascii_lowercase();
        let scheme_len = if lower.starts_with("http://") {
            Some(7)
        } else if lower.starts_with("https://") {
            Some(8)
        } else {
            None
        };
        if let Some(scheme_len) = scheme_len {
            let mut j = i;
            while j < text.len() {
                let ch = text[j..].chars().next().unwrap();
                if ch.is_whitespace() || matches!(ch, '<' | '>' | '"' | '\'' | '`') {
                    break;
                }
                j += ch.len_utf8();
            }
            if j > i + scheme_len {
                out.push((i, text[i..j].to_string()));
            }
            i = j.max(i + 1);
            continue;
        }
        i += text[i..].chars().next().unwrap().len_utf8();
    }
    out
}

/// Segments extracted from a line around an overlay region.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedSegments {
    pub before: String,
    pub before_width: usize,
    pub after: String,
    pub after_width: usize,
}

thread_local! {
    static POOLED_STYLE_TRACKER: std::cell::RefCell<AnsiCodeTracker> =
        std::cell::RefCell::new(AnsiCodeTracker::new());
}

/// Extract "before" and "after" segments from a line in a single pass.
pub fn extract_segments(
    line: &str,
    before_end: usize,
    after_start: usize,
    after_len: usize,
    strict_after: bool,
) -> ExtractedSegments {
    let mut before = String::new();
    let mut before_width = 0usize;
    let mut after = String::new();
    let mut after_width = 0usize;
    let mut current_col = 0usize;
    let mut i = 0usize;
    let mut pending_ansi_before = String::new();
    let mut after_started = false;
    let after_end = after_start + after_len;
    let extractor = AnsiCodeExtractor::new(line);

    POOLED_STYLE_TRACKER.with(|t| t.borrow_mut().clear());

    while i < line.len() {
        if let Some(ansi) = extractor.get(line, i) {
            POOLED_STYLE_TRACKER.with(|t| t.borrow_mut().process(&ansi.code));
            if current_col < before_end {
                pending_ansi_before.push_str(&ansi.code);
            } else if current_col >= after_start && current_col < after_end && after_started {
                after.push_str(&ansi.code);
            }
            i += ansi.length;
            continue;
        }

        let mut text_end = i;
        while text_end < line.len() && extractor.get(line, text_end).is_none() {
            text_end += line[text_end..].chars().next().unwrap().len_utf8();
        }

        for segment in UnicodeSegmentation::graphemes(&line[i..text_end], true) {
            let w = grapheme_width(&segment);

            if current_col < before_end {
                if !pending_ansi_before.is_empty() {
                    before.push_str(&pending_ansi_before);
                    pending_ansi_before.clear();
                }
                before.push_str(&segment);
                before_width += w;
            } else if current_col >= after_start && current_col < after_end {
                let fits = !strict_after || current_col + w <= after_end;
                if fits {
                    if !after_started {
                        after.push_str(
                            &POOLED_STYLE_TRACKER.with(|t| t.borrow().get_active_codes()),
                        );
                        after_started = true;
                    }
                    after.push_str(&segment);
                    after_width += w;
                }
            }

            current_col += w;
            if if after_len == 0 {
                current_col >= before_end
            } else {
                current_col >= after_end
            } {
                break;
            }
        }
        i = text_end;
        if if after_len == 0 {
            current_col >= before_end
        } else {
            current_col >= after_end
        } {
            break;
        }
    }

    ExtractedSegments {
        before,
        before_width,
        after,
        after_width,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `isWhitespaceChar` is `/[[:space:]]/.test(char)` in TS (utils.ts:935-937), i.e. JS
    /// `\s`. That set differs from Rust's `char::is_whitespace` in exactly two points:
    /// it INCLUDES U+FEFF and EXCLUDES U+0085 (NEL).
    #[test]
    fn is_whitespace_char_matches_js_regex() {
        for ch in [
            "\t", "\n", "\u{000b}", "\u{000c}", "\r", " ", "\u{00a0}", "\u{1680}", "\u{2000}",
            "\u{2005}", "\u{200a}", "\u{2028}", "\u{2029}", "\u{202f}", "\u{205f}", "\u{3000}",
            // JS-only member of `\s`: Rust's `char::is_whitespace` says false.
            "\u{feff}",
        ] {
            assert!(
                is_whitespace_char(ch),
                "{ch:?} must be whitespace under JS \\s"
            );
        }
        for ch in [
            // NEL: Rust's `char::is_whitespace` says true, JS `\s` says false.
            "\u{0085}",
            "\u{180e}",
            "\u{200b}",
            "a",
            "",
            "\u{1f600}",
        ] {
            assert!(
                !is_whitespace_char(ch),
                "{ch:?} must NOT be whitespace under JS \\s"
            );
        }
    }

    /// `is_rgi_emoji` must mirror the strict `/^\p{RGI_Emoji}$/v` test
    /// (packages/tui/src/utils.ts:34,163). Every expectation below was read off
    /// Node 25 with Unicode 17. The clusters on
    /// the first list were accepted by the previous "any VS16 / skin tone / keycap"
    /// test and are NOT RGI, so they widened the string by one column.
    #[test]
    fn is_rgi_emoji_matches_the_strict_unicode_set() {
        // RGI: single emoji presentation, VS16 basic, keycap, flag, ZWJ, tag, tone.
        for seg in [
            "\u{2705}",
            "\u{1f44d}",
            "\u{2764}\u{fe0f}",
            "\u{2139}\u{fe0f}",
            "1\u{fe0f}\u{20e3}",
            "#\u{fe0f}\u{20e3}",
            "\u{1f1fa}\u{1f1f8}",
            "\u{1f469}\u{200d}\u{1f4bb}",
            "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}",
            "\u{1f3f4}\u{e0067}\u{e0062}\u{e0073}\u{e0063}\u{e0074}\u{e007f}",
            "\u{1f44d}\u{1f3fb}",
            "\u{270a}\u{1f3fd}",
            "\u{261d}\u{1f3fb}",
        ] {
            assert!(is_rgi_emoji(seg), "{seg:?} is RGI under \\p{{RGI_Emoji}}");
        }
        // NOT RGI, but the previous rule accepted them.
        for seg in [
            "\u{261d}\u{fe0f}\u{1f3fb}", // extra VS16 before a modifier
            "\u{1f1e6}\u{1f1e6}",        // unassigned flag
            "1\u{fe0f}",                 // bare VS16 after a non-emoji character
            "1\u{20e3}",
            "#\u{20e3}",         // keycap without VS16
            "\u{2764}\u{1f3fb}", // modifier after a non-Modifier_Base
            "\u{2764}",          // text presentation without VS16
            "\u{20e3}",
            "\u{fe0f}",  // lone keycap mark / VS16
            "A\u{fe0f}", // VS16 after a non-Emoji base
        ] {
            assert!(
                !is_rgi_emoji(seg),
                "{seg:?} is NOT RGI under \\p{{RGI_Emoji}}"
            );
        }
        // A lone regional indicator is not RGI, but both sides return 2 columns
        // for it before the RGI test (utils.ts:177 / utils.rs regional-indicator
        // branch), so it is excluded from the lists above.
        // Wide (2 columns) exactly where TS's graphemeWidth returns 2.
        assert_eq!(
            visible_width("1\u{fe0f}"),
            1,
            "wrong VS16 widens the string"
        );
        assert_eq!(visible_width("1\u{20e3}"), 1, "bare keycap is one column");
        assert_eq!(
            visible_width("\u{2764}\u{1f3fb}"),
            1,
            "wrong modifier widens the string"
        );
        assert_eq!(
            visible_width("\u{2764}\u{fe0f}"),
            2,
            "VS16 heart is two columns"
        );
        assert_eq!(
            visible_width("1\u{fe0f}\u{20e3}"),
            2,
            "keycap is two columns"
        );
        assert_eq!(
            visible_width("\u{1f469}\u{200d}\u{1f4bb}"),
            2,
            "ZWJ sequence is two columns"
        );
    }

    #[test]
    fn visible_width_ascii_and_wide() {
        assert_eq!(visible_width("hello"), 5);
        assert_eq!(visible_width(""), 0);
        assert_eq!(visible_width("日本語"), 6);
    }

    #[test]
    fn visible_width_strips_ansi_and_tabs() {
        assert_eq!(visible_width("\x1b[31mred\x1b[0m"), 3);
        assert_eq!(visible_width("a\tb"), 5);
        assert_eq!(
            visible_width("\x1b]8;;https://x.test\x07link\x1b]8;;\x07"),
            4
        );
    }

    #[test]
    fn wrap_text_preserves_active_ansi() {
        let wrapped = wrap_text_with_ansi("\x1b[31mhello world\x1b[0m", 5);
        assert!(wrapped.len() >= 2);
        assert!(wrapped[1].starts_with("\x1b[31m"));
    }

    #[test]
    fn truncate_to_width_pads_and_ellipsizes() {
        assert_eq!(truncate_to_width("abcdef", 4.0, "...", false), "a...");
        assert_eq!(truncate_to_width("abc", 5.0, "...", true), "abc  ");
        assert_eq!(truncate_to_width("abc", 0.0, "...", false), "");
    }

    #[test]
    fn strip_ansi_removes_csi_and_osc() {
        assert_eq!(strip_ansi("\x1b[1mbold\x1b[0m"), "bold");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
    }

    #[test]
    fn slice_by_column_handles_wide_chars() {
        assert_eq!(slice_by_column("日本語", 0, 2, false), "日");
        assert_eq!(slice_by_column("日本語", 2, 2, false), "本");
    }

    #[test]
    fn normalize_terminal_output_decomposes_thai_am() {
        assert_eq!(normalize_terminal_output("ก\u{0e33}"), "ก\u{0e4d}\u{0e32}");
        assert_eq!(normalize_terminal_output("a\tb"), "a   b");
        assert_eq!(normalize_terminal_output("plain"), "plain");
    }

    #[test]
    fn url_at_column_finds_bare_url() {
        let line = "see https://example.test/a for details";
        assert_eq!(
            url_at_column(line, 5).as_deref(),
            Some("https://example.test/a")
        );
        assert_eq!(url_at_column(line, 0), None);
    }
}
