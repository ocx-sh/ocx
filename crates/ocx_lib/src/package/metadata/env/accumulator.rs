use std::sync::LazyLock;

use regex::Regex;

use crate::{Error, Result, env};

use super::var;

static VARIABLE_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\\${[a-zA-Z:_]+}").expect("Invalid variable regex!"));

pub struct Accumulator<'a> {
    install_path: std::path::PathBuf,
    env: &'a mut env::Env,
}

impl<'a> Accumulator<'a> {
    pub fn new(install_path: impl AsRef<std::path::Path>, env: &'a mut crate::env::Env) -> Self {
        Accumulator {
            install_path: install_path.as_ref().to_path_buf(),
            env,
        }
    }

    pub fn add(&mut self, var: &var::Var) -> Result<()> {
        let key = &var.key;
        let value = match self.resolve_var(var)? {
            Some(value) => value,
            None => return Ok(()),
        };

        match var.modifier {
            var::Modifier::Path(_) => {
                self.env.add_path(key, &value);
            }
            var::Modifier::Constant(_) => {
                self.env.set(key, value);
            }
        };
        Ok(())
    }

    pub fn resolve_var(&self, var: &var::Var) -> Result<Option<String>> {
        let value = match var.value() {
            Some(value) => value,
            None => return Ok(None),
        };
        let mut value = value.replace("${installPath}", &self.install_path.to_string_lossy());

        if let var::Modifier::Path(path_modifier) = &var.modifier {
            let mut path = std::path::PathBuf::from(&value);
            if path.is_relative() {
                path = self.install_path.join(path);
            }
            if path_modifier.required && !path.exists() {
                return Err(Error::Undefined);
            }
            value = path.to_string_lossy().to_string();
        }

        Ok(Some(value))
    }
}
