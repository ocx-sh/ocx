// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the second alias on the same type, by the bare name it imported.

use super::error::ClientError;

pub type Result<T> = std::result::Result<T, ClientError>;

pub fn send() -> Result<()> {
    unimplemented!()
}
