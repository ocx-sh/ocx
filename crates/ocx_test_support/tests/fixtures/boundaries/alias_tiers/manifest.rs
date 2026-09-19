// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the other right-hand-side spelling. Every real `ocx_oci` alias
//! writes `std::result::Result`; this one writes the bare `Result` the prelude
//! provides, so the scan is shown matching on the path's last segment rather
//! than on its text. The alias is named `ManifestResult` because a bare RHS
//! under the name `Result` would be self-referential and would not compile —
//! which makes this the renamed-alias case too.

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("unsupported media type")]
    Unsupported,
}

pub type ManifestResult<T> = Result<T, ManifestError>;

pub fn parse() -> ManifestResult<()> {
    unimplemented!()
}
