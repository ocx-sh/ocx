// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generates JSON Schemas for OCX types and writes them to stdout.

use ocx_lib::package::metadata::Metadata;
use ocx_lib::profile::ProfileManifest;
use schemars::generate::SchemaSettings;

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

fn main() {
    let schema_type = std::env::args().nth(1).unwrap_or_else(|| "metadata".to_string());

    match schema_type.as_str() {
        "metadata" => {
            println!(
                "{}",
                generate_schema::<Metadata>("https://ocx.sh/schemas/metadata/v1.json")
            );
        }
        "profile" => {
            println!(
                "{}",
                generate_schema::<ProfileManifest>("https://ocx.sh/schemas/profile/v1.json")
            );
        }
        other => {
            eprintln!("Unknown schema type: {other}. Available: metadata, profile");
            std::process::exit(1);
        }
    }
}
