// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use serde::Serialize;

use crate::api::Printable;

/// Tag listing for one or more packages, optionally including platform details.
///
/// Plain format: two-column table (Package | Tag) without platforms, or
/// three-column table (Package | Tag | Platform) when platforms are included.
///
/// JSON format: object keyed by package name; values are tag arrays without
/// platforms, or nested objects (tag → platform arrays) with platforms.
#[derive(Serialize)]
pub struct Tags {
    #[serde(flatten)]
    pub packages: TagsData,
}

impl Tags {
    pub fn without_platforms(packages: HashMap<String, impl IntoIterator<Item = String>>) -> Self {
        Self {
            packages: TagsData::WithoutPlatforms(
                packages
                    .into_iter()
                    .map(|(k, v)| (k, v.into_iter().collect()))
                    .collect(),
            ),
        }
    }

    pub fn with_platforms(packages: HashMap<String, HashMap<String, Vec<String>>>) -> Self {
        Self {
            packages: TagsData::WithPlatforms(packages),
        }
    }
}

/// Polymorphic tag payload: either a flat map of tags per package, or a nested
/// map including platform breakdowns per tag.
#[derive(Serialize)]
#[serde(untagged)]
pub enum TagsData {
    WithoutPlatforms(HashMap<String, Vec<String>>),
    WithPlatforms(HashMap<String, HashMap<String, Vec<String>>>),
}

impl Printable for Tags {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        match &self.packages {
            TagsData::WithoutPlatforms(tags) => {
                for (package, package_tags) in tags {
                    for tag in package_tags {
                        rows[0].push(package.clone());
                        rows[1].push(tag.clone());
                    }
                }
                printer.print_table(&["Package", "Tag"], &rows);
            }
            TagsData::WithPlatforms(tags) => {
                for (package, platform_tags) in tags {
                    for (platform, platform_tags) in platform_tags {
                        for tag in platform_tags {
                            rows[0].push(package.clone());
                            rows[1].push(tag.clone());
                            rows[2].push(platform.clone());
                        }
                    }
                }
                printer.print_table(&["Package", "Tag", "Platform"], &rows);
            }
        }
    }
}
