use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

use crate::api::Reportable;

/// Whether the resource was actually removed or was already absent.
#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum RemovedStatus {
    Removed,
    Absent,
}

impl fmt::Display for RemovedStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RemovedStatus::Removed => write!(f, "Removed"),
            RemovedStatus::Absent => write!(f, "Absent"),
        }
    }
}

/// A single uninstall or deselect result entry.
#[derive(Serialize)]
pub struct RemovedEntry {
    pub package: String,
    pub status: RemovedStatus,
    pub content: Option<PathBuf>,
}

impl RemovedEntry {
    /// Creates a `RemovedEntry` from the package name and the optional content
    /// path returned by the task. `Some(path)` indicates the resource was
    /// removed; `None` indicates a no-op (resource was already absent).
    pub fn from_result(package: String, content_path: Option<PathBuf>) -> Self {
        let status = if content_path.is_some() {
            RemovedStatus::Removed
        } else {
            RemovedStatus::Absent
        };
        Self {
            package,
            status,
            content: content_path,
        }
    }
}

/// Results of an uninstall or deselect operation, one entry per requested package.
///
/// Plain format: three-column table (Package | Status | Content).
///
/// JSON format: array of `{ package, status, content }` objects.
pub struct Removed {
    pub entries: Vec<RemovedEntry>,
}

impl Removed {
    pub fn new(entries: Vec<RemovedEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for Removed {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl Reportable for Removed {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for entry in &self.entries {
            rows[0].push(entry.package.clone());
            rows[1].push(entry.status.to_string());
            rows[2].push(
                entry
                    .content
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            );
        }
        crate::stdout::print_table(&["Package", "Status", "Content"], &rows);
    }
}
