// TODO: Remove this when we have more code in the library
#![allow(dead_code, deprecated)]

mod config;
mod media_type;

#[cfg(test)]
#[path="../test/mod.rs"]
pub(crate) mod test;

pub(crate) use media_type::*;

pub mod auth;
pub mod archive;
pub mod codesign;
pub mod compression;
pub mod env;
pub mod error;
pub mod file_lock;
pub mod file_structure;
pub mod package;
pub mod package_manager;
pub mod utility;
pub mod oci;
pub mod reference_manager;
pub mod symlink;
pub mod shell;
pub mod log;

pub use error::Error;
pub use error::ErrorExt;
pub use error::Result;

pub mod prelude {
    pub use crate::error::Error;
    pub use crate::error::Result;
    pub use crate::error::ErrorExt;
    
    pub use crate::utility::result_ext::ResultExt;
    pub use crate::utility::serde_ext::SerdeExt;
    pub use crate::utility::string_ext::StringExt;
    pub use crate::utility::vec_ext::VecExt;
}
