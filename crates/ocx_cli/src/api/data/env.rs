use ocx_lib::package::metadata::env::var::ModifierKind;
use serde::Serialize;

/// A single resolved environment variable entry, tagged with its modifier kind.
#[derive(Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    #[serde(rename = "type")]
    pub kind: ModifierKind,
}

/// Resolved environment variables for one or more packages, in declaration order.
///
/// Each entry carries its [`ModifierKind`] so callers can apply the correct operation:
/// - [`ModifierKind::Constant`] — replace any existing value for this key.
/// - [`ModifierKind::Path`]     — prepend to any existing value using the platform path separator.
///
/// An ordered list (rather than type-keyed maps) preserves declaration order, allows multiple
/// entries per key with different kinds, and naturally accommodates future modifier types.
pub struct EnvVars {
    pub entries: Vec<EnvEntry>,
}

impl EnvVars {
    pub fn new(entries: Vec<EnvEntry>) -> Self {
        Self { entries }
    }
}

impl Serialize for EnvVars {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.entries.serialize(serializer)
    }
}
