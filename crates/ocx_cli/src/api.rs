use crate::{options, stdout};

pub mod data;

#[derive(Default, Clone)]
pub struct Api {
    format: options::Format,
}

impl Api {
    pub fn new(format: options::Format) -> Self {
        Self { format }
    }

    pub fn report_installs(&self, install: data::install::InstallCollection) -> anyhow::Result<()> {
        match self.format {
            options::Format::Json => {
                println!("{}", serde_json::to_string_pretty(&install)?);
            }
            options::Format::Plain => {
                let mut rows: [Vec<String>; _] = [Vec::new(), Vec::new(), Vec::new()];

                for (package, version) in install.packages {
                    rows[0].push(package);
                    rows[1].push(version.identifier.to_string());
                    rows[2].push(version.content.to_path_buf().display().to_string());
                }
                stdout::print_table(&["Package", "Version", "Content"], &rows);
            }
        }

        
        Ok(())
    }

    pub fn report_tags(&self, tags_report: data::tag::TagCollection) -> anyhow::Result<()> {
        match self.format {
            options::Format::Json => {
                println!("{}", serde_json::to_string_pretty(&tags_report)?);
            }
            options::Format::Plain => {
                let mut rows: [Vec<String>; _] = [Vec::new(), Vec::new(), Vec::new()];
                match tags_report.packages {
                    data::tag::TagCollectionData::WithoutPlatforms(tags) => {
                        for (package, package_tags) in tags {
                            for tag in package_tags {
                                rows[0].push(package.clone());
                                rows[1].push(tag);
                            }
                        }
                        stdout::print_table(&["Package", "Tag"], &rows);
                    }
                    data::tag::TagCollectionData::WithPlatforms(tags) => {
                        for (package, platform_tags) in tags {
                            for (platform, platform_tags) in platform_tags {
                                for tag in platform_tags {
                                    rows[0].push(package.clone());
                                    rows[1].push(tag);
                                    rows[2].push(platform.clone());
                                }
                            }
                        }
                        stdout::print_table(&["Package", "Tag", "Platform"], &rows);
                    }
                }
            }
        }
        Ok(())
    }

    pub fn report_env(&self, env: data::env::EnvVars) -> anyhow::Result<()> {
        match self.format {
            options::Format::Json => {
                println!("{}", serde_json::to_string_pretty(&env)?);
            }
            options::Format::Plain => {
                let mut rows: [Vec<String>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                for entry in env.entries {
                    rows[0].push(entry.key);
                    rows[1].push(entry.value);
                    rows[2].push(entry.kind.to_string());
                }
                stdout::print_table(&["Key", "Value", "Type"], &rows);
            }
        }
        Ok(())
    }

    pub fn report_catalog(&self, catalog: data::catalog::Catalog) -> anyhow::Result<()> {
        match self.format {
            options::Format::Json => {
                println!("{}", serde_json::to_string_pretty(&catalog)?);
            }
            options::Format::Plain => {
                let mut rows: [Vec<String>; _] = [Vec::new(), Vec::new()];
                match catalog.repositories {
                    data::catalog::CatalogData::WithoutTags(repos) => {
                        for repo in repos {
                            rows[0].push(repo);
                        }
                        stdout::print_table(&["Repository"], &rows);
                    }
                    data::catalog::CatalogData::WithTags(tags) => {
                        for (repo, repo_tags) in tags {
                            for tag in repo_tags {
                                rows[0].push(repo.clone());
                                rows[1].push(tag);
                            }
                        }
                        stdout::print_table(&["Repository", "Tag"], &rows);
                    }
                }
            }
        }

        Ok(())
    }
}
