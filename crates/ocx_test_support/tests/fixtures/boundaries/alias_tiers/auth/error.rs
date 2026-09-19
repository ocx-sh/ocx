// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the declaration both hops lead to.

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("no credentials")]
    Missing,
}

pub type Result<T> = std::result::Result<T, AuthError>;
