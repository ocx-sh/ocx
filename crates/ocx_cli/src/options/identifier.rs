use ocx_lib::{Result, oci};

#[derive(Clone)]
pub struct Identifier {
    raw: String,
}

impl Identifier {
    pub fn with_domain(&self, domain: impl AsRef<str>) -> Result<oci::Identifier> {
        oci::Identifier::from_str_with_registry(&self.raw, domain.as_ref())
    }

    pub fn transform_all(identifiers: impl IntoIterator<Item = Self>, domain: impl AsRef<str>) -> Result<Vec<oci::Identifier>> {
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
        Ok(Self {
            raw: s.to_string(),
        })
    }
}