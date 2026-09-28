// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;

/// A single field in the description.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Inner {
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
}

/// Package description metadata (title, description, keywords).
pub struct PackageDescription {
    inner: Option<Inner>,
    identifier: ocx_oci::PackageRef,
}

impl PackageDescription {
    pub fn new(inner: Option<Inner>, identifier: ocx_oci::PackageRef) -> Self {
        Self { inner, identifier }
    }
}

impl Serialize for PackageDescription {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(serializer)
    }
}

impl Printable for PackageDescription {
    fn print_plain(&self, _printer: &ocx_console::DataInterface) {
        match &self.inner {
            Some(inner) => {
                if let Some(title) = &inner.title {
                    println!("Title:       {title}");
                }
                if let Some(description) = &inner.description {
                    println!("Description: {description}");
                }
                if let Some(keywords) = &inner.keywords {
                    println!("Keywords:    {keywords}");
                }
            }
            None => {
                println!("No description found for {}", self.identifier);
            }
        }
    }
}

/// [`PackageDescription`] views keyed by the raw request identifier, in input order, even for one package.
pub struct PackageDescriptions {
    entries: Vec<(String, PackageDescription)>,
}

impl PackageDescriptions {
    pub fn new(entries: Vec<(String, PackageDescription)>) -> Self {
        Self { entries }
    }
}

impl Serialize for PackageDescriptions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, description) in &self.entries {
            map.serialize_entry(key, description)?;
        }
        map.end()
    }
}

impl Printable for PackageDescriptions {
    fn print_plain(&self, printer: &ocx_console::DataInterface) {
        for (key, description) in &self.entries {
            println!("== {key} ==");
            description.print_plain(printer);
        }
    }
}

// Transparent `Serialize`: a package with no description is `null`, not `{}`.
impl schemars::JsonSchema for PackageDescription {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PackageDescription".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Option<Inner>>::json_schema(generator)
    }
}

// Hand-written: `Serialize` writes a map keyed by the request identifier, not the struct's fields.
impl schemars::JsonSchema for PackageDescriptions {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PackageDescriptions".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "additionalProperties": generator.subschema_for::<PackageDescription>(),
        })
    }
}
