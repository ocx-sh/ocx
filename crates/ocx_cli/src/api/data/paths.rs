use std::path::PathBuf;

use serde::Serialize;

use crate::api::Reportable;

/// A single resolved package → content-path entry.
#[derive(Serialize)]
pub struct PathEntry {
    pub package: String,
    pub path: PathBuf,
}

/// Ordered list of resolved content paths, one per requested package.
///
/// Plain format: two-column table (Package | Path).
///
/// JSON format: object keyed by the input package identifier, preserving
/// request order.
pub struct Paths {
    pub entries: Vec<PathEntry>,
}

impl Paths {
    pub fn new(entries: Vec<PathEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for Paths {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for entry in &self.entries {
            map.serialize_entry(&entry.package, &entry.path)?;
        }
        map.end()
    }
}

impl Reportable for Paths {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.path.display().to_string());
        }
        crate::stdout::print_table(&["Package", "Path"], &rows);
    }
}
