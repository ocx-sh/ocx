use std::collections::HashMap;

use crate::{log, utility};

#[cfg(target_os = "windows")]
pub const PATH_SEPARATOR: &str = ";";

#[cfg(not(target_os = "windows"))]
pub const PATH_SEPARATOR: &str = ":";

pub struct Env {
    vars: HashMap<String, String>,
}

impl Default for Env {
    fn default() -> Self {
        Self::new()
    }
}

impl Env {
    pub fn new() -> Self {
        Self {
            vars: std::env::vars().collect(),
        }
    }

    pub fn clean() -> Self {
        Self { vars: HashMap::new() }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    pub fn set(&mut self, key: impl ToString, value: impl ToString) {
        self.vars.insert(key.to_string(), value.to_string());
    }

    pub fn add_path(&mut self, key: impl ToString, value: impl ToString) {
        let (key, value) = (key.to_string(), value.to_string());
        let existing = self.vars.get(&key).map(|s| s.as_str()).unwrap_or("");
        if existing.is_empty() {
            self.vars.insert(key, value);
        } else {
            let new_value = format!("{}{}{}", value, PATH_SEPARATOR, existing);
            self.vars.insert(key, new_value);
        }
    }
}

impl IntoIterator for Env {
    type Item = (String, String);
    type IntoIter = std::collections::hash_map::IntoIter<String, String>;

    fn into_iter(self) -> Self::IntoIter {
        self.vars.into_iter()
    }
}

pub fn var(key: impl AsRef<str>) -> Option<String> {
    #[cfg(test)]
    match crate::test::env::get_override(key.as_ref()) {
        Some(Some(val)) => return Some(val),
        Some(None) => return None,
        None => {}
    }
    match std::env::var(key.as_ref()) {
        Ok(value) => Some(value),
        Err(std::env::VarError::NotPresent) => None,
        Err(std::env::VarError::NotUnicode(os_str)) => {
            log::warn!("Environment variable '{}' is not valid: {:?}", key.as_ref(), os_str);
            None
        }
    }
}

pub fn flag(key: impl AsRef<str>, default: bool) -> bool {
    let key = key.as_ref();
    match var(key).map(|value| utility::boolean_string::BooleanString::try_from(value.as_str()))
        .transpose() {
        Ok(Some(boolean)) => boolean.into(),
        Ok(None) => default,
        Err(error) => {
            log::warn!("Environment variable '{}' has invalid boolean value: {}", key, error);
            default 
        }
    }   
}

pub fn string(key: impl AsRef<str>, default: String) -> String {
    if let Some(value) = var(key) {
        if value.is_empty() {
            default
        } else {
            value
        }
    } else {
        default
    }
}
