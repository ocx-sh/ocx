// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

//! Installing this binary's tracing subscriber.
//!
//! Deciding *what* gets logged and *where it is written* is an application
//! concern: the verbosity flag is clap vocabulary, the `OCX_LOG` cascade is
//! this binary's contract, and only a process that owns `main` may install a
//! global subscriber at all. So the console library paints and suspends, and
//! the wiring that turns it into a `tracing_subscriber` layer lives here —
//! `ocx_console` names no `tracing-subscriber` item, and its manifest is what
//! enforces that once it is its own crate.
//!
//! [`ProgressLogWriter`] is the whole seam: it adapts the console's
//! bar-suspending writer to `MakeWriter` without the console knowing the trait
//! exists.

use ocx_console::progress::{LogWriter, LogWriterHandle};

mod log_level;
mod log_settings;

pub use log_level::LogLevel;
pub use log_settings::LogSettings;

/// Adapts the console's bar-suspending stderr writer to `tracing_subscriber`.
///
/// The fmt layer asks for one writer per event; [`LogWriter::handle`] hands
/// back a buffer that flushes inside `MultiProgress::suspend` on drop, so a log
/// line never tears an active bar. The trait impl lives on this newtype rather
/// than on `LogWriter` itself because the trait is `tracing-subscriber`'s, and
/// a console must not depend on the subscriber it happens to be rendered
/// through.
pub struct ProgressLogWriter(pub LogWriter);

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ProgressLogWriter {
    type Writer = LogWriterHandle;

    fn make_writer(&'a self) -> Self::Writer {
        self.0.handle()
    }
}
