// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{Result, oci};

#[derive(Clone, Debug)]
pub struct Identifier {
    raw: String,
}

impl Identifier {
    pub fn with_domain(&self, domain: impl AsRef<str>) -> Result<oci::Identifier> {
        Ok(oci::Identifier::parse_with_default_registry(
            &self.raw,
            domain.as_ref(),
        )?)
    }

    pub fn transform_all(
        identifiers: impl IntoIterator<Item = Self>,
        domain: impl AsRef<str>,
    ) -> Result<Vec<oci::Identifier>> {
        let domain = domain.as_ref();
        identifiers.into_iter().map(|id| id.with_domain(domain)).collect()
    }

    pub fn transform_optional(identifier: Option<Self>, domain: impl AsRef<str>) -> Result<Option<oci::Identifier>> {
        match identifier {
            Some(id) => Ok(Some(id.with_domain(domain.as_ref())?)),
            None => Ok(None),
        }
    }

    /// Rejects a batch that names the same package twice.
    ///
    /// Commands that emit an identifier-keyed report (`package inspect`,
    /// `package info`) must not receive duplicate references: the keyed shape
    /// would otherwise drop a result row (inspect collapses duplicates through
    /// `drain_package_tasks`) or emit a duplicate JSON key (info). Returns a
    /// usage error (exit 64) naming the first duplicate.
    pub fn reject_duplicate_references(identifiers: &[oci::Identifier]) -> anyhow::Result<()> {
        let mut seen = std::collections::HashSet::new();
        for identifier in identifiers {
            if !seen.insert(identifier.to_string()) {
                return Err(ocx_lib::cli::UsageError::new(format!("duplicate package reference: {identifier}")).into());
            }
        }
        Ok(())
    }

    pub fn raw(&self) -> &str {
        &self.raw
    }
}

impl std::fmt::Display for Identifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl std::str::FromStr for Identifier {
    type Err = oci::IdentifierError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        oci::Identifier::from_str(s)?;
        Ok(Self { raw: s.to_string() })
    }
}
