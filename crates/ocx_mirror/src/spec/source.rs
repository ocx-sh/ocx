// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use serde::Deserialize;

const DEFAULT_TAG_PATTERN: &str = r"^v?(?P<version>\d+\.\d+\.\d+)(?:-(?P<prerelease>[0-9a-zA-Z]+))?$";

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    GithubRelease {
        owner: String,
        repo: String,
        #[serde(default = "default_tag_pattern")]
        tag_pattern: String,
    },
    UrlIndex {
        url: Option<String>,
        versions: Option<HashMap<String, UrlIndexVersion>>,
    },
}

#[derive(Debug, Deserialize)]
pub struct UrlIndexVersion {
    #[serde(default)]
    pub prerelease: bool,
    pub assets: HashMap<String, String>,
}

fn default_tag_pattern() -> String {
    DEFAULT_TAG_PATTERN.to_string()
}

impl Source {
    pub fn validate(&self, errors: &mut Vec<String>) {
        match self {
            Source::GithubRelease { tag_pattern, .. } => match regex::Regex::new(tag_pattern) {
                Ok(re) => {
                    if re.capture_names().flatten().all(|n| n != "version") {
                        errors
                            .push("source.tag_pattern must contain a named capture group (?P<version>...)".to_string());
                    }
                }
                Err(e) => {
                    errors.push(format!("source.tag_pattern is not a valid regex: {e}"));
                }
            },
            Source::UrlIndex { url, versions } => match (url, versions) {
                (Some(_), Some(_)) => {
                    errors.push("source: exactly one of 'url' or 'versions' must be provided, not both".to_string());
                }
                (None, None) => {
                    errors.push("source: exactly one of 'url' or 'versions' must be provided".to_string());
                }
                _ => {}
            },
        }
    }
}
