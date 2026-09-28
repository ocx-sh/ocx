// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Process-outcome vocabulary shared by every OCX binary: [`ExitCode`] and [`ErrorCategory`].
//!
//! [`ErrorCategory`] stays beside [`ExitCode`]: in another crate `#[non_exhaustive]` forces a wildcard arm.

mod error_category;
mod exit_code;

pub use error_category::ErrorCategory;
pub use exit_code::ExitCode;
