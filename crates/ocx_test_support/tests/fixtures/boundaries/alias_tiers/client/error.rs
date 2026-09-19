// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the one declaration both client aliases name.

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("registry unreachable")]
    Unreachable,
}
