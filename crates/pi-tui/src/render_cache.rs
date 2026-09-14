//! Port of packages/tui/src/render-cache.ts.

/// Cache of rendered lines keyed by (width, version).
#[derive(Debug, Default)]
pub struct VersionedRenderCache {
    cached_width: Option<usize>,
    cached_version: Option<u64>,
    cached_lines: Option<Vec<String>>,
}

impl VersionedRenderCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, width: usize, version: u64) -> Option<Vec<String>> {
        if self.cached_width == Some(width) && self.cached_version == Some(version) {
            return self.cached_lines.clone();
        }
        None
    }

    pub fn set(&mut self, width: usize, version: u64, lines: Vec<String>) -> Vec<String> {
        self.cached_width = Some(width);
        self.cached_version = Some(version);
        self.cached_lines = Some(lines.clone());
        lines
    }

    pub fn invalidate(&mut self) {
        self.cached_width = None;
        self.cached_version = None;
        self.cached_lines = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hits_only_for_same_width_and_version() {
        let mut cache = VersionedRenderCache::new();
        assert_eq!(cache.get(80, 1), None);
        cache.set(80, 1, vec!["a".to_string()]);
        assert_eq!(cache.get(80, 1), Some(vec!["a".to_string()]));
        assert_eq!(cache.get(81, 1), None);
        assert_eq!(cache.get(80, 2), None);
        cache.invalidate();
        assert_eq!(cache.get(80, 1), None);
    }
}
