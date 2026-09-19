// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the first of two aliases naming ONE declaration by different
//! paths. This one writes the qualified `error::ClientError`; its sibling
//! `client/transport.rs` writes the bare `ClientError` it imported. Both must
//! land on `client/error.rs` — the collapse-versus-distinguish question the
//! `Error` key turned on, with the opposite right answer: here, merge.

mod error;
mod transport;

pub use error::ClientError;

pub type Result<T> = std::result::Result<T, error::ClientError>;

pub fn fetch() -> Result<Vec<u8>> {
    unimplemented!()
}
