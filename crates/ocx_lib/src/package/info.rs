use serde::{Deserialize, Serialize};

use super::metadata::Metadata;
use crate::oci;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct Info {
    pub identifier: oci::Identifier,
    pub metadata: Metadata,
    pub platform: oci::Platform,
}
