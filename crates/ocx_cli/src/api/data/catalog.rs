use std::collections::HashMap;

use serde::Serialize;

use crate::api::Reportable;

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

impl Reportable for Catalog {
    fn print_plain(&self) {
        let mut rows: [Vec<String>; 2] = [Vec::new(), Vec::new()];
        match &self.repositories {
            CatalogData::WithoutTags(repos) => {
                for repo in repos {
                    rows[0].push(repo.clone());
                }
                crate::stdout::print_table(&["Repository"], &rows);
            }
            CatalogData::WithTags(tags) => {
                for (repo, repo_tags) in tags {
                    for tag in repo_tags {
                        rows[0].push(repo.clone());
                        rows[1].push(tag.clone());
                    }
                }
                crate::stdout::print_table(&["Repository", "Tag"], &rows);
            }
        }
    }
}
