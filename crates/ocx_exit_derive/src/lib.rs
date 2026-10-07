// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! `#[derive(Classify)]`: an error type declares its exit code and `error.detail` slug next to the variant.
//!
//! Depend on `ocx_exit` and use `ocx_exit::Classify`; the derive implements `ocx_exit::ClassifyExitCode`
//! and `ocx_exit::ClassifyErrorKind` and names no other crate.
//!
#![doc = include_str!("../GRAMMAR.md")]

mod expand;
mod spec;

use proc_macro::TokenStream;
use syn::{DeriveInput, parse_macro_input};

/// Implements `ClassifyExitCode` and `ClassifyErrorKind` from the `#[exit(...)]` attributes.
///
/// The grammar is in the crate docs.
#[proc_macro_derive(Classify, attributes(exit))]
pub fn derive_classify(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand::expand(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
