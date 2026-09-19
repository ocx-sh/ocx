// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Presentation vocabulary shared by every OCX binary: tables and trees,
//! styling and themes, progress bars, and the per-stream colour decision.
//!
//! The tier is **closed**: [`DataInterface`] is the surface a command renders
//! through, and everything else here exists to build one or to be handed to
//! one. Nothing is exported because a single caller finds it convenient — a
//! future `ocx_api`, which drives the `ocx` binary rather than linking its
//! commands, reuses this vocabulary to read what the CLI emitted, so the
//! surface is shaped for that consumer.
//!
//! What is deliberately **not** here:
//!
//! - The process-outcome vocabulary. `ExitCode` and `ErrorCategory` are the
//!   seam an SDK links without linking a terminal, so they live in `ocx_exit`,
//!   which depends on nothing.
//! - Argument dispatch and the input-validation errors a command raises before
//!   it does any work. Those belong to the process that owns `main`, and live
//!   in `ocx_cli`.
//! - Any tracing-subscriber wiring. Installing a global subscriber is
//!   something only a process that owns `main` may do; this crate names no
//!   `tracing-subscriber` item at all, which is what lets its manifest drop the
//!   dependency. [`progress::LogWriter`] is a plain per-event buffer the binary
//!   adapts to whatever writer trait its subscriber asks for.
//! - Any domain knowledge. A palette paints the string it is handed; knowing
//!   what an `Identifier` is made of, or what a `private`/`interface` pair
//!   means, belongs to whoever owns that value.

mod data_interface;
mod human;
pub mod options;
mod printer;
mod styles;
mod theme;
mod user_interface;

pub use data_interface::{Annotation, Cell, Column, DataInterface, TreeItem};
pub use human::{human_bytes, human_instant, human_time};
pub use options::{ColorMode, ColorModeConfig, ProgressMode};
pub use printer::{Alignment, Line, Printer, Style};
pub use styles::clap_styles;
pub use theme::{Theme, VisibilityStyle};
pub use user_interface::UserInterface;

pub mod progress;
