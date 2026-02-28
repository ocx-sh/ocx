use std::collections::HashMap;

use ocx_lib::package::InstallInfo;
use serde::Serialize;

use crate::api::Reportable;

#[derive(Serialize)]
pub struct Installs {
    #[serde(flatten)]
    pub packages: HashMap<String, InstallInfo>,
}

impl Installs {
    pub fn new(packages: HashMap<String, InstallInfo>) -> Self {
        Self { packages }
    }
}

impl Reportable for Installs {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for (package, version) in &self.packages {
            rows[0].push(package.clone());
            rows[1].push(version.identifier.to_string());
            rows[2].push(version.content.to_path_buf().display().to_string());
        }
        crate::stdout::print_table(&["Package", "Version", "Content"], &rows);
    }
}
