// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the alias and its type both re-exported from a child module —
//! `ocx_oci::auth`'s shape. Nothing here shares a name with `AuthError`.

mod error;

pub use error::{AuthError, Result};

pub fn token() -> Result<String> {
    unimplemented!()
}
