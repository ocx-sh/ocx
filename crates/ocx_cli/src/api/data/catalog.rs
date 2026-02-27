use std::collections::HashMap;

use serde::Serialize;

#[derive(Serialize)]
pub struct Catalog {
    pub repositories: CatalogData,
}

impl Catalog {
    pub fn new(repositories: CatalogData) -> Self {
        Self { repositories }
    }

    pub fn without_tags(repositories: Vec<String>) -> Self {
        Self::new(CatalogData::WithoutTags(repositories))
    }

    pub fn with_tags(tags: HashMap<String, Vec<String>>) -> Self {
        Self::new(CatalogData::WithTags(tags))
    }
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum CatalogData {
    WithoutTags(Vec<String>),
    WithTags(HashMap<String, Vec<String>>),
}
