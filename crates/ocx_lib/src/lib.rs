// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod config;
mod media_type;

/// Re-exported config-loader error type so crates that depend on `ocx_lib`
/// can reference `ocx_lib::ConfigError` without traversing the private
/// `config` module path. Used by `ocx_cli`'s `ProjectContextError::Config`
/// variant.
pub use config::error::Error as ConfigError;
pub use config::loader::{ConfigInputs, ConfigLoader};
pub use config::mirror::{MirrorConfig, MirrorConfigError, ParsedMirror, ResolvedMirrors, resolve_mirror_map};
pub use config::{Config, RegistryConfig, RegistryDefaults};

#[cfg(test)]
#[path = "../test/mod.rs"]
pub(crate) mod test;

pub(crate) use media_type::*;

pub mod archive;
pub mod auth;
pub mod ci;
pub mod cli;
pub mod codesign;
pub mod compression;
pub mod env;
pub mod error;
pub mod file_structure;
pub mod hardlink;
pub mod log;
pub mod oci;
pub mod package;
pub mod package_manager;
pub mod project;
pub mod publisher;
pub mod reference_manager;
pub mod script;
pub mod setup;
pub mod shell;
pub mod shim;
pub mod symlink;
pub mod utility;

pub use error::Error;
pub use error::Result;

pub mod prelude {
    pub use crate::error::Error;
    pub use crate::error::Result;

    pub use crate::utility::result_ext::ResultExt;
    pub use crate::utility::serde_ext::SerdeExt;
    pub use crate::utility::string_ext::StringExt;
    pub use crate::utility::vec_ext::VecExt;
}
