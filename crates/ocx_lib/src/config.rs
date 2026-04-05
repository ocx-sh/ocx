// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

pub mod error;

use crate::Result;
use serde::Deserialize;

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Config {
    #[serde(rename = "registry")]
    pub registries: Vec<RegistryConfig>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct RegistryConfig {
    /// The registry prefix to match when resolving references.
    /// For example, "registry.ocx.io" would match "registry.ocx.io/packages/my-package:1.0.0".
    /// This registry configuration will only be applied if the prefix matches.
    pub prefix: String,
    /// Optional rewrite to pull or push to a different registry.
    /// This will replace only the prefix.
    pub location: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<AuthenticationConfig>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthenticationConfig {
    Env(AuthenticationConfigByEnv),
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(untagged, rename_all = "snake_case")]
pub enum AuthenticationConfigByEnv {
    Basic { user: String, token: String },
    Bearer { token: String },
}

#[allow(dead_code)]
impl Config {
    pub fn merge(&mut self, other: Config) {
        self.registries.extend(other.registries);
    }

    fn from_file_content(config_str: impl AsRef<str>) -> Result<Self> {
        let config: Config = toml::from_str(config_str.as_ref()).map_err(error::Error::Parse)?;
        Ok(config)
    }

    pub async fn from_file(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref();
        let config_str = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| crate::error::file_error(path, e))?;
        Self::from_file_content(&config_str)
    }

    pub fn user_path() -> Result<Option<std::path::PathBuf>> {
        let home_dir = match std::env::home_dir() {
            Some(path) => path,
            None => return Ok(None),
        };
        Ok(Some(home_dir.join(".ocx").join("config.toml")))
    }

    pub async fn load_default() -> Result<Self> {
        let user_config = match Self::user_path()? {
            Some(path) => path,
            None => return Ok(Self { registries: vec![] }),
        };
        Self::from_file(user_config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test;

    #[test]
    fn test_config_from_file_content() {
        let config_str = test::data::include_str!("config.toml");
        let config = Config::from_file_content(config_str).unwrap();
        assert_eq!(config.registries.len(), 2);
        assert_eq!(config.registries[0].prefix, "ocx.io/packages/");
        assert_eq!(config.registries[0].location, None);
        assert_eq!(config.registries[1].prefix, "custom.registry/packages/");
        assert_eq!(
            config.registries[1].location,
            Some("registry.custom.io/packages/".into())
        );
    }
}
