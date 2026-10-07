// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Process-outcome vocabulary shared by every OCX binary: [`ExitCode`] and [`ErrorCategory`].
//!
//! [`ErrorCategory`] stays beside [`ExitCode`]: in another crate `#[non_exhaustive]` forces a wildcard arm.
//! The classification traits and the [`families!`] registry macro live here too, naming no concrete error type.
//! [`Classify`] derives both traits from `#[exit(...)]` attributes; its grammar is on the derive's own page.
//!
//! The derive refuses, at compile time, what would leave a type unclassified; each refusal is a doctest on
//! the private `refusals` module.

mod classify;
mod error_category;
mod exit_code;
mod families;

pub use classify::{ClassifyErrorKind, ClassifyExitCode, Decision, Detail, DetailEntry, Pick, Row};
pub use error_category::ErrorCategory;
pub use exit_code::{ExitCode, RETIRED};
pub use ocx_exit_derive::Classify;

// One documented item per refusal, so each doctest keeps a block of its own.
#[cfg(doctest)]
mod refusals {
    /// A variant with no `#[exit]` does not compile:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::{Classify, Row};
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(NotFound, slug = "e_a", summary = "A")]
    ///     A,
    ///     B,
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_variant_without_exit() {}

    /// `delegate` on a field whose type lacks the classification traits does not compile (E0599):
    /// ```compile_fail,E0599
    /// # use std::fmt;
    /// # use ocx_exit::{Classify, Row};
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(delegate)]
    ///     A(Inner),
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn delegate_on_a_field_without_the_traits() {}

    /// An exit code name that `ExitCode` lacks does not compile (E0599):
    /// ```compile_fail,E0599
    /// # use std::fmt;
    /// # use ocx_exit::{Classify, Row};
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(NoSuchCode, slug = "e_a", summary = "A")]
    ///     A,
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn an_unknown_exit_code_name() {}

    /// A slug declared twice with a different code or summary does not compile:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::{Classify, Row};
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(NotFound, slug = "e_a", summary = "A")]
    ///     A,
    ///     #[exit(DataError, slug = "e_a", summary = "A")]
    ///     B,
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_slug_declared_twice() {}

    /// A `with` function must answer with a `Pick`; any other value does not compile (E0308):
    /// ```compile_fail,E0308
    /// # use std::fmt;
    /// # use ocx_exit::{Classify, Row};
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(with = pick, rows((NotFound, slug = "e_a", summary = "A")))]
    ///     A,
    /// }
    /// fn pick(_: &E, _: [Row; 1]) -> ocx_exit::ExitCode { ocx_exit::ExitCode::NotFound }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_with_function_that_is_not_a_pick() {}

    /// A key given twice in one `#[exit]` does not compile:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::Classify;
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(NotFound, slug = "e_a", slug = "e_b", summary = "A")]
    ///     A,
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_repeated_key() {}

    /// The type-level `family` given twice does not compile:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::Classify;
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// #[exit(family = "One", family = "Two")]
    /// enum E {
    ///     #[exit(NotFound, slug = "e_a", summary = "A")]
    ///     A,
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_repeated_family() {}

    /// A second exit code in one row does not compile:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::Classify;
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(NotFound, DataError, slug = "e_a", summary = "A")]
    ///     A,
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_surplus_exit_code() {}

    /// A row whose code is `Success` does not compile:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::Classify;
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(Success, slug = "e_a", summary = "A")]
    ///     A,
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_success_row() {}

    /// `reserve` on a delegating struct does not compile: its `DETAILS` is the field type's:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::Classify;
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// #[exit(NotFound, slug = "leaf_a", summary = "A")]
    /// struct Leaf;
    /// # impl fmt::Display for Leaf { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("leaf") } }
    /// # impl std::error::Error for Leaf {}
    /// #[derive(Debug, Classify)]
    /// #[exit(delegate = kind, reserve(Failure, slug = "e_r", summary = "R"))]
    /// struct E { kind: Leaf }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn reserve_on_a_delegating_struct() {}

    /// A plain `chain` with a fallback other than `Failure` does not compile: its code is never answered:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::Classify;
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(chain, fallback(NotFound, slug = "e_a", summary = "A"))]
    ///     A(Inner),
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn a_plain_chain_fallback_that_is_not_failure() {}

    /// `defer` inside `fallback(...)` does not compile:
    /// ```compile_fail
    /// # use std::fmt;
    /// # use ocx_exit::Classify;
    /// # #[derive(Debug)] struct Inner;
    /// # impl fmt::Display for Inner { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("inner") } }
    /// # impl std::error::Error for Inner {}
    /// #[derive(Debug, Classify)]
    /// enum E {
    ///     #[exit(chain, fallback(defer(Failure), slug = "e_a", summary = "A"))]
    ///     A(Inner),
    /// }
    /// # impl fmt::Display for E { fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str("e") } }
    /// # impl std::error::Error for E {}
    /// ```
    fn defer_inside_a_fallback() {}
}
