use std::collections::HashMap;

use serde::Serialize;

use crate::api::Reportable;

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

#[derive(Serialize)]
#[serde(untagged)]
pub enum TagsData {
    WithoutPlatforms(HashMap<String, Vec<String>>),
    WithPlatforms(HashMap<String, HashMap<String, Vec<String>>>),
}

impl Reportable for Tags {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        match &self.packages {
            TagsData::WithoutPlatforms(tags) => {
                for (package, package_tags) in tags {
                    for tag in package_tags {
                        rows[0].push(package.clone());
                        rows[1].push(tag.clone());
                    }
                }
                crate::stdout::print_table(&["Package", "Tag"], &rows);
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
                crate::stdout::print_table(&["Package", "Tag", "Platform"], &rows);
            }
        }
    }
}
