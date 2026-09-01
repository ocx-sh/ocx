// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx` CLI as a library.
//!
//! `main.rs` is a thin shell over [`app::run`]. The library target exists so
//! non-shipping tooling can link the CLI's own types instead of re-declaring
//! them: `ocx_schema` derives the published report contract straight from
//! [`api::data`], which makes the wire format a compile-time fact rather than
//! a hand-maintained parallel document.

pub mod api;
pub mod app;
pub mod build_receipt;
pub mod command;
pub mod conventions;
pub mod error_envelope;
pub mod options;
