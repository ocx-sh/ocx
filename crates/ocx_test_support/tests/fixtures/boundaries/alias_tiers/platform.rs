// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: a `super::`-rooted alias import beside a local one, and an error
//! type named nothing like the module that holds it.

use super::client::Result as ClientResult;

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("unsupported platform")]
    Unsupported,
}

pub type Result<T> = std::result::Result<T, PlatformError>;

pub fn current() -> Result<String> {
    unimplemented!()
}

pub fn raw() -> ClientResult<Vec<u8>> {
    unimplemented!()
}
