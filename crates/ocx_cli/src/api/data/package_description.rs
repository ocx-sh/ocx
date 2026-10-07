// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;

/// One package's repository description.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PackageDescription {
    /// Whether the repository publishes a description; when `false` the three
    /// fields below are absent.
    pub published: bool,
    /// The description's title annotation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The description's one-line summary annotation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The description's keywords annotation, as published.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    #[serde(skip)]
    identifier: ocx_oci::PackageRef,
}

impl PackageDescription {
    /// A package whose repository publishes a description.
    pub fn published(
        identifier: ocx_oci::PackageRef,
        title: Option<String>,
        description: Option<String>,
        keywords: Option<String>,
    ) -> Self {
        Self {
            published: true,
            title,
            description,
            keywords,
            identifier,
        }
    }

    /// A package whose repository publishes no description.
    pub fn absent(identifier: ocx_oci::PackageRef) -> Self {
        Self {
            published: false,
            title: None,
            description: None,
            keywords: None,
            identifier,
        }
    }

    /// The plain rendering of one entry of [`PackageDescriptions`].
    fn print_plain(&self) {
        if !self.published {
            println!("No description found for {}", self.identifier);
            return;
        }
        if let Some(title) = &self.title {
            println!("Title:       {title}");
        }
        if let Some(description) = &self.description {
            println!("Description: {description}");
        }
        if let Some(keywords) = &self.keywords {
            println!("Keywords:    {keywords}");
        }
    }
}

/// The descriptions of every requested package.
#[derive(Serialize, schemars::JsonSchema)]
pub struct PackageDescriptions {
    /// One entry per requested package, keyed by the identifier as given, in request order.
    #[serde(serialize_with = "as_map")]
    #[schemars(with = "std::collections::BTreeMap<String, PackageDescription>")]
    descriptions: Vec<(String, PackageDescription)>,
}

impl PackageDescriptions {
    pub fn new(descriptions: Vec<(String, PackageDescription)>) -> Self {
        Self { descriptions }
    }
}

/// Request order is the contract, so the entries serialize as a map without passing through a sorted one.
fn as_map<S: serde::Serializer>(entries: &[(String, PackageDescription)], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_map(entries.iter().map(|(key, description)| (key, description)))
}

impl Printable for PackageDescriptions {
    const SCHEMA_VERSION: u32 = 1;
    const ROOT: &'static str = "PackageDescriptions";

    fn print_plain(&self, _printer: &ocx_console::DataInterface) {
        for (key, description) in &self.descriptions {
            println!("== {key} ==");
            description.print_plain();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> ocx_oci::PackageRef {
        ocx_oci::PackageRef::parse_with_default_registry(name, "ocx.sh").expect("identifier")
    }

    /// Keys keep request order; a missing description is `published: false`,
    /// never `null`, and a published one omits the fields it does not carry.
    #[test]
    fn descriptions_sit_under_a_named_map_in_request_order() {
        let report = PackageDescriptions::new(vec![
            (
                "zeta:1".to_string(),
                PackageDescription::published(id("zeta:1"), Some("Zeta".into()), None, None),
            ),
            ("alpha:1".to_string(), PackageDescription::absent(id("alpha:1"))),
        ]);
        let json = serde_json::to_string(&report).expect("serialize");
        assert_eq!(
            json,
            r#"{"descriptions":{"zeta:1":{"published":true,"title":"Zeta"},"alpha:1":{"published":false}}}"#
        );
    }
}
