// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! The `ocx` CLI as a library, so `ocx_schema` can derive the published report contract from [`api::data`].

pub mod api;
pub mod app;
pub mod build_receipt;
pub mod clap_parse;
pub mod command;
pub mod conventions;
pub mod error;
pub mod error_document;
pub mod exit;
pub mod options;
pub mod tracing_init;
