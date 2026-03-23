// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use std::collections::HashMap;

use serde::Serialize;

use crate::api::Printable;

/// Tag listing for one or more packages, optionally including platform or variant details.
///
/// Plain format: two-column table (Package | Tag) by default, or
/// (Package | Platform) with `--platforms`, or (Package | Variant) with `--variants`.
///
/// JSON format: object keyed by package name; values are arrays of tags, platforms, or variants.
#[derive(Serialize)]
pub struct Tags {
    #[serde(flatten)]
    pub packages: TagsData,
}

impl Tags {
    pub fn from_tags(packages: HashMap<String, impl IntoIterator<Item = String>>) -> Self {
        Self {
            packages: TagsData::Tags(
                packages
                    .into_iter()
                    .map(|(k, v)| (k, v.into_iter().collect()))
                    .collect(),
            ),
        }
    }

    pub fn from_platforms(packages: HashMap<String, Vec<String>>) -> Self {
        Self {
            packages: TagsData::Platforms(packages),
        }
    }

    pub fn from_variants(packages: HashMap<String, Vec<String>>) -> Self {
        Self {
            packages: TagsData::Variants(packages),
        }
    }
}

/// Polymorphic tag payload.
#[derive(Serialize)]
#[serde(untagged)]
pub enum TagsData {
    Tags(HashMap<String, Vec<String>>),
    Platforms(HashMap<String, Vec<String>>),
    Variants(HashMap<String, Vec<String>>),
}

impl Printable for Tags {
    fn print_plain(&self, printer: &ocx_lib::cli::Printer) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        let (header, data) = match &self.packages {
            TagsData::Tags(tags) => {
                for (package, package_tags) in tags {
                    for tag in package_tags {
                        rows[0].push(package.clone());
                        rows[1].push(tag.clone());
                    }
                }
                ("Tag", &rows)
            }
            TagsData::Platforms(platforms) => {
                for (package, platform_list) in platforms {
                    for platform in platform_list {
                        rows[0].push(package.clone());
                        rows[1].push(platform.clone());
                    }
                }
                ("Platform", &rows)
            }
            TagsData::Variants(variants) => {
                for (package, variant_names) in variants {
                    for variant in variant_names {
                        rows[0].push(package.clone());
                        rows[1].push(variant.clone());
                    }
                }
                ("Variant", &rows)
            }
        };
        printer.print_table(&["Package", header], data);
    }
}
