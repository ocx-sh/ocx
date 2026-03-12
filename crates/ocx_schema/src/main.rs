// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Generates the JSON Schema for OCX metadata.json and writes it to stdout.

use ocx_lib::package::metadata::Metadata;
use schemars::generate::SchemaSettings;

fn main() {
    let mut settings = SchemaSettings::draft2020_12();
    settings.meta_schema = Some("https://json-schema.org/draft/2020-12/schema".into());

    let generator = settings.into_generator();
    let schema = generator.into_root_schema_for::<Metadata>();

    // Serialize to a JSON value so we can inject $id
    let mut value = serde_json::to_value(&schema).expect("failed to serialize schema");
    if let Some(obj) = value.as_object_mut() {
        // Insert $id right after $schema
        obj.insert(
            "$id".to_owned(),
            serde_json::Value::String("https://ocx.sh/schemas/metadata/v1.json".to_owned()),
        );
    }

    let json = serde_json::to_string_pretty(&value).expect("failed to serialize schema");
    println!("{json}");
}
