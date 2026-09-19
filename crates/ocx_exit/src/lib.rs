// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Process-outcome vocabulary shared by every OCX binary and by a future SDK:
//! [`ExitCode`] and [`ErrorCategory`].
//!
//! The tier is **closed** (DEC-40), and it is the smallest closed tier in the
//! workspace. The crate exists for its *consumers*, not for its producer. An SDK that
//! shells out to `ocx` reads two things back — the process status and, under
//! `--format json`, the envelope's `error.kind` — and both are defined here,
//! with no dependency but `serde`. That is the whole surface: two types, one
//! path each.
//!
//! [`ErrorCategory`] lives beside [`ExitCode`] rather than in the binary for
//! one reason: `#[non_exhaustive]` binds only *downstream* crates, so the
//! classification match can be exhaustive with no wildcard here and nowhere
//! else. The compiler, not a hand-maintained table, is what forces every exit
//! code to be classified — see [`ErrorCategory::from_exit_code`].
//!
//! The two modules are private and each type is re-exported at the root, so
//! `ocx_exit::ExitCode` is the only spelling. A second path through a public
//! module would be a synonym to keep in step, not a capability.

mod error_category;
mod exit_code;

pub use error_category::ErrorCategory;
pub use exit_code::ExitCode;
