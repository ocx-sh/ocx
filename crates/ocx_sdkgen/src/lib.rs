// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The machine-interface contract as data: the representation subset every published document must stay inside,
//! the lint that holds `reports`, `errors` and `cli.json` to it, the differ that compares them with the last
//! release, and the generator that turns them into SDK sources.
//!
//! Operates on the documents as JSON only; it links no `ocx_*` crate.

use std::path::Path;

use serde_json::Value;

/// An id enum whose variant name is its published id: `ALL`, `id()`, `title()`, ordering by id and a `Deserialize`
/// that refuses an id outside the set, so an unknown id in a TOML table fails at parse.
macro_rules! id_enum {
    ($(#[$meta:meta])* $name:ident, $noun:literal, { $($variant:ident $title:literal,)+ }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $(#[doc = $title] $variant,)+
        }

        impl $name {
            /// Every id, in declaration order.
            pub const ALL: &[Self] = &[$(Self::$variant,)+];

            /// The published id, the variant name.
            pub const fn id(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant),)+
                }
            }

            /// The one-line title.
            pub const fn title(self) -> &'static str {
                match self {
                    $(Self::$variant => $title,)+
                }
            }

            /// The id's variant, `None` outside the set.
            pub fn from_id(id: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|item| item.id() == id)
            }
        }

        // Ordered by id, the order findings were sorted in while they held the id string.
        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }

        impl Ord for $name {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.id().cmp(other.id())
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.id())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let id = String::deserialize(deserializer)?;
                Self::from_id(&id).ok_or_else(|| serde::de::Error::custom(format!("unknown {} `{id}`", $noun)))
            }
        }
    };
}

pub mod compat;
pub mod ir;
pub mod lint;
pub mod rust;
pub mod subset;
pub mod versions;

/// The three gated documents, parsed: what the differ compares and the generator reads.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Documents {
    /// `reports.json`, the `--format json` report contract.
    pub reports: Value,
    /// `errors.json`, the error document with the exit-code and slug registries.
    pub errors: Value,
    /// `cli.json`, the command grammar with its output modes, env manifest and retired names.
    pub cli: Value,
}

impl Documents {
    /// Parses the three documents from their JSON text.
    ///
    /// # Errors
    ///
    /// A document that is not JSON.
    pub fn parse(reports: &str, errors: &str, cli: &str) -> Result<Self, serde_json::Error> {
        Ok(Self {
            reports: serde_json::from_str(reports)?,
            errors: serde_json::from_str(errors)?,
            cli: serde_json::from_str(cli)?,
        })
    }

    /// Reads `reports.json`, `errors.json` and `cli.json` from `directory`.
    ///
    /// # Errors
    ///
    /// A file that cannot be read or is not JSON; the message names it.
    pub fn read_dir(directory: &Path) -> Result<Self, ReadError> {
        let read = |name: &str| -> Result<Value, ReadError> {
            let path = directory.join(name);
            let text = std::fs::read_to_string(&path).map_err(|source| ReadError::Io {
                path: path.clone(),
                source,
            })?;
            serde_json::from_str(&text).map_err(|source| ReadError::Json { path, source })
        };
        Ok(Self {
            reports: read("reports.json")?,
            errors: read("errors.json")?,
            cli: read("cli.json")?,
        })
    }
}

/// Why a document directory cannot be read.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("cannot read {path}: {source}", path = path.display())]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("{path} is not JSON: {source}", path = path.display())]
    Json {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
}

/// The language of a generated SDK.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum Lang {
    Rust,
}

/// Why an SDK could not be generated.
#[derive(Debug, thiserror::Error)]
pub enum GenerateError {
    #[error(transparent)]
    Ir(#[from] ir::IrError),
    #[error("cannot write {path}: {source}", path = path.display())]
    Write {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
}

/// Writes the SDK for `documents` into `out`, creating it; a file the generator no longer emits is left alone.
///
/// # Errors
///
/// Documents outside the representation subset, or a directory or file that cannot be written.
pub fn generate_sdk(lang: Lang, documents: &Documents, out: &Path) -> Result<(), GenerateError> {
    let ir = ir::load(documents)?;
    let files = match lang {
        Lang::Rust => rust::generate(&ir),
    };
    let failed = |path: &Path, source| GenerateError::Write {
        path: path.to_owned(),
        source,
    };
    std::fs::create_dir_all(out).map_err(|source| failed(out, source))?;
    for file in files {
        let path = out.join(&file.path);
        std::fs::write(&path, file.contents).map_err(|source| failed(&path, source))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn names(directory: &Path) -> BTreeSet<String> {
        std::fs::read_dir(directory)
            .expect("a readable directory")
            .map(|entry| {
                entry
                    .expect("a readable entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn the_sdk_written_to_disk_is_the_committed_golden() {
        let out = tempfile::tempdir().expect("temp dir");
        generate_sdk(Lang::Rust, &ir::tests::documents(), out.path()).expect("the goldens generate");
        let golden = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/rust");
        let written = names(out.path());
        assert!(!written.is_empty(), "nothing was written");
        assert_eq!(written, names(&golden));
        for name in &written {
            assert!(
                std::fs::read(out.path().join(name)).expect("written")
                    == std::fs::read(golden.join(name)).expect("golden"),
                "{name} differs from the committed golden"
            );
        }
    }

    #[test]
    fn an_output_path_that_cannot_be_a_directory_names_itself_in_the_error() {
        let out = tempfile::NamedTempFile::new().expect("temp file");
        let failed = generate_sdk(Lang::Rust, &ir::tests::documents(), out.path()).expect_err("a file is no directory");
        assert!(
            matches!(&failed, GenerateError::Write { path, .. } if path == out.path()),
            "{failed:?}"
        );
    }
}
