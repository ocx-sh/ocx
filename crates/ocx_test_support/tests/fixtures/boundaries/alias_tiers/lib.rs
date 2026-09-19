// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Fixture: the crate root of a tier shaped like `ocx_oci` — four `Result`
//! aliases over three error types, none of them named `Error`.

pub mod auth;
pub mod client;
pub mod manifest;
pub mod platform;
