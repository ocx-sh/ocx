use std::path::PathBuf;

use serde::Serialize;

use crate::api::Reportable;

#[derive(Serialize)]
pub struct Clean {
    pub objects: Vec<PathBuf>,
    pub temp: Vec<PathBuf>,
    pub dry_run: bool,
}

impl Clean {
    pub fn new(objects: Vec<PathBuf>, temp: Vec<PathBuf>, dry_run: bool) -> Self {
        Self { objects, temp, dry_run }
    }
}

impl Reportable for Clean {
    fn print_plain(&self) {
        let suffix = if self.dry_run { " (dry run)" } else { "" };

        if !self.objects.is_empty() {
            let header = format!("Object{}", suffix);
            let mut rows: [Vec<String>; 1] = [Vec::new()];
            for obj in &self.objects {
                rows[0].push(obj.display().to_string());
            }
            crate::stdout::print_table(&[&header], &rows);
        }

        if !self.temp.is_empty() {
            let header = format!("Temp{}", suffix);
            let mut rows: [Vec<String>; 1] = [Vec::new()];
            for s in &self.temp {
                rows[0].push(s.display().to_string());
            }
            crate::stdout::print_table(&[&header], &rows);
        }
    }
}
