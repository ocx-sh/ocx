// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Reportable;

/// System information about the ocx installation.
///
/// Plain format: colored logo with key-value pairs alongside.
///
/// JSON format: flat object with version, registry, platforms, shell, home.
#[derive(Serialize)]
pub struct Info {
    pub version: String,
    pub registry: String,
    pub platforms: Vec<String>,
    pub shell: Option<String>,
    pub home: String,
}

impl Info {
    pub fn new(version: String, registry: String, platforms: Vec<String>, shell: Option<String>, home: String) -> Self {
        Self {
            version,
            registry,
            platforms,
            shell,
            home,
        }
    }
}

impl Reportable for Info {
    fn print_plain(&self, _printer: &ocx_lib::cli::Printer) {
        // Plain format is handled directly by the command (logo rendering).
        // This is only called as a fallback.
        println!("Version:   {}", self.version);
        println!("Registry:  {}", self.registry);
        println!("Platforms: {}", self.platforms.join(", "));
        println!("Shell:     {}", self.shell.as_deref().unwrap_or("n/a"));
        println!("Home:      {}", self.home);
    }
}
