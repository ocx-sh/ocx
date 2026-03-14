// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use crate::error::MirrorError;
use crate::source::url_index::RemoteIndex;
use schemars::generate::SchemaSettings;

#[derive(clap::Args)]
pub struct Schema {
    /// Schema to generate
    #[arg(value_enum)]
    pub target: SchemaTarget,
}

#[derive(clap::ValueEnum, Clone)]
pub enum SchemaTarget {
    UrlIndex,
}

impl Schema {
    pub async fn execute(&self) -> Result<(), MirrorError> {
        match self.target {
            SchemaTarget::UrlIndex => {
                let json = generate_schema::<RemoteIndex>("https://ocx.sh/schemas/url-index/v1.json");
                println!("{json}");
            }
        }
        Ok(())
    }
}

fn generate_schema<T: schemars::JsonSchema>(id: &str) -> String {
    let mut settings = SchemaSettings::draft2020_12();
    settings.meta_schema = Some("https://json-schema.org/draft/2020-12/schema".into());

    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<T>();

    let mut value = serde_json::to_value(&schema).expect("failed to serialize schema");
    if let Some(obj) = value.as_object_mut() {
        obj.insert("$id".to_owned(), serde_json::Value::String(id.to_owned()));
    }

    serde_json::to_string_pretty(&value).expect("failed to serialize schema")
}
