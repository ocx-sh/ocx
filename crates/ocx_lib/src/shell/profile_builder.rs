// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::{
    package::metadata::env::var::{Modifier, Var},
    shell::Shell,
};

#[derive(Clone)]
pub struct ProfileBuilder {
    script: String,
    content: std::path::PathBuf,
    shell: Shell,
}

impl ProfileBuilder {
    pub fn new(content: std::path::PathBuf, shell: Shell) -> Self {
        let script = String::with_capacity(2048);
        Self { script, content, shell }
    }

    pub fn add(&mut self, var: Var) {
        // `export_path` / `export_constant` reject keys that fail POSIX
        // env-var-name validation by returning `None`. Skip the line
        // silently with a stderr note: the only path that produces a bad
        // key is malformed package metadata, which `ocx pull` should have
        // rejected — falling through preserves the rest of the profile
        // rather than aborting the whole build.
        match var.modifier {
            Modifier::Path(path_var) => {
                let value = self.expand_variables(&path_var.value);
                if let Some(line) = self.shell.export_path(&var.key, &value) {
                    self.script.push_str(&line);
                    self.script.push('\n');
                } else {
                    eprintln!("# ocx: skipping invalid env-var key {:?}", var.key);
                }
            }
            Modifier::Constant(constant_var) => {
                let value = self.expand_variables(&constant_var.value);
                if let Some(line) = self.shell.export_constant(&var.key, &value) {
                    self.script.push_str(&line);
                    self.script.push('\n');
                } else {
                    eprintln!("# ocx: skipping invalid env-var key {:?}", var.key);
                }
            }
        }
    }

    pub fn take(self) -> String {
        self.script
    }

    fn expand_variables(&self, var: &str) -> String {
        var.replace("${installPath}", &self.content.to_string_lossy())
    }
}
