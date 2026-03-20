// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared CLI infrastructure for OCX binaries.
//!
//! Provides common types and utilities used by both `ocx` and `ocx-mirror`:
//! - [`LogLevel`] — granular log verbosity control via `--log-level`
//! - [`LogSettings`] — tracing subscriber setup with `OCX_LOG` env var cascade

mod log_level;
mod log_settings;
pub mod options;
mod printer;
mod styles;

pub use log_level::LogLevel;
pub use log_settings::LogSettings;
pub use options::{ColorMode, ColorModeConfig, ProgressMode};
pub use printer::Printer;
pub use styles::clap_styles;

#[cfg(feature = "progress")]
pub use indicatif;
#[cfg(feature = "progress")]
pub use tracing_indicatif;
