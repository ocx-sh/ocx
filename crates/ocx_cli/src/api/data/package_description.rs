// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use ocx_lib::oci;

use crate::api::Printable;

/// A single field in the description.
#[derive(Serialize, schemars::JsonSchema)]
pub struct Inner {
    pub title: Option<String>,
    pub description: Option<String>,
    pub keywords: Option<String>,
}

/// Package description metadata (title, description, keywords).
///
/// Plain format: key-value lines for non-None fields, or "No description found" message.
///
/// JSON format: flat object with title/description/keywords, or `null` when absent.
pub struct PackageDescription {
    inner: Option<Inner>,
    identifier: oci::Identifier,
}

impl PackageDescription {
    pub fn new(inner: Option<Inner>, identifier: oci::Identifier) -> Self {
        Self { inner, identifier }
    }
}

impl Serialize for PackageDescription {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(serializer)
    }
}

impl Printable for PackageDescription {
    fn print_plain(&self, _printer: &ocx_lib::cli::DataInterface) {
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

/// One or more [`PackageDescription`] views keyed by the requested identifier.
///
/// Plain format: a `== <id> ==` header line per package followed by its
/// description fields, in input order.
///
/// JSON format: object keyed by the raw request identifier
/// (`{"<id>": {…}|null}`), preserving input order — the same keyed-object shape
/// `which` uses, applied even for a single package.
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
    fn print_plain(&self, printer: &ocx_lib::cli::DataInterface) {
        for (key, description) in &self.entries {
            println!("== {key} ==");
            description.print_plain(printer);
        }
    }
}

// The `Serialize` impl above is transparent, so the published schema is the
// inner type's. A package with no description serializes as `null`, not as `{}`.
impl schemars::JsonSchema for PackageDescription {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PackageDescription".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <Option<Inner>>::json_schema(generator)
    }
}

// The `Serialize` impl above writes a map keyed by the argument the caller passed, not the struct's
// own fields, so the schema is hand-written to match it.
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
