use std::path::PathBuf;

use serde::Serialize;

use crate::api::Reportable;

#[derive(Serialize)]
pub struct Clean {
    pub objects: Vec<PathBuf>,
    pub dry_run: bool,
}

impl Clean {
    pub fn new(objects: Vec<PathBuf>, dry_run: bool) -> Self {
        Self { objects, dry_run }
    }
}

impl Reportable for Clean {
    fn print_plain(&self) {
        let header = if self.dry_run { "Object (dry run)" } else { "Object" };
        let mut rows: [Vec<String>; 1] = [Vec::new()];
        for obj in &self.objects {
            rows[0].push(obj.display().to_string());
        }
        crate::stdout::print_table(&[header], &rows);
    }
}
