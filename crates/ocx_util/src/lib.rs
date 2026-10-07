// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Domain-free primitives: fs, locking, extension traits, async singleflight, TLS roots, archive extraction, path-context error helpers.
//!
//! Depends on no `ocx_*` crate and knows no domain: what a package or registry is belongs elsewhere.
//! The process environment is read through `ocx_env`; this crate has no reader of its own:
//!
//! ```compile_fail
//! let _ = ocx_util::env::var("X");
//! ```

pub mod archive;
pub mod boolean_string;
pub mod child_process;
pub mod compression;
pub mod error;
pub mod fs;
pub mod list;
pub mod opaque;
pub mod path;
pub mod result_ext;
pub mod schema;
pub mod serde_ext;
pub mod singleflight;
pub mod size;
pub mod string_ext;
pub mod time;
pub mod tls;
pub mod vec_ext;
pub mod wire_words;

/// The extension traits every crate pulls in wholesale.
pub mod prelude {
    pub use crate::result_ext::ResultExt;
    pub use crate::serde_ext::SerdeExt;
    pub use crate::string_ext::StringExt;
    pub use crate::vec_ext::VecExt;
}
