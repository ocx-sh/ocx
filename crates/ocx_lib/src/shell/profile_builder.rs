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
        match var.modifier {
            Modifier::Path(path_var) => {
                let value = self.expand_variables(&path_var.value);
                self.script.push_str(&self.shell.export_path(&var.key, &value));
                self.script.push('\n');
            }
            Modifier::Constant(constant_var) => {
                let value = self.expand_variables(&constant_var.value);
                self.script.push_str(&self.shell.export_constant(&var.key, &value));
                self.script.push('\n');
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
