// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared test material — a dev-dependency of the crates whose tests need it,
//! never a normal dependency, and with no `ocx_*` dependency of its own
//! (C-025): the boundary-test harness (`boundary`), the fixture tree (`data`),
//! named pipes (`fifo`) and the minted TLS PKI (`pki`).

pub mod boundary;
pub mod data;
#[cfg(unix)]
pub mod fifo;
pub mod pki;

// The parser the harness reads source through, re-exported so a guard that
// walks the syntax tree itself pins the same version the harness parsed with.
// `quote` comes along because rendering a node back to tokens is part of that
// walk — a type with no path segment to name has to be reported somehow.
pub use {proc_macro2, quote, syn};
