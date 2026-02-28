use std::collections::HashMap;

use serde::Serialize;

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
