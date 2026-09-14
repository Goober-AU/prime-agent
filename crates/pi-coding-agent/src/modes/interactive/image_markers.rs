//! Port of packages/coding-agent/src/modes/interactive/image-markers.ts

use std::collections::{HashMap, HashSet};

/// Matches `[image #N]` markers inserted when an image is pasted into the editor.
fn image_marker_spans(text: &str) -> Vec<(usize, usize, f64)> {
    let mut spans = Vec::new();
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'[' {
            if let Some((end, id)) = match_image_marker(text, index) {
                spans.push((index, end, id));
                index = end;
                continue;
            }
        }
        index += 1;
    }
    spans
}

/// `\[image #(\d+)\]` anchored at `start`; returns (end offset, parsed id).
fn match_image_marker(text: &str, start: usize) -> Option<(usize, f64)> {
    let rest = text.get(start..)?;
    let mut chars = rest.char_indices();
    let (_, first) = chars.next()?;
    if first != '[' {
        return None;
    }
    let prefix = "[image #";
    if !rest.starts_with(prefix) {
        return None;
    }
    let digits_start = prefix.len();
    let mut digits_end = digits_start;
    for (offset, ch) in rest[digits_start..].char_indices() {
        if ch.is_ascii_digit() {
            digits_end = digits_start + offset + ch.len_utf8();
        } else {
            break;
        }
    }
    if digits_end == digits_start {
        return None;
    }
    if rest.as_bytes().get(digits_end) != Some(&b']') {
        return None;
    }
    let digits = &rest[digits_start..digits_end];
    let value: f64 = digits.parse().ok()?;
    Some((start + digits_end + 1, value))
}

/// The marker text inserted into the editor for pasted image `id`.
pub fn format_image_marker(id: f64) -> String {
    format!("[image #{}]", js_number_to_string(id))
}

/// `Number.toString()` for a non-negative integer value.
fn js_number_to_string(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

/// Marker ids that appear in `text`, in order of appearance.
pub fn image_marker_ids(text: &str) -> Vec<f64> {
    image_marker_spans(text)
        .into_iter()
        .map(|(_, _, id)| id)
        .filter(|id| is_safe_integer(*id))
        .collect()
}

/// `Number.isSafeInteger`
fn is_safe_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0 && value.abs() <= 9007199254740991.0
}

/// Replace every image-marker spelling whose numeric id appears in `remaps`.
pub fn remap_image_markers(text: &str, remaps: &HashMap<i64, i64>) -> String {
    let mut result = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (start, end, id) in image_marker_spans(text) {
        result.push_str(&text[cursor..start]);
        match remaps.get(&(id as i64)) {
            Some(replacement) => result.push_str(&format_image_marker(*replacement as f64)),
            None => result.push_str(&text[start..end]),
        }
        cursor = end;
    }
    result.push_str(&text[cursor..]);
    result
}

/// Images from `pending` whose marker still appears in `text`, in paste order
/// (the map's insertion order). Each image is returned at most once even if its
/// marker is duplicated in the text.
pub fn collect_marked_images<T: Clone>(pending: &[(i64, T)], text: &str) -> Vec<T> {
    if pending.is_empty() {
        return Vec::new();
    }
    let present: HashSet<i64> = image_marker_ids(text).into_iter().map(|id| id as i64).collect();
    let mut images = Vec::new();
    for (id, image) in pending {
        if present.contains(id) {
            images.push(image.clone());
        }
    }
    images
}

/// Evict oldest entries (insertion order) from `images` until the total of
/// `sizeOf` is within `maxBytes`. Ids in `keep` are never evicted, so an image
/// whose marker is still live retains its bytes even if that holds the total
/// above the cap.
pub fn evict_images_to_budget<T>(
    images: &mut Vec<(i64, T)>,
    size_of: impl Fn(&T) -> f64,
    max_bytes: f64,
    keep: &HashSet<i64>,
) {
    let mut total: f64 = images.iter().map(|(_, value)| size_of(value)).sum();
    let keys: Vec<i64> = images.iter().map(|(key, _)| *key).collect();
    for key in keys {
        if total <= max_bytes {
            break;
        }
        if keep.contains(&key) {
            continue;
        }
        if let Some(position) = images.iter().position(|(candidate, _)| *candidate == key) {
            let (_, value) = images.remove(position);
            total -= size_of(&value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_markers() {
        assert_eq!(format_image_marker(3.0), "[image #3]");
        assert_eq!(image_marker_ids("a [image #1] b [image #12] c"), vec![1.0, 12.0]);
        assert!(image_marker_ids("[image #abc] [image #] [image#1]").is_empty());
    }

    #[test]
    fn remaps_only_known_ids() {
        let mut remaps = HashMap::new();
        remaps.insert(1, 7);
        assert_eq!(remap_image_markers("[image #1] [image #2]", &remaps), "[image #7] [image #2]");
    }

    #[test]
    fn collect_returns_paste_order_once_per_marker() {
        let pending = vec![(1i64, "a"), (2, "b"), (3, "c")];
        assert_eq!(collect_marked_images(&pending, "[image #3] [image #1] [image #1]"), vec!["a", "c"]);
        assert!(collect_marked_images::<&str>(&[], "anything").is_empty());
    }

    #[test]
    fn eviction_stops_at_budget_and_keeps_live_ids() {
        let mut images = vec![(1i64, 10.0f64), (2, 10.0), (3, 10.0)];
        let keep: HashSet<i64> = [1].into_iter().collect();
        evict_images_to_budget(&mut images, |value| *value, 15.0, &keep);
        // id 2 evicted first (10 -> 20 > 15), id 1 never evicted, id 3 then evicted.
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].0, 1);

        let mut under_budget = vec![(1i64, 1.0f64)];
        evict_images_to_budget(&mut under_budget, |value| *value, 15.0, &HashSet::new());
        assert_eq!(under_budget.len(), 1);
    }
}
