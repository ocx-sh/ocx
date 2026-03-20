// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use serde::Serialize;

use crate::api::Printable;

/// A single field in the description.
#[derive(Serialize)]
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
    identifier: String,
}

impl PackageDescription {
    pub fn new(inner: Option<Inner>, identifier: String) -> Self {
        Self { inner, identifier }
    }
}

impl Serialize for PackageDescription {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(serializer)
    }
}

impl Printable for PackageDescription {
    fn print_plain(&self, _printer: &ocx_lib::cli::Printer) {
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
