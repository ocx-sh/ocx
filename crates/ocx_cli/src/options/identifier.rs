// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

use ocx_lib::{Result, oci};

#[derive(Clone, Debug)]
pub struct Identifier {
    raw: String,
}

impl Identifier {
    /// Constructs from a body whose OCI shape has already been validated.
    ///
    /// Used by sibling option parsers (e.g. `PackageRef`) that have already
    /// run [`oci::Identifier::from_str`] and want to store the validated raw
    /// string for later `with_domain` resolution without paying the cost of
    /// a second parse.
    pub(super) fn from_validated_raw(raw: impl Into<String>) -> Self {
        Self { raw: raw.into() }
    }

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
    type Err = ocx_lib::Error;

    fn from_str(s: &str) -> Result<Self> {
        oci::Identifier::from_str(s)?;
        Ok(Self { raw: s.to_string() })
    }
}
