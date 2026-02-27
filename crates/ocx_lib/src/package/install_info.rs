use serde::Serialize;

use crate::oci;

use super::metadata;

#[derive(Debug, Clone, Serialize)]
pub struct InstallInfo {
    pub identifier: oci::Identifier,
    pub metadata: metadata::Metadata,
    pub content: std::path::PathBuf,
}
