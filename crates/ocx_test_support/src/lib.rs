// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared test material, a dev-dependency only: boundary harness, fixture tree, named pipes, loopback
//! servers, minted TLS PKI.

pub mod boundary;
pub mod data;
#[cfg(unix)]
pub mod fifo;
pub mod net;
pub mod pki;

// Re-exported so a guard walking the syntax tree itself uses the same `syn` the harness parsed with.
pub use {proc_macro2, quote, syn};
