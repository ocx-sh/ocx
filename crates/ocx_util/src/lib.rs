// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Domain-free primitives: fs, locking, extension traits, async singleflight, TLS roots, archive extraction, path-context error helpers.
//!
//! Depends on no `ocx_*` crate and knows no domain: what a package or registry is belongs elsewhere.

pub mod archive;
pub mod boolean_string;
pub mod child_process;
pub mod compression;
pub mod env;
pub mod error;
pub mod fs;
pub mod list;
pub mod path;
pub mod result_ext;
pub mod schema;
pub mod serde_ext;
pub mod singleflight;
pub mod string_ext;
pub mod tls;
pub mod vec_ext;

/// The extension traits every crate pulls in wholesale.
pub mod prelude {
    pub use crate::result_ext::ResultExt;
    pub use crate::serde_ext::SerdeExt;
    pub use crate::string_ext::StringExt;
    pub use crate::vec_ext::VecExt;
}
