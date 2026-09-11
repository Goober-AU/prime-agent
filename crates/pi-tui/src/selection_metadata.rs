//! Port of packages/tui/src/selection-metadata.ts.

use crate::utils::{create_ansi_code_extractor, visible_width};
use std::collections::HashMap;

const TABLE_MARKER_PREFIX: &str = "\x1b_pi:table:";
const TABLE_START_MARKER: &str = "\x1b_pi:table:start\x07";
const TABLE_END_MARKER: &str = "\x1b_pi:table:end\x07";

/// A selectable table cell region in the rendered frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableCellSelectionRegion {
    pub line: usize,
    pub col: usize,
    pub width: usize,
    /// Identity of the table the cell belongs to (the TypeScript `object` key).
    pub table: usize,
    pub table_top: usize,
    pub table_bottom: usize,
    pub table_left: usize,
    pub table_right: usize,
    pub row: i64,
    pub column: i64,
    pub segment: i64,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CellMarkerKind {
    CellStart,
    CellEnd,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CellMarker {
    kind: CellMarkerKind,
    row: i64,
    column: i64,
    segment: i64,
    /// Absent when the marker carries no encoded content (`undefined` in TS).
    content: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TableBounds {
    top: usize,
    bottom: usize,
    left: usize,
    right: usize,
}

/// Percent-encode one cell's content (port of `encodeURIComponent`).
fn encode_uri_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        let b = *byte;
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Port of `decodeURIComponent` for the marker payload.
fn decode_uri_component(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &value[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn cell_marker(kind: &CellMarkerKind, row: i64, column: i64, segment: i64, content: Option<&str>) -> String {
    let encoded_content = match content {
        None => String::new(),
        Some(value) => format!(":{}", encode_uri_component(value)),
    };
    let kind_str = match kind {
        CellMarkerKind::CellStart => "cell-start",
        CellMarkerKind::CellEnd => "cell-end",
    };
    format!("{TABLE_MARKER_PREFIX}{kind_str}:{row}:{column}:{segment}{encoded_content}\x07")
}

fn parse_cell_marker(code: &str) -> Option<CellMarker> {
    if !code.starts_with(TABLE_MARKER_PREFIX) || !code.ends_with('\x07') {
        return None;
    }
    let body = &code[TABLE_MARKER_PREFIX.len()..code.len() - 1];
    let parts: Vec<&str> = body.split(':').collect();
    let kind = match parts.first() {
        Some(&"cell-start") => CellMarkerKind::CellStart,
        Some(&"cell-end") => CellMarkerKind::CellEnd,
        _ => return None,
    };
    let row: i64 = parts.get(1)?.parse().ok()?;
    let column: i64 = parts.get(2)?.parse().ok()?;
    let segment: i64 = parts.get(3)?.parse().ok()?;
    let content = match parts.get(4) {
        None => None,
        Some(encoded) => Some(decode_uri_component(encoded)),
    };
    Some(CellMarker {
        kind,
        row,
        column,
        segment,
        content,
    })
}

pub fn mark_table_start(line: &str) -> String {
    format!("{TABLE_START_MARKER}{line}")
}

pub fn mark_table_end(line: &str) -> String {
    format!("{line}{TABLE_END_MARKER}")
}

pub fn mark_table_cell(text: &str, row: i64, column: i64, segment: i64, content: &str) -> String {
    let marker_content = if segment == 0 { Some(content) } else { None };
    format!(
        "{}{}{}",
        cell_marker(&CellMarkerKind::CellStart, row, column, segment, marker_content),
        text,
        cell_marker(&CellMarkerKind::CellEnd, row, column, segment, None)
    )
}

/// Strip table markers out of rendered lines and collect selectable cell regions.
pub fn extract_table_cell_selection_regions(
    lines: &[String],
    get_table_identity: &mut dyn FnMut(usize) -> usize,
) -> (Vec<String>, Vec<TableCellSelectionRegion>) {
    if !lines.iter().any(|line| line.contains(TABLE_MARKER_PREFIX)) {
        return (lines.to_vec(), Vec::new());
    }

    let mut clean_lines: Vec<String> = Vec::new();
    let mut regions: Vec<TableCellSelectionRegion> = Vec::new();
    let mut cell_contents: HashMap<usize, HashMap<String, String>> = HashMap::new();
    let mut table_bounds: HashMap<usize, TableBounds> = HashMap::new();
    let mut table: Option<usize> = None;
    let mut table_index = 0usize;

    for (line_index, source) in lines.iter().enumerate() {
        if !source.contains(TABLE_MARKER_PREFIX) {
            clean_lines.push(source.clone());
            continue;
        }
        let mut clean = String::new();
        let mut active_cell: Option<(CellMarker, usize)> = None;
        let mut offset = 0usize;
        let extractor = create_ansi_code_extractor(source);

        while offset < source.len() {
            let ansi = match extractor.get(source, offset) {
                Some(a) => a,
                None => {
                    let ch = source[offset..].chars().next().unwrap();
                    clean.push(ch);
                    offset += ch.len_utf8();
                    continue;
                }
            };

            if ansi.code == TABLE_START_MARKER {
                let identity = get_table_identity(table_index);
                table_index += 1;
                table = Some(identity);
                let col = visible_width(&clean);
                table_bounds.insert(
                    identity,
                    TableBounds {
                        top: line_index,
                        bottom: line_index,
                        left: col,
                        right: col,
                    },
                );
            } else if ansi.code == TABLE_END_MARKER {
                if let Some(current) = table {
                    if let Some(bounds) = table_bounds.get_mut(&current) {
                        bounds.bottom = line_index;
                        bounds.right = visible_width(&clean);
                    }
                }
                table = None;
                active_cell = None;
            } else if let Some(marker) = parse_cell_marker(&ansi.code) {
                if marker.kind == CellMarkerKind::CellStart {
                    let col = visible_width(&clean);
                    if let (Some(current), Some(content)) = (table, marker.content.clone()) {
                        cell_contents
                            .entry(current)
                            .or_default()
                            .insert(format!("{}:{}", marker.row, marker.column), content);
                    }
                    active_cell = Some((marker, col));
                } else if marker.kind == CellMarkerKind::CellEnd && table.is_some() && active_cell.is_some() {
                    let (active, active_col) = active_cell.clone().unwrap();
                    let width = visible_width(&clean).saturating_sub(active_col);
                    if width > 0
                        && marker.row == active.row
                        && marker.column == active.column
                        && marker.segment == active.segment
                    {
                        regions.push(TableCellSelectionRegion {
                            line: line_index,
                            col: active_col,
                            width,
                            table: table.unwrap(),
                            table_top: 0,
                            table_bottom: 0,
                            table_left: 0,
                            table_right: 0,
                            row: marker.row,
                            column: marker.column,
                            segment: marker.segment,
                            content: String::new(),
                        });
                    }
                    active_cell = None;
                } else {
                    clean.push_str(&ansi.code);
                }
            } else {
                clean.push_str(&ansi.code);
            }
            offset += ansi.length;
        }
        clean_lines.push(clean);
    }

    for region in regions.iter_mut() {
        if let Some(bounds) = table_bounds.get(&region.table) {
            region.table_top = bounds.top;
            region.table_bottom = bounds.bottom;
            region.table_left = bounds.left;
            region.table_right = bounds.right;
        }
        region.content = cell_contents
            .get(&region.table)
            .and_then(|contents| contents.get(&format!("{}:{}", region.row, region.column)))
            .cloned()
            .unwrap_or_default();
    }

    (clean_lines, regions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_wrap_and_strip() {
        let line = mark_table_cell("cell", 0, 0, 0, "cell");
        let mut identity = |_: usize| 1usize;
        let (clean, regions) = extract_table_cell_selection_regions(&[line], &mut identity);
        assert_eq!(clean, vec!["cell".to_string()]);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].row, 0);
        assert_eq!(regions[0].column, 0);
        assert_eq!(regions[0].content, "cell");
        assert_eq!(regions[0].width, 4);
    }

    #[test]
    fn table_bounds_are_applied_to_regions() {
        let lines = vec![
            mark_table_start(""),
            mark_table_cell("a", 0, 0, 0, "a"),
            mark_table_cell("b", 0, 1, 0, "b"),
            mark_table_end(""),
        ];
        let mut identity = |_: usize| 7usize;
        let (_, regions) = extract_table_cell_selection_regions(&lines, &mut identity);
        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].table_top, 0);
        assert_eq!(regions[0].table_bottom, 3);
    }

    #[test]
    fn lines_without_markers_pass_through() {
        let lines = vec!["plain".to_string()];
        let mut identity = |_: usize| 1usize;
        let (clean, regions) = extract_table_cell_selection_regions(&lines, &mut identity);
        assert_eq!(clean, lines);
        assert!(regions.is_empty());
    }

    #[test]
    fn uri_component_roundtrip() {
        let encoded = encode_uri_component("a b/c");
        assert_eq!(encoded, "a%20b%2Fc");
        assert_eq!(decode_uri_component(&encoded), "a b/c");
    }
}
