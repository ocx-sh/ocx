use std::collections::HashMap;

use crate::{oci, prelude::*};
use serde::{Deserialize, Serialize};

#[derive(Clone, Deserialize, Serialize)]
pub struct SnapshotEntry {
    /// digest of the package manifest
    pub digest: oci::Digest,
    #[serde(skip_serializing_if = "oci::Platform::is_any", default = "oci::Platform::any")]
    pub platform: oci::Platform,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Snapshot {
    pub entries: HashMap<String, Vec<SnapshotEntry>>,
}

impl Snapshot {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn tags(&self) -> Vec<String> {
        let mut versions: Vec<_> = self.entries.keys().cloned().collect();
        versions.unique();
        versions.sort();
        versions
    }

    pub(crate) fn add_manifest(&mut self, tag: String, digest: oci::Digest, manifest: oci::Manifest) {
        match manifest {
            oci::Manifest::Image(_) => {
                self.add_entry(
                    tag,
                    SnapshotEntry {
                        digest,
                        platform: oci::Platform::any(),
                    },
                );
            }
            oci::Manifest::ImageIndex(index) => {
                let entries = index
                    .manifests
                    .into_iter()
                    .flat_map(|manifest| {
                        Some(SnapshotEntry {
                            digest: manifest.digest.try_into().ok()?,
                            platform: manifest.platform.try_into().ok()?,
                        })
                    })
                    .collect();
                self.add_entry_list(tag, entries);
            }
        }
    }

    pub(crate) fn add_entry(&mut self, tag: String, entry: SnapshotEntry) {
        self.entries.entry(tag).or_default().push(entry);
    }

    pub(crate) fn add_entry_list(&mut self, tag: String, entries: Vec<SnapshotEntry>) {
        self.entries.entry(tag).or_default().extend(entries);
    }
}

impl Default for Snapshot {
    fn default() -> Self {
        Self::new()
    }
}

impl IntoIterator for Snapshot {
    type Item = (String, Vec<SnapshotEntry>);
    type IntoIter = std::collections::hash_map::IntoIter<String, Vec<SnapshotEntry>>;

    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}
