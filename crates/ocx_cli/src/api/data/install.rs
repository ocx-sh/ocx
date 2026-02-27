use std::collections::HashMap;

use ocx_lib::package::InstallInfo;
use serde::Serialize;

#[derive(Serialize)]
pub struct InstallCollection {
    #[serde(flatten)]
    pub packages: HashMap<String, InstallInfo>,
}

impl InstallCollection {
    pub fn new(packages: HashMap<String, InstallInfo>) -> Self {
        Self { packages }
    }
}
