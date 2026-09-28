// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Presentation vocabulary shared by every OCX binary: tables and trees, styling and themes, progress bars,
//! and the per-stream colour decision. [`DataInterface`] is the surface a command renders through.

#[cfg(any(test, feature = "__testing"))]
pub mod capture;
mod data_interface;
mod human;
pub mod options;
mod printer;
mod styles;
mod theme;
mod user_interface;

pub use data_interface::{Annotation, Cell, Column, DataInterface, TreeItem};
pub use human::{human_bytes, human_instant, human_time};
pub use options::{ColorMode, ColorModeConfig, Format, FormatMode, ProgressMode};
pub use printer::{Alignment, Line, Printer, Style};
pub use styles::clap_styles;
pub use theme::{Theme, VisibilityStyle};
pub use user_interface::UserInterface;

pub mod progress;
