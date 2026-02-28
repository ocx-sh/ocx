use serde::Serialize;

use crate::api::Reportable;

#[derive(Serialize)]
pub struct Removed {
    pub packages: Vec<String>,
}

impl Removed {
    pub fn new(packages: Vec<String>) -> Self {
        Self { packages }
    }
}

impl Reportable for Removed {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 1] = [Vec::new()];
        for package in &self.packages {
            rows[0].push(package.clone());
        }
        crate::stdout::print_table(&["Package"], &rows);
    }
}
