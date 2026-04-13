// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

mod config;
mod media_type;

pub use config::loader::{ConfigInputs, ConfigLoader};
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
pub mod file_lock;
pub mod file_structure;
pub mod hardlink;
pub mod log;
pub mod oci;
pub mod package;
pub mod package_manager;
pub mod profile;
pub mod publisher;
pub mod reference_manager;
pub mod shell;
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
