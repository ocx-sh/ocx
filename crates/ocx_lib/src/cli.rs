// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Shared CLI infrastructure for OCX binaries.
//!
//! Provides common types and utilities used by both `ocx` and `ocx-mirror`:
//! - [`LogLevel`] — granular log verbosity control via `--log-level`
//! - [`LogSettings`] — tracing subscriber setup with `OCX_LOG` env var cascade
//! - [`ExitCode`] — typed process exit codes aligned with BSD `sysexits.h`
//! - [`classify_error`] — free function mapping a [`std::error::Error`] chain to [`ExitCode`]

pub mod classify;
pub mod error;
pub mod exit_code;
mod log_level;
mod log_settings;
pub mod options;
mod printer;
mod styles;

pub use classify::{ClassifyExitCode, classify_error};
pub use error::UsageError;
pub use exit_code::ExitCode;
pub use log_level::LogLevel;
pub use log_settings::LogSettings;
pub use options::{ColorMode, ColorModeConfig, ProgressMode};
pub use printer::{Annotation, Printer, TreeItem};
pub use styles::clap_styles;

pub mod progress;
pub use indicatif;
pub use tracing_indicatif;
